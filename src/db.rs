//! The SQLite database that `sensors analyze` imports log lines into.
//!
//! Every line of every log file becomes a row in `events`, apart from blank
//! lines and day changed lines, whose date is stored on the lines after them.
//! The `files` table records how far each file has been imported, so later
//! runs only read the lines added since. A file that shrank or whose first
//! bytes changed was replaced, and is imported again from the start.
//!
//! The importer comes from wot, the web of trust tools for IRC communities,
//! without wot's networks.
//!
//! The reading tables are filled by the `readings` module, and built again
//! from the events whenever needed.
//!
//! The `channels` table holds the channels to process, which nothing
//! rebuilds.

use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::NaiveDate;
use rusqlite::{Connection, OptionalExtension, params};

use crate::discover::{LogFile, TargetKind};
use crate::irssi::{self, DateTracker, Event, Line};
use crate::progress::{Progress, Unit};
use crate::report::thousands;

pub type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

/// How long a write waits for another one to finish before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How many bytes from the start of each file are kept to recognize it.
const HEAD_LEN: u64 = 1024;

/// The schema, as the steps that build it. SQLite's `user_version` stores how
/// many steps a database has had, so an older database is brought up to date
/// by running the steps it is missing.
const MIGRATIONS: [&str; 5] = [SCHEMA_V1, SCHEMA_V2, SCHEMA_V3, SCHEMA_V4, SCHEMA_V5];

const SCHEMA_V1: &str = "
CREATE TABLE files (
    id INTEGER PRIMARY KEY,
    -- Absolute path of the log file.
    path TEXT NOT NULL UNIQUE,
    -- The network tag folder the file was found in.
    tag TEXT NOT NULL,
    target TEXT NOT NULL,
    -- 'channel' or 'query'.
    kind TEXT NOT NULL,
    -- Import resumes from here. Only complete lines are imported.
    bytes_read INTEGER NOT NULL DEFAULT 0,
    lines_read INTEGER NOT NULL DEFAULT 0,
    -- The date in effect after the last imported line, as YYYY-MM-DD.
    date TEXT,
    -- The first bytes of the file, to notice when it was replaced.
    head BLOB NOT NULL DEFAULT x'',
    -- UTC, as YYYY-MM-DD HH:MM:SS.
    imported_at TEXT
);

CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    file_id INTEGER NOT NULL REFERENCES files (id),
    -- Line number in the file, starting at 1.
    line INTEGER NOT NULL,
    -- YYYY-MM-DD, or NULL before the first line giving a date.
    date TEXT,
    -- HH:MM for events, HH:MM:SS for log_opened and log_closed, NULL for
    -- unparsed lines.
    time TEXT,
    kind TEXT NOT NULL,
    nick TEXT,
    ident TEXT,
    host TEXT,
    other_nick TEXT,
    text TEXT,
    detail TEXT
);

CREATE INDEX events_by_file_date ON events (file_id, date);
CREATE INDEX events_by_file_line ON events (file_id, line);
";

const SCHEMA_V2: &str = "
-- Readings, built from the JSON lines sensors post, after each import. The
-- readings module describes how. Everything here can be built again from
-- the events.

-- Each measure from each sensor, as its lines name them.
CREATE TABLE sensor_series (
    id INTEGER PRIMARY KEY,
    -- The network tag folder and channel of the log file.
    tag TEXT NOT NULL,
    channel TEXT NOT NULL,
    -- The nick that posted the lines.
    nick TEXT NOT NULL,
    -- The lines' sensor field, or '' when they have none.
    sensor TEXT NOT NULL,
    -- The field holding the value, such as 'temp'.
    measure TEXT NOT NULL,
    UNIQUE (tag, channel, nick, sensor, measure)
);

-- One value of one measure. A second value at the same time is left out.
CREATE TABLE readings (
    series_id INTEGER NOT NULL REFERENCES sensor_series (id),
    -- The line's timestamp, in milliseconds since 1970 UTC.
    at INTEGER NOT NULL,
    value REAL NOT NULL,
    PRIMARY KEY (series_id, at)
) WITHOUT ROWID;

-- The readings of each measure in each hour, so graphs of long stretches
-- read a row an hour.
CREATE TABLE reading_hours (
    series_id INTEGER NOT NULL REFERENCES sensor_series (id),
    -- Hours since 1970 UTC: at / 3600000.
    hour INTEGER NOT NULL,
    readings INTEGER NOT NULL,
    -- The sum of the values, for the mean over any stretch of hours.
    total REAL NOT NULL,
    low REAL NOT NULL,
    high REAL NOT NULL,
    PRIMARY KEY (series_id, hour)
) WITHOUT ROWID;

-- JSON lines with a timestamp and no readings, such as testBegin.
CREATE TABLE reading_markers (
    id INTEGER PRIMARY KEY,
    tag TEXT NOT NULL,
    channel TEXT NOT NULL,
    nick TEXT NOT NULL,
    -- The line's timestamp, in milliseconds since 1970 UTC.
    at INTEGER NOT NULL,
    -- The whole JSON line.
    text TEXT NOT NULL,
    UNIQUE (tag, channel, nick, at, text)
);

CREATE INDEX reading_markers_by_time ON reading_markers (at);

-- The logs' UTC offset, from the difference between each line's timestamp
-- and the time irssi logged it: how many lines in each hour had each offset.
CREATE TABLE log_offsets (
    -- Hours since 1970 UTC.
    hour INTEGER NOT NULL,
    -- Minutes the logs are ahead of UTC, such as -420 for UTC-7.
    minutes INTEGER NOT NULL,
    lines INTEGER NOT NULL,
    PRIMARY KEY (hour, minutes)
) WITHOUT ROWID;

-- The last event read for readings, so the next build reads only newer ones.
CREATE TABLE reading_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_event_id INTEGER NOT NULL
);
";

const SCHEMA_V3: &str = "
-- Messages and actions said in each channel each day, counted with the
-- readings, so the history page reads a row a day instead of every line.
CREATE TABLE channel_days (
    -- The network tag folder and channel of the log file.
    tag TEXT NOT NULL,
    channel TEXT NOT NULL,
    -- YYYY-MM-DD, in the logs' time zone.
    date TEXT NOT NULL,
    lines INTEGER NOT NULL,
    PRIMARY KEY (tag, channel, date)
) WITHOUT ROWID;

-- Count the lines readings were already read from. Later builds add the
-- lines imported after them.
INSERT INTO channel_days (tag, channel, date, lines)
SELECT f.tag, f.target, e.date, count(*)
FROM events e JOIN files f ON f.id = e.file_id
WHERE e.kind IN ('message', 'action') AND f.kind = 'channel' AND e.date IS NOT NULL
  AND e.id <= (SELECT ifnull(max(last_event_id), 0) FROM reading_state)
GROUP BY f.tag, f.target, e.date;
";

const SCHEMA_V4: &str = "
-- The channels to process, as sensors analyze --channel saved them. Only
-- their log files are imported; the next analysis removes the rest.
CREATE TABLE channels (
    name TEXT PRIMARY KEY COLLATE NOCASE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO channels (name) VALUES ('#greenhouse');
";

const SCHEMA_V5: &str = "
-- Readings a sensor reported when its hardware misbehaved, found while the
-- readings are built, as the faults module describes. Hours, lines and the
-- latest reading leave them out.
CREATE TABLE reading_faults (
    series_id INTEGER NOT NULL REFERENCES sensor_series (id),
    -- The reading's time, in milliseconds since 1970 UTC, as in readings.
    at INTEGER NOT NULL,
    value REAL NOT NULL,
    -- 'range' when out of range, 'spike' when it jumped away and back.
    reason TEXT NOT NULL CHECK (reason IN ('range', 'spike')),
    PRIMARY KEY (series_id, at)
) WITHOUT ROWID;

-- Without a build state, the next sensors analyze reads every line again and
-- finds the faults in readings already read.
DELETE FROM reading_state;
";

const INSERT_EVENT: &str = "
INSERT INTO events (file_id, line, date, time, kind, nick, ident, host, other_nick, text, detail)
VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
";

/// Opens the database, creating it when the file is new and adding any tables
/// an older database is missing.
pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let latest = MIGRATIONS.len() as i64;
    if !(0..=latest).contains(&version) {
        return Err(format!(
            "it uses database schema version {version}, but this version of sensors only \
             knows up to {latest}. Use a newer sensors, or pass a different --db"
        )
        .into());
    }
    if version > 0 && version < latest {
        // Some steps index or empty large tables, which takes a while.
        eprintln!(
            "Upgrading the database from schema version {version} to {latest}, which can take \
             a minute for a large one..."
        );
    }
    for (step, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
        let version = step + 1;
        conn.execute_batch(&format!(
            "BEGIN; {sql} PRAGMA user_version = {version}; COMMIT;"
        ))?;
    }
    // Write-ahead logging, so pages keep reading what was there before while
    // an analysis writes, and see its changes once it commits. It stays set
    // in the file. Writers wait for each other a while rather than failing
    // at once.
    let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

#[derive(Default)]
pub struct ImportSummary {
    pub new_files: usize,
    pub grown_files: usize,
    pub replaced_files: usize,
    pub unchanged_files: usize,
    /// Lines read from the log files during this import.
    pub lines: u64,
    pub errors: Vec<(PathBuf, String)>,
}

/// What the `files` table holds about a file imported before.
struct StoredFile {
    id: i64,
    bytes_read: i64,
    lines_read: i64,
    /// The date in effect after the last imported line, as YYYY-MM-DD.
    date: Option<String>,
    /// The file's first bytes when it was last imported.
    head: Vec<u8>,
}

enum Outcome {
    New,
    Grown,
    Replaced,
    Unchanged,
}

/// Imports quietly, for tests.
#[cfg(test)]
pub fn import(conn: &mut Connection, files: &[LogFile]) -> ImportSummary {
    import_with_progress(conn, files, &mut Progress::quiet())
}

/// Lines between progress reports while a file is read.
const PROGRESS_LINES: u64 = 10_000;

/// Imports the lines added to each file since the last import, reporting
/// progress by bytes as it goes. Each file is imported in its own
/// transaction, so an interrupted import resumes cleanly.
pub fn import_with_progress(
    conn: &mut Connection,
    files: &[LogFile],
    progress: &mut Progress,
) -> ImportSummary {
    let mut summary = ImportSummary::default();
    let total: u64 = files.iter().map(|file| file.bytes).sum();
    progress.stage("Importing log files", total, Unit::Bytes);
    let mut finished: u64 = 0;
    for (index, file) in files.iter().enumerate() {
        let lines_before = summary.lines;
        let mut tick = |offset: i64, lines: u64| {
            progress.update(finished + offset.max(0) as u64, || {
                format!(
                    "file {} of {}, {} new lines",
                    thousands(index as u64 + 1),
                    thousands(files.len() as u64),
                    thousands(lines_before + lines)
                )
            });
        };
        let result = import_file(conn, file, &mut tick);
        finished += file.bytes;
        if let Ok((_, lines)) = &result {
            let lines = lines_before + lines;
            progress.update(finished, || {
                format!(
                    "file {} of {}, {} new lines",
                    thousands(index as u64 + 1),
                    thousands(files.len() as u64),
                    thousands(lines)
                )
            });
        }
        match result {
            Ok((outcome, lines)) => {
                summary.lines += lines;
                match outcome {
                    Outcome::New => summary.new_files += 1,
                    Outcome::Grown => summary.grown_files += 1,
                    Outcome::Replaced => summary.replaced_files += 1,
                    Outcome::Unchanged => summary.unchanged_files += 1,
                }
            }
            Err(err) => summary.errors.push((file.path.clone(), err.to_string())),
        }
    }
    progress.done(&format!("checked {} files", thousands(files.len() as u64)));
    summary
}

/// Imports one file and returns what happened and how many lines were read.
/// `tick` hears the offset reached and the lines read so far, now and then.
fn import_file(
    conn: &mut Connection,
    file: &LogFile,
    tick: &mut dyn FnMut(i64, u64),
) -> Result<(Outcome, u64)> {
    let path = fs::canonicalize(&file.path)
        .unwrap_or_else(|_| file.path.clone())
        .to_string_lossy()
        .into_owned();

    let mut handle = File::open(&file.path)?;
    // Sizes, offsets and line numbers are i64, SQLite's integer type.
    let len = i64::try_from(handle.metadata()?.len())?;
    let mut head = Vec::new();
    (&mut handle).take(HEAD_LEN).read_to_end(&mut head)?;

    let tx = conn.transaction()?;
    let stored = tx
        .query_row(
            "SELECT id, bytes_read, lines_read, date, head FROM files WHERE path = ?1",
            params![path],
            |row| {
                Ok(StoredFile {
                    id: row.get(0)?,
                    bytes_read: row.get(1)?,
                    lines_read: row.get(2)?,
                    date: row.get(3)?,
                    head: row.get(4)?,
                })
            },
        )
        .optional()?;

    let (file_id, outcome, mut offset, mut line_number, mut dates) = match stored {
        None => {
            tx.execute(
                "INSERT INTO files (path, tag, target, kind) VALUES (?1, ?2, ?3, ?4)",
                params![path, file.tag, file.target, kind_name(file.kind)],
            )?;
            let id = tx.last_insert_rowid();
            (id, Outcome::New, 0, 0, DateTracker::default())
        }
        Some(stored) if len >= stored.bytes_read && head.starts_with(&stored.head) => {
            if len == stored.bytes_read {
                return Ok((Outcome::Unchanged, 0));
            }
            let date = stored
                .date
                .map(|date| date.parse::<NaiveDate>())
                .transpose()?;
            let dates = DateTracker::starting_at(date);
            (
                stored.id,
                Outcome::Grown,
                stored.bytes_read,
                stored.lines_read,
                dates,
            )
        }
        Some(stored) => {
            tx.execute("DELETE FROM events WHERE file_id = ?1", params![stored.id])?;
            (stored.id, Outcome::Replaced, 0, 0, DateTracker::default())
        }
    };

    handle.seek(SeekFrom::Start(u64::try_from(offset)?))?;
    let mut reader = BufReader::with_capacity(64 * 1024, handle);
    let mut buf = Vec::new();
    let mut lines = 0;
    {
        let mut insert = tx.prepare_cached(INSERT_EVENT)?;
        loop {
            buf.clear();
            let read = reader.read_until(b'\n', &mut buf)?;
            if read == 0 || buf.last() != Some(&b'\n') {
                // End of file, or a last line irssi has not finished writing.
                // It is imported once it is complete.
                break;
            }
            offset += i64::try_from(read)?;
            line_number += 1;
            lines += 1;
            if lines % PROGRESS_LINES == 0 {
                tick(offset, lines);
            }

            // Old logs may not be valid UTF-8.
            let text = String::from_utf8_lossy(&buf);
            let line = irssi::parse_line(&text);
            dates.update(&line);

            if let Some(row) = event_row(&line, &text) {
                insert.execute(params![
                    file_id,
                    line_number,
                    dates.date().map(|date| date.to_string()),
                    row.time,
                    row.kind,
                    row.nick,
                    row.ident,
                    row.host,
                    row.other_nick,
                    row.text,
                    row.detail,
                ])?;
            }
        }
    }

    tx.execute(
        "UPDATE files
         SET bytes_read = ?1, lines_read = ?2, date = ?3, head = ?4, imported_at = datetime('now')
         WHERE id = ?5",
        params![
            offset,
            line_number,
            dates.date().map(|date| date.to_string()),
            head,
            file_id
        ],
    )?;
    tx.commit()?;

    // A file that only gained part of a line has nothing new yet.
    let outcome = match outcome {
        Outcome::Grown if lines == 0 => Outcome::Unchanged,
        other => other,
    };
    Ok((outcome, lines))
}

fn kind_name(kind: TargetKind) -> &'static str {
    match kind {
        TargetKind::Channel => "channel",
        TargetKind::Query => "query",
    }
}

/// The columns of one `events` row, apart from the file, line and date.
#[derive(Default)]
struct EventRow<'a> {
    time: Option<String>,
    kind: &'static str,
    nick: Option<&'a str>,
    ident: Option<&'a str>,
    host: Option<&'a str>,
    other_nick: Option<&'a str>,
    text: Option<&'a str>,
    detail: Option<&'a str>,
}

/// The row to store for a line, or `None` for lines that are not stored.
fn event_row<'a>(line: &Line<'a>, raw: &'a str) -> Option<EventRow<'a>> {
    let row = match line {
        Line::Blank | Line::DayChanged(_) => return None,
        Line::LogOpened(at) => EventRow {
            time: Some(at.format("%H:%M:%S").to_string()),
            kind: "log_opened",
            ..EventRow::default()
        },
        Line::LogClosed(at) => EventRow {
            time: Some(at.format("%H:%M:%S").to_string()),
            kind: "log_closed",
            ..EventRow::default()
        },
        Line::Unrecognized => EventRow {
            kind: "unparsed",
            text: Some(raw.trim_end()),
            ..EventRow::default()
        },
        Line::Event(time, event) => EventRow {
            time: Some(time.format("%H:%M").to_string()),
            ..event_columns(event)
        },
    };
    Some(row)
}

/// The columns for an event. `docs/database.md` lists what each column holds
/// for each kind.
fn event_columns<'a>(event: &Event<'a>) -> EventRow<'a> {
    let row = EventRow::default();
    match *event {
        Event::Message { mode, nick, text } => EventRow {
            kind: "message",
            nick: Some(nick),
            text: Some(text),
            detail: mode_str(mode),
            ..row
        },
        Event::Action { nick, text } => EventRow {
            kind: "action",
            nick: Some(nick),
            text: Some(text),
            ..row
        },
        Event::Notice { nick, text } => EventRow {
            kind: "notice",
            nick: Some(nick),
            text: Some(text),
            ..row
        },
        Event::ServerNotice { server, text } => EventRow {
            kind: "server_notice",
            text: Some(text),
            detail: Some(server),
            ..row
        },
        Event::Join {
            nick,
            ident,
            host,
            channel,
        } => EventRow {
            kind: "join",
            nick: Some(nick),
            ident: Some(ident),
            host: Some(host),
            detail: Some(channel),
            ..row
        },
        Event::Part {
            nick,
            ident,
            host,
            channel,
            reason,
        } => EventRow {
            kind: "part",
            nick: Some(nick),
            ident: Some(ident),
            host: Some(host),
            text: Some(reason),
            detail: Some(channel),
            ..row
        },
        Event::Quit {
            nick,
            ident,
            host,
            reason,
        } => EventRow {
            kind: "quit",
            nick: Some(nick),
            ident: Some(ident),
            host: Some(host),
            text: Some(reason),
            ..row
        },
        Event::Kick {
            nick,
            channel,
            by,
            reason,
        } => EventRow {
            kind: "kick",
            nick: Some(nick),
            other_nick: Some(by),
            text: Some(reason),
            detail: Some(channel),
            ..row
        },
        Event::NickChange { old, new } => EventRow {
            kind: "nick_change",
            nick: Some(old),
            other_nick: Some(new),
            ..row
        },
        Event::OwnNickChange { new } => EventRow {
            kind: "own_nick_change",
            other_nick: Some(new),
            ..row
        },
        Event::Topic {
            nick,
            channel,
            topic,
        } => EventRow {
            kind: "topic",
            nick: Some(nick),
            text: Some(topic),
            detail: Some(channel),
            ..row
        },
        Event::TopicUnset { nick, channel } => EventRow {
            kind: "topic_unset",
            nick: Some(nick),
            detail: Some(channel),
            ..row
        },
        Event::Mode {
            channel,
            modes,
            by,
            server,
        } => EventRow {
            kind: if server { "server_mode" } else { "mode" },
            nick: Some(by),
            text: Some(modes),
            detail: Some(channel),
            ..row
        },
        Event::Netsplit { servers, nicks } => EventRow {
            kind: "netsplit",
            text: Some(nicks),
            detail: Some(servers),
            ..row
        },
        Event::NetsplitOver { nicks } => EventRow {
            kind: "netsplit_over",
            text: Some(nicks),
            ..row
        },
        Event::Client { text } => EventRow {
            kind: "client",
            text: Some(text),
            ..row
        },
    }
}

fn mode_str(mode: Option<char>) -> Option<&'static str> {
    Some(match mode? {
        '@' => "@",
        '+' => "+",
        '%' => "%",
        '~' => "~",
        '&' => "&",
        '!' => "!",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::Write;

    use super::*;
    use crate::discover::{self, tests::TempDir};

    const LOG: &str = "--- Log opened Tue Nov 03 13:45:10 2026\n\
                       13:46 -!- greenhouse [pi@greenhouse.example] has joined #greenhouse\n\
                       13:46 < greenhouse> {\"testBegin\":true,\"timestamp\":\"2026-11-03T20:45:23.061Z\"}\n\
                       13:47 -!- Something new happened\n\
                       --- Day changed Wed Nov 04 2026\n";

    fn setup(name: &str) -> (TempDir, PathBuf, Connection) {
        let root = TempDir::new(name);
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        let log = root.0.join("wetfish/#greenhouse.log");
        fs::write(&log, LOG).unwrap();
        let conn = open(&root.0.join("sensors.sqlite")).unwrap();
        (root, log, conn)
    }

    fn run(root: &TempDir, conn: &mut Connection) -> ImportSummary {
        let inventory = discover::discover(std::slice::from_ref(&root.0)).unwrap();
        let summary = import(conn, &inventory.files);
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        summary
    }

    fn append(path: &Path, text: &str) {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    /// "line date time kind nick" for every event in line order, with `-`
    /// for empty columns.
    fn events(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT line || ' ' || ifnull(date, '-') || ' ' || ifnull(time, '-')
                        || ' ' || kind || ' ' || ifnull(nick, '-')
                 FROM events ORDER BY line",
            )
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    }

    #[test]
    fn imports_every_line_with_its_date() {
        let (root, _, mut conn) = setup("db-import");
        let summary = run(&root, &mut conn);
        assert_eq!(summary.new_files, 1);
        assert_eq!(summary.lines, 5);

        assert_eq!(
            events(&conn),
            [
                "1 2026-11-03 13:45:10 log_opened -",
                "2 2026-11-03 13:46 join greenhouse",
                "3 2026-11-03 13:46 message greenhouse",
                "4 2026-11-03 - unparsed -",
            ]
        );

        let message: String = conn
            .query_row(
                "SELECT text FROM events WHERE kind = 'message'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            message,
            "{\"testBegin\":true,\"timestamp\":\"2026-11-03T20:45:23.061Z\"}"
        );
    }

    #[test]
    fn imports_only_new_complete_lines() {
        let (root, log, mut conn) = setup("db-grow");
        run(&root, &mut conn);

        let summary = run(&root, &mut conn);
        assert_eq!((summary.unchanged_files, summary.lines), (1, 0));

        // A line irssi has not finished writing waits for its newline.
        append(&log, "00:05 < greenhouse> {\"sensor\":\"in");
        let summary = run(&root, &mut conn);
        assert_eq!((summary.unchanged_files, summary.lines), (1, 0));

        append(&log, "side\"}\n");
        let summary = run(&root, &mut conn);
        assert_eq!((summary.grown_files, summary.lines), (1, 1));

        // The date from the day changed line carries over between imports.
        let events = events(&conn);
        assert_eq!(events.len(), 5);
        assert_eq!(events[4], "6 2026-11-04 00:05 message greenhouse");
    }

    #[test]
    fn imports_a_replaced_file_again() {
        let (root, log, mut conn) = setup("db-replace");
        run(&root, &mut conn);

        fs::write(
            &log,
            "--- Log opened Thu Nov 05 09:00:00 2026\n09:00 < greenhouse> {}\n",
        )
        .unwrap();
        let summary = run(&root, &mut conn);
        assert_eq!((summary.replaced_files, summary.lines), (1, 2));

        assert_eq!(
            events(&conn),
            [
                "1 2026-11-05 09:00:00 log_opened -",
                "2 2026-11-05 09:00 message greenhouse",
            ]
        );
    }

    #[test]
    fn upgrades_an_older_database() {
        let root = TempDir::new("db-upgrade");
        let path = root.0.join("sensors.sqlite");
        Connection::open(&path)
            .unwrap()
            .execute_batch(&format!("{SCHEMA_V1} PRAGMA user_version = 1;"))
            .unwrap();

        let conn = open(&path).unwrap();
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, MIGRATIONS.len() as i64);
        let rows: i64 = conn
            .query_row(
                "SELECT (SELECT count(*) FROM sensor_series) + (SELECT count(*) FROM readings)
                        + (SELECT count(*) FROM reading_hours)
                        + (SELECT count(*) FROM reading_markers)
                        + (SELECT count(*) FROM log_offsets)
                        + (SELECT count(*) FROM reading_state)
                        + (SELECT count(*) FROM channel_days)
                        + (SELECT count(*) FROM reading_faults)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
        let channels: String = conn
            .query_row("SELECT group_concat(name) FROM channels", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(channels, "#greenhouse");
    }

    #[test]
    fn counts_the_days_of_lines_already_read_when_upgrading() {
        let (root, log, mut conn) = setup("db-upgrade-days");
        append(
            &log,
            "00:05 < greenhouse> {}\n00:06  * rachel checks the vents\n",
        );
        run(&root, &mut conn);
        crate::readings::build(&mut conn, false).unwrap();
        let days = |conn: &Connection| -> Vec<(String, i64)> {
            let mut stmt = conn
                .prepare("SELECT date, lines FROM channel_days ORDER BY date")
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        let built = days(&conn);
        assert_eq!(
            built,
            [("2026-11-03".to_string(), 1), ("2026-11-04".to_string(), 2)]
        );

        // The upgrade counts the same days from the lines already read.
        conn.execute_batch(&format!("DROP TABLE channel_days; {SCHEMA_V3}"))
            .unwrap();
        assert_eq!(days(&conn), built);
    }

    #[test]
    fn refuses_a_database_from_another_schema_version() {
        let root = TempDir::new("db-version");
        let path = root.0.join("sensors.sqlite");
        open(&path)
            .unwrap()
            .execute_batch("PRAGMA user_version = 99;")
            .unwrap();
        assert!(open(&path).is_err());
    }
}

//! Readings: the values sensors post to IRC as JSON lines, such as
//!
//! ```text
//! 13:52 < greenhouse> {"sensor":"inside","temp":"67.46","humid":"38.80","timestamp":"2026-11-03T20:52:43.986Z"}
//! ```
//!
//! Each channel message holding a JSON object is read. Its `sensor` field
//! names the sensor and its `timestamp` field says when, and every other
//! field holding a number, or a string that is one, is a reading of the
//! measure it is named after. The line above gives a `temp` and a `humid`
//! reading from the `inside` sensor. A JSON object with a timestamp and no
//! readings, such as `{"testBegin":true,...}`, is kept as a marker.
//!
//! The same pass counts the messages and actions said in each channel each
//! day, in `channel_days`, for the history page.
//!
//! Readings are built from the events after each import. Only the events
//! added since the last build are read, unless a file was replaced or a
//! rebuild was asked for, when everything is read again. Each hour's
//! count, mean, lowest and highest value are kept in `reading_hours`, so
//! graphs of months or years read a row an hour instead of every reading.
//!
//! Timestamps are UTC, while the logs are in the logging computer's time
//! zone. The difference between a line's timestamp and the time irssi
//! logged it gives the logs' UTC offset, kept for each hour in
//! `log_offsets`, so times can be shown as the logs show them, across
//! daylight saving changes.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::Value;

use crate::db::Result;
use crate::progress::{Progress, Unit};
use crate::report::thousands;

/// Milliseconds in an hour, the size of a `reading_hours` row.
pub const HOUR: i64 = 3_600_000;

/// UTC offsets are rounded to this many minutes. Every time zone in use is a
/// whole number of quarter hours from UTC.
const OFFSET_STEP: i64 = 15;

/// The farthest any time zone is from UTC, in minutes.
const MAX_OFFSET: i64 = 14 * 60;

/// Fields that describe a line rather than hold a reading.
const SENSOR_FIELD: &str = "sensor";
const TIMESTAMP_FIELD: &str = "timestamp";

/// Events read between progress reports.
const PROGRESS_EVENTS: u64 = 10_000;

/// What a channel message holds.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    /// A JSON object with at least one reading.
    Readings {
        /// The `sensor` field, or empty when there is none.
        sensor: String,
        /// The `timestamp` field in milliseconds since 1970 UTC, or `None`
        /// when it is missing or not an RFC 3339 time.
        at: Option<i64>,
        /// Each measure and its value, in field name order.
        values: Vec<(String, f64)>,
    },
    /// A JSON object without readings, such as `{"testBegin":true,...}`.
    Marker { at: Option<i64> },
    /// Anything else, such as a person talking in the channel.
    Other,
}

/// Reads one channel message.
pub fn parse(text: &str) -> Parsed {
    let text = text.trim();
    if !text.starts_with('{') {
        return Parsed::Other;
    }
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(text) else {
        return Parsed::Other;
    };

    let sensor = match fields.get(SENSOR_FIELD) {
        Some(Value::String(sensor)) => sensor.trim().to_string(),
        _ => String::new(),
    };
    let at = match fields.get(TIMESTAMP_FIELD) {
        Some(Value::String(at)) => DateTime::parse_from_rfc3339(at.trim())
            .ok()
            .map(|at| at.timestamp_millis()),
        _ => None,
    };
    let values: Vec<(String, f64)> = fields
        .iter()
        .filter(|(name, _)| *name != SENSOR_FIELD && *name != TIMESTAMP_FIELD)
        .filter_map(|(name, value)| Some((name.clone(), number(value)?)))
        .collect();

    if values.is_empty() {
        Parsed::Marker { at }
    } else {
        Parsed::Readings { sensor, at, values }
    }
}

/// A field's value as a finite number: a JSON number, or a string holding
/// one, such as `"67.46"`.
fn number(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

/// The logs' UTC offset in minutes, from a line's timestamp and the date
/// and `HH:MM` time irssi logged it at, or `None` when they are too far
/// apart to be one.
///
/// irssi logs the minute a line arrived, up to a minute after its
/// timestamp's second, and usually a few seconds after the sensor sent it.
/// Half a minute is added for that before rounding to a quarter hour.
pub fn utc_offset(at: i64, date: NaiveDate, time: NaiveTime) -> Option<i64> {
    let logged = date.and_time(time).and_utc().timestamp_millis();
    let difference = (logged - at + 30_000) as f64 / 60_000.0;
    let minutes = (difference / OFFSET_STEP as f64).round() as i64 * OFFSET_STEP;
    (minutes.abs() <= MAX_OFFSET).then_some(minutes)
}

/// The logs' UTC offset over time, as read from `log_offsets`.
pub struct Offsets {
    /// The hour each offset starts at, and the offset in minutes, by hour.
    changes: Vec<(i64, i64)>,
}

impl Offsets {
    /// Reads the offsets, taking the offset most lines had in each hour.
    pub fn load(conn: &Connection) -> Result<Offsets> {
        let mut stmt = conn
            .prepare("SELECT hour, minutes FROM log_offsets ORDER BY hour, lines DESC, minutes")?;
        let mut rows = stmt.query([])?;
        let mut changes: Vec<(i64, i64)> = Vec::new();
        let mut last_hour = None;
        while let Some(row) = rows.next()? {
            let (hour, minutes): (i64, i64) = (row.get(0)?, row.get(1)?);
            if last_hour == Some(hour) {
                // A less common offset in an hour already counted.
                continue;
            }
            last_hour = Some(hour);
            if changes.last().is_none_or(|&(_, last)| last != minutes) {
                changes.push((hour, minutes));
            }
        }
        Ok(Offsets { changes })
    }

    /// One offset for all times, for tests.
    #[cfg(test)]
    pub fn fixed(minutes: i64) -> Offsets {
        Offsets {
            changes: vec![(0, minutes)],
        }
    }

    /// The UTC offset in minutes at a time in milliseconds since 1970 UTC:
    /// the one in effect then, the first one for times before any, or 0
    /// when there are none.
    pub fn minutes_at(&self, at: i64) -> i64 {
        let hour = at.div_euclid(HOUR);
        match self.changes.partition_point(|&(from, _)| from <= hour) {
            0 => self.changes.first().map_or(0, |&(_, minutes)| minutes),
            index => self.changes[index - 1].1,
        }
    }

    /// A time in milliseconds since 1970 UTC, as the logs would show it.
    pub fn local(&self, at: i64) -> Option<NaiveDateTime> {
        let shifted = at.checked_add(self.minutes_at(at) * 60_000)?;
        DateTime::from_timestamp_millis(shifted).map(|at| at.naive_utc())
    }
}

/// What a build of the readings did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct BuildSummary {
    /// Channel messages read.
    pub messages: u64,
    /// Readings added.
    pub readings: u64,
    /// Readings left out because their measure already had one at that
    /// time, such as a line posted twice.
    pub repeated: u64,
    /// Markers added.
    pub markers: u64,
    /// Lines with readings or a marker but no usable timestamp, left out.
    pub untimed: u64,
}

/// Builds the readings quietly, for tests.
#[cfg(test)]
pub fn build(conn: &mut Connection, full: bool) -> Result<BuildSummary> {
    build_with_progress(conn, full, &mut Progress::quiet())
}

/// Reads the readings in the channel messages imported since the last
/// build, or in all of them when `full` is set, in one transaction.
pub fn build_with_progress(
    conn: &mut Connection,
    full: bool,
    progress: &mut Progress,
) -> Result<BuildSummary> {
    let tx = conn.transaction()?;
    let last: i64 = if full {
        tx.execute_batch(
            "DELETE FROM readings;
             DELETE FROM reading_hours;
             DELETE FROM reading_markers;
             DELETE FROM log_offsets;
             DELETE FROM sensor_series;
             DELETE FROM channel_days;
             DELETE FROM reading_state;",
        )?;
        0
    } else {
        tx.query_row(
            "SELECT last_event_id FROM reading_state WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(0)
    };
    let newest: i64 = tx.query_row("SELECT ifnull(max(id), 0) FROM events", [], |row| {
        row.get(0)
    })?;

    let label = if full {
        "Reading sensor lines again"
    } else {
        "Reading sensor lines"
    };
    progress.stage(
        label,
        u64::try_from(newest - last).unwrap_or(0),
        Unit::Items("lines"),
    );
    let mut builder = Builder::load(&tx)?;
    builder.read_events(&tx, last, newest, progress)?;
    builder.save_hours(&tx)?;
    builder.save_offsets(&tx)?;
    builder.save_days(&tx)?;
    tx.execute(
        "INSERT INTO reading_state (id, last_event_id) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET last_event_id = excluded.last_event_id",
        params![newest],
    )?;
    tx.commit()?;

    let summary = builder.summary;
    progress.done(&format!(
        "{} messages, {} new readings, {} new markers",
        thousands(summary.messages),
        thousands(summary.readings),
        thousands(summary.markers),
    ));
    Ok(summary)
}

/// The key of a series: tag, channel, nick, sensor and measure.
type SeriesKey = (String, String, String, String, String);

/// State while readings are built.
struct Builder {
    /// The id of each series already in the database.
    series: HashMap<SeriesKey, i64>,
    /// The series and hours given new readings, whose rows in
    /// `reading_hours` are counted again.
    touched: HashSet<(i64, i64)>,
    /// Lines seen in each hour with each UTC offset.
    offsets: HashMap<(i64, i64), i64>,
    /// Messages and actions said in each tag, channel and day.
    days: HashMap<(String, String, String), i64>,
    summary: BuildSummary,
}

impl Builder {
    fn load(tx: &Transaction<'_>) -> Result<Builder> {
        let mut stmt =
            tx.prepare("SELECT id, tag, channel, nick, sensor, measure FROM sensor_series")?;
        let series = stmt
            .query_map([], |row| {
                Ok((
                    (
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ),
                    row.get(0)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Builder {
            series,
            touched: HashSet::new(),
            offsets: HashMap::new(),
            days: HashMap::new(),
            summary: BuildSummary::default(),
        })
    }

    /// Reads the channel messages and actions with ids after `last`, up to
    /// `newest`, counting them by day. Only messages can hold readings.
    fn read_events(
        &mut self,
        tx: &Transaction<'_>,
        last: i64,
        newest: i64,
        progress: &mut Progress,
    ) -> Result<()> {
        let mut events = tx.prepare(
            "SELECT e.id, f.tag, f.target, e.nick, e.date, e.time, e.text, e.kind
             FROM events e JOIN files f ON f.id = e.file_id
             WHERE e.id > ?1 AND e.id <= ?2 AND e.kind IN ('message', 'action')
               AND f.kind = 'channel'
             ORDER BY e.id",
        )?;
        let mut insert_reading = tx.prepare_cached(
            "INSERT OR IGNORE INTO readings (series_id, at, value) VALUES (?1, ?2, ?3)",
        )?;
        let mut insert_marker = tx.prepare_cached(
            "INSERT OR IGNORE INTO reading_markers (tag, channel, nick, at, text)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;

        let mut rows = events.query(params![last, newest])?;
        let mut read: u64 = 0;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let tag: String = row.get(1)?;
            let channel: String = row.get(2)?;
            let nick: Option<String> = row.get(3)?;
            let date: Option<String> = row.get(4)?;
            let time: Option<String> = row.get(5)?;
            let text: Option<String> = row.get(6)?;
            let kind: String = row.get(7)?;

            read += 1;
            if read.is_multiple_of(PROGRESS_EVENTS) {
                let readings = self.summary.readings;
                progress.update(u64::try_from(id - last).unwrap_or(0), || {
                    format!("{} readings", thousands(readings))
                });
            }

            if let Some(date) = &date {
                *self
                    .days
                    .entry((tag.clone(), channel.clone(), date.clone()))
                    .or_default() += 1;
            }
            if kind != "message" {
                continue;
            }
            self.summary.messages += 1;
            let (Some(nick), Some(text)) = (nick, text) else {
                continue;
            };
            let at = match parse(&text) {
                Parsed::Other => continue,
                Parsed::Marker { at: None } | Parsed::Readings { at: None, .. } => {
                    self.summary.untimed += 1;
                    continue;
                }
                Parsed::Marker { at: Some(at) } => {
                    let added = insert_marker.execute(params![tag, channel, nick, at, text])?;
                    self.summary.markers += added as u64;
                    at
                }
                Parsed::Readings {
                    sensor,
                    at: Some(at),
                    values,
                } => {
                    for (measure, value) in values {
                        let key = (
                            tag.clone(),
                            channel.clone(),
                            nick.clone(),
                            sensor.clone(),
                            measure,
                        );
                        let series = self.series_id(tx, key)?;
                        if insert_reading.execute(params![series, at, value])? == 1 {
                            self.summary.readings += 1;
                            self.touched.insert((series, at.div_euclid(HOUR)));
                        } else {
                            self.summary.repeated += 1;
                        }
                    }
                    at
                }
            };

            let logged = date
                .and_then(|date| date.parse::<NaiveDate>().ok())
                .zip(time.and_then(|time| NaiveTime::parse_from_str(&time, "%H:%M").ok()));
            if let Some((date, time)) = logged
                && let Some(minutes) = utc_offset(at, date, time)
            {
                *self
                    .offsets
                    .entry((at.div_euclid(HOUR), minutes))
                    .or_default() += 1;
            }
        }
        Ok(())
    }

    /// The id of a series, adding it when it is new.
    fn series_id(&mut self, tx: &Transaction<'_>, key: SeriesKey) -> Result<i64> {
        if let Some(&id) = self.series.get(&key) {
            return Ok(id);
        }
        tx.execute(
            "INSERT INTO sensor_series (tag, channel, nick, sensor, measure)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![key.0, key.1, key.2, key.3, key.4],
        )?;
        let id = tx.last_insert_rowid();
        self.series.insert(key, id);
        Ok(id)
    }

    /// Counts each hour given new readings again, from all its readings.
    fn save_hours(&self, tx: &Transaction<'_>) -> Result<()> {
        let mut count = tx.prepare_cached(
            "INSERT OR REPLACE INTO reading_hours (series_id, hour, readings, total, low, high)
             SELECT ?1, ?2, count(*), sum(value), min(value), max(value)
             FROM readings WHERE series_id = ?1 AND at >= ?3 AND at < ?4",
        )?;
        for &(series, hour) in &self.touched {
            count.execute(params![series, hour, hour * HOUR, (hour + 1) * HOUR])?;
        }
        Ok(())
    }

    /// Adds the messages and actions said each day to `channel_days`.
    fn save_days(&self, tx: &Transaction<'_>) -> Result<()> {
        let mut add = tx.prepare_cached(
            "INSERT INTO channel_days (tag, channel, date, lines) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (tag, channel, date) DO UPDATE SET lines = lines + excluded.lines",
        )?;
        for ((tag, channel, date), &lines) in &self.days {
            add.execute(params![tag, channel, date, lines])?;
        }
        Ok(())
    }

    /// Adds the lines seen with each UTC offset to `log_offsets`.
    fn save_offsets(&self, tx: &Transaction<'_>) -> Result<()> {
        let mut add = tx.prepare_cached(
            "INSERT INTO log_offsets (hour, minutes, lines) VALUES (?1, ?2, ?3)
             ON CONFLICT (hour, minutes) DO UPDATE SET lines = lines + excluded.lines",
        )?;
        for (&(hour, minutes), &lines) in &self.offsets {
            add.execute(params![hour, minutes, lines])?;
        }
        Ok(())
    }
}

/// One measure from one sensor, with its readings so far.
pub struct Series {
    pub channel: String,
    pub nick: String,
    /// Empty when the lines have no `sensor` field.
    pub sensor: String,
    pub measure: String,
    pub readings: i64,
    /// The first and last reading's time, in milliseconds since 1970 UTC.
    pub first: Option<i64>,
    pub last: Option<i64>,
    /// The last reading's value.
    pub latest: Option<f64>,
}

/// Every series, by channel, nick, sensor and measure.
pub fn series(conn: &Connection) -> Result<Vec<Series>> {
    let mut stmt = conn.prepare(
        "SELECT s.channel, s.nick, s.sensor, s.measure,
                (SELECT ifnull(sum(readings), 0) FROM reading_hours h WHERE h.series_id = s.id),
                (SELECT min(at) FROM readings r WHERE r.series_id = s.id),
                (SELECT max(at) FROM readings r WHERE r.series_id = s.id),
                (SELECT value FROM readings r WHERE r.series_id = s.id ORDER BY at DESC LIMIT 1)
         FROM sensor_series s
         ORDER BY s.channel, s.nick, s.sensor, s.measure, s.tag",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Series {
                channel: row.get(0)?,
                nick: row.get(1)?,
                sensor: row.get(2)?,
                measure: row.get(3)?,
                readings: row.get(4)?,
                first: row.get(5)?,
                last: row.get(6)?,
                latest: row.get(7)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::path::Path;

    use super::*;
    use crate::db;
    use crate::discover::{self, tests::TempDir};

    /// Milliseconds since 1970 UTC for a UTC time on 2026-11-03.
    fn utc(hour: u32, minute: u32, second: u32, milli: u32) -> i64 {
        NaiveDate::from_ymd_opt(2026, 11, 3)
            .unwrap()
            .and_hms_milli_opt(hour, minute, second, milli)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    #[test]
    fn reads_numbers_and_numeric_strings_as_readings() {
        let parsed = parse(
            r#"{"sensor":"inside","temp":"67.46","humid":38.8,"ok":true,"note":"dry","timestamp":"2026-11-03T20:52:43.986Z"}"#,
        );
        assert_eq!(
            parsed,
            Parsed::Readings {
                sensor: "inside".into(),
                at: Some(utc(20, 52, 43, 986)),
                values: vec![("humid".into(), 38.8), ("temp".into(), 67.46)],
            }
        );
    }

    #[test]
    fn keeps_lines_without_readings_as_markers() {
        assert_eq!(
            parse(r#"{"testBegin":true,"timestamp":"2026-11-03T20:45:23.061Z"}"#),
            Parsed::Marker {
                at: Some(utc(20, 45, 23, 61))
            }
        );
        assert_eq!(parse(r#"{"testBegin":true}"#), Parsed::Marker { at: None });
    }

    #[test]
    fn leaves_out_everything_else() {
        assert_eq!(parse("good morning"), Parsed::Other);
        assert_eq!(parse("{not json"), Parsed::Other);
        assert_eq!(parse("[1, 2]"), Parsed::Other);
        assert_eq!(
            parse(r#"{"temp":"NaN","humid":"","timestamp":"2026-11-03T20:52:43Z"}"#),
            Parsed::Marker {
                at: Some(utc(20, 52, 43, 0))
            }
        );
        assert_eq!(
            parse(r#"{"temp":"67.46","timestamp":"yesterday"}"#),
            Parsed::Readings {
                sensor: String::new(),
                at: None,
                values: vec![("temp".into(), 67.46)],
            }
        );
    }

    #[test]
    fn finds_the_utc_offset_from_the_logged_time() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 3).unwrap();
        let time = |hour, minute| NaiveTime::from_hms_opt(hour, minute, 0).unwrap();
        // Logged at 13:52 in Denver, sent at 20:52:43 UTC.
        assert_eq!(
            utc_offset(utc(20, 52, 43, 986), date, time(13, 52)),
            Some(-420)
        );
        // Logged the minute after it was sent.
        assert_eq!(
            utc_offset(utc(20, 45, 23, 61), date, time(13, 46)),
            Some(-420)
        );
        // India is five and a half hours ahead.
        assert_eq!(utc_offset(utc(8, 0, 10, 0), date, time(13, 30)), Some(330));
        // Too far apart to be an offset.
        assert_eq!(utc_offset(utc(0, 0, 0, 0), date, time(20, 0)), None);
    }

    #[test]
    fn follows_offset_changes_by_hour() {
        let offsets = Offsets {
            changes: vec![(10, -360), (20, -420)],
        };
        assert_eq!(offsets.minutes_at(5 * HOUR), -360);
        assert_eq!(offsets.minutes_at(10 * HOUR), -360);
        assert_eq!(offsets.minutes_at(20 * HOUR - 1), -360);
        assert_eq!(offsets.minutes_at(20 * HOUR + 1), -420);
        assert_eq!(Offsets { changes: vec![] }.minutes_at(0), 0);
        assert_eq!(
            offsets.local(utc(20, 52, 43, 0)).unwrap().to_string(),
            "2026-11-03 13:52:43"
        );
    }

    const LOG: &str = r#"--- Log opened Tue Nov 03 13:45:10 2026
13:46 < greenhouse> {"testBegin":true,"timestamp":"2026-11-03T20:45:23.061Z"}
13:46 < greenhouse> {"sensor":"outside","temp":"65.30","humid":"46.00","timestamp":"2026-11-03T20:46:42.998Z"}
13:46 < greenhouse> {"sensor":"inside","temp":"68.54","humid":"35.50","timestamp":"2026-11-03T20:46:43.051Z"}
13:47 < greenhouse> {"sensor":"outside","temp":"65.48","humid":"45.80","timestamp":"2026-11-03T20:47:13.078Z"}
13:47 < greenhouse> {"sensor":"inside","temp":"68.54","humid":"35.50","timestamp":"2026-11-03T20:47:13.131Z"}
13:47 < rachel> looks good
13:47  * rachel waters the tomatoes
"#;

    fn setup(name: &str) -> (TempDir, Connection) {
        let root = TempDir::new(name);
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(root.0.join("wetfish/#greenhouse.log"), LOG).unwrap();
        let mut conn = db::open(&root.0.join("sensors.sqlite")).unwrap();
        import(&root, &mut conn);
        (root, conn)
    }

    fn import(root: &TempDir, conn: &mut Connection) {
        let inventory = discover::discover(std::slice::from_ref(&root.0)).unwrap();
        let summary = db::import(conn, &inventory.files);
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    }

    fn append(path: &Path, text: &str) {
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    /// "sensor measure readings mean low high" for each series' hours.
    fn hours(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare(
                "SELECT s.sensor || ' ' || s.measure || ' ' || h.readings || ' '
                        || printf('%.2f', h.total / h.readings) || ' ' || h.low || ' ' || h.high
                 FROM reading_hours h JOIN sensor_series s ON s.id = h.series_id
                 ORDER BY s.sensor, s.measure, h.hour",
            )
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap()
    }

    #[test]
    fn builds_readings_hours_markers_and_offsets() {
        let (_root, mut conn) = setup("readings-build");
        let summary = build(&mut conn, false).unwrap();
        assert_eq!(
            summary,
            BuildSummary {
                messages: 6,
                readings: 8,
                repeated: 0,
                markers: 1,
                untimed: 0,
            }
        );
        assert_eq!(
            hours(&conn),
            [
                "inside humid 2 35.50 35.5 35.5",
                "inside temp 2 68.54 68.54 68.54",
                "outside humid 2 45.90 45.8 46.0",
                "outside temp 2 65.39 65.3 65.48",
            ]
        );

        let offsets = Offsets::load(&conn).unwrap();
        assert_eq!(offsets.changes, [(utc(20, 0, 0, 0) / HOUR, -420)]);

        let series = series(&conn).unwrap();
        let described: Vec<String> = series
            .iter()
            .map(|s| format!("{} {} {} {}", s.channel, s.sensor, s.measure, s.readings))
            .collect();
        assert_eq!(
            described,
            [
                "#greenhouse inside humid 2",
                "#greenhouse inside temp 2",
                "#greenhouse outside humid 2",
                "#greenhouse outside temp 2",
            ]
        );
        assert_eq!(series[0].first, Some(utc(20, 46, 43, 51)));
        assert_eq!(series[0].last, Some(utc(20, 47, 13, 131)));
        assert_eq!(series[3].latest, Some(65.48));

        // The action counts as a line of the day, but not as a message.
        let days: (String, String, i64) = conn
            .query_row("SELECT channel, date, lines FROM channel_days", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(days, ("#greenhouse".into(), "2026-11-03".into(), 7));
    }

    #[test]
    fn reads_only_new_lines_and_skips_repeats() {
        let (root, mut conn) = setup("readings-incremental");
        build(&mut conn, false).unwrap();

        // Nothing new.
        let summary = build(&mut conn, false).unwrap();
        assert_eq!((summary.messages, summary.readings), (0, 0));

        let log = root.0.join("wetfish/#greenhouse.log");
        append(
            &log,
            "13:48 < greenhouse> {\"sensor\":\"inside\",\"temp\":\"68.36\",\"humid\":\"37.80\",\"timestamp\":\"2026-11-03T20:48:43.369Z\"}\n\
             13:48 < greenhouse> {\"sensor\":\"inside\",\"temp\":\"68.36\",\"humid\":\"37.80\",\"timestamp\":\"2026-11-03T20:48:43.369Z\"}\n\
             13:49 < greenhouse> {\"sensor\":\"inside\",\"temp\":\"68.18\"}\n",
        );
        import(&root, &mut conn);
        let summary = build(&mut conn, false).unwrap();
        assert_eq!(
            summary,
            BuildSummary {
                messages: 3,
                readings: 2,
                repeated: 2,
                markers: 0,
                untimed: 1,
            }
        );
        assert_eq!(hours(&conn)[1], "inside temp 3 68.48 68.36 68.54");

        // Reading everything again gives the same hours and days.
        let lines = |conn: &Connection| -> i64 {
            conn.query_row("SELECT sum(lines) FROM channel_days", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(lines(&conn), 10);
        let before = hours(&conn);
        let summary = build(&mut conn, true).unwrap();
        assert_eq!((summary.readings, summary.repeated), (10, 2));
        assert_eq!(hours(&conn), before);
        assert_eq!(lines(&conn), 10);
    }
}

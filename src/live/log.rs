//! Log files as irssi writes them, so `sensors live` keeps the same kind of
//! logs as an irssi client in the channel would, and imports them the same
//! way: `<folder>/<network>/<channel>.log`, with irssi's default formats.
//!
//! Times are written in the time zone `sensors live` is given, as irssi
//! writes them in its computer's, so the logs' UTC offset can be worked out
//! from them, and graphs show times as people there read clocks.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use chrono_tz::Tz;

/// irssi's `log_open_string`, `log_close_string` and `log_day_changed`, and
/// its `log_timestamp`, which `irssi.rs` reads.
const OPENED: &str = "--- Log opened %a %b %d %H:%M:%S %Y";
const CLOSED: &str = "--- Log closed %a %b %d %H:%M:%S %Y";
const DAY_CHANGED: &str = "--- Day changed %a %b %d %Y";
const TIMESTAMP: &str = "%H:%M";

/// The log files of a network's channels.
pub struct Logs {
    /// `<folder>/<network>`.
    folder: PathBuf,
    zone: Tz,
    /// The logs being written, by folded channel name.
    open: HashMap<String, Log>,
}

struct Log {
    file: File,
    path: PathBuf,
    /// The date of the last line written, as the logs show dates.
    date: NaiveDate,
}

impl Logs {
    /// The logs of `network` in `folder`, creating its folder.
    pub fn new(folder: &Path, network: &str, zone: Tz) -> io::Result<Logs> {
        let folder = folder.join(network);
        fs::create_dir_all(&folder)?;
        Ok(Logs {
            folder,
            zone,
            open: HashMap::new(),
        })
    }

    /// The path of a channel's log file.
    pub fn path(&self, channel: &str) -> PathBuf {
        self.folder.join(format!("{}.log", file_name(channel)))
    }

    /// The local time of a moment, as the logs show times.
    fn local(&self, at: DateTime<Utc>) -> NaiveDateTime {
        at.with_timezone(&self.zone).naive_local()
    }

    /// Starts logging a channel, as irssi does on joining it, with a log
    /// opened line. A channel already logged carries on as it is.
    pub fn open(&mut self, channel: &str, at: DateTime<Utc>) -> io::Result<()> {
        let key = fold(channel);
        if self.open.contains_key(&key) {
            return Ok(());
        }
        let path = self.path(channel);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let local = self.local(at);
        file.write_all(format!("{}\n", local.format(OPENED)).as_bytes())?;
        self.open.insert(
            key,
            Log {
                file,
                path,
                date: local.date(),
            },
        );
        Ok(())
    }

    /// Writes a timestamped line, such as `< nick> text`, after a day
    /// changed line when the date is not the last line's. A channel not
    /// logged yet is opened first.
    pub fn write(&mut self, channel: &str, at: DateTime<Utc>, text: &str) -> io::Result<()> {
        self.open(channel, at)?;
        let local = self.local(at);
        let Some(log) = self.open.get_mut(&fold(channel)) else {
            return Ok(());
        };
        let mut lines = String::new();
        if local.date() != log.date {
            lines.push_str(&format!("{}\n", local.format(DAY_CHANGED)));
            log.date = local.date();
        }
        lines.push_str(&format!("{} {}\n", local.format(TIMESTAMP), one_line(text)));
        // One write, so the importer never reads half of the lines.
        log.file.write_all(lines.as_bytes())
    }

    /// Stops logging a channel with a log closed line, as irssi does on
    /// leaving it.
    pub fn close(&mut self, channel: &str, at: DateTime<Utc>) -> io::Result<()> {
        let local = self.local(at);
        match self.open.remove(&fold(channel)) {
            Some(mut log) => log
                .file
                .write_all(format!("{}\n", local.format(CLOSED)).as_bytes())
                .map_err(|err| {
                    io::Error::new(err.kind(), format!("{}: {err}", log.path.display()))
                }),
            None => Ok(()),
        }
    }

    /// Closes every log, as irssi does on quitting.
    pub fn close_all(&mut self, at: DateTime<Utc>) {
        let channels: Vec<String> = self.open.keys().cloned().collect();
        for channel in channels {
            if let Err(err) = self.close(&channel, at) {
                eprintln!("warning: closing the log of {channel}: {err}");
            }
        }
    }
}

/// A channel's name, folded to compare channels as IRC does, apart from
/// the few characters only RFC 1459 folds.
pub fn fold(channel: &str) -> String {
    channel.to_ascii_lowercase()
}

/// A channel's log file name, without `.log`: its folded name, with any
/// character a file name can't hold replaced.
fn file_name(channel: &str) -> String {
    fold(channel)
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect()
}

/// Text on one line, since IRC lines never hold a line break but a log line
/// with one would read as two.
fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discover::tests::TempDir;
    use crate::irssi::{self, Event, Line};

    fn utc(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn writes_lines_as_irssi_does() {
        let root = TempDir::new("live-log");
        let zone: Tz = "America/Denver".parse().unwrap();
        let mut logs = Logs::new(&root.0, "wetfish", zone).unwrap();
        // 18:03 in Denver, UTC-7 in November.
        logs.write(
            "#GreenHouse",
            utc("2026-11-05T01:03:07Z"),
            r#"< greenhouse> {"temp":"49.46"}"#,
        )
        .unwrap();
        // The next day there.
        logs.write(
            "#greenhouse",
            utc("2026-11-05T07:01:00Z"),
            "< greenhouse> again",
        )
        .unwrap();
        logs.write(
            "#greenhouse",
            utc("2026-11-05T07:02:00Z"),
            "< rachel> two\nlines",
        )
        .unwrap();
        logs.close_all(utc("2026-11-05T07:03:04Z"));

        let path = root.0.join("wetfish/#greenhouse.log");
        assert_eq!(logs.path("#GREENHOUSE"), path);
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(
            text,
            "--- Log opened Wed Nov 04 18:03:07 2026\n\
             18:03 < greenhouse> {\"temp\":\"49.46\"}\n\
             --- Day changed Thu Nov 05 2026\n\
             00:01 < greenhouse> again\n\
             00:02 < rachel> two lines\n\
             --- Log closed Thu Nov 05 00:03:04 2026\n"
        );
        // Every line reads back as irssi's would.
        for line in text.lines() {
            assert_ne!(irssi::parse_line(line), Line::Unrecognized, "{line}");
        }
        assert!(matches!(
            irssi::parse_line("18:03 < greenhouse> x"),
            Line::Event(
                _,
                Event::Message {
                    nick: "greenhouse",
                    ..
                }
            )
        ));

        // Opening again later adds to the same file.
        logs.open("#greenhouse", utc("2026-11-05T08:00:00Z"))
            .unwrap();
        logs.open("#greenhouse", utc("2026-11-05T08:00:01Z"))
            .unwrap();
        let again = fs::read_to_string(&path).unwrap();
        assert!(again.ends_with(
            "--- Log closed Thu Nov 05 00:03:04 2026\n--- Log opened Thu Nov 05 01:00:00 2026\n"
        ));
    }

    #[test]
    fn names_files_after_channels() {
        assert_eq!(file_name("#GreenHouse"), "#greenhouse");
        assert_eq!(file_name("#a/b"), "#a_b");
    }
}

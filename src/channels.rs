//! The channels sensors processes, saved in the database: `#greenhouse`
//! unless `sensors analyze --channel` named others. Only their log files are
//! imported, and the lines of other channels and of private messages
//! already imported are removed.

use rusqlite::{Connection, params};

use crate::db::Result;
use crate::discover::{LogFile, TargetKind};
use crate::progress::{Progress, Unit};
use crate::report::thousands;

/// The saved channels, sorted.
pub fn load(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT name FROM channels ORDER BY name COLLATE NOCASE")?;
    let rows = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Replaces the saved channels. Each must look like a channel name, which
/// also catches a `#` the shell took for a comment.
pub fn save(conn: &mut Connection, channels: &[String]) -> Result<()> {
    if channels.is_empty() {
        return Err("name at least one channel".into());
    }
    for channel in channels {
        if !is_channel_name(channel) {
            return Err(format!(
                "{channel} is not a channel name; channels start with #, &, ! or +, and need \
                 quotes in the shell: --channel '#greenhouse'"
            )
            .into());
        }
    }
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM channels", [])?;
    {
        let mut insert = tx.prepare("INSERT OR IGNORE INTO channels (name) VALUES (?1)")?;
        for channel in channels {
            insert.execute(params![channel.trim()])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn is_channel_name(name: &str) -> bool {
    let name = name.trim();
    name.len() > 1 && name.starts_with(['#', '&', '!', '+']) && !name.contains(char::is_whitespace)
}

/// Whether a log file is one of the channels, ignoring case.
fn wanted(channels: &[String], kind: TargetKind, target: &str) -> bool {
    kind == TargetKind::Channel
        && channels
            .iter()
            .any(|channel| channel.eq_ignore_ascii_case(target))
}

/// Keeps only the log files of the channels, and says how many were left
/// out.
pub fn keep_wanted(channels: &[String], files: &mut Vec<LogFile>) -> usize {
    let before = files.len();
    files.retain(|file| wanted(channels, file.kind, &file.target));
    before - files.len()
}

/// What removing other channels did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub files: u64,
    /// Lines read from them, blank lines included.
    pub lines: u64,
}

/// Removes the imported files of other channels and of private messages,
/// and their lines, in one transaction.
pub fn remove_others(
    conn: &mut Connection,
    channels: &[String],
    progress: &mut Progress,
) -> Result<Removed> {
    let mut others: Vec<(i64, i64)> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT id, target, kind, lines_read FROM files")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let (id, target, kind, lines): (i64, String, String, i64) =
                (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
            let kind = if kind == "channel" {
                TargetKind::Channel
            } else {
                TargetKind::Query
            };
            if !wanted(channels, kind, &target) {
                others.push((id, lines));
            }
        }
    }
    let mut removed = Removed::default();
    if others.is_empty() {
        return Ok(removed);
    }

    let total: u64 = others.iter().map(|&(_, lines)| lines.max(0) as u64).sum();
    progress.stage("Removing other channels", total, Unit::Items("lines"));
    let tx = conn.transaction()?;
    {
        let mut events = tx.prepare("DELETE FROM events WHERE file_id = ?1")?;
        let mut files = tx.prepare("DELETE FROM files WHERE id = ?1")?;
        for (id, lines) in others {
            events.execute(params![id])?;
            files.execute(params![id])?;
            removed.files += 1;
            removed.lines += lines.max(0) as u64;
            let done = removed.files;
            progress.update(removed.lines, || format!("{} files", thousands(done)));
        }
    }
    tx.commit()?;
    progress.done(&format!(
        "{} files, {} lines",
        thousands(removed.files),
        thousands(removed.lines)
    ));
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::db;
    use crate::discover::tests::TempDir;

    fn file(target: &str, kind: TargetKind) -> LogFile {
        LogFile {
            root: 0,
            path: PathBuf::from(format!("/logs/wetfish/{target}.log")),
            tag: "wetfish".into(),
            target: target.into(),
            kind,
            bytes: 0,
        }
    }

    #[test]
    fn saves_channels_and_starts_with_greenhouse() {
        let root = TempDir::new("channels-save");
        let mut conn = db::open(&root.0.join("sensors.sqlite")).unwrap();
        assert_eq!(load(&conn).unwrap(), ["#greenhouse"]);

        save(
            &mut conn,
            &["#greenhouse".into(), "#Garden".into(), "#garden".into()],
        )
        .unwrap();
        assert_eq!(load(&conn).unwrap(), ["#Garden", "#greenhouse"]);

        assert!(save(&mut conn, &["greenhouse".into()]).is_err());
        assert!(save(&mut conn, &[]).is_err());
        assert_eq!(load(&conn).unwrap(), ["#Garden", "#greenhouse"]);
    }

    #[test]
    fn keeps_only_the_channels_log_files() {
        let channels = ["#greenhouse".to_string()];
        let mut files = vec![
            file("#greenhouse", TargetKind::Channel),
            file("#GreenHouse", TargetKind::Channel),
            file("#wetfish", TargetKind::Channel),
            file("rachel", TargetKind::Query),
        ];
        assert_eq!(keep_wanted(&channels, &mut files), 2);
        let kept: Vec<&str> = files.iter().map(|file| file.target.as_str()).collect();
        assert_eq!(kept, ["#greenhouse", "#GreenHouse"]);
    }
}

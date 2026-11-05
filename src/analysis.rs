//! One analysis, as `sensors analyze` and the review's Analyze page run it:
//! find the log files of the channels to process, remove other channels
//! already imported, import the new lines, and read the readings in them.

use std::path::PathBuf;

use rusqlite::Connection;

use crate::channels::{self, Removed};
use crate::db::{self, ImportSummary, Result};
use crate::discover;
use crate::progress::{self, Progress};
use crate::readings::{self, BuildSummary};
use crate::report::thousands;

/// What an analysis did.
pub struct Outcome {
    /// Other channels' files removed from the database.
    pub removed: Removed,
    pub import: ImportSummary,
    pub build: BuildSummary,
}

/// Imports the lines added to the channels' log files under `roots` since
/// the last run, then reads the readings in them. `channels`, when given,
/// replaces the saved channels first. Removing other channels, a replaced
/// file, or `rebuild` reads the readings from every line again.
pub fn run(
    conn: &mut Connection,
    roots: &[PathBuf],
    channels: Option<&[String]>,
    rebuild: bool,
    progress: &mut Progress,
) -> Result<Outcome> {
    if let Some(channels) = channels {
        channels::save(conn, channels)?;
    }
    let channels = channels::load(conn)?;
    progress.note(&format!("Processing {}", channels.join(", ")));

    let folders: Vec<String> = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    progress.note(&format!("Looking for log files in {}", folders.join(", ")));
    let mut inventory = discover::discover(roots)?;
    let left_out = channels::keep_wanted(&channels, &mut inventory.files);
    progress.note(&format!(
        "Found {} log files of these channels, {} in all, and left out {} others",
        thousands(inventory.files.len() as u64),
        progress::bytes(inventory.files.iter().map(|file| file.bytes).sum()),
        thousands(left_out as u64)
    ));
    for skipped in &inventory.skipped {
        progress.note(&format!(
            "Skipped {}: {}",
            skipped.path.display(),
            skipped.reason
        ));
    }

    let removed = channels::remove_others(conn, &channels, progress)?;
    // Files that could not be read are listed in the summary, and the rest
    // are still imported.
    let import = db::import_with_progress(conn, &inventory.files, progress);
    let full = rebuild || removed.files > 0 || import.replaced_files > 0;
    let build = readings::build_with_progress(conn, full, progress)?;
    Ok(Outcome {
        removed,
        import,
        build,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::tests::TempDir;

    /// The imported files' channels or nicks, sorted.
    fn files(conn: &Connection) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT target FROM files ORDER BY target")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn lines_in(conn: &Connection) -> Vec<(String, i64)> {
        let mut stmt = conn
            .prepare("SELECT channel, sum(lines) FROM channel_days GROUP BY channel ORDER BY 1")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    #[test]
    fn processes_only_the_saved_channels() {
        let root = TempDir::new("analysis-channels");
        let logs = root.0.join("logs");
        fs::create_dir_all(logs.join("wetfish")).unwrap();
        let opened = "--- Log opened Tue Nov 03 13:45:10 2026\n";
        fs::write(
            logs.join("wetfish/#greenhouse.log"),
            format!(
                "{opened}13:46 < greenhouse> {{\"sensor\":\"inside\",\"temp\":\"68.54\",\"timestamp\":\"2026-11-03T20:46:43.051Z\"}}\n"
            ),
        )
        .unwrap();
        fs::write(
            logs.join("wetfish/#wetfish.log"),
            format!("{opened}13:50 < rachel> hello\n13:51 < rachel> again\n"),
        )
        .unwrap();
        fs::write(
            logs.join("wetfish/rachel.log"),
            format!("{opened}13:52 < rachel> private\n"),
        )
        .unwrap();
        let mut conn = db::open(&root.0.join("sensors.sqlite")).unwrap();
        let roots = [logs];
        let quiet = &mut Progress::quiet();

        // By default only #greenhouse.
        let outcome = run(&mut conn, &roots, None, false, quiet).unwrap();
        assert_eq!(outcome.import.new_files, 1);
        assert_eq!(files(&conn), ["#greenhouse"]);
        assert_eq!(lines_in(&conn), [("#greenhouse".to_string(), 1)]);

        // Adding a channel imports it, and saves it for later runs.
        let both = ["#greenhouse".to_string(), "#WETFISH".to_string()];
        let outcome = run(&mut conn, &roots, Some(&both[..]), false, quiet).unwrap();
        assert_eq!((outcome.import.new_files, outcome.removed.files), (1, 0));
        run(&mut conn, &roots, None, false, quiet).unwrap();
        assert_eq!(channels::load(&conn).unwrap(), ["#greenhouse", "#WETFISH"]);
        assert_eq!(
            lines_in(&conn),
            [("#greenhouse".to_string(), 1), ("#wetfish".to_string(), 2)]
        );

        // Dropping one removes its lines and reads the readings again.
        let outcome = run(
            &mut conn,
            &roots,
            Some(&["#wetfish".to_string()][..]),
            false,
            quiet,
        )
        .unwrap();
        assert_eq!(outcome.removed, Removed { files: 1, lines: 2 });
        assert_eq!(lines_in(&conn), [("#wetfish".to_string(), 2)]);
        let readings: i64 = conn
            .query_row("SELECT count(*) FROM readings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(readings, 0);
        assert_eq!(files(&conn), ["#wetfish"]);
    }
}

//! One analysis, as `sensors analyze` and the review's Analyze page run it:
//! find the log files, import their new lines, and read the readings in
//! them.

use std::path::PathBuf;

use rusqlite::Connection;

use crate::db::{self, ImportSummary, Result};
use crate::discover;
use crate::progress::{self, Progress};
use crate::readings::{self, BuildSummary};
use crate::report::thousands;

/// What an analysis did.
pub struct Outcome {
    pub import: ImportSummary,
    pub build: BuildSummary,
}

/// Imports the lines added to the log files under `roots` since the last
/// run, then reads the readings in them. A replaced file, or `rebuild`,
/// reads the readings from every line again.
pub fn run(
    conn: &mut Connection,
    roots: &[PathBuf],
    rebuild: bool,
    progress: &mut Progress,
) -> Result<Outcome> {
    let folders: Vec<String> = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    progress.note(&format!("Looking for log files in {}", folders.join(", ")));
    let inventory = discover::discover(roots)?;
    progress.note(&format!(
        "Found {} log files, {} in all",
        thousands(inventory.files.len() as u64),
        progress::bytes(inventory.files.iter().map(|file| file.bytes).sum())
    ));
    for skipped in &inventory.skipped {
        progress.note(&format!(
            "Skipped {}: {}",
            skipped.path.display(),
            skipped.reason
        ));
    }

    // Files that could not be read are listed in the summary, and the rest
    // are still imported.
    let import = db::import_with_progress(conn, &inventory.files, progress);
    let full = rebuild || import.replaced_files > 0;
    let build = readings::build_with_progress(conn, full, progress)?;
    Ok(Outcome { import, build })
}

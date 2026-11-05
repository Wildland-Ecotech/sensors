mod db;
mod discover;
mod irssi;
mod progress;
mod report;

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use rusqlite::Connection;

use crate::progress::Progress;

/// The default database, in the repository folder. `.gitignore` excludes it.
const DEFAULT_DB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/sensors.sqlite");

/// Graphs readings that sensors post to IRC, from irssi logs.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Import irssi log folders into the database
    Analyze {
        /// Log folders to read, one per irssi instance, each laid out as
        /// <folder>/<network tag>/<channel or nick>.log
        #[arg(required = true, value_name = "LOG_ROOT")]
        roots: Vec<PathBuf>,

        /// SQLite database to import into. It is created if it does not exist.
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB)]
        db: PathBuf,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Analyze { roots, db } => finish(analyze(&roots, &db)),
    }
}

/// Prints a command's error, if it failed, and gives its exit code.
fn finish(result: db::Result<()>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn open(db_path: &Path) -> db::Result<Connection> {
    db::open(db_path).map_err(|err| format!("opening database {}: {err}", db_path.display()).into())
}

/// Imports the lines added to the log files since the last run.
fn analyze(roots: &[PathBuf], db_path: &Path) -> db::Result<()> {
    let mut progress = Progress::to_stderr();
    let folders: Vec<String> = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect();
    progress.note(&format!("Looking for log files in {}", folders.join(", ")));
    let inventory = discover::discover(roots)?;
    progress.note(&format!(
        "Found {} log files, {} in all",
        report::thousands(inventory.files.len() as u64),
        progress::bytes(inventory.files.iter().map(|file| file.bytes).sum())
    ));
    for skipped in &inventory.skipped {
        progress.note(&format!(
            "Skipped {}: {}",
            skipped.path.display(),
            skipped.reason
        ));
    }

    let mut conn = open(db_path)?;
    let summary = db::import_with_progress(&mut conn, &inventory.files, &mut progress);
    // Files that could not be read are listed in the summary, and the rest
    // are still imported.
    report::import_summary(&mut io::stderr().lock(), db_path, &summary)?;
    Ok(())
}

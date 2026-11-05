mod analysis;
mod channels;
mod db;
mod discover;
mod history;
mod irssi;
mod progress;
mod readings;
mod report;
mod review;
mod stats;

use std::io::{self, Write};
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
    /// Import the logs of the channels to process into the database, read
    /// the sensor readings in them, and list each sensor's measures
    Analyze {
        /// Log folders to read, one per irssi instance, each laid out as
        /// <folder>/<network tag>/<channel or nick>.log
        #[arg(required = true, value_name = "LOG_ROOT")]
        roots: Vec<PathBuf>,

        /// A channel to process, instead of the saved ones, which start as
        /// #greenhouse. Repeat it for more; they are saved for later runs, and
        /// lines of other channels are removed. Quote it in the shell:
        /// --channel '#greenhouse'
        #[arg(long = "channel", value_name = "CHANNEL")]
        channels: Vec<String>,

        /// SQLite database to import into. It is created if it does not exist.
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB)]
        db: PathBuf,

        /// Read the readings from every imported line again, rather than only
        /// from the lines imported since the last run
        #[arg(long)]
        rebuild: bool,
    },

    /// Shrink the database file after removing lines, by rewriting it
    Compact {
        /// SQLite database to compact
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB)]
        db: PathBuf,
    },

    /// Review the readings and log lines in a web browser, on this computer
    /// only
    Review {
        /// Port to serve on, at http://127.0.0.1:PORT/
        #[arg(long, default_value_t = 7172)]
        port: u16,

        /// SQLite database to review
        #[arg(long, value_name = "FILE", default_value = DEFAULT_DB)]
        db: PathBuf,
    },
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Analyze {
            roots,
            channels,
            db,
            rebuild,
        } => {
            let channels = (!channels.is_empty()).then_some(channels.as_slice());
            finish(analyze(&roots, channels, &db, rebuild))
        }
        Command::Compact { db } => finish(compact(&db)),
        Command::Review { port, db } => finish(review::run(&db, port)),
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

/// Imports the lines added to the channels' log files since the last run,
/// reads the readings in them, and lists the series. `channels`, when given,
/// replaces the saved channels.
fn analyze(
    roots: &[PathBuf],
    channels: Option<&[String]>,
    db_path: &Path,
    rebuild: bool,
) -> db::Result<()> {
    let mut conn = open(db_path)?;
    let mut progress = Progress::to_stderr();
    let outcome = analysis::run(&mut conn, roots, channels, rebuild, &mut progress)?;
    report::import_summary(&mut io::stderr().lock(), db_path, &outcome.import)?;
    report::build_summary(&mut io::stderr().lock(), &outcome.build)?;
    eprintln!();

    let series = readings::series(&conn)?;
    let offsets = readings::Offsets::load(&conn)?;
    let mut out = io::BufWriter::new(io::stdout().lock());
    match report::series(&mut out, &series, &offsets).and_then(|()| out.flush()) {
        // The reader went away, for example `| head`. Not an error.
        Err(err) if err.kind() != io::ErrorKind::BrokenPipe => Err(err.into()),
        _ => Ok(()),
    }
}

fn compact(db_path: &Path) -> db::Result<()> {
    let conn = open(db_path)?;
    let size = || {
        std::fs::metadata(db_path)
            .map(|meta| meta.len())
            .unwrap_or(0)
    };
    let before = size();
    let mut progress = Progress::to_stderr();
    progress.stage("Compacting the database", 0, progress::Unit::Bytes);
    conn.execute_batch("VACUUM")?;
    progress.done(&format!(
        "{} down to {}",
        progress::bytes(before),
        progress::bytes(size())
    ));
    Ok(())
}

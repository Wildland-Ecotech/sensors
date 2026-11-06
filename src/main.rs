mod analysis;
mod channels;
mod db;
mod discover;
mod faults;
mod graphs;
mod history;
mod irssi;
mod live;
mod progress;
mod readings;
mod report;
mod review;
mod stats;

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono_tz::Tz;
use clap::{Parser, Subcommand};
use rusqlite::Connection;

use crate::progress::Progress;

/// The default database, in the repository folder. `.gitignore` excludes it.
const DEFAULT_DB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/sensors.sqlite");

/// The default log folder of `sensors live`, in the repository folder.
/// `.gitignore` excludes it.
const DEFAULT_LIVE_LOGS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/live-logs");

/// Graphs readings that sensors post to IRC, from irssi logs or live.
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

    /// Join the channels on IRC, log them as irssi would, read each reading
    /// as it is posted, and serve live graphs of them to anyone. Every
    /// option can be set in an environment variable instead
    Live {
        /// Time zone to write the logs in, as the logs of irssi on a
        /// computer there would be, such as America/Denver
        #[arg(long, env = "SENSORS_TIME_ZONE", value_parser = time_zone)]
        time_zone: Tz,

        /// IRC server, reached over TLS
        #[arg(long, env = "SENSORS_SERVER", default_value = "irc.wetfish.net")]
        server: String,

        /// The server's TLS port
        #[arg(long, env = "SENSORS_PORT", default_value_t = 6697)]
        port: u16,

        /// Nick on IRC. When it is taken, NICK_ or NICK__ until it is free
        #[arg(long, env = "SENSORS_NICK", default_value = "sensors")]
        nick: String,

        /// A channel to join, log and process. Repeat it for more, or
        /// separate them with commas in the variable. They replace the saved
        /// channels, as with sensors analyze --channel. Quote it in the
        /// shell: --channel '#greenhouse'
        #[arg(
            long = "channel",
            value_name = "CHANNEL",
            env = "SENSORS_CHANNELS",
            value_delimiter = ',',
            default_value = "#greenhouse"
        )]
        channels: Vec<String>,

        /// The network's folder in the logs, as irssi names it after its tag
        #[arg(long, env = "SENSORS_NETWORK", default_value = "wetfish")]
        network: String,

        /// Log folder to write in and import, laid out as irssi's:
        /// <folder>/<network>/<channel>.log
        #[arg(
            long,
            value_name = "FOLDER",
            env = "SENSORS_LOGS",
            default_value = DEFAULT_LIVE_LOGS
        )]
        logs: PathBuf,

        /// SQLite database to read the readings into. It is created if it
        /// does not exist.
        #[arg(
            long,
            value_name = "FILE",
            env = "SENSORS_DB",
            default_value = DEFAULT_DB
        )]
        db: PathBuf,

        /// Address and port to serve the live graphs on, such as
        /// 0.0.0.0:8080 for every network
        #[arg(
            long,
            value_name = "ADDRESS",
            env = "SENSORS_LISTEN",
            default_value = "127.0.0.1:7173"
        )]
        listen: SocketAddr,
    },

    /// Ask sensors live at ADDRESS whether it is in its channels, for a
    /// container's health check. Exits with 0 when it is
    Health {
        /// The address and port sensors live listens on, such as
        /// 127.0.0.1:8080
        address: SocketAddr,
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
        Command::Live {
            time_zone,
            server,
            port,
            nick,
            channels,
            network,
            logs,
            db,
            listen,
        } => finish(live::run(live::Settings {
            server,
            port,
            nick,
            channels,
            network,
            time_zone,
            logs,
            db,
            listen,
        })),
        Command::Health { address } => finish(live::check(address).map_err(Into::into)),
    }
}

/// A time zone's name, such as America/Denver.
fn time_zone(name: &str) -> Result<Tz, String> {
    name.parse().map_err(|_| {
        format!(
            "{name} is not a time zone; use a name from the tz database, such as \
             America/Denver, America/Los_Angeles or UTC"
        )
    })
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

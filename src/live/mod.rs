//! `sensors live`: an IRC bot that sits in the channels to process, logs
//! them as irssi would, reads each new reading into the database within
//! seconds, and serves the Sensors pages to anyone, so the graphs follow
//! the sensors as they post.
//!
//! It keeps logs rather than writing readings straight to the database, so
//! the live readings go through the same import, time zones and fault
//! checks as logs from irssi, the History of `sensors review` shows their
//! lines, and the database can always be built again from the logs. Logs
//! from irssi can be copied into its log folder too, under a folder of
//! their own, and are imported alongside.
//!
//! Three things run at once: the IRC connection (`irc.rs`), writing the
//! logs (`log.rs`); the analysis, on a thread of its own, reading new lines
//! soon after they are logged; and the live site, from `review`, with `/up`
//! for the container's health check (`health.rs`).

mod health;
mod irc;
mod log;
mod shutdown;

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::slice;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use chrono_tz::Tz;

use crate::analysis;
use crate::channels;
use crate::db;
use crate::progress::Progress;
use crate::report;
use crate::review;

pub use health::{Status, check};

/// What `sensors live` connects to, logs and serves.
pub struct Settings {
    /// The IRC server, reached over TLS.
    pub server: String,
    pub port: u16,
    pub nick: String,
    /// The channels to join, log and process.
    pub channels: Vec<String>,
    /// The network's folder in the logs, as irssi names it after the
    /// network's tag, such as `wetfish`.
    pub network: String,
    /// The time zone the logs are written in.
    pub time_zone: Tz,
    /// The log folder, laid out as `<folder>/<network>/<channel>.log`.
    pub logs: PathBuf,
    pub db: PathBuf,
    /// Where the live site listens.
    pub listen: SocketAddr,
}

/// Runs until stopped with SIGTERM or Ctrl+C.
pub fn run(settings: Settings) -> db::Result<()> {
    // Upgrade the database and save the channels up front, so a mistake in
    // either stops it before it connects.
    let mut conn = db::open(&settings.db)
        .map_err(|err| format!("opening database {}: {err}", settings.db.display()))?;
    channels::save(&mut conn, &settings.channels)?;
    drop(conn);

    let mut logs =
        log::Logs::new(&settings.logs, &settings.network, settings.time_zone).map_err(|err| {
            format!(
                "making the log folder in {}: {err}",
                settings.logs.display()
            )
        })?;
    let analysis = Analysis::start(settings.logs.clone(), settings.db.clone());

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let address = settings.listen;
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|err| format!("listening on {address}: {err}"))?;
        let mut shutdown = shutdown::Shutdown::new()
            .map_err(|err| format!("listening for stop signals: {err}"))?;
        eprintln!(
            "Logging {} in {}, with live graphs at http://{address}/",
            settings.channels.join(", "),
            settings.logs.join(&settings.network).display()
        );

        let status = Status::default();
        let site = review::serve_live(listener, settings.db.clone(), status.clone());
        tokio::select! {
            result = site => result.map_err(|err| format!("serving the live site: {err}"))?,
            () = irc::run(&settings, &mut logs, &analysis, &status, &mut shutdown) => {}
        }
        Ok(())
    })
}

/// How long the analysis waits after being asked, for lines close behind
/// the first, such as the other sensor's reading, to read them together.
const SETTLE: Duration = Duration::from_secs(2);

/// Reads new log lines into the database on a thread of its own, soon
/// after being asked, so the pages never wait for it.
pub struct Analysis {
    asks: Sender<()>,
}

impl Analysis {
    /// Starts the thread, and asks it to read what was logged or copied in
    /// before this run.
    fn start(logs: PathBuf, db: PathBuf) -> Analysis {
        let (asks, asked) = mpsc::channel();
        thread::spawn(move || analyze_when_asked(&logs, &db, &asked));
        let analysis = Analysis { asks };
        analysis.ask();
        analysis
    }

    /// Asks for the lines logged since the last analysis to be read.
    pub fn ask(&self) {
        // Only fails once the thread has stopped, which it said why.
        let _ = self.asks.send(());
    }
}

/// Analyzes the log folder whenever asked, as `sensors analyze` would: the
/// first time with its progress and a summary, since that may import a lot
/// of logs copied in, and then quietly, apart from problems.
fn analyze_when_asked(logs: &Path, db_path: &Path, asked: &Receiver<()>) {
    let mut conn = match db::open(db_path) {
        Ok(conn) => conn,
        Err(err) => {
            eprintln!(
                "error: opening database {} to read the logs into: {err}",
                db_path.display()
            );
            return;
        }
    };
    let mut first = true;
    while asked.recv().is_ok() {
        thread::sleep(SETTLE);
        while asked.try_recv().is_ok() {}
        let mut progress = if first {
            Progress::to_stderr()
        } else {
            Progress::quiet()
        };
        match analysis::run(
            &mut conn,
            slice::from_ref(&logs.to_path_buf()),
            None,
            false,
            &mut progress,
        ) {
            Ok(outcome) => {
                if first || !outcome.import.errors.is_empty() {
                    let mut out = io::stderr().lock();
                    let _ = report::import_summary(&mut out, db_path, &outcome.import);
                    if first {
                        let _ = report::build_summary(&mut out, &outcome.build);
                    }
                }
                first = false;
            }
            Err(err) => eprintln!("error: reading the logs: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::discover::tests::TempDir;
    use crate::readings;

    #[test]
    fn readings_logged_live_reach_the_database() {
        let root = TempDir::new("live");
        let folder = root.0.join("logs");
        let zone: Tz = "America/Denver".parse().unwrap();
        let mut logs = log::Logs::new(&folder, "wetfish", zone).unwrap();
        let at = |text: &str| text.parse::<DateTime<Utc>>().unwrap();
        logs.write(
            "#greenhouse",
            at("2026-11-05T01:03:07.300Z"),
            r#"< greenhouse> {"sensor":"inside","temp":"51.44","humid":"49.60","timestamp":"2026-11-05T01:03:07.248Z"}"#,
        )
        .unwrap();

        let mut conn = db::open(&root.0.join("sensors.sqlite")).unwrap();
        channels::save(&mut conn, &["#greenhouse".to_string()]).unwrap();
        let run = |conn: &mut rusqlite::Connection| {
            analysis::run(
                conn,
                slice::from_ref(&folder),
                None,
                false,
                &mut Progress::quiet(),
            )
            .unwrap()
        };
        assert_eq!(run(&mut conn).build.readings, 2);

        // A line logged later is read on its own, in the logs' time zone.
        logs.write(
            "#greenhouse",
            at("2026-11-05T01:03:37.300Z"),
            r#"< greenhouse> {"sensor":"inside","temp":"51.50","timestamp":"2026-11-05T01:03:37.250Z"}"#,
        )
        .unwrap();
        let outcome = run(&mut conn);
        assert_eq!((outcome.import.lines, outcome.build.readings), (1, 1));
        let offsets = readings::Offsets::load(&conn).unwrap();
        assert_eq!(offsets.minutes_at(1_793_840_587_250), -420);
    }
}

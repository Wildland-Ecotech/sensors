//! Analyzing from the browser: reads new lines of the saved channels from
//! the log folders the imported files came from, then the readings in them,
//! as `sensors analyze` does. It runs in the background while its page shows
//! each stage and how far it has come, reloading itself, with no script.

use std::collections::BTreeSet;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::response::Redirect;
use axum::routing::get;
use maud::{Markup, html};
use rusqlite::Connection;
use serde::Deserialize;

use super::layout::{self, number};
use super::{App, Flash, PageResult, internal, with_db};
use crate::channels::Removed;
use crate::db::{self, ImportSummary};
use crate::progress::{self, Progress, Report, SharedReport};
use crate::readings::BuildSummary;
use crate::{analysis, channels};

/// Seconds between reloads of the page while an analysis runs.
const RELOAD_SECONDS: u32 = 2;

/// The analysis started from the browser since `sensors review` started, if
/// any.
static ANALYSIS: Mutex<Option<Analysis>> = Mutex::new(None);

struct Analysis {
    report: SharedReport,
    started: Instant,
    /// Set when it ends: what it did, or why it failed, and how long it took.
    outcome: Option<(Outcome, Duration)>,
}

/// What an analysis did, sentence by sentence, or why it failed.
type Outcome = Result<Vec<String>, String>;

fn analysis() -> MutexGuard<'static, Option<Analysis>> {
    ANALYSIS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether an analysis started from the browser is running.
pub(super) fn running() -> bool {
    analysis()
        .as_ref()
        .is_some_and(|analysis| analysis.outcome.is_none())
}

pub(super) fn routes() -> Router<App> {
    Router::new().route("/analyze", get(analyze_page).post(start))
}

/// What the page shows of an analysis.
struct Status {
    report: Report,
    elapsed: Duration,
    /// `None` while it runs.
    outcome: Option<Outcome>,
}

fn status() -> Option<Status> {
    let analysis = analysis();
    let analysis = analysis.as_ref()?;
    let report = analysis
        .report
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    Some(Status {
        report,
        elapsed: analysis
            .outcome
            .as_ref()
            .map_or_else(|| analysis.started.elapsed(), |(_, took)| *took),
        outcome: analysis
            .outcome
            .as_ref()
            .map(|(outcome, _)| outcome.clone()),
    })
}

async fn analyze_page(State(app): State<App>, Query(flash): Query<Flash>) -> PageResult {
    let status = status();
    with_db(&app, move |conn| {
        let folders = log_folders(conn).map_err(internal)?;
        let channels = channels::load(conn).map_err(internal)?;
        let running = status
            .as_ref()
            .is_some_and(|status| status.outcome.is_none());
        let body = page(&folders, &channels, status.as_ref());
        Ok(if running {
            layout::reloading_page("Analyze", &flash, body, RELOAD_SECONDS)
        } else {
            layout::page("Analyze", &flash, body)
        })
    })
    .await
}

#[derive(Deserialize)]
struct StartForm {
    /// `yes` to read the readings from every line again.
    rebuild: Option<String>,
}

/// Starts an analysis in the background, unless one is running, and goes to
/// its page.
async fn start(State(app): State<App>, Form(form): Form<StartForm>) -> Redirect {
    let mut slot = analysis();
    if slot
        .as_ref()
        .is_some_and(|analysis| analysis.outcome.is_none())
    {
        return Redirect::to("/analyze");
    }
    let report: SharedReport = Arc::default();
    *slot = Some(Analysis {
        report: Arc::clone(&report),
        started: Instant::now(),
        outcome: None,
    });
    drop(slot);

    let path = Arc::clone(&app.db);
    let force = form.rebuild.as_deref() == Some("yes");
    thread::spawn(move || {
        let mut progress = Progress::to_stderr_and(report);
        progress.note("Analyzing for the browser...");
        // A panic would otherwise leave the analysis running for good.
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| run(&path, force, &mut progress)))
            .unwrap_or_else(|_| Err("The analysis stopped unexpectedly; see the terminal.".into()));
        if let Some(analysis) = analysis().as_mut() {
            let took = analysis.started.elapsed();
            analysis.outcome = Some((outcome, took));
        }
    });
    Redirect::to("/analyze")
}

/// The log folders the imported files came from, each laid out as
/// `<folder>/<network tag>/<channel or nick>.log`, sorted, each once.
fn log_folders(conn: &Connection) -> db::Result<Vec<PathBuf>> {
    let mut stmt = conn.prepare("SELECT path FROM files")?;
    let paths: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let folders: BTreeSet<PathBuf> = paths
        .iter()
        .filter_map(|path| Some(Path::new(path).parent()?.parent()?.to_path_buf()))
        .collect();
    Ok(folders.into_iter().collect())
}

/// Imports new lines from the log folders and reads the readings in them,
/// as `sensors analyze` does. Says what it did.
fn run(path: &Path, force: bool, progress: &mut Progress) -> Outcome {
    let mut conn = db::open(path).map_err(|err| format!("Opening the database: {err}"))?;
    let folders = log_folders(&conn).map_err(|err| format!("Reading the imported files: {err}"))?;
    let (found, missing): (Vec<PathBuf>, Vec<PathBuf>) =
        folders.into_iter().partition(|folder| folder.is_dir());
    let mut said = Vec::new();
    for folder in &missing {
        let note = format!("Skipped {}, which is no longer there.", folder.display());
        progress.note(&note);
        said.push(note);
    }
    if found.is_empty() {
        return Err(
            "None of the log folders are there. Run sensors analyze with your log folders.".into(),
        );
    }

    let outcome = analysis::run(&mut conn, &found, None, force, progress)
        .map_err(|err| format!("Analyzing: {err}"))?;
    if outcome.removed.files > 0 {
        said.push(removed_text(&outcome.removed));
    }
    said.push(import_text(&outcome.import));
    for (file, err) in &outcome.import.errors {
        said.push(format!("Could not import {}: {err}", file.display()));
    }
    said.push(build_text(&outcome.build));
    Ok(said)
}

fn removed_text(removed: &Removed) -> String {
    format!(
        "Removed {} of other channels and private messages, with {}.",
        count(removed.files, "log file"),
        count(removed.lines, "line"),
    )
}

fn import_text(summary: &ImportSummary) -> String {
    format!(
        "Imported {} lines: {} new files, {} grown, {} replaced and {} unchanged.",
        number(summary.lines as i64),
        number(summary.new_files as i64),
        number(summary.grown_files as i64),
        number(summary.replaced_files as i64),
        number(summary.unchanged_files as i64),
    )
}

fn build_text(summary: &BuildSummary) -> String {
    let mut text = format!(
        "Read {}: {} and {}.",
        count(summary.messages, "message"),
        count(summary.readings, "new reading"),
        count(summary.markers, "new marker"),
    );
    if summary.repeated > 0 {
        text.push_str(&format!(
            " Left out {} at the same time as another of the same measure.",
            count(summary.repeated, "reading")
        ));
    }
    if summary.untimed > 0 {
        text.push_str(&format!(
            " Left out {} without a timestamp.",
            count(summary.untimed, "JSON line")
        ));
    }
    text
}

/// A count and a noun, such as `1 reading` or `2,345 readings`.
fn count(n: u64, noun: &str) -> String {
    let suffix = if n == 1 { "" } else { "s" };
    format!(
        "{} {noun}{suffix}",
        number(i64::try_from(n).unwrap_or(i64::MAX))
    )
}

fn page(folders: &[PathBuf], channels: &[String], status: Option<&Status>) -> Markup {
    let running = status.is_some_and(|status| status.outcome.is_none());
    html! {
        h1 { "Analyze" }
        p.note {
            "Reads new lines of the channels below from the log folders, as "
            code { "sensors analyze" } " does with them, and the readings in them. Only what "
            "was added since the last import is read, so it is quick unless a lot was added. "
            "Progress also shows in the terminal running " code { "sensors review" } "."
        }
        h2 { "Channels" }
        ul {
            @for channel in channels {
                li { (channel) }
            }
        }
        p.note {
            "Only these channels are imported, and other channels already imported are "
            "removed. Change them with " code { "sensors analyze --channel '#name' FOLDER" }
            ", repeating " code { "--channel" } " for each."
        }
        h2 { "Log folders" }
        @if folders.is_empty() {
            p {
                "Nothing has been imported yet. Run " code { "sensors analyze FOLDER" }
                " once with your irssi log folders."
            }
        } @else {
            ul {
                @for folder in folders {
                    li { code { (folder.display()) } }
                }
            }
            p.note {
                "The folders the imported files are in. New network tags inside them are "
                "found too; a folder never imported needs " code { "sensors analyze FOLDER" }
                " once."
            }
            @if !running {
                form.inline method="post" action="/analyze" {
                    label {
                        input type="checkbox" name="rebuild" value="yes";
                        " Read the readings from every line again"
                    }
                    button type="submit" { "Analyze now" }
                }
            }
        }
        @if let Some(status) = status {
            (status_section(status))
        }
    }
}

fn status_section(status: &Status) -> Markup {
    let report = &status.report;
    html! {
        section.analysis {
            h2 {
                @match &status.outcome {
                    None => "Analyzing",
                    Some(Ok(_)) => "Last analysis",
                    Some(Err(_)) => "Last analysis failed",
                }
                span.muted { ", " (progress::elapsed(status.elapsed)) }
            }
            @if !report.notes.is_empty() {
                ul {
                    @for note in &report.notes {
                        li { (note) }
                    }
                }
            }
            @if !report.done.is_empty() {
                div.scroll {
                    table {
                        thead {
                            tr {
                                th { "Stage" }
                                th { "Result" }
                                th.num { "Time" }
                            }
                        }
                        tbody {
                            @for stage in &report.done {
                                tr {
                                    td { (stage.label) }
                                    td.wrap { (stage.summary) }
                                    td.num { (stage.took) }
                                }
                            }
                        }
                    }
                }
            }
            @if let (Some(current), None) = (&report.current, &status.outcome) {
                div.current-stage {
                    p { strong { (current.label) } }
                    @if current.total > 0 {
                        progress max=(current.total) value=(current.done.min(current.total)) {
                            (current.done.min(current.total) * 100 / current.total) "%"
                        }
                    } @else {
                        // With no total, the bar only shows that work goes on.
                        progress {}
                    }
                    p.muted { (current.line) }
                }
            }
            @match &status.outcome {
                None => p.note {
                    "This page reloads every " (RELOAD_SECONDS) " seconds until it is done."
                },
                Some(Ok(said)) => {
                    @for sentence in said {
                        p { (sentence) }
                    }
                },
                Some(Err(err)) => p.flash.error role="alert" { (err) },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::progress::{Current, Done};

    #[test]
    fn analyzes_the_folders_imported_files_came_from() {
        let root = TempDir::new("review-analyze");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        let log = root.0.join("wetfish/#greenhouse.log");
        fs::write(
            &log,
            "--- Log opened Tue Nov 03 13:45:10 2026\n\
             13:46 < greenhouse> {\"sensor\":\"inside\",\"temp\":\"68.54\",\"timestamp\":\"2026-11-03T20:46:43.051Z\"}\n",
        )
        .unwrap();
        let path = root.0.join("sensors.sqlite");
        let mut conn = db::open(&path).unwrap();
        let inventory = discover::discover(std::slice::from_ref(&root.0)).unwrap();
        db::import(&mut conn, &inventory.files);
        assert_eq!(log_folders(&conn).unwrap(), std::slice::from_ref(&root.0));

        // A line added since: imported, and its readings read.
        OpenOptions::new()
            .append(true)
            .open(&log)
            .unwrap()
            .write_all(b"13:47 < greenhouse> {\"sensor\":\"inside\",\"temp\":\"68.36\",\"timestamp\":\"2026-11-03T20:47:13.131Z\"}\n")
            .unwrap();
        let report: SharedReport = Arc::default();
        let mut progress = Progress::quiet_and(Arc::clone(&report));
        let said = run(&path, false, &mut progress).unwrap();
        assert!(said[0].contains("1 grown"), "{said:?}");
        assert_eq!(
            said[1],
            "Read 2 messages: 2 new readings and 0 new markers."
        );
        let report = report.lock().unwrap().clone();
        assert_eq!(report.notes[0], "Processing #greenhouse");
        assert!(report.notes[1].starts_with("Looking for log files in"));
        assert!(
            report
                .done
                .iter()
                .any(|stage| stage.label == "Reading sensor lines")
        );

        // Nothing new: nothing read, unless asked to read everything again.
        let mut progress = Progress::quiet_and(Arc::default());
        let said = run(&path, false, &mut progress).unwrap();
        assert_eq!(
            said[1],
            "Read 0 messages: 0 new readings and 0 new markers."
        );
        let said = run(&path, true, &mut progress).unwrap();
        assert_eq!(
            said[1],
            "Read 2 messages: 2 new readings and 0 new markers."
        );
    }

    #[test]
    fn says_what_was_removed() {
        let removed = Removed {
            files: 270,
            lines: 3_412_345,
        };
        assert_eq!(
            removed_text(&removed),
            "Removed 270 log files of other channels and private messages, with 3,412,345 lines."
        );
    }

    #[test]
    fn says_what_a_build_left_out() {
        let summary = BuildSummary {
            messages: 3,
            readings: 2,
            repeated: 2,
            markers: 0,
            untimed: 1,
        };
        assert_eq!(
            build_text(&summary),
            "Read 3 messages: 2 new readings and 0 new markers. Left out 2 readings at the \
             same time as another of the same measure. Left out 1 JSON line without a \
             timestamp."
        );
    }

    #[test]
    fn shows_an_analysis_under_way_and_when_done() {
        let folders = [PathBuf::from("/logs/irclogs")];
        let channels = ["#greenhouse".to_string()];
        let mut status = Status {
            report: Report {
                done: vec![Done {
                    label: "Importing".into(),
                    summary: "3 files".into(),
                    took: "1.2s".into(),
                }],
                current: Some(Current {
                    label: "Reading events".into(),
                    done: 40,
                    total: 160,
                    line: "Reading events: 25% (40 of 160 files), 2.0s".into(),
                }),
                notes: vec!["Found 3 log files, 2.0 KB in all".into()],
            },
            elapsed: Duration::from_secs(5),
            outcome: None,
        };
        let running = page(&folders, &channels, Some(&status)).into_string();
        assert!(running.contains(r#"<progress max="160" value="40">25%</progress>"#));
        assert!(running.contains("<code>/logs/irclogs</code>"));
        assert!(running.contains("<li>#greenhouse</li>"));
        assert!(running.contains("<td>Importing</td>"));
        assert!(!running.contains(r#"action="/analyze""#), "no second start");

        status.outcome = Some(Ok(vec!["Imported 12 lines.".into()]));
        let done = page(&folders, &channels, Some(&status)).into_string();
        assert!(done.contains("<p>Imported 12 lines.</p>"));
        assert!(!done.contains("<progress"));
        assert!(done.contains(r#"action="/analyze""#));

        let reloading = layout::reloading_page("Analyze", &Flash::default(), html! {}, 2);
        assert!(
            reloading
                .into_string()
                .contains(r#"<meta http-equiv="refresh" content="2">"#)
        );
        assert!(!running.contains("http-equiv"), "only the frame reloads");
    }
}

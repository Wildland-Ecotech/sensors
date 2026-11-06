//! `sensors review`: a web interface for the readings and log lines, served
//! on this computer only. New lines can be read from the log folders already
//! imported, as `sensors analyze` does; a folder never imported needs
//! `sensors analyze` once.
//!
//! `sensors live` serves the same Sensors pages to anyone, and nothing
//! else: no History, which would show everything said in the channels, and
//! no Analyze, which it does by itself. See [`Site`].
//!
//! Pages are built on the server with maud and use plain HTML forms, so
//! everything works without JavaScript. The only script is `chart.js`, which
//! adds hover tooltips to the charts. A form posts to an action, which
//! redirects back to a page, so reloading a page never repeats an action.
//! Each request opens the database on a blocking thread, since SQLite calls
//! block.
//!
//! The server comes from wot's `wot review`.

mod analyze;
mod chart;
mod history;
mod layout;
mod pages;
mod sensors;

use std::fmt::Display;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::Markup;
use rusqlite::Connection;
use serde::Deserialize;
use tokio::net::TcpListener;

use crate::db;
use crate::live::Status;

const STYLE: &str = include_str!("style.css");
const CHART_SCRIPT: &str = include_str!("chart.js");

/// Which pages a server shows, and to whom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Site {
    /// `sensors review`: every page, on this computer only.
    Review,
    /// `sensors live`: the Sensors pages only, to anyone.
    Live,
}

/// The site this process serves, set once when its server starts. A
/// process serves one, and pages and errors are built far from the
/// request, so it is kept here rather than passed down to every one.
static SITE: OnceLock<Site> = OnceLock::new();

/// The site this process serves: the review until a live site starts.
fn site() -> Site {
    SITE.get().copied().unwrap_or(Site::Review)
}

#[derive(Clone)]
struct App {
    db: Arc<PathBuf>,
    /// Whether `sensors live` is in its channels, on the live site.
    status: Option<Status>,
}

/// Serves the review pages on 127.0.0.1 until the process is stopped.
pub fn run(db_path: &Path, port: u16) -> db::Result<()> {
    if !db_path.exists() {
        return Err(format!(
            "there is no database at {}; import logs first with sensors analyze",
            db_path.display()
        )
        .into());
    }
    // Upgrade the database once up front, so pages never have to.
    db::open(db_path)?;

    let _ = SITE.set(Site::Review);
    let app = App {
        db: Arc::new(db_path.to_path_buf()),
        status: None,
    };
    let router = Router::new()
        .merge(pages::routes())
        .merge(sensors::routes())
        .merge(history::routes())
        .merge(analyze::routes())
        .route("/style.css", get(style))
        .route("/chart.js", get(chart_script))
        .fallback(not_found)
        .with_state(app);

    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .map_err(|err| format!("listening on {address}: {err}"))?;
        eprintln!(
            "Reviewing {} at http://{address}/ (press Ctrl+C to stop)",
            db_path.display()
        );
        axum::serve(listener, router).await?;
        Ok(())
    })
}

/// Serves the live site on `listener` until the process is stopped: the
/// Sensors pages, with the newest period at `/` too, and `/up` for the
/// health check, answering 200 while `status` says `sensors live` is in its
/// channels. The database must exist.
pub async fn serve_live(listener: TcpListener, db_path: PathBuf, status: Status) -> io::Result<()> {
    let _ = SITE.set(Site::Live);
    let app = App {
        db: Arc::new(db_path),
        status: Some(status),
    };
    let router = Router::new()
        .merge(sensors::live_routes())
        .route("/up", get(up))
        .route("/style.css", get(style))
        .route("/chart.js", get(chart_script))
        .fallback(not_found)
        .with_state(app);
    axum::serve(listener, router).await
}

/// `/up`: 200 while `sensors live` is in its channels, and 503 while it
/// isn't, never cached.
async fn up(State(app): State<App>) -> Response {
    let (status, text) = if app.status.as_ref().is_some_and(Status::connected) {
        (StatusCode::OK, "OK\n")
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "Not in every channel on IRC\n",
        )
    };
    (
        status,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        text,
    )
        .into_response()
}

/// An error page with an HTTP status.
struct PageError {
    status: StatusCode,
    message: String,
}

impl PageError {
    fn not_found(message: impl Into<String>) -> Self {
        PageError {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        PageError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for PageError {
    fn into_response(self) -> Response {
        let title = if self.status == StatusCode::NOT_FOUND {
            "Not found"
        } else {
            "Error"
        };
        let page = layout::page(
            title,
            &Flash::default(),
            pages::error(&self.message, site()),
        );
        (self.status, page).into_response()
    }
}

type PageResult = Result<Markup, PageError>;

/// Runs database work on a blocking thread with its own connection.
async fn with_db<T, F>(app: &App, work: F) -> Result<T, PageError>
where
    T: Send + 'static,
    F: FnOnce(&mut Connection) -> Result<T, PageError> + Send + 'static,
{
    let path = Arc::clone(&app.db);
    tokio::task::spawn_blocking(move || {
        let mut conn = db::open(&path)
            .map_err(|err| PageError::internal(format!("opening the database: {err}")))?;
        work(&mut conn)
    })
    .await
    .map_err(|err| PageError::internal(format!("the page failed to build: {err}")))?
}

/// Turns a database error into an error page.
fn internal(err: impl Display) -> PageError {
    PageError::internal(format!("reading the database: {err}"))
}

/// A notice or error carried in the address, shown at the top of the page.
#[derive(Default, Deserialize)]
struct Flash {
    notice: Option<String>,
    error: Option<String>,
}

/// A form field, or `None` when it was left blank.
fn filled(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

async fn style() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE)
}

async fn chart_script() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        CHART_SCRIPT,
    )
}

async fn not_found() -> PageError {
    PageError::not_found("There is no page here.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_the_health_check() {
        let status = Status::default();
        let app = App {
            db: Arc::new(PathBuf::new()),
            status: Some(status.clone()),
        };
        let answer = up(State(app.clone())).await;
        assert_eq!(answer.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            answer.headers()[header::CACHE_CONTROL].to_str().unwrap(),
            "no-store"
        );
        status.set_connected(true);
        assert_eq!(up(State(app)).await.status(), StatusCode::OK);
    }

    #[test]
    fn treats_blank_fields_as_missing() {
        assert_eq!(filled(&Some("  ".into())), None);
        assert_eq!(filled(&Some(" x ".into())), Some("x"));
        assert_eq!(filled(&None), None);
    }
}

//! `sensors review`: a web interface for the readings and log lines, served
//! on this computer only. New lines can be read from the log folders already
//! imported, as `sensors analyze` does; a folder never imported needs
//! `sensors analyze` once.
//!
//! Pages are built on the server with maud and use plain HTML forms, so
//! everything works without JavaScript. A form posts to an action, which
//! redirects back to a page, so reloading a page never repeats an action.
//! Each request opens the database on a blocking thread, since SQLite calls
//! block.
//!
//! The server comes from wot's `wot review`.

mod analyze;
mod history;
mod layout;
mod pages;

use std::fmt::Display;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::Markup;
use rusqlite::Connection;
use serde::Deserialize;

use crate::db;

const STYLE: &str = include_str!("style.css");

#[derive(Clone)]
struct App {
    db: Arc<PathBuf>,
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

    let app = App {
        db: Arc::new(db_path.to_path_buf()),
    };
    let router = Router::new()
        .merge(pages::routes())
        .merge(history::routes())
        .merge(analyze::routes())
        .route("/style.css", get(style))
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
        let page = layout::page(title, &Flash::default(), pages::error(&self.message));
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

async fn not_found() -> PageError {
    PageError::not_found("There is no page here.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treats_blank_fields_as_missing() {
        assert_eq!(filled(&Some("  ".into())), None);
        assert_eq!(filled(&Some(" x ".into())), Some("x"));
        assert_eq!(filled(&None), None);
    }
}

//! The overview page, and the error page.

use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use maud::{Markup, html};

use super::layout::{self, number};
use super::{App, Flash, PageResult, Site, internal, with_db};
use crate::readings::{self, Offsets, Series};
use crate::stats::{self, Overview};

pub(super) fn routes() -> Router<App> {
    Router::new().route("/", get(overview_page))
}

async fn overview_page(State(app): State<App>, Query(flash): Query<Flash>) -> PageResult {
    with_db(&app, move |conn| {
        let overview = stats::overview(conn).map_err(internal)?;
        let series = readings::series(conn).map_err(internal)?;
        let offsets = Offsets::load(conn).map_err(internal)?;
        Ok(layout::page(
            "Overview",
            &flash,
            self::overview(&overview, &series, &offsets),
        ))
    })
    .await
}

pub fn error(message: &str, site: Site) -> Markup {
    html! {
        h1 { "Something went wrong" }
        p { (message) }
        p {
            a href="/" {
                @match site {
                    Site::Review => "Back to the overview",
                    Site::Live => "Back to the newest readings",
                }
            }
        }
    }
}

pub fn overview(overview: &Overview, series: &[Series], offsets: &Offsets) -> Markup {
    let newest = series.iter().filter_map(|series| series.last).max();
    html! {
        h1 { "Overview" }
        p.lead {
            (number(overview.files)) " log files, " (number(overview.lines)) " lines imported."
            @if let Some(at) = &overview.last_import {
                " Last import " (at) " UTC."
            }
        }
        @if overview.files > 0 {
            form.inline method="post" action="/analyze" {
                button type="submit" { "Analyze now" }
                span.muted {
                    " Reads new lines from the log folders, and the readings in them. "
                    a href="/analyze" { "Folders and progress" }
                }
            }
        } @else {
            p {
                "Nothing has been imported yet. Run " code { "sensors analyze FOLDER" }
                " once with your irssi log folders."
            }
        }

        h2 { "Sensors" }
        @if series.is_empty() {
            p { "No sensor readings yet." }
        } @else {
            div.scroll {
                table {
                    thead {
                        tr {
                            th { "Channel" }
                            th { "Nick" }
                            th { "Sensor" }
                            th { "Measure" }
                            th.num { "Latest" }
                            th { "At" }
                            th.num { "Readings" }
                            th { "Since" }
                        }
                    }
                    tbody {
                        @for series in series {
                            tr {
                                td { (series.channel) }
                                td { (series.nick) }
                                td {
                                    @if series.sensor.is_empty() {
                                        span.muted { "-" }
                                    } @else {
                                        (series.sensor)
                                    }
                                }
                                td { (series.measure) }
                                td.num {
                                    @if let Some(value) = series.latest {
                                        (value)
                                    }
                                }
                                td { (local(series.last, offsets)) }
                                td.num { (number(series.readings)) }
                                td { (local(series.first, offsets)) }
                            }
                        }
                    }
                }
            }
            @if let Some(newest) = newest {
                p.note {
                    "Times are as the logs show them, now " (utc_offset(offsets.minutes_at(newest)))
                    ", worked out from when irssi logged each line."
                }
            }
        }
    }
}

/// A time in milliseconds since 1970 UTC, as the logs would show it, such
/// as `2026-11-04 18:06`, or a dash.
fn local(at: Option<i64>, offsets: &Offsets) -> String {
    at.and_then(|at| offsets.local(at)).map_or_else(
        || "-".to_string(),
        |at| at.format("%Y-%m-%d %H:%M").to_string(),
    )
}

/// A UTC offset in minutes, such as `UTC-7`, `UTC+5:30` or `UTC`.
fn utc_offset(minutes: i64) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let (hours, minutes) = (minutes.abs() / 60, minutes.abs() % 60);
    match (hours, minutes) {
        (0, 0) => "UTC".to_string(),
        (hours, 0) => format!("UTC{sign}{hours}"),
        (hours, minutes) => format!("UTC{sign}{hours}:{minutes:02}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_utc_offsets() {
        assert_eq!(utc_offset(0), "UTC");
        assert_eq!(utc_offset(-420), "UTC-7");
        assert_eq!(utc_offset(330), "UTC+5:30");
        assert_eq!(utc_offset(-570), "UTC-9:30");
    }

    #[test]
    fn shows_each_sensor_with_its_latest_reading() {
        let totals = Overview {
            files: 1,
            lines: 5_729,
            last_import: Some("2026-11-05 01:07:00".into()),
        };
        // 2026-11-03 20:46:43 and 2026-11-05 01:06:37 UTC.
        let series = [Series {
            channel: "#greenhouse".into(),
            nick: "greenhouse".into(),
            sensor: "inside".into(),
            measure: "temp".into(),
            readings: 5_729,
            first: Some(1_793_738_803_051),
            last: Some(1_793_840_797_796),
            latest: Some(51.26),
        }];
        let page = overview(&totals, &series, &Offsets::fixed(-420)).into_string();
        assert!(page.contains("1 log files, 5,729 lines imported."));
        assert!(page.contains(r#"<td class="num">51.26</td><td>2026-11-04 18:06</td>"#));
        assert!(page.contains("<td>2026-11-03 13:46</td>"));
        assert!(page.contains("now UTC-7"));
        assert!(page.contains(r#"action="/analyze""#));

        let empty = overview(&Overview::default(), &[], &Offsets::fixed(0)).into_string();
        assert!(empty.contains("No sensor readings yet."));
        assert!(!empty.contains(r#"action="/analyze""#));
    }
}

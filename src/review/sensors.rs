//! The sensors page: a chart of each measure over a range, every nick in the
//! channel together, with a table of each sensor's latest reading and its
//! lowest, mean and highest over the range, and CSV downloads of it.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{Markup, html};
use serde::Deserialize;

use super::chart::{self, measure_name, sensor_name, slot};
use super::layout::{self, number, url_encode};
use super::{App, Flash, PageError, PageResult, filled, internal, with_db};
use crate::graphs::{self, Graph, Measure, Range, decimal, time_text};
use crate::readings::{HOUR, Offsets};

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/sensors", get(sensors_page))
        .route("/sensors.csv", get(download))
}

/// The range shown when the address names none.
const DEFAULT_RANGE: Range = Range::Week;

#[derive(Default, Deserialize)]
struct GraphQuery {
    channel: Option<String>,
    /// A range's name, such as `week`.
    range: Option<String>,
    /// For a download: `shown`, `hours` or `readings`.
    points: Option<String>,
}

impl GraphQuery {
    fn range(&self) -> Range {
        filled(&self.range)
            .and_then(Range::parse)
            .unwrap_or(DEFAULT_RANGE)
    }
}

/// The channel asked for, ignoring case, or the first with readings.
fn pick_channel(channels: &[String], asked: Option<&str>) -> Option<String> {
    asked
        .and_then(|asked| {
            channels
                .iter()
                .find(|channel| channel.eq_ignore_ascii_case(asked))
        })
        .or(channels.first())
        .cloned()
}

/// The address of a channel's sensors over a range.
fn href(channel: &str, range: Range) -> String {
    format!(
        "/sensors?channel={}&range={}",
        url_encode(channel),
        range.name()
    )
}

/// The address of a download.
fn csv_href(channel: &str, range: Range, points: &str) -> String {
    format!(
        "/sensors.csv?channel={}&range={}&points={points}",
        url_encode(channel),
        range.name()
    )
}

async fn sensors_page(
    State(app): State<App>,
    Query(query): Query<GraphQuery>,
    Query(flash): Query<Flash>,
) -> PageResult {
    with_db(&app, move |conn| {
        let channels = graphs::channels(conn).map_err(internal)?;
        let offsets = Offsets::load(conn).map_err(internal)?;
        let channel = pick_channel(&channels, filled(&query.channel));
        let range = query.range();
        let graph = match &channel {
            Some(channel) => {
                Graph::build(conn, channel, range, &offsets, None).map_err(internal)?
            }
            None => None,
        };
        let title = match &channel {
            Some(channel) => format!("Sensors in {channel}"),
            None => "Sensors".to_string(),
        };
        Ok(layout::page(
            &title,
            &flash,
            page(&channels, channel.as_deref(), range, graph.as_ref()),
        ))
    })
    .await
}

fn page(channels: &[String], channel: Option<&str>, range: Range, graph: Option<&Graph>) -> Markup {
    let Some(channel) = channel else {
        return html! {
            h1 { "Sensors" }
            p {
                "No sensor readings yet. Run " code { "sensors analyze" }
                " or Analyze now to import some."
            }
        };
    };
    html! {
        h1 { "Sensors in " (channel) }
        div.filters {
            div.ranges role="group" aria-label="Range" {
                @for each in Range::EVERY {
                    a href=(href(channel, each))
                        aria-current=[(each == range).then_some("page")] { (each.label()) }
                }
            }
            @if channels.len() > 1 {
                form.inline method="get" action="/sensors" {
                    input type="hidden" name="range" value=(range.name());
                    label {
                        "Channel "
                        select name="channel" {
                            @for each in channels {
                                option value=(each) selected[each == channel] { (each) }
                            }
                        }
                    }
                    button type="submit" { "Show" }
                }
            }
        }
        @match graph {
            None => p { "No readings in this range." },
            Some(graph) => {
                p.lead {
                    (time_text(graph.start, false)) " to " (time_text(graph.end, false))
                    ", as the logs show times."
                }
                p.note {
                    "Each line is the mean of every " (chart::duration_words(graph.point_size))
                    " of readings, and its band spans the lowest to the highest. Every nick "
                    "that posted to " (channel) " counts as the same device."
                }
                @for measure in &graph.measures {
                    @let (name, unit) = measure_name(&measure.measure);
                    section.measure {
                        h2 {
                            (name)
                            @if !unit.is_empty() { " (" (unit) ")" }
                        }
                        (chart::chart(graph, measure))
                        (summary(graph, measure))
                    }
                }
                h2 { "Download" }
                p {
                    "CSV files of this range: "
                    a href=(csv_href(channel, range, "shown")) download { "the points shown" }
                    ", "
                    a href=(csv_href(channel, range, "hours")) download { "hourly points" }
                    @if range.readings_downloadable() {
                        ", or "
                        a href=(csv_href(channel, range, "readings")) download { "every reading" }
                        "."
                    } @else {
                        ". Every reading can be downloaded for a month or less."
                    }
                }
                p.note {
                    "Points have their start time, sensor, measure, how many readings they "
                    "average, and the mean, lowest and highest. Readings have their time as the "
                    "logs show it and in UTC. Every nick counts as the same device."
                }
            }
        }
    }
}

/// Each sensor's latest reading, and its lowest, mean and highest over the
/// range: the chart's values without hovering.
fn summary(graph: &Graph, measure: &Measure) -> Markup {
    html! {
        div.scroll {
            table.summary {
                thead {
                    tr {
                        th { "Sensor" }
                        th.num { "Latest" }
                        th { "At" }
                        th.num title="Over the range shown" { "Low" }
                        th.num title="Over the range shown" { "Mean" }
                        th.num title="Over the range shown" { "High" }
                        th.num title="Over the range shown" { "Readings" }
                    }
                }
                tbody {
                    @for line in &measure.lines {
                        @let range = line.summary();
                        tr {
                            td {
                                span class=(format!("line-key s{}", slot(graph, &line.sensor))) {}
                                " " (sensor_name(&line.sensor))
                            }
                            @match line.latest {
                                Some((at, value)) => {
                                    td.num { (decimal(value)) }
                                    td { (time_text(at, false)) }
                                },
                                None => {
                                    td.num { "-" }
                                    td { "-" }
                                },
                            }
                            @match &range {
                                Some(range) => {
                                    td.num { (decimal(range.low)) }
                                    td.num { (decimal(range.mean())) }
                                    td.num { (decimal(range.high)) }
                                    td.num { (number(range.readings)) }
                                },
                                None => {
                                    td.num { "-" }
                                    td.num { "-" }
                                    td.num { "-" }
                                    td.num { "0" }
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A channel's name for a file name, such as `greenhouse`.
fn file_stem(channel: &str) -> String {
    let stem: String = channel
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    if stem.is_empty() {
        "sensors".to_string()
    } else {
        stem
    }
}

/// A CSV file of a range: the points shown, hourly points, or every reading.
async fn download(
    State(app): State<App>,
    Query(query): Query<GraphQuery>,
) -> Result<Response, PageError> {
    with_db(&app, move |conn| {
        let channels = graphs::channels(conn).map_err(internal)?;
        let offsets = Offsets::load(conn).map_err(internal)?;
        let channel = pick_channel(&channels, filled(&query.channel))
            .ok_or_else(|| PageError::not_found("There are no sensor readings yet."))?;
        let range = query.range();
        let kind = filled(&query.points).unwrap_or("shown");
        let csv = match kind {
            "shown" => Graph::build(conn, &channel, range, &offsets, None)
                .map_err(internal)?
                .map(|graph| graphs::points_csv(&graph)),
            "hours" => Graph::build(conn, &channel, range, &offsets, Some(HOUR))
                .map_err(internal)?
                .map(|graph| graphs::points_csv(&graph)),
            "readings" if !range.readings_downloadable() => {
                return Err(PageError::not_found(
                    "Every reading can be downloaded for a month or less. Choose a shorter \
                     range, or the hourly points.",
                ));
            }
            "readings" => {
                graphs::readings_csv(conn, &channel, range, &offsets).map_err(internal)?
            }
            other => {
                return Err(PageError::not_found(format!(
                    "There is no download of {other}."
                )));
            }
        };
        let csv = csv.ok_or_else(|| PageError::not_found("No readings in this range."))?;
        let filename = format!("{}-{}-{kind}.csv", file_stem(&channel), range.name());
        Ok((
            [
                (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
                (
                    header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{filename}\""),
                ),
            ],
            csv,
        )
            .into_response())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphs::{Line, Point};

    #[test]
    fn picks_a_channel_and_a_range() {
        let channels = ["#garden".to_string(), "#greenhouse".to_string()];
        assert_eq!(
            pick_channel(&channels, Some("#GreenHouse")).as_deref(),
            Some("#greenhouse")
        );
        assert_eq!(
            pick_channel(&channels, Some("#nowhere")).as_deref(),
            Some("#garden")
        );
        assert_eq!(pick_channel(&[], None), None);
        let query = GraphQuery {
            range: Some("year".into()),
            ..GraphQuery::default()
        };
        assert_eq!(query.range(), Range::Year);
        assert_eq!(GraphQuery::default().range(), Range::Week);
        assert_eq!(
            csv_href("#greenhouse", Range::Day, "readings"),
            "/sensors.csv?channel=%23greenhouse&range=day&points=readings"
        );
        assert_eq!(file_stem("#greenhouse"), "greenhouse");
        assert_eq!(file_stem("#"), "sensors");
    }

    #[test]
    fn shows_charts_tables_ranges_and_downloads() {
        let point = |at: i64, value: f64| Point {
            at,
            readings: 2,
            total: value * 2.0,
            low: value - 0.5,
            high: value + 0.5,
        };
        // 2026-11-04 00:00 and 01:00 as the logs show times.
        let (first, second) = (1_793_750_400_000, 1_793_754_000_000);
        let graph = Graph {
            start: first - 365 * graphs::DAY,
            end: second,
            point_size: 12 * HOUR,
            sensors: vec!["inside".into(), "outside".into()],
            measures: vec![Measure {
                measure: "temp".into(),
                lines: vec![
                    Line {
                        sensor: "inside".into(),
                        points: vec![point(first, 51.26)],
                        latest: Some((second, 51.44)),
                    },
                    Line {
                        sensor: "outside".into(),
                        points: vec![],
                        latest: None,
                    },
                ],
            }],
        };
        let channels = ["#greenhouse".to_string()];
        let html = page(&channels, Some("#greenhouse"), Range::Year, Some(&graph)).into_string();
        assert!(html.contains("<h1>Sensors in #greenhouse</h1>"));
        assert!(html.contains(r#"<a href="/sensors?channel=%23greenhouse&amp;range=year" aria-current="page">Year</a>"#));
        assert!(html.contains(r#"<a href="/sensors?channel=%23greenhouse&amp;range=day">Day</a>"#));
        assert!(html.contains("<h2>Temperature (°F)</h2>"));
        assert!(html.contains("Each line is the mean of every 12 hours of readings"));
        assert!(html.contains(r#"<td class="num">51.44</td><td>2026-11-04 01:00</td>"#));
        assert!(html.contains(r#"<td class="num">50.76</td><td class="num">51.26</td><td class="num">51.76</td><td class="num">2</td>"#));
        assert!(html.contains("points=hours"));
        assert!(
            !html.contains("points=readings"),
            "a year is too long for every reading"
        );
        assert!(html.contains("Every reading can be downloaded for a month or less."));
        assert!(!html.contains("<select"), "one channel needs no menu");

        let none = page(&[], None, Range::Week, None).into_string();
        assert!(none.contains("No sensor readings yet."));
    }
}

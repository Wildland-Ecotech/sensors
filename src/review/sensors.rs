//! The sensors page: a chart of each measure over a range, every nick in the
//! channel together, with a table of each sensor's latest reading and its
//! lowest, mean and highest over the range, and CSV downloads of it. Faulty
//! readings are left out of all of them, and flagged unless the address says
//! `faults=hide`.

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
    /// `hide` to hide faulty readings.
    faults: Option<String>,
}

impl GraphQuery {
    fn range(&self) -> Range {
        filled(&self.range)
            .and_then(Range::parse)
            .unwrap_or(DEFAULT_RANGE)
    }

    /// Whether to flag faulty readings.
    fn faults(&self) -> bool {
        filled(&self.faults) != Some("hide")
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

/// What the page shows: a channel over a range, flagging faulty readings
/// or not. Links keep what they do not change.
#[derive(Clone, Copy)]
struct View<'a> {
    channel: &'a str,
    range: Range,
    faults: bool,
}

impl View<'_> {
    /// The query for this view, after `?`.
    fn query(&self) -> String {
        let mut query = format!(
            "channel={}&range={}",
            url_encode(self.channel),
            self.range.name()
        );
        if !self.faults {
            query.push_str("&faults=hide");
        }
        query
    }

    /// The address of the page.
    fn href(&self) -> String {
        format!("/sensors?{}", self.query())
    }

    /// The address of a download.
    fn csv_href(&self, points: &str) -> String {
        format!("/sensors.csv?{}&points={points}", self.query())
    }

    fn with_range(self, range: Range) -> Self {
        View { range, ..self }
    }

    fn with_faults(self, faults: bool) -> Self {
        View { faults, ..self }
    }
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
        let view = channel.as_deref().map(|channel| View {
            channel,
            range,
            faults: query.faults(),
        });
        Ok(layout::page(
            &title,
            &flash,
            page(&channels, view, graph.as_ref()),
        ))
    })
    .await
}

fn page(channels: &[String], view: Option<View<'_>>, graph: Option<&Graph>) -> Markup {
    let Some(view) = view else {
        return html! {
            h1 { "Sensors" }
            p {
                "No sensor readings yet. Run " code { "sensors analyze" }
                " or Analyze now to import some."
            }
        };
    };
    let channel = view.channel;
    let faulty: usize = graph
        .iter()
        .flat_map(|graph| &graph.measures)
        .flat_map(|measure| &measure.lines)
        .map(|line| line.faults.len())
        .sum();
    html! {
        h1 { "Sensors in " (channel) }
        div.filters {
            div.ranges role="group" aria-label="Range" {
                @for each in Range::EVERY {
                    a href=(view.with_range(each).href())
                        aria-current=[(each == view.range).then_some("page")] { (each.label()) }
                }
            }
            @if channels.len() > 1 {
                form.inline method="get" action="/sensors" {
                    input type="hidden" name="range" value=(view.range.name());
                    @if !view.faults {
                        input type="hidden" name="faults" value="hide";
                    }
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
            @if faulty > 0 || !view.faults {
                p.fault-toggle {
                    @if view.faults {
                        span.fault-key {} " " (number(faulty as i64)) " faulty reading"
                        @if faulty != 1 { "s" } " flagged. "
                        a href=(view.with_faults(false).href()) { "Hide them" }
                    } @else {
                        "Faulty readings are hidden. "
                        a href=(view.with_faults(true).href()) { "Flag them" }
                    }
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
                    "that posted to " (channel) " counts as the same device. Faulty readings "
                    "are left out of the lines and the tables"
                    @if view.faults { ", and flagged as red marks along the edge" }
                    "."
                }
                @for measure in &graph.measures {
                    @let (name, unit) = measure_name(&measure.measure);
                    section.measure {
                        h2 {
                            (name)
                            @if !unit.is_empty() { " (" (unit) ")" }
                        }
                        (chart::chart(graph, measure, view.faults))
                        (summary(graph, measure, view.faults))
                    }
                }
                h2 { "Download" }
                p {
                    "CSV files of this range: "
                    a href=(view.csv_href("shown")) download { "the points shown" }
                    ", "
                    a href=(view.csv_href("hours")) download { "hourly points" }
                    @if view.range.readings_downloadable() {
                        ", or "
                        a href=(view.csv_href("readings")) download { "every reading" }
                        "."
                    } @else {
                        ". Every reading can be downloaded for a month or less."
                    }
                }
                p.note {
                    "Points have their start time, sensor, measure, how many readings they "
                    "average, and the mean, lowest and highest, leaving faulty readings out. "
                    "Readings have their time as the logs show it and in UTC"
                    @if view.faults {
                        ", with faulty readings marked " code { "range" } " or " code { "spike" }
                        " in the last column"
                    }
                    ". Every nick counts as the same device."
                }
            }
        }
    }
}

/// Each sensor's latest reading, and its lowest, mean and highest over the
/// range: the chart's values without hovering. With `faults`, how many
/// faulty readings were left out.
fn summary(graph: &Graph, measure: &Measure, faults: bool) -> Markup {
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
                        @if faults {
                            th.num title="Left out, over the range shown" { "Faulty" }
                        }
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
                            @if faults {
                                td.num { (number(line.faults.len() as i64)) }
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
            "readings" => graphs::readings_csv(conn, &channel, range, &offsets, query.faults())
                .map_err(internal)?,
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
    use crate::graphs::{Fault, Line, Point};

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
        let view = View {
            channel: "#greenhouse",
            range: Range::Day,
            faults: true,
        };
        assert_eq!(
            view.csv_href("readings"),
            "/sensors.csv?channel=%23greenhouse&range=day&points=readings"
        );
        assert_eq!(
            view.with_faults(false).with_range(Range::Year).href(),
            "/sensors?channel=%23greenhouse&range=year&faults=hide"
        );
        let hide = GraphQuery {
            faults: Some("hide".into()),
            ..GraphQuery::default()
        };
        assert!(!hide.faults());
        assert!(GraphQuery::default().faults());
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
                        faults: vec![Fault {
                            at: first,
                            value: 302.0,
                            reason: "range".into(),
                        }],
                        latest: Some((second, 51.44)),
                    },
                    Line {
                        sensor: "outside".into(),
                        points: vec![],
                        faults: vec![],
                        latest: None,
                    },
                ],
            }],
        };
        let channels = ["#greenhouse".to_string()];
        let view = View {
            channel: "#greenhouse",
            range: Range::Year,
            faults: true,
        };
        let html = page(&channels, Some(view), Some(&graph)).into_string();
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

        assert!(html.contains("1 faulty reading flagged."));
        assert!(html.contains(
            r#"<a href="/sensors?channel=%23greenhouse&amp;range=year&amp;faults=hide">Hide them</a>"#
        ));
        assert!(
            html.contains(
                r#"<td class="num">51.76</td><td class="num">2</td><td class="num">1</td>"#
            )
        );
        assert!(html.contains("fault top"));

        let hidden = page(&channels, Some(view.with_faults(false)), Some(&graph)).into_string();
        assert!(hidden.contains("Faulty readings are hidden."));
        assert!(hidden.contains(
            r#"<a href="/sensors?channel=%23greenhouse&amp;range=day&amp;faults=hide">Day</a>"#
        ));
        assert!(!hidden.contains("Faulty</th>"));
        assert!(!hidden.contains("fault top"));
        assert!(hidden.contains("points=hours"));

        let none = page(&[], None, None).into_string();
        assert!(none.contains("No sensor readings yet."));
    }
}

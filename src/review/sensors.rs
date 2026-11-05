//! The sensors page: a chart of each measure over a calendar period, every
//! nick in the channel together, with a table of each sensor's latest reading and its
//! lowest, mean and highest over the period, and CSV downloads of it. Faulty
//! readings are left out of all of them, and flagged unless the address says
//! `faults=hide`. The address's `at` is a date in the period to show; without
//! it, the period holding the newest reading.

use axum::Router;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::NaiveDate;
use maud::{Markup, html};
use serde::Deserialize;

use super::chart::{self, measure_name, sensor_name, slot};
use super::layout::{self, number, url_encode};
use super::{App, Flash, PageError, PageResult, filled, internal, with_db};
use crate::graphs::{self, Graph, Measure, Period, Range, date_of, decimal, time_text};
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
    /// A date in the period to show, as `YYYY-MM-DD`.
    at: Option<String>,
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

    /// The date asked for, if it is one.
    fn at(&self) -> Option<NaiveDate> {
        filled(&self.at).and_then(|at| NaiveDate::parse_from_str(at, "%Y-%m-%d").ok())
    }
}

/// What a page or download shows: the channel's period, and how far its
/// readings go.
struct Shown {
    channel: String,
    period: Period,
    /// The first and newest reading, as the logs show times.
    first: i64,
    last: i64,
}

/// The period the address asks for, kept within the readings: the one
/// holding its date, or the newest. `None` when there are no readings.
fn shown(
    conn: &rusqlite::Connection,
    query: &GraphQuery,
    offsets: &Offsets,
) -> Result<Option<Shown>, PageError> {
    let channels = graphs::channels(conn).map_err(internal)?;
    let Some(channel) = pick_channel(&channels, filled(&query.channel)) else {
        return Ok(None);
    };
    let Some((first, last)) = graphs::readings_extent(conn, &channel, offsets).map_err(internal)?
    else {
        return Ok(None);
    };
    let date = query
        .at()
        .unwrap_or(date_of(last))
        .clamp(date_of(first), date_of(last));
    Ok(Some(Shown {
        channel,
        period: Period::containing(query.range(), date, first, last),
        first,
        last,
    }))
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

/// What the page shows: a channel over the period of a range holding a
/// date, or the newest, flagging faulty readings or not. Links keep what
/// they do not change.
#[derive(Clone, Copy)]
struct View<'a> {
    channel: &'a str,
    range: Range,
    /// `None` for the newest period.
    at: Option<NaiveDate>,
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
        if let Some(at) = self.at {
            query.push_str(&format!("&at={at}"));
        }
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

    fn at(self, at: Option<NaiveDate>) -> Self {
        View { at, ..self }
    }
}

/// What a period is, such as `Thursday, November 5, 2026`, `Week of
/// November 2, 2026`, `November 2026` or `2026`.
fn period_name(period: &Period) -> String {
    let first = period.first_day();
    match period.range {
        Range::Day => first.format("%A, %B %-d, %Y").to_string(),
        Range::Week => format!("Week of {}", first.format("%B %-d, %Y")),
        Range::Month => first.format("%B %Y").to_string(),
        Range::Year => first.format("%Y").to_string(),
        Range::All => format!(
            "Everything, {} to {}",
            date_of(period.start),
            date_of(period.end - 1)
        ),
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
        let Some(shown) = shown(conn, &query, &offsets)? else {
            return Ok(layout::page("Sensors", &flash, page(&channels, None, None)));
        };
        let graph =
            Graph::build(conn, &shown.channel, &shown.period, &offsets, None).map_err(internal)?;
        let title = format!("{} in {}", period_name(&shown.period), shown.channel);
        Ok(layout::page(
            &title,
            &flash,
            page(
                &channels,
                Some((&shown, query.at(), query.faults())),
                graph.as_ref(),
            ),
        ))
    })
    .await
}

/// The page for a period: what is shown, the date the address asked for, if
/// any, and whether faulty readings are flagged.
fn page(
    channels: &[String],
    shown: Option<(&Shown, Option<NaiveDate>, bool)>,
    graph: Option<&Graph>,
) -> Markup {
    let Some((shown, at, faults)) = shown else {
        return html! {
            h1 { "Sensors" }
            p {
                "No sensor readings yet. Run " code { "sensors analyze" }
                " or Analyze now to import some."
            }
        };
    };
    let channel = shown.channel.as_str();
    let period = &shown.period;
    let view = View {
        channel,
        range: period.range,
        at,
        faults,
    };
    let holds_newest = period.end > shown.last;
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
                    @if let Some(at) = view.at {
                        input type="hidden" name="at" value=(at.to_string());
                    }
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
            @if period.range != Range::All {
                div.period role="group" aria-label="Period" {
                    @match period.earlier(shown.first) {
                        Some(date) => a href=(view.at(Some(date)).href()) rel="prev" { "← Earlier" },
                        None => span.muted { "← Earlier" },
                    }
                    @match period.later(shown.last) {
                        Some(date) => a href=(view.at(Some(date)).href()) rel="next" { "Later →" },
                        None => span.muted { "Later →" },
                    }
                    @if !holds_newest {
                        a href=(view.at(None).href()) { "Newest" }
                    }
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
            None => p { "No readings in this period." },
            Some(graph) => {
                h2.period-name { (period_name(period)) }
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
                    "CSV files of this period: "
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
                        th.num title="The last reading in this period" { "Last" }
                        th { "At" }
                        th.num title="Over the period shown" { "Low" }
                        th.num title="Over the period shown" { "Mean" }
                        th.num title="Over the period shown" { "High" }
                        th.num title="Over the period shown" { "Readings" }
                        @if faults {
                            th.num title="Left out, over the period shown" { "Faulty" }
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
        let offsets = Offsets::load(conn).map_err(internal)?;
        let Shown {
            channel, period, ..
        } = shown(conn, &query, &offsets)?
            .ok_or_else(|| PageError::not_found("There are no sensor readings yet."))?;
        let kind = filled(&query.points).unwrap_or("shown");
        let csv = match kind {
            "shown" => Graph::build(conn, &channel, &period, &offsets, None)
                .map_err(internal)?
                .map(|graph| graphs::points_csv(&graph)),
            "hours" => Graph::build(conn, &channel, &period, &offsets, Some(HOUR))
                .map_err(internal)?
                .map(|graph| graphs::points_csv(&graph)),
            "readings" if !period.range.readings_downloadable() => {
                return Err(PageError::not_found(
                    "Every reading can be downloaded for a month or less. Choose a shorter \
                     period, or the hourly points.",
                ));
            }
            "readings" => graphs::readings_csv(conn, &channel, &period, &offsets, query.faults())
                .map_err(internal)?,
            other => {
                return Err(PageError::not_found(format!(
                    "There is no download of {other}."
                )));
            }
        };
        let csv = csv.ok_or_else(|| PageError::not_found("No readings in this period."))?;
        let when = match period.range {
            Range::All => String::new(),
            _ => format!("-{}", period.first_day()),
        };
        let filename = format!(
            "{}-{}{when}-{kind}.csv",
            file_stem(&channel),
            period.range.name()
        );
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
    use crate::graphs::{Fault, Line, Point, midnight};

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

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
            at: None,
            faults: true,
        };
        assert_eq!(
            view.csv_href("readings"),
            "/sensors.csv?channel=%23greenhouse&range=day&points=readings"
        );
        assert_eq!(
            view.at(Some(date(2026, 11, 2))).href(),
            "/sensors?channel=%23greenhouse&range=day&at=2026-11-02"
        );
        let at = GraphQuery {
            at: Some("2025-06-11".into()),
            ..GraphQuery::default()
        };
        assert_eq!(at.at(), Some(date(2025, 6, 11)));
        let bad = GraphQuery {
            at: Some("June".into()),
            ..GraphQuery::default()
        };
        assert_eq!(bad.at(), None);
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
    fn names_periods() {
        let at = |range, day| Period::containing(range, day, 0, midnight(date(2026, 11, 5)));
        assert_eq!(
            period_name(&at(Range::Day, date(2026, 11, 5))),
            "Thursday, November 5, 2026"
        );
        assert_eq!(
            period_name(&at(Range::Week, date(2026, 11, 5))),
            "Week of November 2, 2026"
        );
        assert_eq!(
            period_name(&at(Range::Month, date(2026, 11, 5))),
            "November 2026"
        );
        assert_eq!(period_name(&at(Range::Year, date(2026, 11, 5))), "2026");
        let all = Period::containing(
            Range::All,
            date(2026, 11, 5),
            midnight(date(2025, 3, 9)),
            midnight(date(2026, 11, 5)),
        );
        assert_eq!(period_name(&all), "Everything, 2025-03-09 to 2026-11-05");
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
        let (first_reading, last_reading) = (midnight(date(2025, 3, 9)), second);
        let shown = Shown {
            channel: "#greenhouse".into(),
            period: Period::containing(Range::Year, date(2026, 11, 4), first_reading, last_reading),
            first: first_reading,
            last: last_reading,
        };
        let html = page(&channels, Some((&shown, None, true)), Some(&graph)).into_string();
        assert!(html.contains(r#"<h2 class="period-name">2026</h2>"#));
        assert!(html.contains(
            r#"<a href="/sensors?channel=%23greenhouse&amp;range=year&amp;at=2025-12-31" rel="prev">← Earlier</a>"#
        ));
        assert!(html.contains(r#"<span class="muted">Later →</span>"#));
        assert!(!html.contains(">Newest<"), "the newest period is shown");
        assert!(html.contains(r#"title="The last reading in this period">Last</th>"#));
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

        let hidden = page(&channels, Some((&shown, None, false)), Some(&graph)).into_string();
        assert!(hidden.contains("Faulty readings are hidden."));
        assert!(hidden.contains(
            r#"<a href="/sensors?channel=%23greenhouse&amp;range=day&amp;faults=hide">Day</a>"#
        ));
        assert!(!hidden.contains("Faulty</th>"));
        assert!(!hidden.contains("fault top"));
        assert!(hidden.contains("points=hours"));

        // A week in the past, with the newest a link away.
        let june = Shown {
            period: Period::containing(Range::Week, date(2025, 6, 11), first_reading, last_reading),
            ..shown
        };
        let past = page(
            &channels,
            Some((&june, Some(date(2025, 6, 11)), true)),
            Some(&graph),
        )
        .into_string();
        assert!(past.contains(r#"<h2 class="period-name">Week of June 9, 2025</h2>"#));
        assert!(past.contains(
            r#"href="/sensors?channel=%23greenhouse&amp;range=week&amp;at=2025-06-16" rel="next">Later →</a>"#
        ));
        assert!(
            past.contains(r#"<a href="/sensors?channel=%23greenhouse&amp;range=week">Newest</a>"#)
        );
        assert!(past.contains(
            r#"<a href="/sensors?channel=%23greenhouse&amp;range=month&amp;at=2025-06-11">Month</a>"#
        ));

        let none = page(&[], None, None).into_string();
        assert!(none.contains("No sensor readings yet."));
    }
}

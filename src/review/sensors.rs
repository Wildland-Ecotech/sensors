//! The sensors page: each sensor's newest readings, then a chart of each
//! measure over a calendar period, every nick in the channel together, with
//! a table of each sensor's last reading and its lowest, mean and highest
//! over the period, and CSV downloads of it. Faulty readings are left out of all of them, and flagged unless the
//! address says `faults=hide`. The address's `at` is a date in the period to
//! show; without it, the period holding the newest reading.
//!
//! Charts zoom in a step at a time, from everything to a year, a month, a
//! week and a day, and breadcrumbs zoom back out. A fault mark opens a
//! close-up: the two hours around it, every reading on its own, with its
//! faulty readings listed and, in the review, linked to their lines in the
//! history.
//!
//! The live site serves these pages and nothing else, with the newest
//! period at `/` too. Showing the newest period, its page reloads itself
//! every `FOLLOW_SECONDS` to show new readings.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, NaiveDate, NaiveDateTime};
use maud::{Markup, html};
use serde::Deserialize;

use super::chart::{self, Options, measure_name, sensor_name, slot, slot_among};
use super::layout::{self, number, url_encode};
use super::{App, Flash, PageError, PageResult, Site, filled, internal, site, with_db};
use crate::faults::Reason;
use crate::graphs::{
    self, CLOSE_UP_SIDE, Graph, MINUTE, Measure, Period, Range, date_of, decimal, measure_rank,
    time_text,
};
use crate::readings::{self, HOUR, Offsets};

/// How often the live site's page of the newest period reloads, about as
/// often as the greenhouse posts.
const FOLLOW_SECONDS: u32 = 30;

pub(super) fn routes() -> Router<App> {
    Router::new()
        .route("/sensors", get(sensors_page))
        .route("/sensors.csv", get(download))
        .route("/sensors/close-up", get(close_up_page))
}

/// The live site's pages: these, and the newest period at `/`.
pub(super) fn live_routes() -> Router<App> {
    routes().route("/", get(sensors_page))
}

/// The range one step in from another, or `None` from a day.
fn zoom_in(range: Range) -> Option<Range> {
    match range {
        Range::All => Some(Range::Year),
        Range::Year => Some(Range::Month),
        Range::Month => Some(Range::Week),
        Range::Week => Some(Range::Day),
        Range::Day => None,
    }
}

/// The address of a close-up around a time, as the logs show times.
fn close_up_href(channel: &str, at: i64) -> String {
    let at = DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .naive_utc()
        .format("%Y-%m-%dT%H:%M:%S");
    format!("/sensors/close-up?channel={}&at={at}", url_encode(channel))
}

/// A period's short name for the breadcrumbs, such as `2025`, `June 2025`,
/// `Week of June 9` or `Wednesday, June 11`.
fn crumb_name(period: &Period) -> String {
    let first = period.first_day();
    match period.range {
        Range::All => "All".to_string(),
        Range::Year => first.format("%Y").to_string(),
        Range::Month => first.format("%B %Y").to_string(),
        Range::Week => format!("Week of {}", first.format("%B %-d")),
        Range::Day => first.format("%A, %B %-d").to_string(),
    }
}

/// The way back out from a range to everything, through each period
/// holding `date`: links to each wider one, then `current`, unlinked.
fn crumbs(view: View<'_>, date: NaiveDate, shown: &Shown, current: &str) -> Markup {
    const STEPS: [Range; 5] = [
        Range::All,
        Range::Year,
        Range::Month,
        Range::Week,
        Range::Day,
    ];
    // Wider than what is shown: every step for a close-up.
    let wider: Vec<Range> = STEPS
        .into_iter()
        .take_while(|&range| !current.is_empty() || range != view.range)
        .collect();
    html! {
        nav.crumbs aria-label="Zoom" {
            @for range in wider {
                @let period = Period::containing(range, date, shown.first, shown.last);
                a href=(view.with_range(range).at(Some(date)).href()) { (crumb_name(&period)) }
                span.muted aria-hidden="true" { " › " }
            }
            span aria-current="page" {
                @if current.is_empty() {
                    (crumb_name(&shown.period))
                } @else {
                    (current)
                }
            }
        }
    }
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
    /// The date asked for, kept within the readings, or the newest.
    date: NaiveDate,
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
        date,
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
            return Ok(layout::page(
                "Sensors",
                &flash,
                page(site(), &channels, None, None, &Newest::default()),
            ));
        };
        let newest = newest(conn, &shown.channel, &offsets, now())?;
        let graph =
            Graph::build(conn, &shown.channel, &shown.period, &offsets, None).map_err(internal)?;
        let title = format!("{} in {}", period_name(&shown.period), shown.channel);
        let body = page(
            site(),
            &channels,
            Some((&shown, query.at(), query.faults())),
            graph.as_ref(),
            &newest,
        );
        // The live site follows new readings while they would show.
        if site() == Site::Live && shown.period.end > shown.last {
            Ok(layout::following_page(&title, &flash, body, FOLLOW_SECONDS))
        } else {
            Ok(layout::page(&title, &flash, body))
        }
    })
    .await
}

/// Now, in milliseconds since 1970 UTC.
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// Each sensor's newest readings in a channel, faulty ones left out.
#[derive(Default)]
struct Newest {
    /// When the page was built, in milliseconds since 1970 UTC, to say how
    /// long ago the readings are.
    now: i64,
    /// Sorted, as graphs list sensors, so their colors match.
    sensors: Vec<NewestSensor>,
}

struct NewestSensor {
    sensor: String,
    /// The newest of its readings, in milliseconds since 1970 UTC, and as
    /// the logs show times.
    at: i64,
    local: String,
    /// Each measure's newest value, temperature first.
    values: Vec<(String, f64)>,
}

/// The newest reading of each sensor's measures, of every nick together.
fn newest(
    conn: &rusqlite::Connection,
    channel: &str,
    offsets: &Offsets,
    now: i64,
) -> Result<Newest, PageError> {
    let mut found: BTreeMap<(String, String), (i64, f64)> = BTreeMap::new();
    for series in readings::series(conn).map_err(internal)? {
        if !series.channel.eq_ignore_ascii_case(channel) {
            continue;
        }
        if let (Some(at), Some(value)) = (series.last, series.latest) {
            let newest = found
                .entry((series.sensor, series.measure))
                .or_insert((at, value));
            if at > newest.0 {
                *newest = (at, value);
            }
        }
    }
    let mut sensors: Vec<NewestSensor> = Vec::new();
    for ((sensor, measure), (at, value)) in found {
        match sensors.last_mut() {
            Some(last) if last.sensor == sensor => {
                last.at = last.at.max(at);
                last.values.push((measure, value));
            }
            _ => sensors.push(NewestSensor {
                sensor,
                at,
                local: String::new(),
                values: vec![(measure, value)],
            }),
        }
    }
    for sensor in &mut sensors {
        sensor
            .values
            .sort_by_key(|(measure, _)| measure_rank(measure).0);
        sensor.local = offsets
            .local(sensor.at)
            .map(|at| at.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
    }
    Ok(Newest { now, sensors })
}

/// How long before `now` a time was, such as `just now`, `3 minutes ago`,
/// `5 hours ago` or `2 days ago`.
fn ago(now: i64, at: i64) -> String {
    let minutes = (now - at).max(0) / MINUTE;
    let (count, unit) = if minutes < 60 {
        (minutes, "minute")
    } else if minutes < 48 * 60 {
        (minutes / 60, "hour")
    } else {
        (minutes / (24 * 60), "day")
    };
    match count {
        0 => "just now".to_string(),
        1 => format!("1 {unit} ago"),
        _ => format!("{count} {unit}s ago"),
    }
}

/// Each sensor's newest readings, with when they were taken.
fn newest_readings(newest: &Newest) -> Markup {
    let names: Vec<String> = newest
        .sensors
        .iter()
        .map(|sensor| sensor.sensor.clone())
        .collect();
    html! {
        @if !newest.sensors.is_empty() {
            section.newest aria-label="Newest readings" {
                @for sensor in &newest.sensors {
                    div.newest-sensor {
                        div.newest-name {
                            span class=(format!("line-key s{}", slot_among(&names, &sensor.sensor))) {}
                            " " (sensor_name(&sensor.sensor))
                        }
                        div.newest-values {
                            @for (measure, value) in &sensor.values {
                                @let (name, unit) = measure_name(measure);
                                span title=(name) {
                                    strong { (decimal(*value)) }
                                    @if !unit.is_empty() { " " (unit) }
                                }
                            }
                        }
                        p.newest-time { (sensor.local) " · " (ago(newest.now, sensor.at)) }
                    }
                }
            }
        }
    }
}

/// The page for a period: what is shown, the date the address asked for, if
/// any, and whether faulty readings are flagged, under the newest readings.
fn page(
    site: Site,
    channels: &[String],
    shown: Option<(&Shown, Option<NaiveDate>, bool)>,
    graph: Option<&Graph>,
    newest: &Newest,
) -> Markup {
    let Some((shown, at, faults)) = shown else {
        return html! {
            h1 { "Sensors" }
            @match site {
                Site::Review => p {
                    "No sensor readings yet. Run " code { "sensors analyze" }
                    " or Analyze now to import some."
                },
                Site::Live => p {
                    "No sensor readings yet. They show here within seconds of a sensor posting "
                    "one."
                },
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
    let zoom_to = zoom_in(period.range);
    let zoom = |date: NaiveDate| {
        view.with_range(zoom_to.unwrap_or(Range::Day))
            .at(Some(date))
            .href()
    };
    let close_up = |at: i64| close_up_href(channel, at);
    let options = Options {
        faults: view.faults,
        zoom: zoom_to.map(|_| &zoom as &dyn Fn(NaiveDate) -> String),
        close_up: Some(&close_up),
    };
    let faulty: usize = graph
        .iter()
        .flat_map(|graph| &graph.measures)
        .flat_map(|measure| &measure.lines)
        .map(|line| line.faults.len())
        .sum();
    html! {
        h1 { "Sensors in " (channel) }
        (newest_readings(newest))
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
                (crumbs(view, shown.date, shown, ""))
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
                        (chart::chart(graph, measure, &options))
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

#[derive(Default, Deserialize)]
struct CloseUpQuery {
    channel: Option<String>,
    /// As the logs show times, such as `2026-11-03T13:47:13`.
    at: Option<String>,
}

impl CloseUpQuery {
    /// The time asked for, as the logs show times, if it is one.
    fn at(&self) -> Option<i64> {
        let at = NaiveDateTime::parse_from_str(filled(&self.at)?, "%Y-%m-%dT%H:%M:%S").ok()?;
        Some(at.and_utc().timestamp_millis())
    }
}

async fn close_up_page(
    State(app): State<App>,
    Query(query): Query<CloseUpQuery>,
    Query(flash): Query<Flash>,
) -> PageResult {
    with_db(&app, move |conn| {
        let at = query.at().ok_or_else(|| {
            PageError::not_found("A close-up needs a time, such as at=2026-11-03T13:47:13.")
        })?;
        let offsets = Offsets::load(conn).map_err(internal)?;
        let graph_query = GraphQuery {
            channel: query.channel.clone(),
            range: Some(Range::Day.name().to_string()),
            at: Some(date_of(at).to_string()),
            ..GraphQuery::default()
        };
        let shown = shown(conn, &graph_query, &offsets)?
            .ok_or_else(|| PageError::not_found("There are no sensor readings yet."))?;
        let graph = Graph::close_up(conn, &shown.channel, at, &offsets)
            .map_err(internal)?
            .ok_or_else(|| PageError::not_found("There are no sensor readings yet."))?;
        let title = format!("Close-up of {} in {}", time_text(at, true), shown.channel);
        Ok(layout::page(
            &title,
            &flash,
            close_up(site(), &shown, at, &graph),
        ))
    })
    .await
}

/// The two hours around a time, every reading on its own, and the faulty
/// readings in them, linked to their lines in the review's history.
fn close_up(site: Site, shown: &Shown, at: i64, graph: &Graph) -> Markup {
    let channel = shown.channel.as_str();
    let view = View {
        channel,
        range: Range::Day,
        at: Some(shown.date),
        faults: true,
    };
    let mut faults: Vec<(&str, &str, &graphs::Fault)> = graph
        .measures
        .iter()
        .flat_map(|measure| {
            measure.lines.iter().flat_map(move |line| {
                line.faults
                    .iter()
                    .map(move |fault| (measure.measure.as_str(), line.sensor.as_str(), fault))
            })
        })
        .collect();
    faults.sort_by_key(|(_, _, fault)| fault.at);
    let clock = |at: i64| {
        time_text(at, true)
            .get(11..)
            .unwrap_or_default()
            .to_string()
    };
    html! {
        h1 { "Sensors in " (channel) }
        (crumbs(view, shown.date, shown, "Close-up"))
        h2.period-name {
            "Close-up of " (clock(at)) " on "
            (period_name(&Period::containing(Range::Day, shown.date, shown.first, shown.last)))
        }
        div.period role="group" aria-label="Close-up" {
            a href=(close_up_href(channel, at - CLOSE_UP_SIDE)) rel="prev" { "← An hour earlier" }
            a href=(close_up_href(channel, at + CLOSE_UP_SIDE)) rel="next" { "An hour later →" }
        }
        p.lead {
            "Every reading from " (clock(graph.start)) " to " (clock(graph.end))
            ", as the logs show times, none averaged."
        }
        p.note {
            "Faulty readings are left out of the lines and shown in red: spikes as dots at "
            "their values, readings out of range as marks along the edge. Each line breaks "
            "where readings are more than two minutes apart."
        }
        @for measure in &graph.measures {
            @let (name, unit) = measure_name(&measure.measure);
            section.measure {
                h2 {
                    (name)
                    @if !unit.is_empty() { " (" (unit) ")" }
                }
                (chart::chart(graph, measure, &Options::flags(true)))
            }
        }
        h2 { "Faulty readings" }
        @if faults.is_empty() {
            p { "No faulty readings in these two hours." }
        } @else {
            div.scroll {
                table {
                    thead {
                        tr {
                            th { "Time" }
                            th { "Sensor" }
                            th { "Measure" }
                            th.num { "Value" }
                            th { "Why" }
                            @if site == Site::Review {
                                th { "Line" }
                            }
                        }
                    }
                    tbody {
                        @for (measure, sensor, fault) in &faults {
                            tr {
                                td { (clock(fault.at)) }
                                td { (sensor_name(sensor)) }
                                td { (measure_name(measure).0) }
                                td.num { (decimal(fault.value)) }
                                td { (Reason::describe(&fault.reason)) }
                                @if site == Site::Review {
                                    td { a href=(history_href(channel, fault)) { "In the history" } }
                                }
                            }
                        }
                    }
                }
            }
            @if site == Site::Review {
                p.note {
                    "Each line links to the history of its day, showing the log line it came "
                    "from, to read with the lines around it."
                }
            }
        }
    }
}

/// The address of a faulty reading's line in the history: its day, and the
/// line's own timestamp to find it by.
fn history_href(channel: &str, fault: &graphs::Fault) -> String {
    let stamp = DateTime::from_timestamp_millis(fault.utc)
        .map(|at| at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default();
    format!(
        "/history?day={}&channel={}&q={}",
        date_of(fault.at),
        url_encode(channel),
        url_encode(&stamp)
    )
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
    use std::fs;

    use super::*;
    use crate::discover::tests::TempDir;
    use crate::graphs::{Fault, Line, Point, midnight};
    use crate::progress::Progress;
    use crate::{analysis, db};

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
            gap: 12 * HOUR,
            sensors: vec!["inside".into(), "outside".into()],
            measures: vec![Measure {
                measure: "temp".into(),
                lines: vec![
                    Line {
                        sensor: "inside".into(),
                        points: vec![point(first, 51.26)],
                        faults: vec![Fault {
                            at: first,
                            utc: first + 7 * HOUR,
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
            date: date(2026, 11, 4),
            first: first_reading,
            last: last_reading,
        };
        // Read three minutes after the inside sensor's newest reading.
        let newest = Newest {
            now: second + 7 * HOUR + 3 * graphs::MINUTE,
            sensors: vec![NewestSensor {
                sensor: "inside".into(),
                at: second + 7 * HOUR,
                local: "2026-11-04 01:00".into(),
                values: vec![("temp".into(), 51.44), ("humid".into(), 49.6)],
            }],
        };
        let html = page(
            Site::Review,
            &channels,
            Some((&shown, None, true)),
            Some(&graph),
            &newest,
        )
        .into_string();
        assert!(html.contains(
            r#"<div class="newest-name"><span class="line-key s1"></span> inside</div>"#
        ));
        assert!(html.contains(
            r#"<span title="Temperature"><strong>51.44</strong> °F</span><span title="Humidity"><strong>49.6</strong> % RH</span>"#
        ));
        assert!(html.contains("2026-11-04 01:00 · 3 minutes ago"));
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

        // Zooming in to a month, and out from the breadcrumbs.
        assert!(html.contains(
            r#"data-zoom="/sensors?channel=%23greenhouse&amp;range=month&amp;at=0001-01-01""#
        ));
        assert!(html.contains(
            r#"<nav class="crumbs" aria-label="Zoom"><a href="/sensors?channel=%23greenhouse&amp;range=all&amp;at=2026-11-04">All</a>"#
        ));
        assert!(html.contains(r#"<span aria-current="page">2026</span>"#));
        assert!(html.contains(
            r#"href="/sensors/close-up?channel=%23greenhouse&amp;at=2026-11-04T00:00:00""#
        ));

        let hidden = page(
            Site::Review,
            &channels,
            Some((&shown, None, false)),
            Some(&graph),
            &Newest::default(),
        )
        .into_string();
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
            date: date(2025, 6, 11),
            ..shown
        };
        let past = page(
            Site::Review,
            &channels,
            Some((&june, Some(date(2025, 6, 11)), true)),
            Some(&graph),
            &Newest::default(),
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

        assert!(past.contains(r#">June 2025</a>"#));
        assert!(past.contains(r#"<span aria-current="page">Week of June 9</span>"#));
        assert!(past.contains("range=day&amp;at=0001-01-01"));

        // A day is the last step in.
        let day = Shown {
            period: Period::containing(Range::Day, date(2025, 6, 11), first_reading, last_reading),
            ..june
        };
        let one_day = page(
            Site::Review,
            &channels,
            Some((&day, Some(date(2025, 6, 11)), true)),
            Some(&graph),
            &Newest::default(),
        )
        .into_string();
        assert!(!one_day.contains("data-zoom"));
        assert!(one_day.contains(r#"<span aria-current="page">Wednesday, June 11</span>"#));

        let none = page(Site::Review, &[], None, None, &Newest::default()).into_string();
        assert!(none.contains("No sensor readings yet."));
        assert!(none.contains("sensors analyze"));
        let live = page(Site::Live, &[], None, None, &Newest::default()).into_string();
        assert!(live.contains("within seconds") && !live.contains("Analyze"));
    }

    #[test]
    fn says_how_long_ago() {
        let now = 1_793_840_797_796;
        assert_eq!(ago(now, now - 59_000), "just now");
        assert_eq!(ago(now, now + 5_000), "just now");
        assert_eq!(ago(now, now - graphs::MINUTE), "1 minute ago");
        assert_eq!(ago(now, now - 59 * graphs::MINUTE), "59 minutes ago");
        assert_eq!(ago(now, now - HOUR), "1 hour ago");
        assert_eq!(ago(now, now - 47 * HOUR), "47 hours ago");
        assert_eq!(ago(now, now - 72 * HOUR), "3 days ago");
    }

    #[test]
    fn finds_each_sensors_newest_readings() {
        let root = TempDir::new("sensors-newest");
        let logs = root.0.join("logs");
        fs::create_dir_all(logs.join("wetfish")).unwrap();
        // The outside sensor's newest humidity is faulty, and the inside
        // sensor's newest reading came from a nick after a reconnect.
        fs::write(
            logs.join("wetfish/#greenhouse.log"),
            r#"--- Log opened Wed Nov 04 18:00:10 2026
18:03 < greenhouse> {"sensor":"outside","temp":"49.46","humid":"58.80","timestamp":"2026-11-05T01:03:07.195Z"}
18:03 < greenhouse> {"sensor":"inside","temp":"51.44","humid":"49.60","timestamp":"2026-11-05T01:03:07.248Z"}
18:03 < greenhouse> {"sensor":"outside","temp":"49.50","humid":"58.90","timestamp":"2026-11-05T01:03:37.195Z"}
18:04 < greenhouse> {"sensor":"outside","temp":"49.55","humid":"302","timestamp":"2026-11-05T01:04:07.195Z"}
18:04 < greenhouse_> {"sensor":"inside","temp":"51.50","humid":"49.70","timestamp":"2026-11-05T01:04:07.248Z"}
"#,
        )
        .unwrap();
        let mut conn = db::open(&root.0.join("sensors.sqlite")).unwrap();
        analysis::run(&mut conn, &[logs], None, false, &mut Progress::quiet()).unwrap();
        let offsets = Offsets::load(&conn).unwrap();

        let greenhouse = newest(&conn, "#GreenHouse", &offsets, 0).unwrap();
        // "sensor time measure value ...", for each sensor.
        let found: Vec<String> = greenhouse
            .sensors
            .iter()
            .map(|sensor| {
                let values: Vec<String> = sensor
                    .values
                    .iter()
                    .map(|(measure, value)| format!("{measure} {value}"))
                    .collect();
                format!("{} {} {}", sensor.sensor, sensor.local, values.join(" "))
            })
            .collect();
        assert_eq!(
            found,
            [
                "inside 2026-11-04 18:04 temp 51.5 humid 49.7",
                "outside 2026-11-04 18:04 temp 49.55 humid 58.9",
            ]
        );
        assert!(
            newest(&conn, "#elsewhere", &offsets, 0)
                .unwrap()
                .sensors
                .is_empty()
        );
    }

    #[test]
    fn shows_a_close_up_with_its_faults_and_their_lines() {
        // 2026-11-04 00:00 as the logs show times, 07:00 UTC.
        let at = 1_793_750_400_000;
        let close_up_graph = Graph {
            start: at - CLOSE_UP_SIDE,
            end: at + CLOSE_UP_SIDE,
            point_size: 1,
            gap: 2 * graphs::MINUTE,
            sensors: vec!["inside".into()],
            measures: vec![Measure {
                measure: "humid".into(),
                lines: vec![Line {
                    sensor: "inside".into(),
                    points: vec![Point {
                        at: at - 30_000,
                        readings: 1,
                        total: 40.0,
                        low: 40.0,
                        high: 40.0,
                    }],
                    faults: vec![Fault {
                        at,
                        utc: at + 7 * HOUR,
                        value: 75.5,
                        reason: "spike".into(),
                    }],
                    latest: None,
                }],
            }],
        };
        let shown = Shown {
            channel: "#greenhouse".into(),
            period: Period::containing(Range::Day, date(2026, 11, 4), 0, at + HOUR),
            date: date(2026, 11, 4),
            first: 0,
            last: at + HOUR,
        };
        let html = close_up(Site::Review, &shown, at, &close_up_graph).into_string();
        assert!(html.contains(
            r#"<h2 class="period-name">Close-up of 00:00:00 on Wednesday, November 4, 2026</h2>"#
        ));
        assert!(html.contains(r#">Wednesday, November 4</a>"#));
        assert!(html.contains(r#"<span aria-current="page">Close-up</span>"#));
        assert!(html.contains("Every reading from 23:00:00 to 01:00:00"));
        assert!(html.contains(
            r#"href="/sensors/close-up?channel=%23greenhouse&amp;at=2026-11-03T23:00:00" rel="prev""#
        ));
        assert!(html.contains("<h2>Humidity (% RH)</h2>"));
        assert!(html.contains("fault-dot"));
        assert!(html.contains(
            r#"<td>00:00:00</td><td>inside</td><td>Humidity</td><td class="num">75.5</td><td>spike</td>"#
        ));
        assert!(html.contains(
            r#"href="/history?day=2026-11-04&amp;channel=%23greenhouse&amp;q=2026-11-04T07%3A00%3A00.000Z""#
        ));
        // The live site has no history to link to.
        let live = close_up(Site::Live, &shown, at, &close_up_graph).into_string();
        assert!(live.contains("<td>spike</td></tr>"));
        assert!(!live.contains("/history") && !live.contains("<th>Line</th>"));

        let quiet = Graph {
            measures: vec![],
            ..close_up_graph
        };
        assert!(
            close_up(Site::Review, &shown, at, &quiet)
                .into_string()
                .contains("No faulty readings in these two hours.")
        );

        let query = CloseUpQuery {
            channel: None,
            at: Some("2026-11-04T00:00:00".into()),
        };
        assert_eq!(query.at(), Some(at));
        assert_eq!(CloseUpQuery::default().at(), None);
        assert_eq!(
            close_up_href("#greenhouse", at + 1_234),
            "/sensors/close-up?channel=%23greenhouse&at=2026-11-04T00:00:01"
        );
    }
}

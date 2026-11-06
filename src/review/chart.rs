//! Charts of readings, drawn on the server: one chart a measure, with a line
//! a sensor through the mean of each point and a faint band from its lowest
//! to its highest reading. Faulty readings, which the lines and the axis
//! leave out, are flagged as red marks along the top or bottom edge, on the
//! side of the chart their values lie, with what they were in the mark's
//! title. In a close-up of every reading, spikes are drawn as red dots at
//! their values instead.
//!
//! A chart can zoom: the times below it link to the period one step in, and
//! each fault mark to a close-up around it. Each edge mark carries its faulty
//! readings in `data-faults`, for the script to draw them into the chart
//! while the mark is hovered or focused. They need no script. `chart.js`
//! adds a crosshair and a tooltip from the points in the chart's
//! `data-points`, and zooms where the chart is clicked, from its
//! `data-zoom`, `data-start` and `data-end`.
//!
//! The lines are an SVG stretched to the plot's box, with strokes that keep
//! their width, while the axis labels are HTML placed by percentage, so text
//! stays legible at any width.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime};
use maud::{Markup, html};
use serde_json::json;

use crate::faults::Reason;
use crate::graphs::{DAY, Graph, Line, MINUTE, Measure, Point, date_of, decimal, time_text};
use crate::readings::HOUR;

/// The SVG's drawing space, stretched to the plot's box.
const SIZE: f64 = 1000.0;

/// Colors for sensors, in their sorted order.
const SLOTS: usize = 8;

/// The most time ticks along the bottom.
const MAX_TICKS: usize = 8;

/// What a chart offers besides its lines.
pub struct Options<'a> {
    /// Flag faulty readings.
    pub faults: bool,
    /// The address of the period one step in holding a date, for clicks and
    /// the times below; `None` at the last step.
    pub zoom: Option<&'a dyn Fn(NaiveDate) -> String>,
    /// The address of a close-up around a time, as the logs show it, for
    /// the fault marks.
    pub close_up: Option<&'a dyn Fn(i64) -> String>,
}

impl Options<'_> {
    /// Flagging faulty readings or not, with nothing to zoom to.
    pub fn flags(faults: bool) -> Self {
        Options {
            faults,
            zoom: None,
            close_up: None,
        }
    }
}

/// A sensor's color slot, from 1, kept across measures and ranges; 0, gray,
/// past the eighth sensor.
pub fn slot(graph: &Graph, sensor: &str) -> usize {
    slot_among(&graph.sensors, sensor)
}

/// A sensor's color slot among a channel's sensors, sorted, as a graph
/// lists them.
pub fn slot_among(sensors: &[String], sensor: &str) -> usize {
    match sensors.iter().position(|known| known == sensor) {
        Some(index) if index < SLOTS => index + 1,
        _ => 0,
    }
}

/// What a measure is called, and its unit. The greenhouse's temperatures
/// step by 0.18, a tenth of a degree Celsius in Fahrenheit.
pub fn measure_name(measure: &str) -> (String, &'static str) {
    match measure {
        "temp" => ("Temperature".to_string(), "°F"),
        "humid" => ("Humidity".to_string(), "% RH"),
        other => (other.to_string(), ""),
    }
}

/// A sensor's name, or a dash for lines without one.
pub fn sensor_name(sensor: &str) -> &str {
    if sensor.is_empty() { "-" } else { sensor }
}

/// How long a point is, such as `2 minutes`, `1 hour` or `1 week`, or
/// `each reading` for every reading on its own.
pub fn duration_words(ms: i64) -> String {
    if ms < MINUTE {
        return "each reading".to_string();
    }
    let (count, unit) = if ms % (7 * DAY) == 0 {
        (ms / (7 * DAY), "week")
    } else if ms % DAY == 0 {
        (ms / DAY, "day")
    } else if ms % HOUR == 0 {
        (ms / HOUR, "hour")
    } else {
        (ms / MINUTE, "minute")
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

/// A point's start, with the time of day when points are shorter than a
/// day, and to the second for every reading on its own.
fn point_time(at: i64, point_size: i64) -> String {
    let text = time_text(at, point_size < MINUTE);
    if point_size >= DAY {
        text.get(..10).unwrap_or(&text).to_string()
    } else {
        text
    }
}

/// One sensor's faulty readings in one point's time.
struct FaultGroup {
    /// The point's start, as the logs show times.
    at: i64,
    /// The first faulty reading's time, as the logs show times.
    first: i64,
    /// The line's index among the measure's lines.
    line: usize,
    values: Vec<f64>,
    /// Each value's time, as the logs show times.
    times: Vec<i64>,
    reasons: BTreeSet<String>,
}

impl FaultGroup {
    /// What they were, such as `2 faulty readings: 302, 302.5, out of range`.
    fn describe(&self) -> String {
        const SHOWN: usize = 3;
        let mut values: Vec<String> = self
            .values
            .iter()
            .take(SHOWN)
            .map(|&v| decimal(v))
            .collect();
        if self.values.len() > SHOWN {
            values.push("…".to_string());
        }
        let reasons: Vec<&str> = self.reasons.iter().map(|r| Reason::describe(r)).collect();
        let count = self.values.len();
        format!(
            "{count} faulty reading{}: {}, {}",
            if count == 1 { "" } else { "s" },
            values.join(", "),
            reasons.join(" and ")
        )
    }
}

/// The faulty readings of a measure, grouped by sensor and point.
fn fault_groups(graph: &Graph, measure: &Measure) -> Vec<FaultGroup> {
    let mut groups: BTreeMap<(i64, usize), FaultGroup> = BTreeMap::new();
    for (index, line) in measure.lines.iter().enumerate() {
        for fault in &line.faults {
            let at = fault.at.div_euclid(graph.point_size) * graph.point_size;
            let group = groups.entry((at, index)).or_insert_with(|| FaultGroup {
                at,
                first: fault.at,
                line: index,
                values: Vec::new(),
                times: Vec::new(),
                reasons: BTreeSet::new(),
            });
            group.first = group.first.min(fault.at);
            group.values.push(fault.value);
            group.times.push(fault.at);
            group.reasons.insert(fault.reason.clone());
        }
    }
    groups.into_values().collect()
}

impl FaultGroup {
    /// Whether it is drawn as a dot at its value: spikes, in a close-up of
    /// every reading, where they are what is being looked at.
    fn dot(&self, graph: &Graph) -> bool {
        graph.point_size < MINUTE && self.reasons.iter().all(|reason| reason == "spike")
    }
}

impl FaultGroup {
    /// Its readings for the script, oldest first, as `[[x, value], ...]`,
    /// with `x` from 0 to 1 across the plot.
    fn preview(&self, scale: &Scale) -> String {
        let mut readings: Vec<(i64, f64)> = self
            .times
            .iter()
            .copied()
            .zip(self.values.iter().copied())
            .collect();
        readings.sort_by_key(|&(at, _)| at);
        let readings: Vec<[f64; 2]> = readings
            .into_iter()
            .map(|(at, value)| [(scale.x(at) / SIZE * 10_000.0).round() / 10_000.0, value])
            .collect();
        json!(readings).to_string()
    }
}

/// A fault mark, linking to a close-up when there is one. An edge mark
/// carries its line's index and its readings, for the script to draw.
fn fault_mark(
    class: &str,
    style: &str,
    title: &str,
    href: Option<String>,
    preview: Option<(usize, String)>,
) -> Markup {
    let (line, faults) = preview.unzip();
    html! {
        @match href {
            Some(href) => a class=(class) style=(style) title=(title) href=(href)
                data-line=[line] data-faults=[faults] {},
            None => span class=(class) style=(style) title=(title)
                data-line=[line] data-faults=[faults] {},
        }
    }
}

/// One measure's chart: its legend, the plot with its axes, the faulty
/// readings' marks when they are flagged, and the points for the script.
pub fn chart(graph: &Graph, measure: &Measure, options: &Options<'_>) -> Markup {
    let (name, unit) = measure_name(&measure.measure);
    let groups = if options.faults {
        fault_groups(graph, measure)
    } else {
        Vec::new()
    };
    // Spikes drawn as dots count towards the axis; other faults never do.
    let dots = || {
        groups
            .iter()
            .filter(|group| group.dot(graph))
            .flat_map(|group| &group.values)
            .copied()
    };
    let points = || measure.lines.iter().flat_map(|line| &line.points);
    let low = points()
        .map(|point| point.low)
        .chain(dots())
        .fold(f64::INFINITY, f64::min);
    let high = points()
        .map(|point| point.high)
        .chain(dots())
        .fold(f64::NEG_INFINITY, f64::max);
    if !low.is_finite() || !high.is_finite() {
        return html! { p { "No " (name.to_lowercase()) " readings in this period." } };
    }
    let (values, decimals) = value_ticks(low, high);
    let scale = Scale {
        start: graph.start,
        end: graph.end,
        low: values[0],
        high: values[values.len() - 1],
    };
    let middle = (scale.low + scale.high) / 2.0;
    let zoom = options.zoom;
    // The address of a period one step in, at a date the script replaces.
    let zoom_template = zoom
        .zip(NaiveDate::from_ymd_opt(1, 1, 1))
        .map(|(zoom, sentinel)| zoom(sentinel));
    let label = format!(
        "{name} from {} to {}: {}",
        time_text(graph.start, false),
        time_text(graph.end, false),
        measure
            .lines
            .iter()
            .filter_map(|line| {
                let summary = line.summary()?;
                Some(format!(
                    "{} {} to {}",
                    sensor_name(&line.sensor),
                    decimal(summary.low),
                    decimal(summary.high)
                ))
            })
            .collect::<Vec<_>>()
            .join(", ")
    );
    html! {
        figure.chart data-points=(points_json(graph, measure, unit, &groups, &scale))
            data-zoom=[zoom_template] data-start=(graph.start) data-end=(graph.end) {
            @if measure.lines.len() > 1 || !groups.is_empty() {
                div.chart-legend {
                    @for line in &measure.lines {
                        span.legend-item {
                            span class=(format!("line-key s{}", slot(graph, &line.sensor))) {}
                            (sensor_name(&line.sensor))
                        }
                    }
                    @if !groups.is_empty() {
                        span.legend-item {
                            span.fault-key {}
                            "Faulty readings, left out"
                        }
                    }
                }
            }
            div.chart-body {
                div.y-axis aria-hidden="true" {
                    @for value in &values {
                        span style=(format!("top: {:.2}%", scale.y(*value) / SIZE * 100.0)) {
                            // Adding zero turns -0 into 0.
                            (format!("{:.decimals$}", value + 0.0))
                        }
                    }
                }
                div.plot.zoomable[zoom.is_some()] role="img" aria-label=(label) {
                    svg viewBox="0 0 1000 1000" preserveAspectRatio="none" aria-hidden="true" {
                        @for value in &values {
                            line.grid x1="0" x2="1000"
                                y1=(format!("{:.1}", scale.y(*value)))
                                y2=(format!("{:.1}", scale.y(*value))) {}
                        }
                        // The script rescales this while it draws faults off the axis.
                        g.data {
                            @for line in &measure.lines {
                                @let paths = paths(line, graph.point_size, graph.gap, &scale);
                                @let slot = slot(graph, &line.sensor);
                                path class=(format!("band s{slot}")) d=(paths.band) {}
                                path class=(format!("line s{slot}")) d=(paths.line) {}
                            }
                        }
                    }
                    @if !groups.is_empty() {
                        div.fault-marks {
                            @for group in &groups {
                                @let line = &measure.lines[group.line];
                                @let left = scale.x(group.at + graph.point_size / 2) / SIZE * 100.0;
                                @let title = format!(
                                    "{} {}: {}",
                                    point_time(group.at, graph.point_size),
                                    sensor_name(&line.sensor),
                                    group.describe()
                                );
                                @let href = || options.close_up.map(|close_up| close_up(group.first));
                                @if group.dot(graph) {
                                    @for value in &group.values {
                                        (fault_mark(
                                            "fault-dot",
                                            &format!(
                                                "left: {left:.2}%; top: {:.2}%",
                                                scale.y(*value) / SIZE * 100.0
                                            ),
                                            &title,
                                            None,
                                            None,
                                        ))
                                    }
                                } @else {
                                    @for top in [true, false] {
                                        @if group.values.iter().any(|&v| (v >= middle) == top) {
                                            (fault_mark(
                                                if top { "fault top" } else { "fault bottom" },
                                                &format!("left: {left:.2}%"),
                                                &title,
                                                href(),
                                                Some((group.line, group.preview(&scale))),
                                            ))
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                div.x-axis {
                    @for (at, text) in time_ticks(graph.start, graph.end) {
                        @let style = format!("left: {:.2}%", scale.x(at) / SIZE * 100.0);
                        @match zoom {
                            Some(zoom) => a style=(style) href=(zoom(date_of(at))) { (text) },
                            None => span style=(style) aria-hidden="true" { (text) },
                        }
                    }
                }
            }
        }
    }
}

/// Where times and values fall in the SVG's drawing space.
struct Scale {
    start: i64,
    end: i64,
    low: f64,
    high: f64,
}

impl Scale {
    fn x(&self, at: i64) -> f64 {
        let span = (self.end - self.start).max(1) as f64;
        ((at - self.start) as f64 / span * SIZE).clamp(0.0, SIZE)
    }

    fn y(&self, value: f64) -> f64 {
        SIZE - (value - self.low) / (self.high - self.low) * SIZE
    }

    /// A point's place: the middle of its time, kept inside the range.
    fn point_x(&self, point: &Point, point_size: i64) -> f64 {
        self.x(point.at + point_size / 2)
    }
}

/// A line's SVG paths.
struct Paths {
    line: String,
    band: String,
}

/// The paths of a line, broken wherever points are more than `gap` apart,
/// so a gap in the readings shows as a gap. A point on its own is a dot.
fn paths(line: &Line, point_size: i64, gap: i64, scale: &Scale) -> Paths {
    let mut paths = Paths {
        line: String::new(),
        band: String::new(),
    };
    let mut runs: Vec<&[Point]> = Vec::new();
    let mut from = 0;
    for i in 1..=line.points.len() {
        if i == line.points.len() || line.points[i].at - line.points[i - 1].at > gap {
            runs.push(&line.points[from..i]);
            from = i;
        }
    }
    for run in runs.into_iter().filter(|run| !run.is_empty()) {
        let at = |point: &Point, value: f64| {
            format!(
                "{:.1},{:.1}",
                scale.point_x(point, point_size),
                scale.y(value)
            )
        };
        let first = &run[0];
        let _ = write!(paths.line, "M{}", at(first, first.mean()));
        if run.len() == 1 {
            // A zero-length segment, drawn as a dot by its round cap.
            let _ = write!(paths.line, " L{}", at(first, first.mean()));
            continue;
        }
        for point in &run[1..] {
            let _ = write!(paths.line, " L{}", at(point, point.mean()));
        }
        let _ = write!(paths.band, "M{}", at(first, first.high));
        for point in &run[1..] {
            let _ = write!(paths.band, " L{}", at(point, point.high));
        }
        for point in run.iter().rev() {
            let _ = write!(paths.band, " L{}", at(point, point.low));
        }
        paths.band.push('Z');
    }
    paths
}

/// The points as JSON for the script: each point's time label and place
/// across the plot, and each sensor's mean, lowest and highest value there,
/// or `null` where it has none. In a close-up, where each reading is its own
/// point and sensors post a moment apart, `near` is how far across the plot
/// the tooltip may look for a sensor's nearest reading: the line's gap.
fn points_json(
    graph: &Graph,
    measure: &Measure,
    unit: &str,
    groups: &[FaultGroup],
    axis: &Scale,
) -> String {
    let scale = Scale {
        start: graph.start,
        end: graph.end,
        low: 0.0,
        high: 1.0,
    };
    let mut index: BTreeMap<i64, usize> = BTreeMap::new();
    for point in measure.lines.iter().flat_map(|line| &line.points) {
        index.insert(point.at, 0);
    }
    for group in groups {
        index.insert(group.at, 0);
    }
    for (i, slot) in index.values_mut().enumerate() {
        *slot = i;
    }
    let round = |value: f64| (value * 100.0).round() / 100.0;
    let near = if graph.point_size < MINUTE {
        let span = (graph.end - graph.start).max(1) as f64;
        (graph.gap as f64 / span * 1_000_000.0).round() / 1_000_000.0
    } else {
        0.0
    };
    let times: Vec<String> = index
        .keys()
        .map(|&at| point_time(at, graph.point_size))
        .collect();
    let x: Vec<f64> = index
        .keys()
        .map(|&at| {
            let middle = at + graph.point_size / 2;
            (scale.x(middle) / SIZE * 10_000.0).round() / 10_000.0
        })
        .collect();
    let series: Vec<serde_json::Value> = measure
        .lines
        .iter()
        .enumerate()
        .map(|(number, line)| {
            let mut mean = vec![None; index.len()];
            let mut low = vec![None; index.len()];
            let mut high = vec![None; index.len()];
            let mut faults: Vec<Option<String>> = vec![None; index.len()];
            for point in &line.points {
                let i = index[&point.at];
                mean[i] = Some(round(point.mean()));
                low[i] = Some(round(point.low));
                high[i] = Some(round(point.high));
            }
            for group in groups.iter().filter(|group| group.line == number) {
                faults[index[&group.at]] = Some(group.describe());
            }
            json!({
                "name": sensor_name(&line.sensor),
                "slot": slot(graph, &line.sensor),
                "mean": mean,
                "low": low,
                "high": high,
                "faults": faults,
            })
        })
        .collect();
    json!({
        "times": times,
        "x": x,
        "unit": unit,
        "every": duration_words(graph.point_size),
        "near": near,
        // The values at the bottom and top of the chart.
        "low": axis.low,
        "high": axis.high,
        "series": series,
    })
    .to_string()
}

/// Round values for the side of a chart, from at or below `low` to at or
/// above `high`, about five of them, and how many decimals they need.
fn value_ticks(low: f64, high: f64) -> (Vec<f64>, usize) {
    let (low, high) = if high - low < 1e-9 {
        (low - 1.0, high + 1.0)
    } else {
        (low, high)
    };
    let rough = (high - low) / 5.0;
    let magnitude = 10f64.powf(rough.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|&step| step >= rough * (1.0 - 1e-9))
        .unwrap_or(10.0 * magnitude);
    let first = (low / step).floor() as i64;
    let last = (high / step).ceil() as i64;
    let values = (first..=last).map(|n| n as f64 * step).collect();
    let decimals = (0..=6)
        .find(|&d| {
            let scaled = step * 10f64.powi(d as i32);
            (scaled - scaled.round()).abs() < 1e-6
        })
        .unwrap_or(6);
    (values, decimals)
}

/// How far apart time ticks are.
#[derive(Clone, Copy)]
enum Step {
    Minutes(i64),
    Hours(i64),
    Days(i64),
    Weeks,
    Months(i32),
}

const STEPS: [Step; 17] = [
    Step::Minutes(10),
    Step::Minutes(15),
    Step::Minutes(30),
    Step::Hours(1),
    Step::Hours(3),
    Step::Hours(6),
    Step::Hours(12),
    Step::Days(1),
    Step::Days(2),
    Step::Weeks,
    Step::Months(1),
    Step::Months(2),
    Step::Months(3),
    Step::Months(6),
    Step::Months(12),
    Step::Months(24),
    Step::Months(60),
];

/// Ticks along the bottom from `start` up to, but not at, `end`, where the
/// next period starts, on round hours, midnights,
/// Mondays or the first of a month, with their labels: the time of day, a
/// date such as `Nov 3`, a month, or a year at January.
fn time_ticks(start: i64, end: i64) -> Vec<(i64, String)> {
    let mut ticks = Vec::new();
    for step in STEPS {
        ticks = ticks_every(step, start, end);
        if ticks.len() <= MAX_TICKS {
            break;
        }
    }
    ticks
}

fn ticks_every(step: Step, start: i64, end: i64) -> Vec<(i64, String)> {
    let mut ticks = Vec::new();
    let every = |size: i64, shift: i64, ticks: &mut Vec<(i64, String)>, day_labels: bool| {
        let mut at = (start - shift).div_euclid(size) * size + shift;
        if at < start {
            at += size;
        }
        while at < end && ticks.len() <= MAX_TICKS {
            let format = if day_labels || at.rem_euclid(DAY) == 0 {
                "%b %-d"
            } else {
                "%H:%M"
            };
            ticks.push((at, naive(at).format(format).to_string()));
            at += size;
        }
    };
    match step {
        Step::Minutes(minutes) => every(minutes * MINUTE, 0, &mut ticks, false),
        Step::Hours(hours) => every(hours * HOUR, 0, &mut ticks, false),
        Step::Days(days) => every(days * DAY, 0, &mut ticks, true),
        // 1970-01-05 was a Monday.
        Step::Weeks => every(7 * DAY, 4 * DAY, &mut ticks, true),
        Step::Months(months) => {
            let date = naive(start).date();
            let mut month = date.year() * 12 + date.month0() as i32;
            if date.day() != 1 || naive(start).time() != NaiveTime::MIN {
                month += 1;
            }
            month += (months - month.rem_euclid(months)) % months;
            while let Some(first) =
                NaiveDate::from_ymd_opt(month.div_euclid(12), month.rem_euclid(12) as u32 + 1, 1)
            {
                let at = first.and_time(NaiveTime::MIN).and_utc().timestamp_millis();
                if at >= end || ticks.len() > MAX_TICKS {
                    break;
                }
                let format = if months >= 12 || first.month() == 1 {
                    "%Y"
                } else {
                    "%b"
                };
                ticks.push((at, first.format(format).to_string()));
                month += months;
            }
        }
    }
    ticks
}

/// A time as the logs show it, as a date and time.
fn naive(at: i64) -> NaiveDateTime {
    DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .naive_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphs::Fault;

    /// Milliseconds for a time on a day of November 2026.
    fn nov(day: u32, hour: u32, minute: u32) -> i64 {
        NaiveDate::from_ymd_opt(2026, 11, day)
            .unwrap()
            .and_hms_opt(hour, minute, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    fn labels(ticks: &[(i64, String)]) -> Vec<&str> {
        ticks.iter().map(|(_, text)| text.as_str()).collect()
    }

    #[test]
    fn finds_round_values_for_the_side() {
        assert_eq!(
            value_ticks(48.92, 71.42),
            (vec![45.0, 50.0, 55.0, 60.0, 65.0, 70.0, 75.0], 0)
        );
        assert_eq!(
            value_ticks(35.5, 38.8),
            (vec![35.0, 36.0, 37.0, 38.0, 39.0], 0)
        );
        assert_eq!(value_ticks(67.46, 67.64).1, 2);
        let (flat, _) = value_ticks(50.0, 50.0);
        assert!(flat[0] <= 49.0 && *flat.last().unwrap() >= 51.0);
    }

    #[test]
    fn puts_time_ticks_on_round_times() {
        let day = time_ticks(nov(4, 5, 6), nov(5, 5, 6));
        assert_eq!(
            labels(&day),
            [
                "06:00", "09:00", "12:00", "15:00", "18:00", "21:00", "Nov 5", "03:00"
            ]
        );
        let week = time_ticks(nov(1, 12, 0), nov(8, 12, 0));
        assert_eq!(
            labels(&week),
            [
                "Nov 2", "Nov 3", "Nov 4", "Nov 5", "Nov 6", "Nov 7", "Nov 8"
            ]
        );
        // November, up to December 1.
        let month = time_ticks(nov(1, 0, 0), nov(30, 0, 0) + DAY);
        assert_eq!(
            labels(&month),
            ["Nov 2", "Nov 9", "Nov 16", "Nov 23", "Nov 30"]
        );
        let year = time_ticks(nov(5, 0, 0) - 365 * DAY, nov(5, 0, 0));
        assert_eq!(labels(&year), ["2026", "Mar", "May", "Jul", "Sep", "Nov"]);
        let years = time_ticks(nov(5, 0, 0) - 600 * DAY, nov(5, 0, 0));
        assert_eq!(
            labels(&years),
            ["Apr", "Jul", "Oct", "2026", "Apr", "Jul", "Oct"]
        );
    }

    #[test]
    fn names_measures_and_point_sizes() {
        assert_eq!(measure_name("temp"), ("Temperature".to_string(), "°F"));
        assert_eq!(measure_name("moisture"), ("moisture".to_string(), ""));
        assert_eq!(duration_words(2 * MINUTE), "2 minutes");
        assert_eq!(duration_words(HOUR), "1 hour");
        assert_eq!(duration_words(12 * HOUR), "12 hours");
        assert_eq!(duration_words(7 * DAY), "1 week");
        assert_eq!(point_time(nov(3, 13, 46), 2 * MINUTE), "2026-11-03 13:46");
        assert_eq!(point_time(nov(3, 0, 0), DAY), "2026-11-03");
    }

    fn point(at: i64, mean: f64) -> Point {
        Point {
            at,
            readings: 1,
            total: mean,
            low: mean - 1.0,
            high: mean + 1.0,
        }
    }

    fn graph(lines: Vec<Line>) -> Graph {
        Graph {
            start: nov(3, 0, 0),
            end: nov(3, 10, 0),
            point_size: HOUR,
            gap: HOUR,
            sensors: vec!["inside".into(), "outside".into()],
            measures: vec![Measure {
                measure: "temp".into(),
                lines,
            }],
        }
    }

    #[test]
    fn breaks_lines_at_gaps_and_draws_lone_points_as_dots() {
        let line = Line {
            sensor: "inside".into(),
            points: vec![
                point(nov(3, 0, 0), 50.0),
                point(nov(3, 1, 0), 60.0),
                point(nov(3, 5, 0), 55.0),
            ],
            faults: vec![],
            latest: None,
        };
        let scale = Scale {
            start: nov(3, 0, 0),
            end: nov(3, 10, 0),
            low: 40.0,
            high: 70.0,
        };
        let paths = paths(&line, HOUR, HOUR, &scale);
        assert_eq!(
            paths.line,
            "M50.0,666.7 L150.0,333.3M550.0,500.0 L550.0,500.0"
        );
        assert_eq!(
            paths.band,
            "M50.0,633.3 L150.0,300.0 L150.0,366.7 L50.0,700.0Z"
        );
    }

    #[test]
    fn draws_a_chart_with_a_legend_and_points_for_the_script() {
        let lines = vec![
            Line {
                sensor: "inside".into(),
                points: vec![point(nov(3, 0, 0), 50.0), point(nov(3, 1, 0), 60.0)],
                faults: vec![],
                latest: None,
            },
            Line {
                sensor: "outside".into(),
                points: vec![point(nov(3, 1, 0), 45.5)],
                faults: vec![],
                latest: None,
            },
        ];
        let graph = graph(lines);
        let html = chart(&graph, &graph.measures[0], &Options::flags(true)).into_string();
        assert!(html.contains(r#"<span class="line-key s1"></span>inside"#));
        assert!(html.contains(r#"<span class="line-key s2"></span>outside"#));
        assert!(html.contains(r#"<path class="line s2" d="M150.0,"#));
        assert!(html.contains(r#"aria-label="Temperature from 2026-11-03 00:00 to 2026-11-03 10:00: inside 49 to 61, outside 44.5 to 46.5""#));
        assert!(html.contains(
            "&quot;times&quot;:[&quot;2026-11-03 00:00&quot;,&quot;2026-11-03 01:00&quot;]"
        ));
        assert!(html.contains("&quot;mean&quot;:[null,45.5]"));
        assert!(html.contains("&quot;every&quot;:&quot;1 hour&quot;"));

        let empty = graph_with_no_points();
        assert!(
            chart(&empty, &empty.measures[0], &Options::flags(true))
                .into_string()
                .contains("No temperature readings")
        );
    }

    #[test]
    fn flags_faulty_readings_at_the_edge_they_lie_beyond() {
        let fault = |at: i64, value: f64, reason: &str| Fault {
            at,
            utc: at + 7 * HOUR,
            value,
            reason: reason.into(),
        };
        let line = Line {
            sensor: "inside".into(),
            points: vec![point(nov(3, 0, 0), 50.0), point(nov(3, 1, 0), 60.0)],
            faults: vec![
                fault(nov(3, 0, 10), 302.0, "range"),
                fault(nov(3, 0, 40), 302.5, "range"),
                fault(nov(3, 2, 0), -58.0, "range"),
            ],
            latest: None,
        };
        let graph = graph(vec![line]);
        let html = chart(&graph, &graph.measures[0], &Options::flags(true)).into_string();
        assert!(html.contains(
            r#"<span class="fault top" style="left: 5.00%" title="2026-11-03 00:00 inside: 2 faulty readings: 302, 302.5, out of range" data-line="0" data-faults="[[0.0167,302.0],[0.0667,302.5]]"></span>"#
        ));
        assert!(html.contains(r#"<span class="fault bottom" style="left: 25.00%""#));
        assert!(html.contains("Faulty readings, left out"));
        assert!(html.contains("&quot;faults&quot;:[&quot;2 faulty readings: 302, 302.5, out of range&quot;,null,&quot;1 faulty reading: -58, out of range&quot;]"));
        // The axis leaves them out.
        assert!(!html.contains(">300<"));

        let hidden = chart(&graph, &graph.measures[0], &Options::flags(false)).into_string();
        assert!(!hidden.contains("fault top"));
        assert!(!hidden.contains("Faulty readings"));
        assert!(hidden.contains("&quot;faults&quot;:[null,null]"));
    }

    #[test]
    fn zooms_from_clicks_times_and_fault_marks() {
        let line = Line {
            sensor: "inside".into(),
            points: vec![point(nov(3, 0, 0), 50.0), point(nov(3, 1, 0), 60.0)],
            faults: vec![Fault {
                at: nov(3, 0, 10),
                utc: nov(3, 7, 10),
                value: 302.0,
                reason: "range".into(),
            }],
            latest: None,
        };
        let graph = graph(vec![line]);
        let zoom = |date: NaiveDate| format!("/zoom?at={date}");
        let close_up = |at: i64| format!("/close-up?at={}", time_text(at, true));
        let options = Options {
            faults: true,
            zoom: Some(&zoom),
            close_up: Some(&close_up),
        };
        let html = chart(&graph, &graph.measures[0], &options).into_string();
        assert!(html.contains(r#"data-zoom="/zoom?at=0001-01-01""#));
        assert!(html.contains(&format!(r#"data-start="{}""#, nov(3, 0, 0))));
        assert!(html.contains(r#"<div class="plot zoomable""#));
        assert!(html.contains(r#"<a style="left: 0.00%" href="/zoom?at=2026-11-03">Nov 3</a>"#));
        assert!(html.contains(r#"href="/close-up?at=2026-11-03 00:10:00""#));

        // The last step, a day, does not zoom.
        let plain = chart(&graph, &graph.measures[0], &Options::flags(true)).into_string();
        assert!(!plain.contains("data-zoom"));
        assert!(!plain.contains("zoomable"));
        assert!(plain.contains(r#"<span class="fault top""#));
    }

    #[test]
    fn draws_spikes_at_their_values_in_a_close_up() {
        let reading = |minute: u32, value: f64| point(nov(3, 13, minute), value);
        let line = Line {
            sensor: "inside".into(),
            points: vec![reading(40, 45.0), reading(41, 45.5), reading(43, 46.0)],
            faults: vec![
                Fault {
                    at: nov(3, 13, 42),
                    utc: nov(3, 20, 42),
                    value: 70.0,
                    reason: "spike".into(),
                },
                Fault {
                    at: nov(3, 13, 44),
                    utc: nov(3, 20, 44),
                    value: 302.0,
                    reason: "range".into(),
                },
            ],
            latest: None,
        };
        let close_up = Graph {
            start: nov(3, 13, 0),
            end: nov(3, 15, 0),
            point_size: 1,
            gap: 2 * MINUTE,
            ..graph(vec![line])
        };
        let html = chart(&close_up, &close_up.measures[0], &Options::flags(true)).into_string();
        // The axis reaches the spike, but not the reading out of range.
        assert!(html.contains(">70<"));
        assert!(!html.contains(">300<"));
        assert!(html.contains(r#"<span class="fault-dot" style="left: 35.00%; top: "#));
        assert!(html.contains(r#"<span class="fault top" style="left: 36.67%""#));
        assert!(html.contains("&quot;every&quot;:&quot;each reading&quot;"));
        assert!(html.contains("&quot;2026-11-03 13:42:00&quot;"));
        // Two minutes of the two hours, for the tooltip to find each sensor.
        assert!(html.contains("&quot;near&quot;:0.016667"));
        // Readings up to two minutes apart are joined, as 13:41 and 13:43 are.
        let paths = paths(
            &close_up.measures[0].lines[0],
            1,
            2 * MINUTE,
            &Scale {
                start: close_up.start,
                end: close_up.end,
                low: 40.0,
                high: 80.0,
            },
        );
        assert_eq!(paths.line.matches('M').count(), 1);
    }

    fn graph_with_no_points() -> Graph {
        graph(vec![Line {
            sensor: "inside".into(),
            points: vec![],
            faults: vec![],
            latest: None,
        }])
    }
}

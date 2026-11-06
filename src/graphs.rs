//! Graphs: one channel's readings over a calendar period, averaged into
//! points, and the same as CSV. A period is a day, a Monday to Sunday week,
//! a month or a year, as the logs show times, or everything.
//!
//! Every nick that posted to a channel counts as the same device, since a
//! device that reconnects often comes back under a fallback nick such as
//! `greenhouse1`. So a line is one measure from one sensor, every nick
//! together: the inside temperature, say.
//!
//! A period is cut into points of one size, the smallest that keeps a line
//! under `MAX_POINTS`. Points under an hour average the readings themselves,
//! leaving out a second copy of a reading posted under two nicks at the same
//! moment. Points of an hour or more add up the hourly rows in
//! `reading_hours`, so a year reads a row an hour, but count such a copy
//! twice.
//!
//! Faulty readings, as the `faults` module finds them, are left out of every
//! point, and kept beside each line for the page to flag. A close-up shows
//! the two hours around a time with every reading as its own point.
//!
//! Times on graphs are as the logs show them, in milliseconds since 1970 as
//! if the logs' time zone were UTC, so points and ticks fall on the logs'
//! hours and midnights.

use std::collections::BTreeMap;
use std::fmt::Write;

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, NaiveTime};
use rusqlite::types::Value;
use rusqlite::{Connection, params, params_from_iter};

use crate::db::Result;
use crate::readings::{HOUR, Offsets};

pub const MINUTE: i64 = 60_000;
pub const DAY: i64 = 24 * HOUR;

/// The most points a line has, short of ranges over 15 years.
const MAX_POINTS: i64 = 800;

/// Point sizes, smallest first. Each divides a day or is a whole number of
/// days, so points start on the logs' hours and midnights.
const POINT_SIZES: [i64; 11] = [
    MINUTE,
    2 * MINUTE,
    5 * MINUTE,
    15 * MINUTE,
    30 * MINUTE,
    HOUR,
    3 * HOUR,
    6 * HOUR,
    12 * HOUR,
    DAY,
    7 * DAY,
];

/// How much time a graph shows: a calendar day, week, month or year, or
/// everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Range {
    Day,
    Week,
    Month,
    Year,
    All,
}

impl Range {
    pub const EVERY: [Range; 5] = [
        Range::Day,
        Range::Week,
        Range::Month,
        Range::Year,
        Range::All,
    ];

    /// The range named in an address, such as `week`.
    pub fn parse(name: &str) -> Option<Range> {
        Range::EVERY.into_iter().find(|range| range.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Range::Day => "day",
            Range::Week => "week",
            Range::Month => "month",
            Range::Year => "year",
            Range::All => "all",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Range::Day => "Day",
            Range::Week => "Week",
            Range::Month => "Month",
            Range::Year => "Year",
            Range::All => "All",
        }
    }

    /// Whether every reading in it can be downloaded, as a CSV file held in
    /// memory: about 350,000 rows for a month of the greenhouse.
    pub fn readings_downloadable(self) -> bool {
        matches!(self, Range::Day | Range::Week | Range::Month)
    }
}

/// How far a close-up reaches on each side of its time.
pub const CLOSE_UP_SIDE: i64 = HOUR;

/// A calendar period of a range, as the logs show times.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Period {
    pub range: Range,
    /// When it starts, and when the next one starts.
    pub start: i64,
    pub end: i64,
}

/// Midnight at the start of a date, as the logs show times.
pub fn midnight(date: NaiveDate) -> i64 {
    date.and_time(NaiveTime::MIN).and_utc().timestamp_millis()
}

/// The date of a time, as the logs show times.
pub fn date_of(at: i64) -> NaiveDate {
    DateTime::from_timestamp_millis(at)
        .unwrap_or_default()
        .date_naive()
}

impl Period {
    /// The period of `range` that holds `date`. For everything, the
    /// readings' first and last times, as the logs show them.
    pub fn containing(range: Range, date: NaiveDate, first: i64, last: i64) -> Period {
        let (start, end) = match range {
            Range::Day => (date, date + Days::new(1)),
            Range::Week => {
                let monday = date - Days::new(u64::from(date.weekday().num_days_from_monday()));
                (monday, monday + Days::new(7))
            }
            Range::Month => {
                let first = date.with_day(1).unwrap_or(date);
                (first, first + Months::new(1))
            }
            Range::Year => {
                let first = NaiveDate::from_ymd_opt(date.year(), 1, 1).unwrap_or(date);
                (first, first + Months::new(12))
            }
            Range::All => {
                return Period {
                    range,
                    start: first,
                    end: last + 1,
                };
            }
        };
        Period {
            range,
            start: midnight(start),
            end: midnight(end),
        }
    }

    /// The day it starts on.
    pub fn first_day(&self) -> NaiveDate {
        date_of(self.start)
    }

    /// A date in the period before, when readings go back that far.
    pub fn earlier(&self, first: i64) -> Option<NaiveDate> {
        (self.range != Range::All && self.start > first).then(|| date_of(self.start - 1))
    }

    /// A date in the period after, when readings go on that far.
    pub fn later(&self, last: i64) -> Option<NaiveDate> {
        (self.range != Range::All && self.end <= last).then(|| date_of(self.end))
    }
}

/// The point size for a stretch of time: the smallest that keeps a line
/// under `MAX_POINTS`, or a week.
pub fn point_size(span: i64) -> i64 {
    POINT_SIZES
        .into_iter()
        .find(|&size| span / size <= MAX_POINTS)
        .unwrap_or(7 * DAY)
}

/// Every channel with readings, sorted.
pub fn channels(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT channel FROM sensor_series ORDER BY channel")?;
    let rows = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// One measure from one sensor in a channel, and its series, one a nick.
struct Group {
    measure: String,
    sensor: String,
    ids: Vec<i64>,
}

/// The measures with a known order come first: temperature, then humidity.
pub fn measure_rank(measure: &str) -> (usize, &str) {
    const KNOWN: [&str; 2] = ["temp", "humid"];
    let rank = KNOWN
        .iter()
        .position(|known| *known == measure)
        .unwrap_or(KNOWN.len());
    (rank, measure)
}

/// The channel's groups, by measure, then sensor.
fn groups(conn: &Connection, channel: &str) -> Result<Vec<Group>> {
    let mut stmt = conn.prepare(
        "SELECT id, measure, sensor FROM sensor_series WHERE channel = ?1 COLLATE NOCASE",
    )?;
    let mut rows = stmt.query(params![channel])?;
    let mut by_key: BTreeMap<(String, String), Vec<i64>> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let (id, measure, sensor): (i64, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
        by_key.entry((measure, sensor)).or_default().push(id);
    }
    let mut groups: Vec<Group> = by_key
        .into_iter()
        .map(|((measure, sensor), ids)| Group {
            measure,
            sensor,
            ids,
        })
        .collect();
    groups.sort_by(|a, b| {
        measure_rank(&a.measure)
            .cmp(&measure_rank(&b.measure))
            .then_with(|| a.sensor.cmp(&b.sensor))
    });
    Ok(groups)
}

/// `?first` to `?first + count - 1`, separated by commas.
fn placeholders(first: usize, count: usize) -> String {
    (first..first + count)
        .map(|n| format!("?{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The first and newest reading in the groups, in milliseconds since 1970
/// UTC. Each series is asked on its own, so its primary key answers at once.
fn extent(conn: &Connection, groups: &[Group]) -> Result<Option<(i64, i64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT (SELECT min(at) FROM readings WHERE series_id = ?1),
                (SELECT max(at) FROM readings WHERE series_id = ?1)",
    )?;
    let mut extent: Option<(i64, i64)> = None;
    for id in groups.iter().flat_map(|group| &group.ids) {
        let (first, last): (Option<i64>, Option<i64>) =
            stmt.query_row(params![id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        if let (Some(first), Some(last)) = (first, last) {
            extent = Some(match extent {
                Some((a, b)) => (a.min(first), b.max(last)),
                None => (first, last),
            });
        }
    }
    Ok(extent)
}

/// The first and newest reading in a channel, as the logs show times.
pub fn readings_extent(
    conn: &Connection,
    channel: &str,
    offsets: &Offsets,
) -> Result<Option<(i64, i64)>> {
    let groups = groups(conn, channel)?;
    Ok(extent(conn, &groups)?.map(|(first, last)| (local(offsets, first), local(offsets, last))))
}

/// A time in milliseconds since 1970 UTC, as the logs show it.
fn local(offsets: &Offsets, at: i64) -> i64 {
    at + offsets.minutes_at(at) * MINUTE
}

/// A time as the logs show it, in milliseconds since 1970 UTC. The offset
/// is looked up twice, the second time at the UTC time the first gave, to
/// be right on either side of a daylight saving change.
fn utc(offsets: &Offsets, local: i64) -> i64 {
    let guess = local - offsets.minutes_at(local) * MINUTE;
    local - offsets.minutes_at(guess) * MINUTE
}

/// A period's stretch in milliseconds since 1970 UTC, both ends included.
fn utc_window(offsets: &Offsets, period: &Period) -> (i64, i64) {
    (utc(offsets, period.start), utc(offsets, period.end) - 1)
}

/// Leaves out the faulty readings of `readings r`.
const NOT_FAULTY: &str =
    "NOT EXISTS (SELECT 1 FROM reading_faults f WHERE f.series_id = r.series_id AND f.at = r.at)";

/// The group's series ids, then `start` and `end`, as query values.
fn group_values(group: &Group, start: i64, end: i64) -> Vec<Value> {
    let mut values: Vec<Value> = group.ids.iter().map(|&id| Value::from(id)).collect();
    values.push(Value::from(start));
    values.push(Value::from(end));
    values
}

/// The readings of a group from `start` to `end` that are not faulty, one a
/// moment: a reading posted under two nicks at once counts once.
fn readings(conn: &Connection, group: &Group, start: i64, end: i64) -> Result<Vec<(i64, f64)>> {
    let n = group.ids.len();
    let mut stmt = conn.prepare(&format!(
        "SELECT at, avg(value) FROM readings r
         WHERE series_id IN ({}) AND at >= ?{} AND at <= ?{} AND {NOT_FAULTY}
         GROUP BY at ORDER BY at",
        placeholders(1, n),
        n + 1,
        n + 2
    ))?;
    let rows = stmt
        .query_map(
            params_from_iter(group_values(group, start, end).iter()),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The faulty readings of a group from `start` to `end`, one a moment, with
/// why each is faulty.
fn faulty(
    conn: &Connection,
    group: &Group,
    start: i64,
    end: i64,
) -> Result<Vec<(i64, f64, String)>> {
    let n = group.ids.len();
    let mut stmt = conn.prepare(&format!(
        "SELECT at, avg(value), min(reason) FROM reading_faults
         WHERE series_id IN ({}) AND at >= ?{} AND at <= ?{}
         GROUP BY at ORDER BY at",
        placeholders(1, n),
        n + 1,
        n + 2
    ))?;
    let rows = stmt
        .query_map(
            params_from_iter(group_values(group, start, end).iter()),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The last reading of a group up to `end` that is not faulty, as its time
/// in milliseconds since 1970 UTC and its value.
fn latest(conn: &Connection, group: &Group, end: i64) -> Result<Option<(i64, f64)>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT at, value FROM readings r WHERE series_id = ?1 AND at <= ?2 AND {NOT_FAULTY}
         ORDER BY at DESC LIMIT 1"
    ))?;
    let mut latest: Option<(i64, f64)> = None;
    for id in &group.ids {
        let mut rows = stmt.query(params![id, end])?;
        if let Some(row) = rows.next()? {
            let found: (i64, f64) = (row.get(0)?, row.get(1)?);
            if latest.is_none_or(|(at, _)| found.0 > at) {
                latest = Some(found);
            }
        }
    }
    Ok(latest)
}

/// Readings averaged over one point's time.
#[derive(Clone, Debug, PartialEq)]
pub struct Point {
    /// When it starts, as the logs show times.
    pub at: i64,
    pub readings: i64,
    /// The sum of the readings, for means over several points.
    pub total: f64,
    pub low: f64,
    pub high: f64,
}

impl Point {
    pub fn mean(&self) -> f64 {
        self.total / self.readings as f64
    }

    fn add(&mut self, readings: i64, total: f64, low: f64, high: f64) {
        self.readings += readings;
        self.total += total;
        self.low = self.low.min(low);
        self.high = self.high.max(high);
    }
}

/// A faulty reading, left out of the points.
#[derive(Clone, Debug, PartialEq)]
pub struct Fault {
    /// When, as the logs show times.
    pub at: i64,
    /// When, in milliseconds since 1970 UTC, as its line's timestamp says.
    pub utc: i64,
    pub value: f64,
    /// `range` or `spike`.
    pub reason: String,
}

/// One measure from one sensor, every nick together, over the range.
#[derive(Debug)]
pub struct Line {
    pub sensor: String,
    /// Oldest first, only points with readings that are not faulty.
    pub points: Vec<Point>,
    /// The faulty readings over the range, oldest first.
    pub faults: Vec<Fault>,
    /// The last reading in the period that is not faulty, as the logs show
    /// its time, and its value.
    pub latest: Option<(i64, f64)>,
}

impl Line {
    /// The whole range together, as one point, if it has readings.
    pub fn summary(&self) -> Option<Point> {
        let mut points = self.points.iter();
        let mut summary = points.next()?.clone();
        for point in points {
            summary.add(point.readings, point.total, point.low, point.high);
        }
        Some(summary)
    }
}

/// One measure's lines, one a sensor.
#[derive(Debug)]
pub struct Measure {
    pub measure: String,
    pub lines: Vec<Line>,
}

/// A channel's readings over a period.
#[derive(Debug)]
pub struct Graph {
    /// When the period starts, and when the next one starts, as the logs
    /// show times.
    pub start: i64,
    pub end: i64,
    /// How long each point is, in milliseconds: 1 for every reading on its
    /// own.
    pub point_size: i64,
    /// The longest time between two points that a line joins, in
    /// milliseconds; further apart, the line breaks.
    pub gap: i64,
    /// Every sensor in the channel, sorted, so each keeps its color across
    /// measures and ranges.
    pub sensors: Vec<String>,
    pub measures: Vec<Measure>,
}

impl Graph {
    /// A channel's graph over a period, with points of `point_size`, or of
    /// the size the period calls for. `None` when the channel has no
    /// readings at all.
    pub fn build(
        conn: &Connection,
        channel: &str,
        period: &Period,
        offsets: &Offsets,
        point_size: Option<i64>,
    ) -> Result<Option<Graph>> {
        let size = point_size.unwrap_or_else(|| self::point_size(period.end - period.start));
        Graph::between(
            conn,
            channel,
            (period.start, period.end),
            offsets,
            size,
            size,
        )
    }

    /// The hour on each side of `at`, as the logs show times, with every
    /// reading as its own point, joined unless two minutes apart.
    pub fn close_up(
        conn: &Connection,
        channel: &str,
        at: i64,
        offsets: &Offsets,
    ) -> Result<Option<Graph>> {
        let window = (at - CLOSE_UP_SIDE, at + CLOSE_UP_SIDE);
        Graph::between(conn, channel, window, offsets, 1, 2 * MINUTE)
    }

    /// A channel's graph from `start` up to `end`, as the logs show times.
    fn between(
        conn: &Connection,
        channel: &str,
        (local_start, local_end): (i64, i64),
        offsets: &Offsets,
        size: i64,
        gap: i64,
    ) -> Result<Option<Graph>> {
        let groups = groups(conn, channel)?;
        if groups.is_empty() {
            return Ok(None);
        }
        let (start, end) = (utc(offsets, local_start), utc(offsets, local_end) - 1);
        let bucket = |at: i64| local(offsets, at).div_euclid(size) * size;

        let mut sensors: Vec<String> = groups.iter().map(|group| group.sensor.clone()).collect();
        sensors.sort();
        sensors.dedup();

        let mut hours = conn.prepare(
            "SELECT hour, readings, total, low, high FROM reading_hours
             WHERE series_id = ?1 AND hour >= ?2 AND hour <= ?3",
        )?;
        let mut measures: Vec<Measure> = Vec::new();
        for group in &groups {
            let mut points: BTreeMap<i64, Point> = BTreeMap::new();
            let mut add = |at: i64, readings: i64, total: f64, low: f64, high: f64| {
                points
                    .entry(bucket(at))
                    .or_insert_with(|| Point {
                        at: bucket(at),
                        readings: 0,
                        total: 0.0,
                        low,
                        high,
                    })
                    .add(readings, total, low, high);
            };
            if size < HOUR {
                for (at, value) in readings(conn, group, start, end)? {
                    add(at, 1, value, value, value);
                }
            } else {
                for id in &group.ids {
                    let mut rows =
                        hours.query(params![id, start.div_euclid(HOUR), end.div_euclid(HOUR)])?;
                    while let Some(row) = rows.next()? {
                        let hour: i64 = row.get(0)?;
                        add(
                            hour * HOUR,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        );
                    }
                }
            }
            let faults = faulty(conn, group, start, end)?
                .into_iter()
                .map(|(at, value, reason)| Fault {
                    at: local(offsets, at),
                    utc: at,
                    value,
                    reason,
                })
                .collect();
            let line = Line {
                sensor: group.sensor.clone(),
                points: points.into_values().collect(),
                faults,
                latest: latest(conn, group, end)?.map(|(at, value)| (local(offsets, at), value)),
            };
            let same = measures
                .last()
                .is_some_and(|measure| measure.measure == group.measure);
            match measures.last_mut() {
                Some(measure) if same => measure.lines.push(line),
                _ => {
                    let measure = Measure {
                        measure: group.measure.clone(),
                        lines: vec![line],
                    };
                    measures.push(measure);
                }
            }
        }
        Ok(Some(Graph {
            start: local_start,
            end: local_end,
            point_size: size,
            gap,
            sensors,
            measures,
        }))
    }
}

/// A time as the logs show it, such as `2026-11-03 13:46`, with seconds when
/// asked.
pub fn time_text(local: i64, seconds: bool) -> String {
    let format = if seconds {
        "%Y-%m-%d %H:%M:%S"
    } else {
        "%Y-%m-%d %H:%M"
    };
    DateTime::from_timestamp_millis(local)
        .map(|at| at.naive_utc().format(format).to_string())
        .unwrap_or_default()
}

/// A number with at most three decimals and no trailing zeros, such as
/// `68.45` or `46`.
pub fn decimal(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "0".to_string()
    } else {
        text.to_string()
    }
}

/// A CSV field, quoted when it holds a comma, a quote or a line break.
fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// A graph's points as CSV, oldest first: each point's start as the logs
/// show times, its sensor and measure, how many readings it averages, and
/// their mean, lowest and highest value.
pub fn points_csv(graph: &Graph) -> String {
    let mut rows: Vec<(i64, usize, &str, &str, &Point)> = Vec::new();
    for (order, measure) in graph.measures.iter().enumerate() {
        for line in &measure.lines {
            for point in &line.points {
                rows.push((
                    point.at,
                    order,
                    line.sensor.as_str(),
                    measure.measure.as_str(),
                    point,
                ));
            }
        }
    }
    rows.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
    let mut csv = String::from("time,sensor,measure,readings,mean,low,high\n");
    for (at, _, sensor, measure, point) in rows {
        let _ = writeln!(
            csv,
            "{},{},{},{},{},{},{}",
            time_text(at, false),
            csv_field(sensor),
            csv_field(measure),
            point.readings,
            decimal(point.mean()),
            decimal(point.low),
            decimal(point.high),
        );
    }
    csv
}

/// Every reading of a channel over a period as CSV, oldest first: its time as
/// the logs show it and in UTC, its sensor, measure and value, and why it is
/// faulty, if it is. Faulty readings are left out unless `faults` is set.
/// `None` when the period is too long to hold in memory, or the channel has
/// no readings.
pub fn readings_csv(
    conn: &Connection,
    channel: &str,
    period: &Period,
    offsets: &Offsets,
    faults: bool,
) -> Result<Option<String>> {
    if !period.range.readings_downloadable() {
        return Ok(None);
    }
    let groups = groups(conn, channel)?;
    if groups.is_empty() {
        return Ok(None);
    }
    let (start, end) = utc_window(offsets, period);
    let mut rows: Vec<(i64, usize, &Group, f64, String)> = Vec::new();
    for (order, group) in groups.iter().enumerate() {
        for (at, value) in readings(conn, group, start, end)? {
            rows.push((at, order, group, value, String::new()));
        }
        if faults {
            for (at, value, reason) in faulty(conn, group, start, end)? {
                rows.push((at, order, group, value, reason));
            }
        }
    }
    rows.sort_by_key(|row| (row.0, row.1));
    let mut csv = String::from("time,utc,sensor,measure,value,fault\n");
    for (at, _, group, value, reason) in rows {
        let utc = DateTime::from_timestamp_millis(at)
            .map(|at| at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
            .unwrap_or_default();
        let _ = writeln!(
            csv,
            "{},{utc},{},{},{value},{reason}",
            time_text(local(offsets, at), true),
            csv_field(&group.sensor),
            csv_field(&group.measure),
        );
    }
    Ok(Some(csv))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, readings};

    /// One device under two nicks, posting one inside reading under both.
    const LOG: &str = r#"--- Log opened Tue Nov 03 13:45:10 2026
13:46 < greenhouse> {"sensor":"outside","temp":"65.30","humid":"46.00","timestamp":"2026-11-03T20:46:42.998Z"}
13:46 < greenhouse> {"sensor":"inside","temp":"68.54","humid":"35.50","timestamp":"2026-11-03T20:46:43.051Z"}
13:47 < greenhouse> {"sensor":"inside","temp":"68.36","humid":"36.50","timestamp":"2026-11-03T20:47:13.131Z"}
13:47 < greenhouse1> {"sensor":"inside","temp":"68.36","humid":"36.50","timestamp":"2026-11-03T20:47:13.131Z"}
15:10 < greenhouse1> {"sensor":"inside","temp":"70.00","humid":"30.00","timestamp":"2026-11-03T22:10:00.000Z"}
"#;

    fn setup(name: &str) -> (TempDir, Connection, Offsets) {
        let root = TempDir::new(name);
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(root.0.join("wetfish/#greenhouse.log"), LOG).unwrap();
        let mut conn = db::open(&root.0.join("sensors.sqlite")).unwrap();
        let inventory = discover::discover(std::slice::from_ref(&root.0)).unwrap();
        db::import(&mut conn, &inventory.files);
        readings::build(&mut conn, false).unwrap();
        let offsets = Offsets::load(&conn).unwrap();
        (root, conn, offsets)
    }

    /// "time readings mean low high" for each point of a line.
    fn points(line: &Line) -> Vec<String> {
        line.points
            .iter()
            .map(|point| {
                format!(
                    "{} {} {} {} {}",
                    time_text(point.at, false),
                    point.readings,
                    decimal(point.mean()),
                    decimal(point.low),
                    decimal(point.high)
                )
            })
            .collect()
    }

    fn day(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).unwrap()
    }

    /// The period of a range holding 2026-11-03.
    fn period(range: Range) -> Period {
        Period::containing(range, day(11, 3), 0, 0)
    }

    #[test]
    fn finds_calendar_periods_and_their_neighbors() {
        let span = |period: Period| {
            format!(
                "{} to {}",
                time_text(period.start, false),
                time_text(period.end, false)
            )
        };
        // Thursday 2026-11-05.
        let (first, last) = (midnight(day(3, 9)), midnight(day(11, 5)) + 13 * HOUR);
        let at = |range| Period::containing(range, day(11, 5), first, last);
        assert_eq!(span(at(Range::Day)), "2026-11-05 00:00 to 2026-11-06 00:00");
        assert_eq!(
            span(at(Range::Week)),
            "2026-11-02 00:00 to 2026-11-09 00:00"
        );
        assert_eq!(
            span(at(Range::Month)),
            "2026-11-01 00:00 to 2026-12-01 00:00"
        );
        assert_eq!(
            span(at(Range::Year)),
            "2026-01-01 00:00 to 2027-01-01 00:00"
        );
        assert_eq!(span(at(Range::All)), "2026-03-09 00:00 to 2026-11-05 13:00");
        assert_eq!(at(Range::Week).first_day(), day(11, 2));

        // Earlier and later, as far as the readings go.
        assert_eq!(at(Range::Week).earlier(first), Some(day(11, 1)));
        assert_eq!(at(Range::Week).later(last), None);
        assert_eq!(at(Range::Month).earlier(first), Some(day(10, 31)));
        let march = Period::containing(Range::Month, day(3, 20), first, last);
        assert_eq!(march.earlier(first), None);
        assert_eq!(march.later(last), Some(day(4, 1)));
        assert_eq!(at(Range::Year).earlier(first), None);
        assert_eq!(at(Range::All).earlier(first), None);
        assert_eq!(at(Range::All).later(last), None);
    }

    #[test]
    fn picks_point_sizes_for_each_range() {
        assert_eq!(point_size(DAY), 2 * MINUTE);
        assert_eq!(point_size(7 * DAY), 15 * MINUTE);
        assert_eq!(point_size(30 * DAY), HOUR);
        assert_eq!(point_size(365 * DAY), 12 * HOUR);
        assert_eq!(point_size(600 * DAY), DAY);
        assert_eq!(point_size(20 * 365 * DAY), 7 * DAY);
        assert_eq!(Range::parse("week"), Some(Range::Week));
        assert_eq!(Range::parse("decade"), None);
        assert!(Range::Month.readings_downloadable());
        assert!(!Range::Year.readings_downloadable());
    }

    #[test]
    fn merges_nicks_into_one_line_per_sensor() {
        let (_root, conn, offsets) = setup("graphs-day");
        assert_eq!(channels(&conn).unwrap(), ["#greenhouse"]);
        let graph = Graph::build(&conn, "#GREENHOUSE", &period(Range::Day), &offsets, None)
            .unwrap()
            .unwrap();
        assert_eq!(graph.point_size, 2 * MINUTE);
        assert_eq!(time_text(graph.start, true), "2026-11-03 00:00:00");
        assert_eq!(time_text(graph.end, true), "2026-11-04 00:00:00");
        let (first, last) = readings_extent(&conn, "#greenhouse", &offsets)
            .unwrap()
            .unwrap();
        assert_eq!(time_text(first, true), "2026-11-03 13:46:42");
        assert_eq!(time_text(last, true), "2026-11-03 15:10:00");
        assert_eq!(graph.sensors, ["inside", "outside"]);

        let measures: Vec<&str> = graph.measures.iter().map(|m| m.measure.as_str()).collect();
        assert_eq!(measures, ["temp", "humid"]);
        let temp = &graph.measures[0];
        assert_eq!(temp.lines[0].sensor, "inside");
        // The reading posted under both nicks counts once.
        assert_eq!(
            points(&temp.lines[0]),
            [
                "2026-11-03 13:46 2 68.45 68.36 68.54",
                "2026-11-03 15:10 1 70 70 70"
            ]
        );
        assert_eq!(
            points(&temp.lines[1]),
            ["2026-11-03 13:46 1 65.3 65.3 65.3"]
        );
        let latest = temp.lines[0].latest.unwrap();
        assert_eq!(
            (time_text(latest.0, false).as_str(), latest.1),
            ("2026-11-03 15:10", 70.0)
        );

        let summary = temp.lines[0].summary().unwrap();
        assert_eq!(
            (
                summary.readings,
                decimal(summary.mean()),
                summary.low,
                summary.high
            ),
            (3, "68.967".to_string(), 68.36, 70.0)
        );
    }

    #[test]
    fn adds_up_hours_for_long_ranges() {
        let (_root, conn, offsets) = setup("graphs-year");
        let graph = Graph::build(&conn, "#greenhouse", &period(Range::Year), &offsets, None)
            .unwrap()
            .unwrap();
        assert_eq!(graph.point_size, 12 * HOUR);
        // Hourly rows count the reading posted under both nicks twice.
        assert_eq!(
            points(&graph.measures[0].lines[0]),
            ["2026-11-03 12:00 4 68.815 68.36 70"]
        );
        let hourly = Graph::build(
            &conn,
            "#greenhouse",
            &period(Range::Year),
            &offsets,
            Some(HOUR),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            points(&hourly.measures[0].lines[0]),
            [
                "2026-11-03 13:00 3 68.42 68.36 68.54",
                "2026-11-03 15:00 1 70 70 70"
            ]
        );
        assert!(
            Graph::build(&conn, "#nowhere", &period(Range::Day), &offsets, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn writes_points_and_readings_as_csv() {
        let (_root, conn, offsets) = setup("graphs-csv");
        let graph = Graph::build(&conn, "#greenhouse", &period(Range::Day), &offsets, None)
            .unwrap()
            .unwrap();
        assert_eq!(
            points_csv(&graph),
            "time,sensor,measure,readings,mean,low,high\n\
             2026-11-03 13:46,inside,temp,2,68.45,68.36,68.54\n\
             2026-11-03 13:46,outside,temp,1,65.3,65.3,65.3\n\
             2026-11-03 13:46,inside,humid,2,36,35.5,36.5\n\
             2026-11-03 13:46,outside,humid,1,46,46,46\n\
             2026-11-03 15:10,inside,temp,1,70,70,70\n\
             2026-11-03 15:10,inside,humid,1,30,30,30\n"
        );
        assert_eq!(
            readings_csv(&conn, "#greenhouse", &period(Range::Day), &offsets, true)
                .unwrap()
                .unwrap(),
            "time,utc,sensor,measure,value,fault\n\
             2026-11-03 13:46:42,2026-11-03T20:46:42.998Z,outside,temp,65.3,\n\
             2026-11-03 13:46:42,2026-11-03T20:46:42.998Z,outside,humid,46,\n\
             2026-11-03 13:46:43,2026-11-03T20:46:43.051Z,inside,temp,68.54,\n\
             2026-11-03 13:46:43,2026-11-03T20:46:43.051Z,inside,humid,35.5,\n\
             2026-11-03 13:47:13,2026-11-03T20:47:13.131Z,inside,temp,68.36,\n\
             2026-11-03 13:47:13,2026-11-03T20:47:13.131Z,inside,humid,36.5,\n\
             2026-11-03 15:10:00,2026-11-03T22:10:00.000Z,inside,temp,70,\n\
             2026-11-03 15:10:00,2026-11-03T22:10:00.000Z,inside,humid,30,\n"
        );
        assert_eq!(
            readings_csv(&conn, "#greenhouse", &period(Range::Year), &offsets, true).unwrap(),
            None
        );
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(decimal(-0.0001), "0");
    }

    #[test]
    fn leaves_faults_out_of_points_and_keeps_them_beside_lines() {
        let (root, mut conn, _) = setup("graphs-faults");
        let log = root.0.join("wetfish/#greenhouse.log");
        let mut text = fs::read_to_string(&log).unwrap();
        text.push_str(
            "15:11 < greenhouse1> {\"sensor\":\"inside\",\"temp\":\"302.00\",\
             \"timestamp\":\"2026-11-03T22:11:00.000Z\"}\n",
        );
        fs::write(&log, text).unwrap();
        let inventory = discover::discover(std::slice::from_ref(&root.0)).unwrap();
        db::import(&mut conn, &inventory.files);
        readings::build(&mut conn, false).unwrap();
        let offsets = Offsets::load(&conn).unwrap();

        let graph = Graph::build(&conn, "#greenhouse", &period(Range::Day), &offsets, None)
            .unwrap()
            .unwrap();
        let inside = &graph.measures[0].lines[0];
        assert_eq!(points(inside).len(), 2, "302 is no point");
        assert_eq!(inside.latest.unwrap().1, 70.0, "nor the latest reading");
        assert_eq!(
            inside.faults,
            [Fault {
                at: inside.faults[0].at,
                utc: inside.faults[0].utc,
                value: 302.0,
                reason: "range".into()
            }]
        );
        assert_eq!(time_text(inside.faults[0].at, false), "2026-11-03 15:11");

        let with = readings_csv(&conn, "#greenhouse", &period(Range::Day), &offsets, true)
            .unwrap()
            .unwrap();
        assert!(
            with.ends_with("2026-11-03 15:11:00,2026-11-03T22:11:00.000Z,inside,temp,302,range\n")
        );
        let without = readings_csv(&conn, "#greenhouse", &period(Range::Day), &offsets, false)
            .unwrap()
            .unwrap();
        assert!(!without.contains("302"));
    }

    #[test]
    fn shows_every_reading_in_a_close_up() {
        let (_root, conn, offsets) = setup("graphs-close-up");
        let at = midnight(day(11, 3)) + 13 * HOUR + 47 * MINUTE;
        let graph = Graph::close_up(&conn, "#greenhouse", at, &offsets)
            .unwrap()
            .unwrap();
        assert_eq!((graph.point_size, graph.gap), (1, 2 * MINUTE));
        assert_eq!(time_text(graph.start, false), "2026-11-03 12:47");
        assert_eq!(time_text(graph.end, false), "2026-11-03 14:47");
        // Each reading its own point; 15:10 is outside the two hours.
        let inside: Vec<String> = graph.measures[0].lines[0]
            .points
            .iter()
            .map(|point| format!("{} {}", time_text(point.at, true), decimal(point.mean())))
            .collect();
        assert_eq!(
            inside,
            ["2026-11-03 13:46:43 68.54", "2026-11-03 13:47:13 68.36"]
        );
    }
}

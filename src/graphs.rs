//! Graphs: one channel's readings over a range of time, averaged into
//! points, and the same as CSV.
//!
//! Every nick that posted to a channel counts as the same device, since a
//! device that reconnects often comes back under a fallback nick such as
//! `greenhouse1`. So a line is one measure from one sensor, every nick
//! together: the inside temperature, say.
//!
//! A range is cut into points of one size, the smallest that keeps a line
//! under `MAX_POINTS`. Points under an hour average the readings themselves,
//! leaving out a second copy of a reading posted under two nicks at the same
//! moment. Points of an hour or more add up the hourly rows in
//! `reading_hours`, so a year reads a row an hour, but count such a copy
//! twice.
//!
//! Times on graphs are as the logs show them, in milliseconds since 1970 as
//! if the logs' time zone were UTC, so points and ticks fall on the logs'
//! hours and midnights.

use std::collections::BTreeMap;
use std::fmt::Write;

use chrono::DateTime;
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

/// The longest range every reading can be downloaded for, as a CSV file
/// held in memory: about 350,000 rows for a month of the greenhouse.
pub const MAX_READINGS_SPAN: i64 = 31 * DAY;

/// How much time a graph shows, ending at the newest reading.
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

    /// How long it is, or `None` for everything.
    pub fn span(self) -> Option<i64> {
        match self {
            Range::Day => Some(DAY),
            Range::Week => Some(7 * DAY),
            Range::Month => Some(30 * DAY),
            Range::Year => Some(365 * DAY),
            Range::All => None,
        }
    }

    /// Whether every reading in it can be downloaded.
    pub fn readings_downloadable(self) -> bool {
        self.span().is_some_and(|span| span <= MAX_READINGS_SPAN)
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
fn measure_rank(measure: &str) -> (usize, &str) {
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

/// The UTC stretch a range covers: the range up to the newest reading, or
/// everything.
fn window(conn: &Connection, groups: &[Group], range: Range) -> Result<Option<(i64, i64)>> {
    let Some((first, last)) = extent(conn, groups)? else {
        return Ok(None);
    };
    let start = match range.span() {
        Some(span) => last - span,
        None => first,
    };
    Ok(Some((start, last)))
}

/// A time in milliseconds since 1970 UTC, as the logs show it.
fn local(offsets: &Offsets, at: i64) -> i64 {
    at + offsets.minutes_at(at) * MINUTE
}

/// The readings of a group from `start` to `end`, one a moment: a reading
/// posted under two nicks at once counts once.
fn readings(conn: &Connection, group: &Group, start: i64, end: i64) -> Result<Vec<(i64, f64)>> {
    let mut values: Vec<Value> = group.ids.iter().map(|&id| Value::from(id)).collect();
    values.push(Value::from(start));
    values.push(Value::from(end));
    let n = group.ids.len();
    let mut stmt = conn.prepare(&format!(
        "SELECT at, avg(value) FROM readings
         WHERE series_id IN ({}) AND at >= ?{} AND at <= ?{}
         GROUP BY at ORDER BY at",
        placeholders(1, n),
        n + 1,
        n + 2
    ))?;
    let rows = stmt
        .query_map(params_from_iter(values.iter()), |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The most recent reading of a group, as its time in milliseconds since
/// 1970 UTC and its value.
fn latest(conn: &Connection, group: &Group) -> Result<Option<(i64, f64)>> {
    let mut stmt = conn.prepare_cached(
        "SELECT at, value FROM readings WHERE series_id = ?1 ORDER BY at DESC LIMIT 1",
    )?;
    let mut latest: Option<(i64, f64)> = None;
    for id in &group.ids {
        let mut rows = stmt.query(params![id])?;
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

/// One measure from one sensor, every nick together, over the range.
#[derive(Debug)]
pub struct Line {
    pub sensor: String,
    /// Oldest first, only points with readings.
    pub points: Vec<Point>,
    /// The most recent reading ever, as the logs show its time, and its value.
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

/// A channel's readings over a range.
#[derive(Debug)]
pub struct Graph {
    /// When the range starts and ends, as the logs show times.
    pub start: i64,
    pub end: i64,
    /// How long each point is, in milliseconds.
    pub point_size: i64,
    /// Every sensor in the channel, sorted, so each keeps its color across
    /// measures and ranges.
    pub sensors: Vec<String>,
    pub measures: Vec<Measure>,
}

impl Graph {
    /// A channel's graph over a range, with points of `point_size`, or of
    /// the size the range calls for. `None` when it has no readings.
    pub fn build(
        conn: &Connection,
        channel: &str,
        range: Range,
        offsets: &Offsets,
        point_size: Option<i64>,
    ) -> Result<Option<Graph>> {
        let groups = groups(conn, channel)?;
        let Some((start, end)) = window(conn, &groups, range)? else {
            return Ok(None);
        };
        let size = point_size.unwrap_or_else(|| self::point_size(end - start));
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
            let line = Line {
                sensor: group.sensor.clone(),
                points: points.into_values().collect(),
                latest: latest(conn, group)?.map(|(at, value)| (local(offsets, at), value)),
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
            start: local(offsets, start),
            end: local(offsets, end),
            point_size: size,
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

/// Every reading of a channel over a range as CSV, oldest first: its time as
/// the logs show it and in UTC, its sensor, measure and value. `None` when
/// the range is too long to hold in memory, or has no readings.
pub fn readings_csv(
    conn: &Connection,
    channel: &str,
    range: Range,
    offsets: &Offsets,
) -> Result<Option<String>> {
    if !range.readings_downloadable() {
        return Ok(None);
    }
    let groups = groups(conn, channel)?;
    let Some((start, end)) = window(conn, &groups, range)? else {
        return Ok(None);
    };
    let mut rows: Vec<(i64, usize, &Group, f64)> = Vec::new();
    for (order, group) in groups.iter().enumerate() {
        for (at, value) in readings(conn, group, start, end)? {
            rows.push((at, order, group, value));
        }
    }
    rows.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let mut csv = String::from("time,utc,sensor,measure,value\n");
    for (at, _, group, value) in rows {
        let utc = DateTime::from_timestamp_millis(at)
            .map(|at| at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
            .unwrap_or_default();
        let _ = writeln!(
            csv,
            "{},{utc},{},{},{value}",
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
        let graph = Graph::build(&conn, "#GREENHOUSE", Range::Day, &offsets, None)
            .unwrap()
            .unwrap();
        assert_eq!(graph.point_size, 2 * MINUTE);
        assert_eq!(time_text(graph.end, true), "2026-11-03 15:10:00");
        assert_eq!(time_text(graph.start, true), "2026-11-02 15:10:00");
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
        let graph = Graph::build(&conn, "#greenhouse", Range::Year, &offsets, None)
            .unwrap()
            .unwrap();
        assert_eq!(graph.point_size, 12 * HOUR);
        // Hourly rows count the reading posted under both nicks twice.
        assert_eq!(
            points(&graph.measures[0].lines[0]),
            ["2026-11-03 12:00 4 68.815 68.36 70"]
        );
        let hourly = Graph::build(&conn, "#greenhouse", Range::Year, &offsets, Some(HOUR))
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
            Graph::build(&conn, "#nowhere", Range::Day, &offsets, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn writes_points_and_readings_as_csv() {
        let (_root, conn, offsets) = setup("graphs-csv");
        let graph = Graph::build(&conn, "#greenhouse", Range::Day, &offsets, None)
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
            readings_csv(&conn, "#greenhouse", Range::Day, &offsets)
                .unwrap()
                .unwrap(),
            "time,utc,sensor,measure,value\n\
             2026-11-03 13:46:42,2026-11-03T20:46:42.998Z,outside,temp,65.3\n\
             2026-11-03 13:46:42,2026-11-03T20:46:42.998Z,outside,humid,46\n\
             2026-11-03 13:46:43,2026-11-03T20:46:43.051Z,inside,temp,68.54\n\
             2026-11-03 13:46:43,2026-11-03T20:46:43.051Z,inside,humid,35.5\n\
             2026-11-03 13:47:13,2026-11-03T20:47:13.131Z,inside,temp,68.36\n\
             2026-11-03 13:47:13,2026-11-03T20:47:13.131Z,inside,humid,36.5\n\
             2026-11-03 15:10:00,2026-11-03T22:10:00.000Z,inside,temp,70\n\
             2026-11-03 15:10:00,2026-11-03T22:10:00.000Z,inside,humid,30\n"
        );
        assert_eq!(
            readings_csv(&conn, "#greenhouse", Range::Year, &offsets).unwrap(),
            None
        );
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(decimal(-0.0001), "0");
    }
}

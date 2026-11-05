//! Human-readable text reports.

use std::io::{self, Write};
use std::path::Path;

use crate::db::ImportSummary;
use crate::readings::{BuildSummary, Offsets, Series};

/// Summarizes what an import added to the database.
pub fn import_summary(
    out: &mut impl Write,
    db_path: &Path,
    summary: &ImportSummary,
) -> io::Result<()> {
    writeln!(out, "Database: {}", db_path.display())?;
    writeln!(
        out,
        "Imported {}: {}, {} with new lines, {} replaced, {} unchanged",
        count_u64(summary.lines, "new line"),
        count(summary.new_files, "new file"),
        thousands(summary.grown_files as u64),
        thousands(summary.replaced_files as u64),
        thousands(summary.unchanged_files as u64),
    )?;
    for (path, err) in &summary.errors {
        writeln!(out, "Could not import {}: {err}", path.display())?;
    }
    Ok(())
}

/// Says what a build of the readings left out, if anything.
pub fn build_summary(out: &mut impl Write, summary: &BuildSummary) -> io::Result<()> {
    if summary.repeated > 0 {
        writeln!(
            out,
            "Left out {} at the same time as another of the same measure",
            count_u64(summary.repeated, "reading")
        )?;
    }
    if summary.untimed > 0 {
        writeln!(
            out,
            "Left out {} without a timestamp",
            count_u64(summary.untimed, "JSON line")
        )?;
    }
    if summary.faults > 0 {
        writeln!(
            out,
            "Flagged {} in all, left out of graphs",
            count_u64(summary.faults, "faulty reading")
        )?;
    }
    Ok(())
}

/// Lists each measure from each sensor, with its readings and when they
/// start and end, in the logs' time zone.
pub fn series(out: &mut impl Write, series: &[Series], offsets: &Offsets) -> io::Result<()> {
    if series.is_empty() {
        return writeln!(out, "No sensor readings yet.");
    }
    let header = [
        "Channel",
        "Nick",
        "Sensor",
        "Measure",
        "Readings",
        "From and to",
    ];
    let rows: Vec<[String; 6]> = series
        .iter()
        .map(|series| {
            [
                series.channel.clone(),
                series.nick.clone(),
                if series.sensor.is_empty() {
                    "-".to_string()
                } else {
                    series.sensor.clone()
                },
                series.measure.clone(),
                thousands(u64::try_from(series.readings).unwrap_or(0)),
                span(series.first, series.last, offsets),
            ]
        })
        .collect();
    let mut widths = header.map(|title| title.chars().count());
    for row in &rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: [&str; 6]| {
        format!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:>w4$}  {}",
            cells[0],
            cells[1],
            cells[2],
            cells[3],
            cells[4],
            cells[5],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
            w4 = widths[4],
        )
    };
    writeln!(out, "{}", line(header))?;
    for row in &rows {
        writeln!(out, "{}", line(row.each_ref().map(String::as_str)))?;
    }
    Ok(())
}

/// When readings start and end, such as `2026-11-03 13:46 to 2026-11-04
/// 18:06`, in the logs' time zone.
fn span(first: Option<i64>, last: Option<i64>, offsets: &Offsets) -> String {
    let time = |at: Option<i64>| {
        at.and_then(|at| offsets.local(at)).map_or_else(
            || "-".to_string(),
            |at| at.format("%Y-%m-%d %H:%M").to_string(),
        )
    };
    format!("{} to {}", time(first), time(last))
}

fn count(n: usize, noun: &str) -> String {
    count_u64(n as u64, noun)
}

fn count_u64(n: u64, noun: &str) -> String {
    let suffix = if n == 1 { "" } else { "s" };
    format!("{} {noun}{suffix}", thousands(n))
}

/// A count with thousands separators, such as `12,345`.
pub fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn separates_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(5_729), "5,729");
        assert_eq!(thousands(12_345_678), "12,345,678");
    }

    #[test]
    fn lists_series_in_the_logs_time_zone() {
        // 2026-11-03 20:46:43 and 2026-11-04 01:06:37 UTC.
        let (first, last) = (1_793_738_803_051, 1_793_754_397_796);
        let series = [
            Series {
                channel: "#greenhouse".into(),
                nick: "greenhouse".into(),
                sensor: "inside".into(),
                measure: "humid".into(),
                readings: 2_864,
                first: Some(first),
                last: Some(last),
                latest: Some(35.5),
            },
            Series {
                channel: "#greenhouse".into(),
                nick: "greenhouse".into(),
                sensor: String::new(),
                measure: "pressure".into(),
                readings: 0,
                first: None,
                last: None,
                latest: None,
            },
        ];
        let offsets = Offsets::fixed(-420);
        let mut out = Vec::new();
        super::series(&mut out, &series, &offsets).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Channel      Nick        Sensor  Measure   Readings  From and to\n\
             #greenhouse  greenhouse  inside  humid        2,864  2026-11-03 13:46 to 2026-11-03 18:06\n\
             #greenhouse  greenhouse  -       pressure         0  - to -\n"
        );
    }

    #[test]
    fn says_what_a_build_left_out() {
        let mut out = Vec::new();
        build_summary(&mut out, &BuildSummary::default()).unwrap();
        assert!(out.is_empty());
        let summary = BuildSummary {
            repeated: 2,
            untimed: 1,
            faults: 1,
            ..BuildSummary::default()
        };
        build_summary(&mut out, &summary).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Left out 2 readings at the same time as another of the same measure\n\
             Left out 1 JSON line without a timestamp\n\
             Flagged 1 faulty reading in all, left out of graphs\n"
        );
    }

    #[test]
    fn summarizes_an_import() {
        let summary = ImportSummary {
            new_files: 1,
            grown_files: 2,
            replaced_files: 0,
            unchanged_files: 3,
            lines: 5_729,
            errors: vec![(PathBuf::from("/logs/x/#y.log"), "denied".into())],
        };
        let mut out = Vec::new();
        import_summary(&mut out, Path::new("sensors.sqlite"), &summary).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Database: sensors.sqlite\n\
             Imported 5,729 new lines: 1 new file, 2 with new lines, 0 replaced, 3 unchanged\n\
             Could not import /logs/x/#y.log: denied\n"
        );
    }
}

//! Human-readable text reports.

use std::io::{self, Write};
use std::path::Path;

use crate::db::ImportSummary;

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

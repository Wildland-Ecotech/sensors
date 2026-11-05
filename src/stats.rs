//! Totals for the review's overview.

use rusqlite::Connection;

use crate::db::Result;

/// What has been imported.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Overview {
    pub files: i64,
    pub lines: i64,
    /// When the most recent import read a file, as `YYYY-MM-DD HH:MM:SS` UTC.
    pub last_import: Option<String>,
}

/// Counts the imported files and lines.
pub fn overview(conn: &Connection) -> Result<Overview> {
    let overview = conn.query_row(
        "SELECT count(*), ifnull(sum(lines_read), 0), max(imported_at) FROM files",
        [],
        |row| {
            Ok(Overview {
                files: row.get(0)?,
                lines: row.get(1)?,
                last_import: row.get(2)?,
            })
        },
    )?;
    Ok(overview)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::db;
    use crate::discover::{self, tests::TempDir};

    #[test]
    fn counts_files_and_lines() {
        let root = TempDir::new("stats-overview");
        let mut conn = db::open(&root.0.join("sensors.sqlite")).unwrap();
        assert_eq!(overview(&conn).unwrap(), Overview::default());

        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(
            root.0.join("wetfish/#greenhouse.log"),
            "--- Log opened Tue Nov 03 13:45:10 2026\n13:46 < greenhouse> {}\n",
        )
        .unwrap();
        let inventory = discover::discover(std::slice::from_ref(&root.0)).unwrap();
        db::import(&mut conn, &inventory.files);
        let overview = overview(&conn).unwrap();
        assert_eq!((overview.files, overview.lines), (1, 2));
        assert!(overview.last_import.is_some());
    }
}

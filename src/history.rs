//! History: the lines said in channels, one day at a time, and the days with
//! lines. Days are counted in `channel_days` with the readings, so listing
//! years of days reads a row a day, and a page of lines only reads its day.
//! Private messages are left out.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, params};

use crate::db::Result;

/// Lines shown on one page of a day.
pub const PAGE_SIZE: usize = 200;

/// One day's lines in channels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Day {
    /// Channels with lines that day, sorted.
    pub channels: BTreeSet<String>,
    /// Messages and actions said that day.
    pub lines: i64,
}

/// Every channel with lines, sorted.
pub fn channels(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT channel FROM channel_days ORDER BY channel")?;
    let rows = stmt
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Every day with lines, in one channel or all, by `YYYY-MM-DD`.
pub fn days(conn: &Connection, channel: Option<&str>) -> Result<BTreeMap<String, Day>> {
    let mut stmt = conn.prepare(
        "SELECT date, channel, lines FROM channel_days
         WHERE ?1 IS NULL OR channel = ?1 COLLATE NOCASE",
    )?;
    let mut rows = stmt.query(params![channel])?;
    let mut days: BTreeMap<String, Day> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let (date, channel, lines): (String, String, i64) = (row.get(0)?, row.get(1)?, row.get(2)?);
        let day = days.entry(date).or_default();
        day.channels.insert(channel);
        day.lines += lines;
    }
    Ok(days)
}

/// The most recent day with lines, in one channel or all.
pub fn latest_day(conn: &Connection, channel: Option<&str>) -> Result<Option<String>> {
    let day = conn.query_row(
        "SELECT max(date) FROM channel_days WHERE ?1 IS NULL OR channel = ?1 COLLATE NOCASE",
        params![channel],
        |row| row.get(0),
    )?;
    Ok(day)
}

/// Which lines of a day to show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub channel: Option<String>,
    /// Text the line contains, ignoring case for ASCII letters.
    pub text: Option<String>,
    /// From 0, `PAGE_SIZE` lines each, most recent first.
    pub page: usize,
}

/// One line said in a channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// `HH:MM`.
    pub time: String,
    pub channel: String,
    pub nick: String,
    /// An action, shown as `* nick text`.
    pub action: bool,
    pub text: String,
}

/// A page of a day's lines, most recent first, and whether older lines
/// follow. Lines of the same minute are in file and line order, the latest
/// first.
///
/// Files are read first, then each file's lines of the day through the file
/// and date index. Without that order SQLite reads every line to find the
/// day's.
pub fn lines(conn: &Connection, day: &str, filter: &Filter) -> Result<(Vec<Line>, bool)> {
    let pattern = filter
        .text
        .as_deref()
        .map(|text| format!("%{}%", escape_like(text)));
    let offset = filter.page.saturating_mul(PAGE_SIZE);
    let mut stmt = conn.prepare(
        "SELECT ifnull(e.time, ''), f.target, ifnull(e.nick, ''), e.kind, ifnull(e.text, '')
         FROM files f CROSS JOIN events e INDEXED BY events_by_file_date
              ON e.file_id = f.id AND e.date = ?1
         WHERE f.kind = 'channel' AND e.kind IN ('message', 'action')
           AND (?2 IS NULL OR f.target = ?2 COLLATE NOCASE)
           AND (?3 IS NULL OR e.text LIKE ?3 ESCAPE '\\')
         ORDER BY e.time DESC, f.id, e.line DESC
         LIMIT ?4 OFFSET ?5",
    )?;
    let mut lines: Vec<Line> = stmt
        .query_map(
            params![
                day,
                filter.channel,
                pattern,
                i64::try_from(PAGE_SIZE + 1)?,
                i64::try_from(offset)?
            ],
            |row| {
                let kind: String = row.get(3)?;
                Ok(Line {
                    time: row.get(0)?,
                    channel: row.get(1)?,
                    nick: row.get(2)?,
                    action: kind == "action",
                    text: row.get(4)?,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    let more = lines.len() > PAGE_SIZE;
    lines.truncate(PAGE_SIZE);
    Ok((lines, more))
}

/// Escapes `%`, `_` and `\` for a LIKE pattern that uses `\` as its escape.
fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::discover::{self, tests::TempDir};
    use crate::{db, readings};

    const GREENHOUSE: &str = "--- Log opened Tue Nov 03 13:45:10 2026
13:46 < greenhouse> {\"testBegin\":true,\"timestamp\":\"2026-11-03T20:45:23.061Z\"}
13:46 < greenhouse> {\"sensor\":\"inside\",\"temp\":\"68.54\",\"timestamp\":\"2026-11-03T20:46:43.051Z\"}
13:47 -!- rachel [rachel@example] has joined #greenhouse
13:47 < rachel> 50%_off seeds
13:47  * rachel waters the tomatoes
--- Day changed Wed Nov 04 2026
00:01 < greenhouse> {\"sensor\":\"inside\",\"temp\":\"51.44\",\"timestamp\":\"2026-11-04T07:01:07.248Z\"}
";

    fn setup(name: &str) -> (TempDir, Connection) {
        let root = TempDir::new(name);
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        fs::write(root.0.join("wetfish/#greenhouse.log"), GREENHOUSE).unwrap();
        fs::write(
            root.0.join("wetfish/#garden.log"),
            "--- Log opened Tue Nov 03 09:00:00 2026\n09:00 < rachel> morning\n",
        )
        .unwrap();
        fs::write(
            root.0.join("wetfish/rachel.log"),
            "--- Log opened Tue Nov 03 09:00:00 2026\n09:01 < rachel> a private message\n",
        )
        .unwrap();
        let mut conn = db::open(&root.0.join("sensors.sqlite")).unwrap();
        let inventory = discover::discover(std::slice::from_ref(&root.0)).unwrap();
        db::import(&mut conn, &inventory.files);
        readings::build(&mut conn, false).unwrap();
        (root, conn)
    }

    fn texts(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                let mark = if line.action { "*" } else { "" };
                format!(
                    "{} {} {mark}{} {}",
                    line.time, line.channel, line.nick, line.text
                )
            })
            .collect()
    }

    #[test]
    fn lists_channels_and_days_without_private_messages() {
        let (_root, conn) = setup("history-days");
        assert_eq!(channels(&conn).unwrap(), ["#garden", "#greenhouse"]);

        let all = days(&conn, None).unwrap();
        let listed: Vec<(&str, i64, usize)> = all
            .iter()
            .map(|(date, day)| (date.as_str(), day.lines, day.channels.len()))
            .collect();
        assert_eq!(listed, [("2026-11-03", 5, 2), ("2026-11-04", 1, 1)]);

        let garden = days(&conn, Some("#GARDEN")).unwrap();
        assert_eq!(garden.keys().collect::<Vec<_>>(), ["2026-11-03"]);
        assert_eq!(
            latest_day(&conn, None).unwrap().as_deref(),
            Some("2026-11-04")
        );
        assert_eq!(
            latest_day(&conn, Some("#garden")).unwrap().as_deref(),
            Some("2026-11-03")
        );
        assert_eq!(latest_day(&conn, Some("#nowhere")).unwrap(), None);
    }

    #[test]
    fn shows_a_days_lines_newest_first() {
        let (_root, conn) = setup("history-lines");
        let (lines, more) = lines(&conn, "2026-11-03", &Filter::default()).unwrap();
        assert!(!more);
        assert_eq!(
            texts(&lines),
            [
                "13:47 #greenhouse *rachel waters the tomatoes",
                "13:47 #greenhouse rachel 50%_off seeds",
                "13:46 #greenhouse greenhouse {\"sensor\":\"inside\",\"temp\":\"68.54\",\"timestamp\":\"2026-11-03T20:46:43.051Z\"}",
                "13:46 #greenhouse greenhouse {\"testBegin\":true,\"timestamp\":\"2026-11-03T20:45:23.061Z\"}",
                "09:00 #garden rachel morning",
            ]
        );

        let filter = Filter {
            channel: Some("#Greenhouse".into()),
            text: Some("%_OFF".into()),
            page: 0,
        };
        let (lines, _) = super::lines(&conn, "2026-11-03", &filter).unwrap();
        assert_eq!(texts(&lines), ["13:47 #greenhouse rachel 50%_off seeds"]);

        // A wildcard in the text is matched as itself.
        let filter = Filter {
            text: Some("_".into()),
            ..Filter::default()
        };
        assert_eq!(
            super::lines(&conn, "2026-11-03", &filter).unwrap().0.len(),
            1
        );
    }

    #[test]
    fn pages_through_a_busy_day() {
        let root = TempDir::new("history-pages");
        fs::create_dir_all(root.0.join("wetfish")).unwrap();
        let mut log = String::from("--- Log opened Tue Nov 03 00:00:00 2026\n");
        for n in 0..PAGE_SIZE + 5 {
            log.push_str(&format!(
                "{:02}:{:02} < greenhouse> line {n}\n",
                n / 60,
                n % 60
            ));
        }
        fs::write(root.0.join("wetfish/#greenhouse.log"), log).unwrap();
        let mut conn = db::open(&root.0.join("sensors.sqlite")).unwrap();
        let inventory = discover::discover(std::slice::from_ref(&root.0)).unwrap();
        db::import(&mut conn, &inventory.files);

        let (first, more) = lines(&conn, "2026-11-03", &Filter::default()).unwrap();
        assert_eq!((first.len(), more), (PAGE_SIZE, true));
        assert_eq!(first[0].text, format!("line {}", PAGE_SIZE + 4));
        let second = Filter {
            page: 1,
            ..Filter::default()
        };
        let (rest, more) = lines(&conn, "2026-11-03", &second).unwrap();
        assert_eq!((rest.len(), more), (5, false));
        assert_eq!(rest[4].text, "line 0");
    }
}

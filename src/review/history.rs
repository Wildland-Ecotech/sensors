//! The history page: the lines said in channels one day at a time, the way
//! irssi shows them, and every day with lines.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Query, State};
use axum::routing::get;
use chrono::NaiveDate;
use maud::{Markup, html};
use serde::Deserialize;

use super::layout::{self, number, url_encode};
use super::{App, Flash, PageResult, filled, internal, with_db};
use crate::history::{self, Day, Filter, Line, PAGE_SIZE};

pub(super) fn routes() -> Router<App> {
    Router::new().route("/history", get(history_page))
}

#[derive(Default, Deserialize)]
struct HistoryQuery {
    channel: Option<String>,
    /// `YYYY-MM-DD`; the most recent day with lines when missing.
    day: Option<String>,
    /// Text the lines contain.
    q: Option<String>,
    /// From 0.
    page: Option<String>,
}

impl HistoryQuery {
    fn filter(&self) -> Filter {
        Filter {
            channel: filled(&self.channel).map(str::to_string),
            text: filled(&self.q).map(str::to_string),
            page: filled(&self.page)
                .and_then(|page| page.parse().ok())
                .unwrap_or(0),
        }
    }

    /// The day asked for, if it is a date.
    fn day(&self) -> Option<String> {
        filled(&self.day)
            .filter(|day| NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok())
            .map(str::to_string)
    }
}

/// The address of a day's history with a filter and page.
fn href(day: &str, filter: &Filter) -> String {
    let mut href = format!("/history?day={}", url_encode(day));
    if let Some(channel) = &filter.channel {
        href.push_str(&format!("&channel={}", url_encode(channel)));
    }
    if let Some(text) = &filter.text {
        href.push_str(&format!("&q={}", url_encode(text)));
    }
    if filter.page > 0 {
        href.push_str(&format!("&page={}", filter.page));
    }
    href
}

/// A line the way irssi shows it: `<nick> text`, or `* nick text` for an
/// action.
fn said(line: &Line) -> Markup {
    html! {
        @if line.action {
            "* " strong { (line.nick) } " " (line.text)
        } @else {
            span.muted { "<" } strong { (line.nick) } span.muted { ">" }
            " " (line.text)
        }
    }
}

async fn history_page(
    State(app): State<App>,
    Query(query): Query<HistoryQuery>,
    Query(flash): Query<Flash>,
) -> PageResult {
    with_db(&app, move |conn| {
        let filter = query.filter();
        let channels = history::channels(conn).map_err(internal)?;
        let days = history::days(conn, filter.channel.as_deref()).map_err(internal)?;
        let day = match query.day() {
            Some(day) => Some(day),
            None => history::latest_day(conn, filter.channel.as_deref()).map_err(internal)?,
        };
        let (lines, more) = match &day {
            Some(day) => history::lines(conn, day, &filter).map_err(internal)?,
            None => (Vec::new(), false),
        };
        let title = match &day {
            Some(day) => format!("History of {day}"),
            None => "History".to_string(),
        };
        Ok(layout::page(
            &title,
            &flash,
            page(day.as_deref(), &filter, &channels, &days, &lines, more),
        ))
    })
    .await
}

fn page(
    day: Option<&str>,
    filter: &Filter,
    channels: &[String],
    days: &BTreeMap<String, Day>,
    lines: &[Line],
    more: bool,
) -> Markup {
    let Some(day) = day else {
        return html! {
            h1 { "History" }
            p {
                "No lines yet. Run " code { "sensors analyze" }
                " or Analyze now to import some."
            }
        };
    };
    let filtered = filter.channel.is_some() || filter.text.is_some();
    let at_page = |page: usize| {
        href(
            day,
            &Filter {
                page,
                ..filter.clone()
            },
        )
    };
    html! {
        h1 { "History of " (day) }
        p.lead {
            @match days.get(day) {
                Some(found) => {
                    (number(found.lines)) " lines in "
                    @for (i, channel) in found.channels.iter().enumerate() {
                        @if i > 0 { ", " }
                        (channel)
                    }
                    "."
                },
                None => "No lines this day.",
            }
        }
        p.note { "Messages and actions in channels. Private messages are left out." }

        form.search method="get" action="/history" {
            label {
                "Channel "
                select name="channel" {
                    option value="" { "All channels" }
                    @for channel in channels {
                        option value=(channel)
                            selected[filter.channel.as_deref().is_some_and(|c| c.eq_ignore_ascii_case(channel))] {
                            (channel)
                        }
                    }
                }
            }
            label { "Day " input type="date" name="day" value=(day); }
            label { "Contains " input type="text" name="q" value=[filter.text.as_deref()]; }
            button type="submit" { "Show" }
            @if filtered {
                a href=(href(day, &Filter::default())) { "Clear" }
            }
        }

        @if lines.is_empty() {
            p {
                @if filter.page > 0 {
                    "No more lines this day."
                } @else if filtered {
                    "No lines match this day."
                } @else {
                    "No lines this day."
                }
            }
        } @else {
            p.note {
                "Most recent first, " (PAGE_SIZE) " to a page"
                @if filter.page > 0 { ", page " (filter.page + 1) }
                "."
            }
            div.scroll {
                table.lines {
                    thead {
                        tr {
                            th { "Time" }
                            th { "Channel" }
                            th { "Line" }
                        }
                    }
                    tbody {
                        @for line in lines {
                            tr {
                                td { (line.time) }
                                td { (line.channel) }
                                td.wrap { (said(line)) }
                            }
                        }
                    }
                }
            }
        }
        @if filter.page > 0 || more {
            p.pager {
                @if filter.page > 0 {
                    a href=(at_page(filter.page - 1)) { "Newer" }
                }
                @if more {
                    a href=(at_page(filter.page + 1)) { "Older" }
                }
            }
        }

        h2 { "Days" }
        @if days.is_empty() {
            p { "No days with lines." }
        } @else {
            (months(day, filter, days))
        }
    }
}

/// The days, by month, most recent first, each month in a fold. The month
/// of the day shown is open.
fn months(shown: &str, filter: &Filter, days: &BTreeMap<String, Day>) -> Markup {
    let mut by_month: BTreeMap<&str, Vec<(&String, &Day)>> = BTreeMap::new();
    for (date, day) in days {
        by_month
            .entry(date.get(..7).unwrap_or(date))
            .or_default()
            .push((date, day));
    }
    // A day link keeps the channel, and starts the day at its first page.
    let day_filter = Filter {
        channel: filter.channel.clone(),
        ..Filter::default()
    };
    html! {
        @for (month, days) in by_month.iter().rev() {
            details open[shown.starts_with(month)] {
                summary {
                    (month) ": " (days.len()) " day" @if days.len() != 1 { "s" } ", "
                    (number(days.iter().map(|(_, day)| day.lines).sum::<i64>())) " lines"
                }
                div.scroll {
                    table {
                        thead {
                            tr {
                                th { "Day" }
                                th.num { "Lines" }
                                th { "Channels" }
                            }
                        }
                        tbody {
                            @for (date, day) in days.iter().rev() {
                                tr {
                                    td {
                                        @if date.as_str() == shown {
                                            strong { (date) }
                                        } @else {
                                            a href=(href(date, &day_filter)) { (date) }
                                        }
                                    }
                                    td.num { (number(day.lines)) }
                                    td.wrap {
                                        @for (i, channel) in day.channels.iter().enumerate() {
                                            @if i > 0 { ", " }
                                            (channel)
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn keeps_filters_in_page_links() {
        let query = HistoryQuery {
            channel: Some("#greenhouse".into()),
            q: Some("a b".into()),
            page: Some("2".into()),
            day: Some("not a day".into()),
        };
        let filter = query.filter();
        assert_eq!(filter.page, 2);
        assert_eq!(query.day(), None, "an invalid day is ignored");
        assert_eq!(
            href("2026-11-03", &filter),
            "/history?day=2026-11-03&channel=%23greenhouse&q=a%20b&page=2"
        );
        assert_eq!(
            href("2026-11-03", &Filter::default()),
            "/history?day=2026-11-03"
        );
    }

    #[test]
    fn shows_a_days_lines_pages_and_days() {
        let day = |lines, channels: &[&str]| Day {
            channels: channels
                .iter()
                .map(|c| c.to_string())
                .collect::<BTreeSet<_>>(),
            lines,
        };
        let days = BTreeMap::from([
            ("2026-10-31".to_string(), day(10, &["#greenhouse"])),
            (
                "2026-11-03".to_string(),
                day(2_449, &["#garden", "#greenhouse"]),
            ),
            ("2026-11-04".to_string(), day(3_280, &["#greenhouse"])),
        ]);
        let lines = [
            Line {
                time: "13:47".into(),
                channel: "#greenhouse".into(),
                nick: "rachel".into(),
                action: true,
                text: "waters the tomatoes".into(),
            },
            Line {
                time: "13:46".into(),
                channel: "#greenhouse".into(),
                nick: "greenhouse".into(),
                action: false,
                text: "{\"sensor\":\"inside\",\"temp\":\"68.54\"}".into(),
            },
        ];
        let filter = Filter {
            channel: Some("#greenhouse".into()),
            text: None,
            page: 1,
        };
        let channels = ["#garden".to_string(), "#greenhouse".to_string()];
        let page = page(Some("2026-11-03"), &filter, &channels, &days, &lines, true).into_string();

        assert!(page.contains("<h1>History of 2026-11-03</h1>"));
        assert!(page.contains("2,449 lines in #garden, #greenhouse."));
        assert!(page.contains(r##"<option value="#greenhouse" selected>#greenhouse</option>"##));
        assert!(page.contains("* <strong>rachel</strong> waters the tomatoes"));
        assert!(page.contains(
            r#"<strong>greenhouse</strong><span class="muted">&gt;</span> {&quot;sensor&quot;"#
        ));
        assert!(
            page.contains(r#"href="/history?day=2026-11-03&amp;channel=%23greenhouse">Newer</a>"#)
        );
        assert!(page.contains(
            r#"href="/history?day=2026-11-03&amp;channel=%23greenhouse&amp;page=2">Older</a>"#
        ));
        assert!(page.contains("<details open><summary>2026-11: 2 days, 5,729 lines</summary>"));
        assert!(page.contains("<details><summary>2026-10: 1 day, 10 lines</summary>"));
        assert!(page.contains("<strong>2026-11-03</strong>"));
        assert!(page.contains(
            r#"href="/history?day=2026-11-04&amp;channel=%23greenhouse">2026-11-04</a>"#
        ));

        let empty =
            super::page(None, &Filter::default(), &[], &BTreeMap::new(), &[], false).into_string();
        assert!(empty.contains("No lines yet."));
    }
}

//! The page frame shared by every review page, and small helpers for links
//! and numbers.

use maud::{DOCTYPE, Markup, html};

use super::Flash;

/// A complete page with the site navigation and any notice or error.
pub fn page(title: &str, flash: &Flash, body: Markup) -> Markup {
    frame(title, flash, body, None)
}

/// A page that reloads itself every `seconds`, to follow work under way
/// with no script.
pub fn reloading_page(title: &str, flash: &Flash, body: Markup, seconds: u32) -> Markup {
    frame(title, flash, body, Some(seconds))
}

fn frame(title: &str, flash: &Flash, body: Markup, reload: Option<u32>) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                @if let Some(seconds) = reload {
                    meta http-equiv="refresh" content=(seconds);
                }
                title { (title) " · sensors review" }
                link rel="stylesheet" href="/style.css";
            }
            body {
                header {
                    nav {
                        a.brand href="/" { "sensors review" }
                        a href="/" { "Overview" }
                        a href="/history" { "History" }
                        a href="/analyze" { "Analyze" }
                    }
                }
                main {
                    @if super::analyze::running() {
                        p.analysis-banner role="status" {
                            "An analysis is running. Pages show the data from before it until it "
                            "finishes. "
                            a href="/analyze" { "See how far it has come" }
                        }
                    }
                    @if let Some(notice) = &flash.notice {
                        p.flash.notice role="status" { (notice) }
                    }
                    @if let Some(error) = &flash.error {
                        p.flash.error role="alert" { (error) }
                    }
                    (body)
                }
            }
        }
    }
}

/// Percent-encodes a query string value, leaving only unreserved characters.
pub fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A count with thousands separators, such as `12,345`.
pub fn number(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
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
    use super::*;

    #[test]
    fn encodes_values_for_links() {
        assert_eq!(url_encode("#greenhouse"), "%23greenhouse");
        assert_eq!(url_encode("a b&c=d"), "a%20b%26c%3Dd");
        assert_eq!(url_encode("2026-11-03"), "2026-11-03");
    }

    #[test]
    fn formats_numbers() {
        assert_eq!(number(0), "0");
        assert_eq!(number(1_234_567), "1,234,567");
        assert_eq!(number(-1_000), "-1,000");
    }

    #[test]
    fn escapes_text_and_shows_messages() {
        let flash = Flash {
            notice: Some("Saved <ok>".into()),
            error: None,
        };
        let page = page("<b>", &flash, html! { p { "a < b" } }).into_string();
        assert!(page.contains("<title>&lt;b&gt; · sensors review</title>"));
        assert!(page.contains("<p>a &lt; b</p>"));
        assert!(page.contains("Saved &lt;ok&gt;"));
    }
}

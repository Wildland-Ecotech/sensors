//! The page frame shared by every page, and small helpers for links and
//! numbers.

use maud::{DOCTYPE, Markup, html};

use super::{Flash, Site};

/// A complete page with the site navigation and any notice or error.
pub fn page(title: &str, flash: &Flash, body: Markup) -> Markup {
    frame(super::site(), title, flash, body, None)
}

/// A page that reloads itself every `seconds`, to follow work under way
/// with no script.
pub fn reloading_page(title: &str, flash: &Flash, body: Markup, seconds: u32) -> Markup {
    frame(super::site(), title, flash, body, Some(seconds))
}

fn frame(site: Site, title: &str, flash: &Flash, body: Markup, reload: Option<u32>) -> Markup {
    let name = match site {
        Site::Review => "sensors review",
        Site::Live => "sensors",
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                @if let Some(seconds) = reload {
                    meta http-equiv="refresh" content=(seconds);
                }
                title { (title) " · " (name) }
                link rel="stylesheet" href="/style.css";
                script src="/chart.js" defer {}
            }
            body {
                header {
                    nav {
                        a.brand href="/" { (name) }
                        @if site == Site::Review {
                            a href="/" { "Overview" }
                            a href="/sensors" { "Sensors" }
                            a href="/history" { "History" }
                            a href="/analyze" { "Analyze" }
                        }
                    }
                }
                main {
                    @if site == Site::Review && super::analyze::running() {
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
                @if site == Site::Live {
                    // The AGPL asks for the source to be offered to everyone
                    // using it over a network.
                    footer {
                        "Readings as the sensors post them to IRC. "
                        a href=(env!("CARGO_PKG_REPOSITORY")) { "Source code" }
                    }
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
        assert!(page.contains(r#"<a href="/history">History</a>"#));
    }

    #[test]
    fn shows_only_the_sensors_on_the_live_site() {
        let live = frame(Site::Live, "Sensors", &Flash::default(), html! {}, None).into_string();
        assert!(live.contains("<title>Sensors · sensors</title>"));
        assert!(live.contains(r#"<a class="brand" href="/">sensors</a></nav>"#));
        assert!(!live.contains("/history") && !live.contains("/analyze"));
        assert!(
            live.contains(
                r#"<a href="https://github.com/Wildland-Ecotech/sensors">Source code</a>"#
            )
        );
    }
}

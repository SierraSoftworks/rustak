//! Small formatting and navigation helpers shared across pages.

use chrono::{DateTime, Utc};

/// Builds an href for in-app navigation, preserving demo mode across full-page
/// navigations by carrying the `?demo` query forward when it is active.
pub fn nav_href(path: &str) -> String {
    if crate::fixtures::is_demo() {
        let separator = if path.contains('?') { '&' } else { '?' };
        format!("{path}{separator}demo")
    } else {
        path.to_string()
    }
}

/// The browser window. Panicking here is the honest response: every path in
/// this crate runs inside a browser, so its absence is not a condition anything
/// could recover from.
pub fn window() -> web_sys::Window {
    web_sys::window().expect("a browser window should be available")
}

/// Percent-encodes a value for use in a URL path segment or query component.
pub fn urlencode(value: &str) -> String {
    js_sys::encode_uri_component(value).into()
}

/// Formats a UTC timestamp as an RFC 3339 string with a `Z` suffix, for example
/// `2026-06-08T12:48:38Z`.
pub fn format_iso8601(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Formats a duration in seconds using the single largest sensible unit, for
/// example `45s`, `5m`, `2h` or `3d`. The sign is ignored.
pub fn short_duration(secs: i64) -> String {
    let abs = secs.unsigned_abs();
    if abs < 60 {
        format!("{abs}s")
    } else if abs < 3600 {
        format!("{}m", abs / 60)
    } else if abs < 86_400 {
        format!("{}h", abs / 3600)
    } else {
        format!("{}d", abs / 86_400)
    }
}

/// Formats a timestamp relative to now, for example `15m ago`, `in 5m`, or
/// `now` when it is within a second of the present.
pub fn short_relative(dt: DateTime<Utc>) -> String {
    let secs = dt.signed_duration_since(Utc::now()).num_seconds();
    if secs.abs() < 1 {
        return "now".to_string();
    }
    let magnitude = short_duration(secs);
    if secs < 0 {
        format!("{magnitude} ago")
    } else {
        format!("in {magnitude}")
    }
}

/// Renders an optional timestamp as a relative phrase, or an em dash when there
/// is nothing to say. Used by every list that has a "last seen" column, so that
/// "never" looks the same in all of them.
pub fn optional_relative(dt: Option<DateTime<Utc>>) -> String {
    dt.map(short_relative).unwrap_or_else(|| "—".to_string())
}

/// Up to two upper-case initials derived from a display name or an email
/// address, for the avatar in the app bar.
pub fn initials(name: &str) -> String {
    let from_words: String = name
        .split(|c: char| c.is_whitespace() || c == '.' || c == '@' || c == '_' || c == '-')
        .filter(|word| !word.is_empty())
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect();

    let initials = if from_words.is_empty() {
        name.chars().take(2).collect()
    } else {
        from_words
    };

    initials.to_uppercase()
}

//! Small formatting and navigation helpers shared across pages.

pub mod mgrs;
pub mod sidc;

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

/// How long to wait for the clipboard before giving up on it.
///
/// Not a nicety. A browser that has *denied* clipboard access does not reject
/// `writeText` — in Chromium the promise simply never settles — so without a
/// deadline the button would spin silently for ever and the person holding a
/// one-time secret would have no idea whether it had been copied.
const CLIPBOARD_TIMEOUT_MS: u32 = 3_000;

/// Puts a value on the system clipboard, reporting whether it worked.
///
/// The asynchronous Clipboard API is the only one that does not need a
/// synthetic selection, but it exists only in a secure context and a browser
/// may refuse it outright — so the caller is told, and every place that offers
/// a copy button shows the value beside it for somebody to take by hand.
pub async fn copy_to_clipboard(value: &str) -> Result<(), String> {
    let refused = "Your browser would not let us reach the clipboard. \
                   Select the value and copy it by hand.";

    let write =
        wasm_bindgen_futures::JsFuture::from(window().navigator().clipboard().write_text(value));
    let deadline = gloo_timers::future::TimeoutFuture::new(CLIPBOARD_TIMEOUT_MS);

    futures::pin_mut!(write);
    futures::pin_mut!(deadline);

    match futures::future::select(write, deadline).await {
        futures::future::Either::Left((Ok(_), _)) => Ok(()),
        futures::future::Either::Left((Err(_), _)) | futures::future::Either::Right(_) => {
            Err(refused.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    #[test]
    fn a_duration_is_shown_in_its_largest_sensible_unit() {
        assert_eq!(short_duration(0), "0s");
        assert_eq!(short_duration(45), "45s");
        assert_eq!(short_duration(59), "59s");
        assert_eq!(short_duration(60), "1m");
        assert_eq!(short_duration(3_599), "59m");
        assert_eq!(short_duration(3_600), "1h");
        assert_eq!(short_duration(86_399), "23h");
        assert_eq!(short_duration(86_400), "1d");
        // The sign belongs to the phrase around it, not to the magnitude.
        assert_eq!(short_duration(-90), short_duration(90));
    }

    #[test]
    fn a_timestamp_says_which_side_of_now_it_is_on() {
        let now = Utc::now();

        assert_eq!(short_relative(now), "now");
        // A heartbeat a few minutes ago, which is the case the Services page
        // exists to show.
        assert_eq!(short_relative(now - Duration::minutes(15)), "15m ago");
        assert_eq!(short_relative(now - Duration::hours(3)), "3h ago");
        assert_eq!(short_relative(now - Duration::days(2)), "2d ago");
        // A second past the boundary, because the clock moves between building
        // the timestamp and formatting it: a bare `now + 5m` has already become
        // four minutes and fifty-nine seconds by the time it is read.
        assert_eq!(
            short_relative(now + Duration::minutes(5) + Duration::seconds(1)),
            "in 5m"
        );
    }

    #[test]
    fn a_timestamp_that_is_not_there_is_an_em_dash_everywhere() {
        // "Never reported" has to look the same on every list that has a
        // last-seen column, which is what this helper is for.
        assert_eq!(optional_relative(None), "—");
        assert_eq!(
            optional_relative(Some(Utc::now() - Duration::minutes(47))),
            "47m ago"
        );
    }
}

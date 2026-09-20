//! What this sidecar tells the admin UI about itself.
//!
//! The harness's own heartbeat says "healthy" and nothing else, which is not
//! enough to run a feed from: an administrator looking at the Services page
//! wants to know whether the upstream is answering, how many aircraft are on
//! the map, and how much of what the feed offered actually went out. This
//! module builds that heartbeat and sends it through
//! [`SidecarContext::control`](rustak_client::sidecar::SidecarContext::control),
//! which is the documented way for a plugin to say more than "healthy"
//! (`docs/plugins.md` → Registering with the server).
//!
//! # The three states
//!
//! | State | When |
//! |---|---|
//! | `unhealthy` | The source has never connected — a setting is wrong, not a network having a bad minute |
//! | `degraded` | It connected once and has been reconnecting for more than two poll intervals |
//! | `healthy` | It is answering, or it has only just stopped |
//!
//! # Nothing secret goes in `metrics`
//!
//! The admin UI renders it as it arrives. What goes in is the source's kind and
//! name, its connection state, the counters and the tracked count — and the
//! last error, which comes from [`SourceState`], whose messages are built from
//! `human_errors` text rather than from a request that carried a credential.

use std::time::Duration;

use rustak_api::{Heartbeat, ServiceState};
use rustak_client::feed::FeedCounters;

use crate::sources::SourceState;

/// How often the sidecar repeats a heartbeat that has not changed.
///
/// A plugin that reported on every tick would double the control API's load for
/// a line that says the same thing; one that only reported on a change would
/// leave a stale row behind after the server's sweep.
pub const REPEAT_AFTER: Duration = Duration::from_secs(30);

/// Everything this sidecar knows about itself, as a heartbeat.
#[must_use]
pub fn heartbeat(
    kind: &str,
    state: &SourceState,
    counters: FeedCounters,
    tracked: usize,
) -> Heartbeat {
    let service_state = service_state(state);
    let connection = connection(state);

    Heartbeat {
        state: service_state,
        message: Some(message(state, tracked, connection)),
        metrics: serde_json::json!({
            "source": {
                "kind": kind,
                "name": state.name(),
                "connection": connection,
                "since": state.since(),
                "last_error": state.last_error(),
            },
            "tracked": tracked,
            "feed": counters,
        }),
    }
}

/// Which of the three states this source is in.
#[must_use]
pub fn service_state(state: &SourceState) -> ServiceState {
    if !state.ever_connected() {
        // A source that has never worked is a configuration to look at rather
        // than an outage to wait out.
        return ServiceState::Unhealthy;
    }

    match state.reconnecting_for() {
        Some(elapsed) if elapsed > grace(state) => ServiceState::Degraded,
        _ => ServiceState::Healthy,
    }
}

/// How long a source may be reconnecting before it is worth reporting.
///
/// Two poll intervals: one missed poll is a packet that went missing, two in a
/// row is an upstream that has gone.
fn grace(state: &SourceState) -> chrono::Duration {
    chrono::Duration::from_std(state.interval() * 2).unwrap_or_else(|_| chrono::Duration::zero())
}

/// The connection, in one word an administrator can filter on.
fn connection(state: &SourceState) -> &'static str {
    match (state.is_connected(), state.ever_connected()) {
        (true, _) => "connected",
        (false, true) => "reconnecting",
        (false, false) => "never_connected",
    }
}

/// One sentence for the Services page.
fn message(state: &SourceState, tracked: usize, connection: &str) -> String {
    match (connection, state.last_error()) {
        ("connected", _) => format!("{} is answering; {tracked} aircraft tracked.", state.name()),
        (_, Some(error)) => format!("{} is not answering: {error}", state.name()),
        _ => format!("{} has not answered yet.", state.name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SourceState {
        SourceState::new("adsb.lol", Duration::from_secs(5))
    }

    fn counters() -> FeedCounters {
        FeedCounters {
            offered: 40,
            published: 12,
            suppressed: 28,
            expired: 3,
        }
    }

    #[test]
    fn a_source_that_has_never_connected_is_unhealthy() {
        let mut state = state();
        state.failed("connection refused");

        let beat = heartbeat("readsb", &state, counters(), 0);

        assert_eq!(beat.state, ServiceState::Unhealthy);
        assert_eq!(beat.metrics["source"]["connection"], "never_connected");
        assert!(
            beat.message
                .expect("a message")
                .contains("connection refused"),
            "the reason belongs where somebody will read it",
        );
    }

    #[test]
    fn a_source_that_is_answering_is_healthy_and_says_what_it_is_carrying() {
        let mut state = state();
        state.succeeded();

        let beat = heartbeat("aggregator", &state, counters(), 137);

        assert_eq!(beat.state, ServiceState::Healthy);
        assert_eq!(beat.metrics["source"]["kind"], "aggregator");
        assert_eq!(beat.metrics["source"]["name"], "adsb.lol");
        assert_eq!(beat.metrics["source"]["connection"], "connected");
        assert_eq!(beat.metrics["tracked"], 137);
        assert_eq!(beat.metrics["feed"]["offered"], 40);
        assert_eq!(beat.metrics["feed"]["published"], 12);
        assert_eq!(beat.metrics["feed"]["suppressed"], 28);
        assert_eq!(beat.metrics["feed"]["expired"], 3);
        assert!(beat.metrics["source"]["last_error"].is_null());
        assert!(
            beat.message.expect("a message").contains("137 aircraft"),
            "the number is the point",
        );
    }

    #[test]
    fn one_missed_poll_is_not_yet_degraded() {
        let mut state = state();
        state.succeeded();
        state.failed("timed out");

        assert_eq!(
            service_state(&state),
            ServiceState::Healthy,
            "a single blip must not turn a fleet amber",
        );
    }

    #[test]
    fn reconnecting_for_longer_than_two_intervals_is_degraded() {
        let mut state = SourceState::new("adsb.fi", Duration::from_secs(0));
        state.succeeded();
        std::thread::sleep(Duration::from_millis(5));
        state.failed("timed out");
        std::thread::sleep(Duration::from_millis(5));

        let beat = heartbeat("aggregator", &state, counters(), 4);

        assert_eq!(beat.state, ServiceState::Degraded);
        assert_eq!(beat.metrics["source"]["connection"], "reconnecting");
    }

    #[test]
    fn nothing_in_a_heartbeat_is_a_credential() {
        // `metrics` is rendered in the admin UI as it arrives, so this asserts
        // the shape rather than trusting the caller.
        let mut state = state();
        state.succeeded();

        let beat = heartbeat("opensky", &state, counters(), 1);
        let rendered = serde_json::to_string(&beat.metrics).expect("it serialises");

        for key in [
            "secret",
            "token",
            "password",
            "client_secret",
            "authorization",
        ] {
            assert!(!rendered.to_lowercase().contains(key), "{rendered}");
        }
    }
}

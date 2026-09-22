//! What this sidecar tells the admin UI about itself.
//!
//! The harness's own heartbeat says "healthy" and nothing else. An
//! administrator looking at the Services page wants to know whether FIRMS is
//! answering, how many detections are on the map, and whether a backlog is
//! still draining. [`FirmsSidecar::health`](rustak_client::sidecar::Sidecar::health)
//! hands this to the harness after every tick.
//!
//! | State | When |
//! |---|---|
//! | `unhealthy` | The source has never answered: a key or a setting is wrong |
//! | `degraded` | It answered once and has been failing for more than two polls |
//! | `healthy` | It is answering, or has only just stopped |
//!
//! **Nothing secret goes in `metrics`**: the admin UI renders it as it
//! arrives. The last error comes from [`SourceState`], whose messages are
//! redacted of the MAP_KEY before they are recorded.

use rustak_api::{Heartbeat, ServiceState};

use crate::hotspots::Counters;
use crate::sources::SourceState;

/// Everything this sidecar knows about itself, as a heartbeat.
#[must_use]
pub fn heartbeat(
    kind: &str,
    state: &SourceState,
    counters: Counters,
    tracked: usize,
    pending: usize,
) -> Heartbeat {
    let connection = connection(state);

    Heartbeat {
        state: service_state(state),
        message: Some(message(state, tracked, connection)),
        metrics: serde_json::json!({
            "source": {
                "kind": kind,
                "name": state.name(),
                "connection": connection,
                "since": state.since(),
                "last_error": state.last_error(),
                "poll_s": state.interval().as_secs(),
                "rate_limited": state.rate_limited(),
            },
            "tracked": tracked,
            // Detections waiting for their first publication: above zero for
            // long means `max_per_tick` is smaller than the day is bad.
            "pending": pending,
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

    // Two poll intervals: one missed poll is a bad minute, two is an outage.
    let grace = chrono::Duration::from_std(state.interval() * 2)
        .unwrap_or_else(|_| chrono::Duration::zero());

    match state.reconnecting_for() {
        Some(elapsed) if elapsed > grace => ServiceState::Degraded,
        _ => ServiceState::Healthy,
    }
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
        ("connected", _) => format!(
            "{} is answering; {tracked} fire detections on the map.",
            state.name(),
        ),
        (_, Some(error)) => format!("{} is not answering: {error}", state.name()),
        _ => format!("{} has not answered yet.", state.name()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn counters() -> Counters {
        Counters {
            offered: 40,
            published: 12,
            republished: 24,
            suppressed: 28,
            expired: 3,
        }
    }

    #[test]
    fn a_source_that_is_answering_says_what_it_is_carrying() {
        let mut state = SourceState::new("NASA FIRMS", Duration::from_secs(600));
        state.succeeded();

        let beat = heartbeat("firms", &state, counters(), 137, 4);

        assert_eq!(beat.state, ServiceState::Healthy);
        assert_eq!(beat.metrics["source"]["kind"], "firms");
        assert_eq!(beat.metrics["source"]["connection"], "connected");
        assert_eq!(beat.metrics["source"]["poll_s"], 600);
        assert_eq!(beat.metrics["tracked"], 137);
        assert_eq!(beat.metrics["pending"], 4);
        assert_eq!(beat.metrics["feed"]["republished"], 24);
        assert!(beat.message.expect("a message").contains("137"));
    }

    #[test]
    fn a_source_that_has_never_answered_is_a_setting_to_look_at() {
        let mut state = SourceState::new("NASA FIRMS", Duration::from_secs(600));
        state.failed("NASA FIRMS refused the request: Invalid MAP_KEY.");

        let beat = heartbeat("firms", &state, Counters::default(), 0, 0);

        assert_eq!(beat.state, ServiceState::Unhealthy);
        assert_eq!(beat.metrics["source"]["connection"], "never_connected");
        assert!(
            beat.message.expect("a message").contains("Invalid MAP_KEY"),
            "the reason belongs where somebody will read it",
        );
    }

    #[test]
    fn an_outage_is_degraded_only_once_it_has_outlasted_a_missed_poll() {
        let mut blip = SourceState::new("NASA FIRMS", Duration::from_secs(600));
        blip.succeeded();
        blip.failed("timed out");

        assert_eq!(service_state(&blip), ServiceState::Healthy);

        let mut outage = SourceState::new("NASA FIRMS", Duration::ZERO);
        outage.succeeded();
        outage.failed("timed out");
        std::thread::sleep(Duration::from_millis(5));

        assert_eq!(service_state(&outage), ServiceState::Degraded);
    }
}

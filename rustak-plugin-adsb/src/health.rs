//! What this sidecar tells the admin UI about itself.
//!
//! The harness's own heartbeat says "healthy" and nothing else, which is not
//! enough to run a feed from: an administrator looking at the Services page
//! wants to know whether the upstream is answering, how many aircraft are on
//! the map, and how much of what the feed offered actually went out. This
//! module builds that heartbeat, and
//! [`AdsbSidecar::health`](rustak_client::sidecar::Sidecar::health) hands it to
//! the harness after every tick — the hook `docs/plugins.md` → "Saying more
//! than healthy" documents, which the harness reports *instead of* its own
//! floor rather than a moment before it.
//!
//! # The three states
//!
//! | State | When |
//! |---|---|
//! | `unhealthy` | The source has never connected — a setting is wrong, not a network having a bad minute |
//! | `degraded` | It connected once and has been reconnecting for more than two poll intervals — or more than half of its last twenty polls were refused `429` |
//! | `healthy` | It is answering, or it has only just stopped — **including while it is slowing down for a provider that rate-limits it** |
//!
//! Rate limiting that the source absorbs by polling less often is the plugin
//! working, not the feed degrading: the aircraft are still on the map, a little
//! less often, and `source.poll_effective_s` beside `source.poll_configured_s`
//! says by how much. It becomes `degraded` only when the provider is refusing
//! more often than it answers, which slowing down has evidently not fixed.
//!
//! # Nothing secret goes in `metrics`
//!
//! The admin UI renders it as it arrives. What goes in is the source's kind and
//! name, its connection state, its cadence, the counters and the tracked
//! count — and the last error, which comes from [`SourceState`], whose messages
//! are built from `human_errors` text rather than from a request that carried a
//! credential.

use rustak_api::{Heartbeat, ServiceState};
use rustak_client::feed::FeedCounters;

use crate::sources::SourceState;

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
                // The cadence *as it stands*, which is not always the one in
                // the file: a provider that rate-limits is polled less often,
                // and an administrator looking at a feed that is behind should
                // be able to see that from the Services page rather than from
                // a log line five hours old.
                "poll_effective_s": state.interval().as_secs(),
                // And the one it was opened with — an operator's `poll` or the
                // provider's default — which is the floor the one above eases
                // back to. The two differing is the whole story at a glance.
                "poll_configured_s": state.configured().as_secs(),
                "rate_limited": state.rate_limited(),
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
        // Adapting to a rate limit is healthy; being refused more often than
        // answered, after adapting, is not.
        _ if state.mostly_refused().is_some() => ServiceState::Degraded,
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
    if let Some((refused, of)) = state.mostly_refused() {
        return format!(
            "{} is rate-limiting this feed: {refused} of its last {of} requests were refused \
             (429). Polling every {}s; {tracked} aircraft tracked.",
            state.name(),
            state.interval().as_secs(),
        );
    }

    match (connection, state.last_error()) {
        ("connected", _) => format!("{} is answering; {tracked} aircraft tracked.", state.name()),
        (_, Some(error)) => format!("{} is not answering: {error}", state.name()),
        _ => format!("{} has not answered yet.", state.name()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
        assert_eq!(beat.metrics["source"]["poll_effective_s"], 5);
        assert_eq!(beat.metrics["source"]["poll_configured_s"], 5);
        assert_eq!(beat.metrics["source"]["rate_limited"], 0);
        assert!(
            beat.message.expect("a message").contains("137 aircraft"),
            "the number is the point",
        );
    }

    #[test]
    fn a_source_that_was_asked_to_slow_down_says_so_on_the_heartbeat() {
        // The cadence an administrator reads has to be the one being used, not
        // the one that was configured: the Dublin deployment was polling at
        // half the rate its file asked for and nothing on the page said so.
        let mut state = state();
        state.succeeded();
        state.wait_for(Some(Duration::from_secs(10)));
        state.succeeded();
        state.wait_for(Some(Duration::from_secs(10)));

        let beat = heartbeat("aggregator", &state, counters(), 12);

        assert_eq!(beat.metrics["source"]["poll_effective_s"], 10);
        assert_eq!(beat.metrics["source"]["poll_configured_s"], 5);
        assert_eq!(beat.metrics["source"]["rate_limited"], 2);
    }

    #[test]
    fn a_source_that_is_slowing_down_for_a_provider_that_names_no_delay_is_healthy() {
        // The second Dublin finding: 429 with no Retry-After. The plugin backs
        // off, the page shows both numbers, and nothing turns amber — rate
        // limiting that is being absorbed is not degradation.
        let mut state = state();
        state.succeeded();
        state.wait_for(None);
        state.succeeded();
        state.wait_for(None);

        let beat = heartbeat("aggregator", &state, counters(), 12);

        assert_eq!(beat.state, ServiceState::Healthy);
        assert_eq!(beat.metrics["source"]["poll_effective_s"], 8);
        assert_eq!(beat.metrics["source"]["poll_configured_s"], 5);
        assert_eq!(beat.metrics["source"]["rate_limited"], 2);
        assert!(
            beat.message.expect("a message").contains("is answering"),
            "and the sentence is the ordinary one",
        );
    }

    #[test]
    fn a_source_refused_more_often_than_it_is_answered_is_degraded_and_says_why() {
        let mut state = state();
        state.succeeded();

        for _ in 0..10 {
            state.wait_for(None);
        }

        assert_eq!(
            service_state(&state),
            ServiceState::Healthy,
            "ten refusals are not yet more than half of twenty polls",
        );

        state.wait_for(None);

        let beat = heartbeat("aggregator", &state, counters(), 3);
        let message = beat.message.expect("a message");

        assert_eq!(beat.state, ServiceState::Degraded);
        assert!(
            message.contains("11 of its last 12 requests were refused (429)"),
            "{message}",
        );
        assert!(message.contains("adsb.lol"), "{message}");
        assert_eq!(beat.metrics["source"]["rate_limited"], 11);
        assert_eq!(beat.metrics["source"]["connection"], "connected");

        // And it clears on its own, once the window is mostly answers again.
        for _ in 0..10 {
            state.succeeded();
        }

        assert_eq!(service_state(&state), ServiceState::Healthy);
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

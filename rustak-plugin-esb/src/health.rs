//! What this sidecar tells the admin UI about itself.
//!
//! | State | When |
//! |---|---|
//! | `unhealthy` | The source has never answered: a setting is wrong |
//! | `degraded` | It answered once and has been failing for two intervals |
//! | `healthy` | It is answering, or has only just stopped |
//!
//! Nothing secret goes in `metrics`: the last error comes from
//! [`SourceState`], whose messages never carry the subscription key.

use chrono::Utc;
use rustak_api::{Heartbeat, ServiceState};
use rustak_client::feed::FeedCounters;

use crate::publish::Summary;
use crate::sources::SourceState;

/// Everything this sidecar knows about itself, as a heartbeat.
#[must_use]
pub fn heartbeat(
    kind: &str,
    state: &SourceState,
    counters: FeedCounters,
    summary: Summary,
) -> Heartbeat {
    let connection = connection(state);

    Heartbeat {
        state: service_state(state),
        message: Some(message(state, summary, connection)),
        metrics: serde_json::json!({
            "source": {
                "kind": kind,
                "name": state.name(),
                "connection": connection,
                "since": state.since(),
                "last_success": state.last_success(),
                "last_error": state.last_error(),
                "poll_s": state.interval().as_secs(),
            },
            "outages": summary,
            "feed": counters,
        }),
    }
}

/// Which of the three states this source is in.
#[must_use]
pub fn service_state(state: &SourceState) -> ServiceState {
    if state.last_success().is_none() {
        return ServiceState::Unhealthy;
    }

    let grace = chrono::Duration::from_std(state.interval() * 2).unwrap_or_default();

    if !state.is_connected() && Utc::now() - state.since() > grace {
        ServiceState::Degraded
    } else {
        ServiceState::Healthy
    }
}

fn connection(state: &SourceState) -> &'static str {
    match (state.is_connected(), state.last_success().is_some()) {
        (true, _) => "connected",
        (false, true) => "reconnecting",
        (false, false) => "never_connected",
    }
}

fn message(state: &SourceState, summary: Summary, connection: &str) -> String {
    match (connection, state.last_error()) {
        ("connected", _) => format!(
            "{} is answering; {} faults affecting {} customers, {} planned, {} restored.",
            state.name(),
            summary.fault,
            summary.customers,
            summary.planned,
            summary.restored,
        ),
        (_, Some(error)) => format!("{} is not answering: {error}", state.name()),
        _ => format!("{} has not answered yet.", state.name()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn state() -> SourceState {
        SourceState::new("ESB PowerCheck", Duration::from_secs(300))
    }

    fn summary() -> Summary {
        Summary {
            fault: 42,
            planned: 12,
            restored: 5,
            other: 0,
            customers: 3100,
        }
    }

    #[test]
    fn a_source_that_has_never_answered_is_unhealthy_and_says_why() {
        let mut state = state();
        state.failed("refused the subscription key");

        let beat = heartbeat(
            "powercheck",
            &state,
            FeedCounters::default(),
            Summary::default(),
        );

        assert_eq!(beat.state, ServiceState::Unhealthy);
        assert_eq!(beat.metrics["source"]["connection"], "never_connected");
        assert!(
            beat.message
                .is_some_and(|message| message.contains("subscription key"))
        );
    }

    #[test]
    fn an_answering_source_is_healthy_and_says_what_is_on_the_map() {
        let mut state = state();
        state.succeeded();

        let beat = heartbeat("powercheck", &state, FeedCounters::default(), summary());

        assert_eq!(beat.state, ServiceState::Healthy);
        assert_eq!(beat.metrics["outages"]["fault"], 42);
        assert_eq!(beat.metrics["outages"]["customers"], 3100);
        assert!(
            beat.message
                .is_some_and(|message| message.contains("42 faults"))
        );
    }

    #[test]
    fn a_source_that_has_only_just_stopped_is_still_healthy() {
        let mut state = state();
        state.succeeded();
        state.failed("timed out");

        assert_eq!(service_state(&state), ServiceState::Healthy);
    }
}

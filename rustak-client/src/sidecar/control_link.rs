//! The sidecar's end of the control API: registration, heartbeats, and the
//! server-event feed.
//!
//! [`Link`](super::Link) is the same idea for the CoT stream, and the two are
//! deliberately parallel — the harness holds one of each and selects on both.
//! What differs is what "keeping it up" means: the stream's reconnection lives
//! in [`Reconnecting`](crate::stream::Reconnecting), and the feed's lives in the
//! task this module spawns.
//!
//! # Why the feed is a task and a channel rather than a stream
//!
//! The harness loop is a `tokio::select!`, and a branch of one may be cancelled
//! every time another branch wins. Opening an HTTP response is not cancel-safe —
//! a request dropped half way through its handshake is a request that never
//! happened, and a sidecar whose ticks are frequent enough would never finish
//! opening the feed. A `mpsc::Receiver::recv` *is* cancel-safe, so the opening,
//! the reading and the backoff happen on a task of their own and the loop only
//! ever waits on the channel.
//!
//! # Nothing here stops the sidecar
//!
//! A control API that refuses a registration, loses a heartbeat or drops the
//! feed is logged and retried. The plugin's actual job is the CoT it publishes,
//! and a server that cannot take a heartbeat right now is not a reason to stop
//! doing it — the server notices the missing heartbeats on its own, which is
//! what `service.status` and the sweep are for.
//!
//! # The plugin's heartbeat is the heartbeat
//!
//! The server stores the last heartbeat it was given — state, message and
//! metrics, wholesale — so only one of them can be the one an administrator
//! sees. [`ControlLink::report`] is where that is decided: what
//! [`Sidecar::health`](super::Sidecar::health) answered, or nothing at all when
//! the plugin has already called [`ControlClient::heartbeat`] itself during the
//! tick, and only failing both of those the harness's own `healthy()`.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rustak_api::Heartbeat;
use rustak_core::prelude::*;
use rustak_core::service::ServiceDescriptor;
use tokio::sync::mpsc;

use crate::control::{ControlClient, ServerEvent};

use super::SidecarContext;

/// How deep the channel between the feed task and the harness loop is.
///
/// Small: the harness takes an event, hands it to the plugin and comes back. A
/// deep queue here would only hide a plugin whose `on_event` is too slow, and
/// hide it as a growing delay rather than as a dropped event.
const FEED_QUEUE: usize = 32;

/// How long the feed task waits before reopening a feed that ended.
const RETRY_MIN: Duration = Duration::from_secs(1);

/// The longest it waits, however many times it has failed.
const RETRY_MAX: Duration = Duration::from_secs(60);

/// The sidecar's control-API connection, and what the harness does with it.
pub(crate) struct ControlLink {
    /// [`None`] for a sidecar with no `[server] control`, which makes every
    /// method here a no-op and [`next`](Self::next) a future that never
    /// resolves.
    control: Option<Arc<ControlClient>>,

    /// What this sidecar publishes about itself, re-sent whenever the server
    /// says it has forgotten.
    descriptor: ServiceDescriptor,

    /// Server events, from the task that keeps the feed open.
    events: Option<mpsc::Receiver<ServerEvent>>,
}

impl ControlLink {
    /// Builds the link a context describes, and starts the feed task.
    pub(crate) fn open<S>(context: &SidecarContext<S>) -> Self {
        let Some(control) = context.control.clone() else {
            tracing::debug!(
                "This sidecar has no [server] control, so it will not register or report health.",
            );

            return Self {
                control: None,
                descriptor: context.descriptor().clone(),
                events: None,
            };
        };

        let (sender, receiver) = mpsc::channel(FEED_QUEUE);
        tokio::spawn(feed(
            Arc::clone(&control),
            sender,
            context.shutdown().clone(),
        ));

        Self {
            control: Some(control),
            descriptor: context.descriptor().clone(),
            events: Some(receiver),
        }
    }

    /// Registers this sidecar, logging a refusal rather than returning it.
    ///
    /// Called once at start-up and again whenever a heartbeat finds the
    /// registration has gone.
    pub(crate) async fn register(&self) {
        let Some(control) = &self.control else {
            return;
        };

        match control.register(&self.descriptor).await {
            Ok(summary) => tracing::info!(
                service = %summary.descriptor.name,
                "Registered with the server.",
            ),
            Err(err) => tracing::warn!(
                error = %err,
                "Could not register with the server; the sidecar is running anyway.",
            ),
        }
    }

    /// Reports the heartbeat this tick should carry, unless the plugin has
    /// already reported one of its own.
    ///
    /// `beat` is whatever [`Sidecar::health`](super::Sidecar::health) answered,
    /// and [`None`] is the default hook — a plugin with nothing in particular to
    /// say, which the harness reports as [`Heartbeat::healthy`].
    ///
    /// Nothing at all is sent when the plugin called
    /// [`ControlClient::heartbeat`] itself during this tick. The server keeps
    /// the last heartbeat it was given, so a harness that always sent its own
    /// would overwrite the plugin's a moment after it landed — which is the
    /// whole reason [`health`](super::Sidecar::health) exists.
    pub(crate) async fn report(&self, beat: Option<Heartbeat>) {
        let Some(control) = &self.control else {
            return;
        };

        if control.take_reported() {
            tracing::debug!(
                "The plugin reported its own health this tick; the harness will not talk over it.",
            );

            return;
        }

        self.heartbeat(&beat.unwrap_or_else(Heartbeat::healthy))
            .await;
    }

    /// Reports a heartbeat, and re-registers if the server has forgotten us.
    pub(crate) async fn heartbeat(&self, beat: &Heartbeat) {
        let Some(control) = &self.control else {
            return;
        };

        match control.post_heartbeat(beat).await {
            Ok(Some(status)) => tracing::debug!(state = status.state.as_str(), "Reported health."),
            Ok(None) => {
                tracing::info!("The server has no registration for this sidecar; registering.");
                self.register().await;
            }
            Err(err) => tracing::warn!(error = %err, "Could not report a heartbeat."),
        }
    }

    /// The next server event, or a future that never resolves for a sidecar
    /// with no feed.
    ///
    /// Cancel-safe: the whole reason the feed is a task and a channel.
    pub(crate) async fn next(&mut self) -> Option<ServerEvent> {
        match &mut self.events {
            Some(events) => events.recv().await,
            None => std::future::pending().await,
        }
    }
}

impl std::fmt::Debug for ControlLink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlLink")
            .field("service", &self.descriptor.name)
            .field("configured", &self.control.is_some())
            .finish_non_exhaustive()
    }
}

/// Keeps the server-event feed open until the sidecar stops.
///
/// Resumes from the last event it delivered, so a feed that dropped for a second
/// costs nothing; one that dropped for longer than the server's ring costs a gap
/// in the ids, which is what a plugin that keeps state watches for.
async fn feed(control: Arc<ControlClient>, sender: mpsc::Sender<ServerEvent>, shutdown: Shutdown) {
    let mut after: Option<u64> = None;
    let mut wait = RETRY_MIN;

    while !shutdown.is_cancelled() {
        match control.events(after).await {
            Ok(mut stream) => {
                tracing::info!("The server-event feed is open.");
                wait = RETRY_MIN;

                loop {
                    let event = tokio::select! {
                        biased;

                        () = shutdown.cancelled() => return,
                        event = stream.next() => event,
                    };

                    let Some(event) = event else { break };

                    after = Some(event.id);

                    // A closed receiver is the harness stopping, not a failure.
                    if sender.send(event).await.is_err() {
                        return;
                    }
                }

                tracing::debug!("The server-event feed ended; it will be reopened.");
            }
            Err(err) => tracing::warn!(error = %err, "Could not open the server-event feed."),
        }

        tokio::select! {
            biased;

            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }

        wait = (wait * 2).min(RETRY_MAX);
    }
}

#[cfg(test)]
mod tests {
    use rustak_core::config;

    use super::*;
    use crate::sidecar::{NoSettings, SidecarConfig};

    fn context(toml: &str) -> SidecarContext<NoSettings> {
        let config: SidecarConfig<NoSettings> = config::load_str(toml).unwrap();

        SidecarContext::from_config(config, "1.2.3", Shutdown::new()).unwrap()
    }

    #[tokio::test]
    async fn a_sidecar_with_no_control_endpoint_registers_nothing_and_waits_forever() {
        // The offline case that makes `--check` and an offline unit test work:
        // every method is a no-op and the feed is a branch that never fires.
        let mut link = ControlLink::open(&context(
            r#"
            [service]
            name = "example"
            "#,
        ));

        link.register().await;
        link.heartbeat(&Heartbeat::healthy()).await;

        let waited = tokio::time::timeout(Duration::from_millis(50), link.next()).await;

        assert!(waited.is_err(), "the feed branch must never be ready");
        assert!(format!("{link:?}").contains("configured: false"));
    }

    #[tokio::test]
    async fn a_refused_registration_is_logged_rather_than_returned() {
        // A control API that is down must not stop a sidecar publishing CoT.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let link = ControlLink::open(&context(&format!(
            r#"
            [service]
            name = "example"

            [server]
            control = "{}"
            "#,
            server.uri()
        )));

        // Returns `()`: there is no failure for the harness to act on.
        link.register().await;
        link.heartbeat(&Heartbeat::healthy()).await;
    }

    /// A control API that takes any heartbeat, and remembers what it was sent.
    async fn recording() -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path_regex(r".*/heartbeat$"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "state": "healthy" })),
            )
            .mount(&server)
            .await;

        server
    }

    /// Every heartbeat body the server was sent, in order.
    async fn beats(server: &wiremock::MockServer) -> Vec<Heartbeat> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|request| request.url.path().ends_with("/heartbeat"))
            .map(|request| request.body_json::<Heartbeat>().expect("a heartbeat body"))
            .collect()
    }

    #[tokio::test]
    async fn a_plugin_with_nothing_to_say_is_reported_healthy() {
        let server = recording().await;
        let link = ControlLink::open(&context(&format!(
            r#"
            [service]
            name = "example"

            [server]
            control = "{}"
            "#,
            server.uri()
        )));

        link.report(None).await;

        assert_eq!(beats(&server).await, vec![Heartbeat::healthy()]);
    }

    #[tokio::test]
    async fn what_the_health_hook_answered_is_what_goes_out() {
        // The whole point: the plugin's state, message and metrics reach the
        // server *instead of* the harness's `healthy()`, not before it.
        let server = recording().await;
        let link = ControlLink::open(&context(&format!(
            r#"
            [service]
            name = "example"

            [server]
            control = "{}"
            "#,
            server.uri()
        )));
        let reported = Heartbeat {
            state: rustak_api::ServiceState::Degraded,
            message: Some("The upstream has not answered for four minutes.".into()),
            metrics: serde_json::json!({ "tracked": 612 }),
        };

        link.report(Some(reported.clone())).await;

        assert_eq!(beats(&server).await, vec![reported]);
    }

    #[tokio::test]
    async fn a_plugin_that_reported_for_itself_is_not_talked_over() {
        // The escape hatch: a plugin calling `control.heartbeat` during its tick
        // claims the tick, and the harness sends nothing on top of it. The
        // server keeps the last heartbeat it was given, so a second one here
        // would be the plugin's report lost a millisecond after it landed.
        let server = recording().await;
        let context = context(&format!(
            r#"
            [service]
            name = "example"

            [server]
            control = "{}"
            "#,
            server.uri()
        ));
        let link = ControlLink::open(&context);
        let reported = Heartbeat {
            state: rustak_api::ServiceState::Unhealthy,
            message: Some("The receiver is not answering.".into()),
            metrics: serde_json::Value::Null,
        };

        context
            .control()
            .expect("a control client")
            .heartbeat(&reported)
            .await
            .expect("the mock takes it");
        link.report(None).await;

        assert_eq!(
            beats(&server).await,
            vec![reported],
            "the harness must not have added one of its own",
        );

        // And the next tick is reported normally: claiming a tick claims one.
        link.report(None).await;

        assert_eq!(beats(&server).await.len(), 2);
        assert_eq!(beats(&server).await[1], Heartbeat::healthy());
    }

    #[tokio::test]
    async fn events_from_the_feed_reach_the_harness() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/v1/events"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                "id: 1\nevent: channel.changed\ndata: {\"id\":1,\"at\":\"2026-09-18T12:00:00.000Z\",\"type\":\"channel.changed\",\"username\":\"ada\"}\n\n",
            ))
            .mount(&server)
            .await;
        let mut link = ControlLink::open(&context(&format!(
            r#"
            [service]
            name = "example"

            [server]
            control = "{}"
            "#,
            server.uri()
        )));

        let event = tokio::time::timeout(Duration::from_secs(5), link.next())
            .await
            .expect("the feed delivers within the timeout")
            .expect("an event");

        assert_eq!(event.id, 1);
        assert_eq!(event.name(), "channel.changed");
    }
}

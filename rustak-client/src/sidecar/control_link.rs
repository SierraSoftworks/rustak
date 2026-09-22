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
//! # …and it says so once, not once a tick
//!
//! Everything that calls the control API from here goes through
//! [`LinkHealth`]: an outage is one warning with the cause chain, then `debug`
//! until it changes, a reminder every five minutes, and one line when it comes
//! back. Attempts are backed off rather than repeated every tick, and a
//! heartbeat that is due into a link known to be down is skipped rather than
//! sent and logged. A refusal is not an outage — the server answered — so it is
//! throttled but holds nothing else back. See
//! [`link_health`](super::link_health) for the numbers the first live
//! deployment produced without any of this.
//!
//! # The credential can change under it
//!
//! A sidecar under an orchestrator has no `[service] token`: it buys an access
//! token with the workload identity its task already holds, and that token
//! expires. So every call here puts a live one in place first
//! ([`ControlClient::set_credential`]), and a `401` — which is what a rotated
//! signing key or a revoked token looks like — costs one fresh exchange and one
//! retry rather than an hour of silence.
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

use rustak_api::Heartbeat;
use rustak_core::prelude::*;
use rustak_core::service::ServiceDescriptor;
use tokio::sync::mpsc;

use crate::control::{ControlClient, ServerEvent};

use super::SidecarContext;
use super::link_health::{LinkHealth, humanised, note, recovered};
use super::workload::AccessTokens;

/// How deep the channel between the feed task and the harness loop is.
///
/// Small: the harness takes an event, hands it to the plugin and comes back. A
/// deep queue here would only hide a plugin whose `on_event` is too slow, and
/// hide it as a growing delay rather than as a dropped event.
const FEED_QUEUE: usize = 32;

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

    /// The exchange that buys the control-API token, for a sidecar whose
    /// credential is its orchestrator's identity rather than a secret from a
    /// file.
    workload: Option<Arc<AccessTokens>>,

    /// Whether the link is up, shared with the feed task: one link, one answer,
    /// and one log line when it changes.
    health: Arc<LinkHealth>,
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
                workload: None,
                health: Arc::new(LinkHealth::new()),
            };
        };

        let health = Arc::new(LinkHealth::new());
        let (sender, receiver) = mpsc::channel(FEED_QUEUE);
        tokio::spawn(super::event_feed::run(
            Arc::clone(&control),
            context.workload.clone(),
            Arc::clone(&health),
            sender,
            context.shutdown().clone(),
        ));

        Self {
            control: Some(control),
            descriptor: context.descriptor().clone(),
            events: Some(receiver),
            workload: context.workload.clone(),
            health,
        }
    }

    /// Puts a live access token in place before a call goes out, answering
    /// whether the call is worth making.
    ///
    /// A no-op — and a `true` — for a sidecar whose credential is a
    /// `[service] token`: there is nothing to exchange and nothing to expire.
    /// A failed exchange is a failure *of the link*, and the call it was for is
    /// abandoned rather than sent with a credential we know is missing: two
    /// failures for one cause is exactly the noise this is here to stop.
    async fn ensure_credential(&self) -> bool {
        let (Some(control), Some(tokens)) = (&self.control, &self.workload) else {
            return true;
        };

        match tokens.current().await {
            Ok(token) => {
                control.set_credential(Some(token));

                true
            }
            Err(err) => {
                self.note(
                    "exchange this sidecar's workload identity for an access token",
                    &err,
                );

                false
            }
        }
    }

    /// Whether the last call was refused as unauthenticated, and a fresh token
    /// has been put in place to try once more with.
    ///
    /// The cached access token lasts an hour; waiting for it to expire after a
    /// signing key rotated would be an hour of a sidecar that looks alive and
    /// reports nothing.
    async fn refreshed_after_refusal(&self) -> bool {
        let (Some(control), Some(tokens)) = (&self.control, &self.workload) else {
            return false;
        };

        if !control.take_unauthorized() {
            return false;
        }

        tracing::info!(
            "The server refused this sidecar's access token; exchanging its workload identity again.",
        );
        tokens.invalidate();

        self.ensure_credential().await
    }

    /// Registers this sidecar, logging a refusal rather than returning it.
    ///
    /// Called once at start-up and again whenever a heartbeat finds the
    /// registration has gone.
    pub(crate) async fn register(&self) {
        let Some(control) = &self.control else {
            return;
        };

        if !self.due("register") {
            return;
        }

        if !self.ensure_credential().await {
            return;
        }

        let mut outcome = control.register(&self.descriptor).await;

        if outcome.is_err() && self.refreshed_after_refusal().await {
            outcome = control.register(&self.descriptor).await;
        }

        match outcome {
            Ok(summary) => {
                recovered(self.health.succeeded());
                tracing::info!(
                    service = %summary.descriptor.name,
                    "Registered with the server.",
                );
            }
            Err(err) => self.note("register with the server", &err),
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

        if !self.due("report a heartbeat") {
            return;
        }

        if !self.ensure_credential().await {
            return;
        }

        let mut outcome = control.post_heartbeat(beat).await;

        if outcome.is_err() && self.refreshed_after_refusal().await {
            outcome = control.post_heartbeat(beat).await;
        }

        match outcome {
            Ok(Some(status)) => {
                recovered(self.health.succeeded());
                tracing::debug!(state = status.state.as_str(), "Reported health.");
            }
            Ok(None) => {
                recovered(self.health.succeeded());
                tracing::info!("The server has no registration for this sidecar; registering.");
                self.register().await;
            }
            Err(err) => self.note("report a heartbeat", &err),
        }
    }

    /// Records a failed call and says as much about it as its state calls for.
    ///
    /// See [`link_health::note`](super::link_health::note) for the rule: only a
    /// server we could not reach is an outage, and a refusal — which is the
    /// server answering — leaves everything else free to carry on.
    fn note(&self, what: &str, err: &Error) {
        note(&self.health, what, err);
    }

    /// Whether a call is worth making, given what the link last did.
    ///
    /// While the link is down the answer is `false` until the backoff has run
    /// out, and the tick that finds it still down says so at `debug` — which is
    /// how a heartbeat every thirty seconds stops being a request every thirty
    /// seconds into a server that is not answering. The attempt that *is*
    /// allowed through is what finds out that the link is back.
    fn due(&self, what: &str) -> bool {
        if self.health.due() {
            return true;
        }

        tracing::debug!(
            retry_in = %humanised(self.health.backoff()),
            "The control link is down; not trying to {what} yet.",
        );

        false
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
            .field("down", &self.health.is_down())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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

        // And the link is *up*: the server answered, which is what matters for
        // whether anything else is worth trying. A 503 on one route must not
        // stop a heartbeat going to another.
        assert!(format!("{link:?}").contains("down: false"), "{link:?}");
    }

    #[tokio::test]
    async fn a_server_that_cannot_be_reached_at_all_takes_the_link_down() {
        // Port 1 refuses the connection, which is a transport failure — the
        // shape a TLS handshake failure has, and the one the first live
        // deployment produced 428 log lines from. From here the harness backs
        // off and skips heartbeats instead of sending one per tick.
        let link = ControlLink::open(&context(
            r#"
            [service]
            name = "example"

            [server]
            control = "http://127.0.0.1:1"
            "#,
        ));

        link.heartbeat(&Heartbeat::healthy()).await;

        assert!(format!("{link:?}").contains("down: true"), "{link:?}");

        // Still no failure for the harness to act on, and the calls inside the
        // backoff are skipped rather than sent.
        link.heartbeat(&Heartbeat::healthy()).await;
        link.register().await;

        assert!(format!("{link:?}").contains("down: true"), "{link:?}");
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

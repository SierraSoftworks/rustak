//! The task that keeps the server-event feed open, and what it says while it
//! does.
//!
//! Split out of [`control_link`](super::control_link) because the two halves
//! answer different questions. That module is "what does the harness do with
//! the control API"; this one is "what does one long-lived response body cost
//! an operator to read".
//!
//! # The feed is a body, not a request
//!
//! `GET /api/v1/events` answers a `text/event-stream` that is meant to stay
//! open for hours. The first live deployment read it through the client the
//! heartbeat uses, which carries `reqwest`'s **total** timeout of thirty
//! seconds — so every feed was cut at thirty seconds, reopened, and announced
//! itself again. Two idle sidecars produced 74 server log lines in two and a
//! half minutes without a single thing being wrong.
//!
//! The client is now [`http::feed_client`](crate::http::feed_client): a connect
//! timeout, an idle (read) timeout that the server's keep-alive comments reset,
//! and no total deadline at all.
//!
//! # And a reopening is not news
//!
//! A feed that closes cleanly and reopens cleanly is the ordinary course of
//! things — a server restart, a token that expired under it, a proxy recycling
//! a connection — so it is `debug`. `info` is kept for the first open and for
//! the one that ends an outage [`LinkHealth`] announced, and
//! [`Reopenings`] counts the rest into a single `warn` when there are enough of
//! them in five minutes to mean something is cutting the feed.
//!
//! # The token is bought once, not once per opening
//!
//! [`AccessTokens`] caches the access token until a minute before it expires,
//! so reopening the feed costs one request rather than two. The exchange is
//! still attempted *before* each opening, because a feed held open for hours
//! outlives the token that opened it and a reopening has to carry a live one.

use std::sync::Arc;

use chrono::Utc;
use futures::StreamExt;
use rustak_core::prelude::*;
use tokio::sync::mpsc;

use crate::control::{ControlClient, ServerEvent};

use super::link_health::{LinkHealth, Opened, Reopenings, Report, humanised, note, recovered};
use super::workload::AccessTokens;

/// Keeps the server-event feed open until the sidecar stops.
///
/// Resumes from the last event it delivered, so a feed that dropped for a second
/// costs nothing; one that dropped for longer than the server's ring costs a gap
/// in the ids, which is what a plugin that keeps state watches for.
pub(crate) async fn run(
    control: Arc<ControlClient>,
    workload: Option<Arc<AccessTokens>>,
    health: Arc<LinkHealth>,
    sender: mpsc::Sender<ServerEvent>,
    shutdown: Shutdown,
) {
    let mut after: Option<u64> = None;
    let mut reopenings = Reopenings::new();

    while !shutdown.is_cancelled() {
        // The feed is held open for hours, so the token it opened with may have
        // expired by the time it drops and is reopened. Asking here rather than
        // once at start-up is what keeps a reopened feed working; the cache in
        // `AccessTokens` is what keeps it from being a request every time.
        let exchanged = match &workload {
            None => true,
            Some(tokens) => match tokens
                .for_call(
                    &health,
                    "exchange this sidecar's workload identity for the event feed",
                )
                .await
            {
                Some(token) => {
                    control.set_credential(Some(token));

                    true
                }
                None => false,
            },
        };

        // Opening the feed with a credential we know is missing would be a
        // second failure for one cause, and a second log line for it.
        if exchanged {
            match control.events(after).await {
                Ok(mut stream) => {
                    let report = health.succeeded();
                    recovered(report);
                    announce_open(&mut reopenings, report);

                    loop {
                        let event = tokio::select! {
                            biased;

                            () = shutdown.cancelled() => return,
                            event = stream.next() => event,
                        };

                        let Some(event) = event else { break };

                        after = Some(event.id);

                        // A closed receiver is the harness stopping, not a
                        // failure.
                        if sender.send(event).await.is_err() {
                            return;
                        }
                    }

                    reopenings.closed(Utc::now());
                    tracing::debug!("The server-event feed ended; it will be reopened.");
                }
                Err(err) => {
                    // A refusal or an outage is `LinkHealth`'s to announce, and
                    // the opening that follows it is a reopening either way.
                    reopenings.closed(Utc::now());
                    note(&health, "open the server-event feed", &err);
                }
            }
        }

        // The shared backoff: the heartbeat that failed a moment ago moved this
        // on too, so one outage is one sequence of attempts rather than two.
        // A credential the server is refusing has a longer wait of its own, and
        // reopening the feed cannot help until that has run out either.
        let wait = wait_for(&health, workload.as_deref());

        tokio::select! {
            biased;

            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

/// How long to wait before trying again.
///
/// The longer of the two waits: the link's, for a server that cannot be
/// reached, and the credential's, for one that will not take what we have.
/// Waiting the shorter of them would be a request we already know the answer
/// to.
fn wait_for(health: &LinkHealth, workload: Option<&AccessTokens>) -> std::time::Duration {
    let link = health.backoff();

    match workload {
        Some(tokens) => link.max(tokens.retry_in()),
        None => link,
    }
}

/// Says as much about a successful opening as its history calls for.
fn announce_open(reopenings: &mut Reopenings, report: Report) {
    let recovered = matches!(report, Report::Recovered { .. });

    match reopenings.opened(recovered, Utc::now()) {
        Opened::Announce => tracing::info!("The server-event feed is open."),
        Opened::Quiet => tracing::debug!("The server-event feed is open again."),
        Opened::Churning { closes, within } => tracing::warn!(
            closes,
            "The server-event feed has closed and reopened {closes} times in the last {}; something between this sidecar and [server] control is cutting it.",
            humanised(within),
        ),
    }
}

#[cfg(test)]
mod tests {
    use rustak_core::identity::ServiceName;
    use rustak_core::service::ServiceIdentity;

    use super::*;

    /// A feed task against `uri`, with a channel to read what it delivers.
    fn started(uri: &str, shutdown: &Shutdown) -> mpsc::Receiver<ServerEvent> {
        let identity = ServiceIdentity::new(ServiceName::parse("weather").unwrap());
        let control = Arc::new(
            ControlClient::with_http(uri, reqwest::Client::new(), &identity).expect("a client"),
        );
        let (sender, receiver) = mpsc::channel(8);

        tokio::spawn(run(
            control,
            None,
            Arc::new(LinkHealth::new()),
            sender,
            shutdown.clone(),
        ));

        receiver
    }

    #[tokio::test]
    async fn a_feed_that_closes_is_reopened_from_the_last_event_it_delivered() {
        // The resume contract, through the task rather than through the stream:
        // the second opening has to carry `Last-Event-ID`, or a feed that was
        // cut for a second loses whatever happened during it.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/api/v1/events"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_string(
                "id: 4\nevent: channel.changed\ndata: {\"id\":4,\"at\":\"2026-09-22T00:00:00.000Z\",\"type\":\"channel.changed\",\"username\":\"ada\"}\n\n",
            ))
            .mount(&server)
            .await;

        let shutdown = Shutdown::new();
        let mut events = started(&server.uri(), &shutdown);

        let first = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .expect("the feed delivers")
            .expect("an event");
        assert_eq!(first.id, 4);

        // The body ended, so the task reopens; the resume header is what says
        // it did not start again from nothing.
        let resumed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let requests = server.received_requests().await.unwrap_or_default();

                if requests
                    .iter()
                    .any(|request| request.headers.contains_key("last-event-id"))
                {
                    return;
                }

                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await;

        shutdown.cancel();

        assert!(resumed.is_ok(), "the feed should reopen from id 4");
    }
}

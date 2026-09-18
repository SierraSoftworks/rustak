//! `GET /api/v1/events`: the server-event feed, as Server-Sent Events.
//!
//! One long-lived `text/event-stream` response carrying what
//! [`plugins::events`](crate::plugins::events) publishes. A sidecar consumes it
//! through `rustak_client::control::events`; a browser could consume the same
//! bytes with `EventSource`, which is why each frame names itself in the SSE
//! `event:` field as well as in the body.
//!
//! # Resuming
//!
//! A client that reconnects sends `Last-Event-ID` (or `?lastEventId=`, because
//! `EventSource` cannot set a header on its first connection) and is given what
//! it missed from the bus's ring before the live feed starts. Subscribing
//! happens *before* the backlog is read, so an event published in between is
//! replayed rather than lost — and the ids are checked on the way out, so it is
//! not delivered twice.
//!
//! # What a stalled reader costs
//!
//! Nothing but its own connection. The bus is a `broadcast`, so a reader that
//! stops reading eventually lags, and lagging is handled here by refilling from
//! the ring rather than by ending the response: a plugin that was busy for a
//! second sees every event it can still be given and a gap in the ids where it
//! cannot.
//!
//! # Who may read it
//!
//! Administrators and services. The feed says which devices are on the stream
//! and which packages arrived, across every channel, so it is not something an
//! ordinary account is given — and a service is already trusted with the same
//! view through its own subscription.

use std::collections::VecDeque;

use actix_web::http::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use actix_web::web::Bytes;
use actix_web::{HttpRequest, HttpResponse, web};
use futures::Stream;
use rustak_api::event::ServerEvent;
use tokio::sync::broadcast::error::RecvError;

use crate::plugins::{ServerEvents, auth};
use crate::prelude::*;

use super::error::{ApiError, ApiResult};

/// The exact content type an SSE response carries.
const EVENT_STREAM: HeaderValue = HeaderValue::from_static("text/event-stream");

/// `Cache-Control` for a response that must not be buffered or replayed.
const NO_STORE: HeaderValue = HeaderValue::from_static("no-cache, no-store, must-revalidate");

/// Tells nginx and friends not to buffer the response, which would otherwise
/// hold every frame until the connection closed.
const NO_BUFFERING: HeaderValue = HeaderValue::from_static("no");

/// How long a consumer should wait before reconnecting, sent once as `retry:`.
const RETRY_MS: u64 = 5_000;

/// How often a comment frame is written into an idle stream.
///
/// Without one, an idle feed is indistinguishable from a dead connection to
/// every proxy between us and the plugin, and the first thing a plugin would
/// learn about a silent hour is a reset.
const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(20);

/// Registers the feed.
pub fn routes(config: &mut web::ServiceConfig) {
    config.route("/events", web::get().to(feed));
}

/// `GET /api/v1/events`.
///
/// # Errors
///
/// A `401` without a usable credential and a `403` for an account that is
/// neither an administrator nor a service.
pub async fn feed(context: web::Data<AppContext>, request: HttpRequest) -> ApiResult {
    let caller = auth::caller(&context, &request)
        .await
        .map_err(super::services::refusal)?;

    if !caller.is_admin() && !caller.is_service() {
        return Err(ApiError::forbidden(
            "The server-event feed is for administrators and services.",
        ));
    }

    let events = context.events().clone();
    // Subscribed before the backlog is read, so nothing published in between is
    // missed; `seen` is what keeps it from being sent twice.
    let receiver = events.subscribe();
    let backlog = match resume_from(&request) {
        Some(id) => events.since(id),
        None => Vec::new(),
    };

    info!(
        caller = %caller.username(),
        resuming = backlog.len(),
        "A consumer opened the server-event feed."
    );

    Ok(HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, EVENT_STREAM))
        .insert_header((CACHE_CONTROL, NO_STORE))
        .insert_header(("X-Accel-Buffering", NO_BUFFERING))
        .streaming(frames(Feed {
            seen: 0,
            backlog: backlog.into(),
            receiver,
            events,
            shutdown: context.shutdown().clone(),
            opened: false,
        })))
}

/// What one open feed is in the middle of.
struct Feed {
    /// The highest id already written, so a backlog entry the live channel also
    /// carries is written once.
    seen: u64,
    /// What is still owed from the resume, oldest first.
    backlog: VecDeque<ServerEvent>,
    receiver: tokio::sync::broadcast::Receiver<ServerEvent>,
    events: ServerEvents,
    shutdown: Shutdown,
    /// Whether the `retry:` preamble has been written.
    opened: bool,
}

/// The response body: a preamble, the backlog, then the live feed.
fn frames(feed: Feed) -> impl Stream<Item = Result<Bytes, actix_web::Error>> {
    futures::stream::unfold(feed, |mut feed| async move {
        if !feed.opened {
            feed.opened = true;

            return Some((Ok(Bytes::from(format!("retry: {RETRY_MS}\n\n"))), feed));
        }

        // Built per frame, so the comment is written a keepalive period after
        // the *last* thing written rather than on a fixed cadence: a busy feed
        // never carries one, and an idle one carries exactly what a proxy needs.
        let mut keepalive =
            tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);

        loop {
            if let Some(event) = feed.backlog.pop_front() {
                feed.seen = feed.seen.max(event.id);

                return Some((Ok(frame(&event)), feed));
            }

            tokio::select! {
                biased;

                () = feed.shutdown.cancelled() => return None,
                received = feed.receiver.recv() => match received {
                    Ok(event) if event.id <= feed.seen => continue,
                    Ok(event) => {
                        feed.seen = event.id;

                        return Some((Ok(frame(&event)), feed));
                    }
                    // Too far behind. Refill from the ring rather than ending
                    // the response: what can still be delivered is delivered,
                    // and the gap in the ids is what says the rest cannot.
                    Err(RecvError::Lagged(missed)) => {
                        warn!(missed, "A server-event consumer fell behind.");
                        feed.backlog = feed.events.since(feed.seen).into();
                    }
                    Err(RecvError::Closed) => return None,
                },
                _ = keepalive.tick() => return Some((Ok(Bytes::from_static(b": keep-alive\n\n")), feed)),
            }
        }
    })
}

/// One event, as the SSE wire format spells it.
///
/// The name is written into the `event:` field *and* left in the body, so that a
/// browser's `EventSource` can dispatch on it without parsing and a Rust client
/// can parse without carrying the frame's fields alongside the payload.
fn frame(event: &ServerEvent) -> Bytes {
    let data = serde_json::to_string(event).unwrap_or_else(|err| {
        error!(error = %err, "Could not render a server event.");

        String::from("{}")
    });

    Bytes::from(format!(
        "id: {}\nevent: {}\ndata: {data}\n\n",
        event.id,
        event.name()
    ))
}

/// The id a reconnecting consumer last saw.
///
/// The header is what the SSE specification says a client resends; the query
/// parameter is for `EventSource`, which cannot set a header on its first
/// connection and has nowhere else to put one. A value that is not a number is
/// ignored rather than refused: it costs the consumer its backlog, and refusing
/// would cost it the feed.
fn resume_from(request: &HttpRequest) -> Option<u64> {
    let header = request
        .headers()
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);

    let query = || {
        request
            .query_string()
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| key.eq_ignore_ascii_case("lastEventId"))
            .map(|(_, value)| value.to_string())
    };

    header.or_else(query)?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::event::{ChannelEvent, ServerEventPayload};

    use super::*;
    use crate::testing::TestServer;

    fn channel(context: &AppContext, username: &str) {
        context
            .events()
            .publish(ServerEventPayload::ChannelChanged(ChannelEvent {
                username: username.to_string(),
            }));
    }

    macro_rules! app {
        ($server:expr) => {
            test::init_service(App::new().configure($server.app())).await
        };
    }

    #[actix_web::test]
    async fn the_feed_is_not_open_to_an_ordinary_account() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;
        let app = app!(server);

        let refused = test::TestRequest::get()
            .uri("/api/v1/events")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .send_request(&app)
            .await;

        assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn it_answers_nothing_without_a_credential() {
        let server = TestServer::start().await;
        let app = app!(server);

        let refused = test::TestRequest::get()
            .uri("/api/v1/events")
            .send_request(&app)
            .await;

        assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn an_administrator_resumes_from_the_last_event_they_saw() {
        // The whole of `Last-Event-ID`: what the consumer missed arrives before
        // the live feed does, in order, and nothing before it does.
        let server = TestServer::start().await;
        for name in ["first", "second", "third"] {
            channel(&server.context, name);
        }
        let (_, session) = server.signed_in("ada", true).await;
        let app = app!(server);

        let response = test::TestRequest::get()
            .uri("/api/v1/events")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .insert_header(("last-event-id", "1"))
            .send_request(&app)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("text/event-stream"),
        );

        // The shutdown ends the response, so the body is the backlog and stops.
        server.context.shutdown().cancel();
        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

        assert!(body.starts_with("retry: 5000\n\n"), "{body}");
        assert!(body.contains("id: 2\nevent: channel.changed\n"), "{body}");
        assert!(body.contains("id: 3\n"), "{body}");
        assert!(!body.contains("\"first\""), "{body}");
    }

    #[actix_web::test]
    async fn the_query_parameter_resumes_the_same_way_the_header_does() {
        // `EventSource` cannot set a header on its first connection.
        let server = TestServer::start().await;
        channel(&server.context, "first");
        channel(&server.context, "second");
        let (_, session) = server.signed_in("ada", true).await;
        let app = app!(server);

        let response = test::TestRequest::get()
            .uri("/api/v1/events?lastEventId=1")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .send_request(&app)
            .await;

        server.context.shutdown().cancel();
        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

        assert!(body.contains("id: 2\n"), "{body}");
        assert!(!body.contains("\"first\""), "{body}");
    }

    #[actix_web::test]
    async fn a_consumer_that_asks_for_nothing_gets_only_what_happens_next() {
        let server = TestServer::start().await;
        channel(&server.context, "before");
        let (_, session) = server.signed_in("ada", true).await;
        let app = app!(server);

        let response = test::TestRequest::get()
            .uri("/api/v1/events")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .send_request(&app)
            .await;

        server.context.shutdown().cancel();
        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

        assert_eq!(body, "retry: 5000\n\n", "{body}");
    }
}

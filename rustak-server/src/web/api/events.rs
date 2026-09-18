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
//! Administrators and services — and then only what each of them may see. A
//! service account is an *ordinary* account, normally with no channel
//! memberships at all, so "may open the feed" and "may be shown this event" are
//! two different questions; [`Audience`](crate::plugins::Audience) and
//! [`Subscriber`] answer the second, using the same rules as
//! `Hub::snapshot_for`, `packages::readable` and the mission listing
//! (R-01 H3).
//!
//! # Authorization is re-checked, not assumed
//!
//! An SSE response outlives the request that opened it — for hours, if nothing
//! goes wrong. A revoked token, a disabled account, a demoted administrator and
//! a removed channel membership must all take effect on an open feed, or the
//! feed is the one place in the server where revocation does not work
//! (R-01 H4). So the credential is resolved again every minute, and
//! immediately when something invalidates the account; the subscriber's
//! channels are rebuilt from the answer, and a refusal ends the response rather
//! than downgrading it.

use std::collections::VecDeque;

use actix_web::http::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use actix_web::web::Bytes;
use actix_web::{HttpRequest, HttpResponse, web};
use futures::Stream;
use rustak_api::event::ServerEvent;
use tokio::sync::broadcast::error::RecvError;

use crate::plugins::events::PublishedEvent;
use crate::plugins::visibility::Subscriber;
use crate::plugins::{Caller, ServerEvents, auth};
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

/// How often an open feed re-resolves the credential that opened it.
///
/// Short enough that a revocation an operator has just made is honoured while
/// they are still watching, long enough that a fleet of sidecars costs a
/// handful of indexed reads a minute. A revocation or a disable does not wait
/// for it: those are pushed, through
/// [`ServerEvents::invalidate`](crate::plugins::ServerEvents::invalidate).
const REAUTHORIZE: std::time::Duration = std::time::Duration::from_secs(60);

/// How many feeds may be open at once.
const MAX_FEEDS: usize = 64;

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
    let caller = authorized(&context, &request).await?;
    let subscriber = subscriber_for(&context, &caller).await?;

    let events = context.events().clone();

    // A broadcast receiver holds its own buffer of up to `RING` events, so an
    // unbounded number of open feeds is an unbounded amount of memory one
    // credential can pin (R-01 M12). Far above any real fleet: a sidecar opens
    // one feed and reconnects into the same slot.
    if events.subscribers() >= MAX_FEEDS {
        warn!(
            open = events.subscribers(),
            "Refused a server-event feed: too many are open."
        );

        return Err(ApiError::new(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "Too many consumers are reading the server-event feed. Try again shortly.",
        ));
    }

    // Subscribed before the backlog is read, so nothing published in between is
    // missed; `seen` is what keeps it from being sent twice.
    let receiver = events.subscribe();
    let invalidations = events.invalidations();
    let resume = resume_from(&request);
    let backlog = match resume {
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
            // A consumer that named no resume point asked for what happens
            // next. Starting at 0 meant that lagging before its first read
            // replayed the whole ring at it (R-01 L14).
            seen: resume.unwrap_or_else(|| events.latest_id()),
            backlog: backlog.into(),
            receiver,
            invalidations,
            events,
            shutdown: context.shutdown().clone(),
            opened: false,
            context: context.clone(),
            request,
            subscriber,
            next_check: tokio::time::Instant::now() + REAUTHORIZE,
        })))
}

/// The caller, once they are somebody the feed is for.
///
/// # Errors
///
/// A `401` without a usable credential and a `403` for an account that is
/// neither an administrator nor a service.
async fn authorized(context: &AppContext, request: &HttpRequest) -> Result<Caller, ApiError> {
    let caller = auth::caller(context, request)
        .await
        .map_err(super::services::refusal)?;

    if !caller.is_admin() && !caller.is_service() {
        return Err(ApiError::forbidden(
            "The server-event feed is for administrators and services.",
        ));
    }

    Ok(caller)
}

/// What this caller may be shown, as of now.
///
/// Rebuilt on every re-authorization rather than resolved once, because a
/// channel taken away mid-connection has to narrow the feed that is already
/// open.
async fn subscriber_for(
    context: &web::Data<AppContext>,
    caller: &Caller,
) -> Result<Subscriber, ApiError> {
    let identity = &caller.identity;

    // The **effective** set, not the raw memberships: a connection announces
    // itself with the set `stream::resolver` gave it, which is memberships
    // narrowed by the active-state preference and widened by `__ANON__` when
    // `anon_group_default` is on. Comparing an effective set against a raw one
    // is not a rule, it is an accident — and it would hide every `__ANON__`
    // event from an account with no explicit channels, which is most sidecars.
    let mut effective = identity.principal.clone();
    effective.groups = std::sync::Arc::new(
        crate::identity::members::effective_for_account(
            context.db(),
            identity.user.id,
            context.config().auth.anon_group_default,
        )
        .await
        .map_err(|err| super::subject::failed(context, &err))?,
    );

    let viewer = crate::files::viewer_for(
        context.db(),
        Some(identity.user.username.as_str()),
        Some(&effective),
    )
    .await
    .map_err(|err| super::subject::failed(context, &err))?;

    let service = context
        .db()
        .services()
        .get_by_user(identity.user.id)
        .await
        .map_err(|err| super::subject::failed(context, &err))?
        .map(|row| row.name);

    Ok(Subscriber {
        username: identity.user.username.clone(),
        is_admin: caller.is_admin(),
        groups: effective.groups,
        viewer,
        service,
    })
}

/// What one open feed is in the middle of.
struct Feed {
    /// The highest id already written, so a backlog entry the live channel also
    /// carries is written once.
    seen: u64,
    /// What is still owed from the resume, oldest first.
    backlog: VecDeque<std::sync::Arc<PublishedEvent>>,
    receiver: tokio::sync::broadcast::Receiver<std::sync::Arc<PublishedEvent>>,
    /// Accounts whose feeds must re-authorize now.
    invalidations: tokio::sync::broadcast::Receiver<Username>,
    events: ServerEvents,
    shutdown: Shutdown,
    /// Whether the `retry:` preamble has been written.
    opened: bool,
    /// Held so the credential can be resolved again while the feed is open.
    context: web::Data<AppContext>,
    request: HttpRequest,
    /// What this caller may be shown, as of the last authorization.
    subscriber: Subscriber,
    /// When the credential is next resolved again.
    ///
    /// On the feed rather than beside the keepalive, because the stream's
    /// closure is re-entered for every frame it yields: a deadline built there
    /// would restart on each one, and a feed busy enough to carry an event a
    /// minute would never re-authorize at all.
    next_check: tokio::time::Instant,
}

impl Feed {
    /// Resolves the credential again and rebuilds what this caller may see.
    ///
    /// Answers whether the feed may carry on.
    async fn reauthorize(&mut self) -> bool {
        let caller = match authorized(&self.context, &self.request).await {
            Ok(caller) => caller,
            Err(err) => {
                info!(
                    caller = %self.subscriber.username,
                    status = err.status().as_u16(),
                    "Ending a server-event feed whose caller is no longer authorized."
                );

                return false;
            }
        };

        match subscriber_for(&self.context, &caller).await {
            Ok(subscriber) => {
                self.subscriber = subscriber;
                self.next_check = tokio::time::Instant::now() + REAUTHORIZE;

                true
            }
            // A read failed rather than the caller being refused. Ending the
            // response is the conservative answer: carrying on would mean
            // filtering against a channel set we could not confirm.
            Err(_) => false,
        }
    }
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
            if let Some(published) = feed.backlog.pop_front() {
                feed.seen = feed.seen.max(published.event.id);

                if !feed.subscriber.may_see(&published.audience) {
                    continue;
                }

                return Some((Ok(frame(&published.event)), feed));
            }

            tokio::select! {
                biased;

                () = feed.shutdown.cancelled() => return None,
                () = tokio::time::sleep_until(feed.next_check) => {
                    if !feed.reauthorize().await {
                        return None;
                    }
                }
                // Pushed rather than waited for: disabling an account or
                // revoking the token behind it takes its feed away now.
                invalidated = feed.invalidations.recv() => match invalidated {
                    Ok(username) if username != feed.subscriber.username => continue,
                    // A lagged invalidation channel means one may have been
                    // missed, so the safe reading is "re-check anyway".
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        if !feed.reauthorize().await {
                            return None;
                        }
                    }
                    Err(RecvError::Closed) => return None,
                },
                received = feed.receiver.recv() => match received {
                    Ok(published) if published.event.id <= feed.seen => continue,
                    Ok(published) => {
                        feed.seen = published.event.id;

                        if !feed.subscriber.may_see(&published.audience) {
                            continue;
                        }

                        return Some((Ok(frame(&published.event)), feed));
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
    use rustak_api::event::{ChannelEvent, PackageEvent, ServerEventPayload};
    use rustak_api::{CredentialKind, UserKind};

    use super::*;
    use crate::db::repos::{NewUser, UserRow};
    use crate::identity::credentials::{MintRequest, mint};
    use crate::testing::TestServer;

    /// A service account with no channel memberships, and its token.
    ///
    /// The shape R-01 H3 reproduces: a sidecar is an ordinary account that
    /// normally holds nothing at all, and the feed used to hand it the whole
    /// installation.
    async fn sidecar(server: &TestServer, name: &str) -> (UserRow, String) {
        let username = Username::parse(&format!("svc.{name}")).unwrap();
        let user = server
            .db()
            .users()
            .create(NewUser {
                kind: UserKind::Service,
                ..NewUser::service(username.clone())
            })
            .await
            .unwrap();

        let minted = mint(
            server.db(),
            &server.config().auth,
            &user,
            MintRequest::new(CredentialKind::ServiceToken, "Test sidecar", &username),
        )
        .await
        .unwrap();

        (user, minted.secret.expose().to_string())
    }

    /// A `package.uploaded` for a package shared with `groups`.
    fn package(context: &AppContext, name: &str, hash: &str, groups: &[&str]) {
        context.events().publish(
            ServerEventPayload::PackageUploaded(PackageEvent {
                uid: format!("res-{name}"),
                name: name.to_string(),
                hash: hash.to_string(),
                size: 12,
                mission_package: false,
                submitter: Some("ada".to_string()),
            }),
            crate::plugins::Audience::Channels(
                groups.iter().map(|group| (*group).to_string()).collect(),
            ),
        );
    }

    /// `channel.changed`, published the way `identity::members` publishes it —
    /// audience and all, so the tests below filter against the real rule.
    fn channel(context: &AppContext, username: &str) {
        context
            .events()
            .channel_changed(&Username::from_storage(username));
    }

    /// The same event with no audience rule, for the resume mechanics.
    fn anything(context: &AppContext, username: &str) {
        context.events().publish(
            ServerEventPayload::ChannelChanged(ChannelEvent {
                username: username.to_string(),
            }),
            crate::plugins::Audience::Everyone,
        );
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
            anything(&server.context, name);
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
        anything(&server.context, "first");
        anything(&server.context, "second");
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
        anything(&server.context, "before");
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

    #[actix_web::test]
    async fn a_service_is_not_shown_a_package_it_could_not_download() {
        // R-01 H3. The hash is the download handle for
        // `GET /Marti/sync/content?hash=`, so this frame was at least as
        // sensitive as the package itself — and `packages::readable` answers an
        // out-of-channel package with the same `404` as a missing one.
        let server = TestServer::start().await;
        let (_, token) = sidecar(&server, "weather").await;
        package(&server.context, "blue-only.zip", "deadbeef", &["Blue"]);
        let app = app!(server);

        let response = test::TestRequest::get()
            .uri("/api/v1/events?lastEventId=0")
            .insert_header(("authorization", format!("Bearer {token}")))
            .send_request(&app)
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        server.context.shutdown().cancel();
        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

        assert!(!body.contains("blue-only.zip"), "{body}");
        assert!(!body.contains("deadbeef"), "{body}");
        assert_eq!(body, "retry: 5000\n\n", "{body}");
    }

    #[actix_web::test]
    async fn an_administrator_is_shown_the_same_package() {
        // The other half: the refusal above has to come from the channel
        // filter rather than from the event never being published.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        package(&server.context, "blue-only.zip", "deadbeef", &["Blue"]);
        let app = app!(server);

        let response = test::TestRequest::get()
            .uri("/api/v1/events?lastEventId=0")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .send_request(&app)
            .await;

        server.context.shutdown().cancel();
        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

        assert!(body.contains("blue-only.zip"), "{body}");
    }

    #[actix_web::test]
    async fn a_service_is_not_told_whose_channels_changed() {
        let server = TestServer::start().await;
        let (_, token) = sidecar(&server, "weather").await;
        channel(&server.context, "ada");
        let app = app!(server);

        let response = test::TestRequest::get()
            .uri("/api/v1/events?lastEventId=0")
            .insert_header(("authorization", format!("Bearer {token}")))
            .send_request(&app)
            .await;

        server.context.shutdown().cancel();
        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

        assert!(!body.contains("\"ada\""), "{body}");
    }

    #[actix_web::test]
    async fn a_service_still_hears_about_its_own_account() {
        let server = TestServer::start().await;
        let (user, token) = sidecar(&server, "weather").await;
        channel(&server.context, user.username.as_str());
        let app = app!(server);

        let response = test::TestRequest::get()
            .uri("/api/v1/events?lastEventId=0")
            .insert_header(("authorization", format!("Bearer {token}")))
            .send_request(&app)
            .await;

        server.context.shutdown().cancel();
        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

        assert!(body.contains("svc.weather"), "{body}");
    }

    #[actix_web::test]
    async fn an_open_feed_ends_when_its_account_is_switched_off() {
        // R-01 H4/H5. The feed used to be resolved once and then stream until
        // the process stopped, so a revoked token, a disabled account and a
        // demoted administrator all kept receiving indefinitely. Nothing
        // cancels the shutdown token here: the response ends because the
        // credential was re-checked.
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", true).await;
        let app = app!(server);

        let response = test::TestRequest::get()
            .uri("/api/v1/events")
            .insert_header(("authorization", format!("Bearer {}", session.token)))
            .send_request(&app)
            .await;

        assert_eq!(response.status(), StatusCode::OK);

        server
            .db()
            .users()
            .set_disabled(user.id, true)
            .await
            .unwrap();
        crate::identity::sessions::end_all(&server.context, &user).await;

        let body = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

        assert_eq!(body, "retry: 5000\n\n", "{body}");
    }

    #[actix_web::test]
    async fn a_consumer_that_asks_for_nothing_is_not_replayed_the_ring_when_it_lags() {
        // R-01 L14. `seen` started at 0, so a consumer that sent no
        // `Last-Event-ID` and then lagged was refilled with the whole ring —
        // the opposite of what it asked for.
        let server = TestServer::start().await;
        for name in ["first", "second"] {
            anything(&server.context, name);
        }
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

//! `GET /api/v1/map/events`: what the router relays, as Server-Sent Events.
//!
//! The shape is [`events`](super::super::events)' and so are the reasons for
//! it: a preamble, then frames as they happen, a comment when nothing has, and
//! a credential that is resolved again while the response is open — because an
//! SSE response outlives the request that opened it, and a revoked token or a
//! removed channel has to take effect on a map somebody left open overnight.
//!
//! # What is different
//!
//! There is nothing to resume from. The tap keeps no history — `cot_latest` is
//! the history — so a reader that falls behind is sent a `reset` and reads the
//! snapshot again, which is one request and always right, rather than being
//! replayed a backlog that may or may not reach back far enough.

use std::sync::Arc;

use actix_web::http::StatusCode;
use actix_web::http::header::{CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use actix_web::web::Bytes;
use actix_web::{HttpRequest, HttpResponse, web};
use futures::Stream;
use rustak_api::MapUpdate;
use rustak_core::identity::can_reach;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

use crate::plugins::auth;
use crate::prelude::*;
use crate::stream::RelayedCot;

use super::super::error::{ApiError, ApiResult};
use super::super::extract::Authenticated;
use super::feature;

const EVENT_STREAM: HeaderValue = HeaderValue::from_static("text/event-stream");
const NO_STORE: HeaderValue = HeaderValue::from_static("no-cache, no-store, must-revalidate");

/// How long a page should wait before reconnecting, sent once as `retry:`.
const RETRY_MS: u64 = 5_000;

/// How often a comment frame is written into an idle stream, so that no proxy
/// mistakes a quiet map for a dead connection.
const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(20);

/// How often an open feed resolves its credential and its channels again.
const REAUTHORIZE: std::time::Duration = std::time::Duration::from_secs(60);

/// How many relayed messages are taken in a row before the feed looks up to
/// see whether it should still be running.
const MAX_DRAIN: usize = 64;

/// How many maps may be open at once. Each holds a ring of relayed messages,
/// so an unbounded number is an unbounded amount of memory one account can pin.
const MAX_FEEDS: usize = 64;

/// `GET /api/v1/map/events`.
///
/// # Errors
///
/// A `503` when this installation has no CoT stream to watch, or too many maps
/// are already watching it.
pub async fn feed(
    context: web::Data<AppContext>,
    request: HttpRequest,
    caller: Authenticated,
) -> ApiResult {
    let Ok(live) = context.live() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "The CoT stream is not running on this server, so there is nothing live to watch.",
        ));
    };

    let tap = live.router().tap();
    if tap.watchers() >= MAX_FEEDS {
        warn!(
            open = tap.watchers(),
            "Refused a map feed: too many are open."
        );

        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many maps are open on this server. Try again shortly.",
        ));
    }

    let mut feed = Feed {
        receiver: tap.subscribe(),
        invalidations: context.events().invalidations(),
        shutdown: context.shutdown().clone(),
        viewer: Viewer {
            username: caller.user.username.clone(),
            is_admin: caller.principal.is_admin,
            groups: Arc::clone(&caller.principal.groups),
        },
        index: GroupIndex::default(),
        pending: Vec::new(),
        opened: false,
        drained: 0,
        next_check: tokio::time::Instant::now() + REAUTHORIZE,
        context: context.clone(),
        request,
    };
    feed.index = feed.read_index().await.unwrap_or_default();

    info!(caller = %feed.viewer.username, "Somebody opened a map.");

    Ok(HttpResponse::Ok()
        .insert_header((CONTENT_TYPE, EVENT_STREAM))
        .insert_header((CACHE_CONTROL, NO_STORE))
        .insert_header(("X-Accel-Buffering", HeaderValue::from_static("no")))
        .streaming(frames(feed)))
}

/// Who is watching, as of the last time anybody checked.
struct Viewer {
    username: Username,
    is_admin: bool,
    groups: Arc<GroupSet>,
}

impl Viewer {
    fn may_see(&self, relayed: &RelayedCot) -> bool {
        self.is_admin || can_reach(&relayed.groups, &self.groups)
    }
}

/// What one open feed is in the middle of.
struct Feed {
    receiver: Receiver<Arc<RelayedCot>>,
    /// Accounts whose feeds must re-authorize now.
    invalidations: Receiver<Username>,
    shutdown: Shutdown,
    viewer: Viewer,
    /// Channel names, read when the feed opens and again with the credential:
    /// a read per relayed message would put SQLite on the routing path's heels.
    index: GroupIndex,
    /// Frames already rendered and not yet written — one message can be
    /// several, when a delete names several uids.
    pending: Vec<Bytes>,
    /// Whether the `retry:` preamble has been written.
    opened: bool,
    /// How many messages have been taken since the feed last waited.
    drained: usize,
    /// On the feed rather than beside the keepalive, because the stream's
    /// closure is re-entered for every frame it yields.
    next_check: tokio::time::Instant,
    context: web::Data<AppContext>,
    request: HttpRequest,
}

impl Feed {
    async fn read_index(&self) -> Option<GroupIndex> {
        self.context.db().groups().index().await.ok()
    }

    /// Resolves the credential again. Answers whether the feed may carry on.
    async fn reauthorize(&mut self) -> bool {
        let Ok(caller) = auth::caller(&self.context, &self.request).await else {
            info!(
                caller = %self.viewer.username,
                "Ending a map feed whose caller is no longer authorized."
            );

            return false;
        };

        // A failed read ends the response: carrying on would mean naming
        // channels from an index nobody could confirm.
        let Some(index) = self.read_index().await else {
            return false;
        };

        self.viewer.is_admin = caller.is_admin();
        self.viewer.groups = Arc::clone(&caller.identity.principal.groups);
        self.index = index;
        self.next_check = tokio::time::Instant::now() + REAUTHORIZE;

        true
    }

    /// Tells the page to read the snapshot again rather than guessing at what
    /// the ring no longer holds.
    fn fell_behind(&mut self, missed: u64) {
        debug!(missed, "A map fell behind and was told to start again.");
        self.pending.push(frame(&MapUpdate::Reset));
    }

    /// Queues whatever one relayed message means for this viewer's map.
    fn take(&mut self, relayed: &RelayedCot) {
        if !self.viewer.may_see(relayed) {
            return;
        }

        let event = relayed.encoded.event();

        for uid in feature::removals(event) {
            self.pending.push(frame(&MapUpdate::Remove { uid }));
        }

        if feature::drawable(&event.r#type) {
            let groups = feature::channel_names(&relayed.groups, &self.index);
            let drawn = feature::from_event(event, relayed.received_at, groups);

            self.pending
                .push(frame(&MapUpdate::Upsert(Box::new(drawn))));
        }
    }
}

/// The response body: a preamble, then the live feed.
fn frames(feed: Feed) -> impl Stream<Item = Result<Bytes, actix_web::Error>> {
    futures::stream::unfold(feed, |mut feed| async move {
        if !feed.opened {
            feed.opened = true;

            return Some((Ok(Bytes::from(format!("retry: {RETRY_MS}\n\n"))), feed));
        }

        // Built per frame, so the comment is written a keepalive period after
        // the last thing written rather than on a fixed cadence.
        let mut keepalive =
            tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);

        loop {
            if !feed.pending.is_empty() {
                return Some((Ok(feed.pending.remove(0)), feed));
            }

            // Whatever has already arrived is written before anything is
            // waited for, so a burst goes out as a burst and a server that is
            // stopping still says what it had relayed. Only so much of it,
            // though: a feed that is never idle would otherwise never reach
            // the `select!` below, and that is where the credential is
            // checked again and a revocation is heard.
            if feed.drained < MAX_DRAIN {
                match feed.receiver.try_recv() {
                    Ok(relayed) => {
                        feed.drained += 1;
                        feed.take(&relayed);
                        continue;
                    }
                    Err(TryRecvError::Lagged(missed)) => {
                        feed.drained += 1;
                        feed.fell_behind(missed);
                        continue;
                    }
                    Err(TryRecvError::Closed) => return None,
                    Err(TryRecvError::Empty) => {}
                }
            }
            feed.drained = 0;

            tokio::select! {
                biased;

                () = feed.shutdown.cancelled() => return None,
                () = tokio::time::sleep_until(feed.next_check) => {
                    if !feed.reauthorize().await {
                        return None;
                    }
                }
                invalidated = feed.invalidations.recv() => match invalidated {
                    Ok(username) if username != feed.viewer.username => continue,
                    // A lagged invalidation channel may have dropped ours, so
                    // the safe reading is "check anyway".
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        if !feed.reauthorize().await {
                            return None;
                        }
                    }
                    Err(RecvError::Closed) => return None,
                },
                received = feed.receiver.recv() => match received {
                    Ok(relayed) => feed.take(&relayed),
                    Err(RecvError::Lagged(missed)) => feed.fell_behind(missed),
                    Err(RecvError::Closed) => return None,
                },
                _ = keepalive.tick() => return Some((Ok(Bytes::from_static(b": keep-alive\n\n")), feed)),
            }
        }
    })
}

/// One update, as the SSE wire format spells it.
fn frame(update: &MapUpdate) -> Bytes {
    let data = serde_json::to_string(update).unwrap_or_else(|err| {
        error!(error = %err, "Could not render a map update.");

        String::from("{}")
    });

    Bytes::from(format!("event: {}\ndata: {data}\n\n", update.name()))
}

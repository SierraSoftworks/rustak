//! ESB Networks' PowerCheck: the API behind <https://powercheck.esbnetworks.ie>.
//!
//! Two requests. `GET /outages` lists everything ESB currently shows as an id,
//! a type and a position; `GET /outages/<id>/` says the rest about one of them.
//! So the list is polled on a slow timer and puts a marker on the map at once,
//! and the details are fetched a few per tick and fill the marker in.
//!
//! # This is somebody else's server, at its busiest when we care most
//!
//! The API is undocumented and unofficial, and the night a storm takes supply
//! from a hundred thousand homes is the night everybody is asking it. So:
//!
//! - every request names this plugin and links to it;
//! - the list is asked for no more often than [`POLL_FLOOR`];
//! - details are fetched only for outages inside the [`Scope`], only when an
//!   outage is new or [`DETAIL_REFRESH`] old, never once it is final, and a
//!   batch stops at [`DETAIL_BUDGET`] so a slow upstream cannot hold a tick;
//! - a `429` to either request holds **both** back for as long as it asks, a
//!   detail that fails or outlasts the budget holds the rest back for
//!   [`DETAIL_COOLDOWN`], and [`DENIED_LIMIT`] refusals in a row (`401`/`403`
//!   — a key ESB has rotated) stop the source.
//!
//! # An upstream that is down does not clear the map
//!
//! A failed poll answers the error once and then goes on answering the last
//! list that worked, for up to [`HOLD`]: during a storm, stale markers beat an
//! empty map.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue};
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

use super::{OutageFeed, SourceState, http_client, retry_after};
use crate::outage::Outage;
use crate::scope::Scope;
use crate::wire::{Detail, Listing};

/// Where the API lives unless a settings file says otherwise.
pub const DEFAULT_BASE_URL: &str = "https://api.esb.ie/esbn/powercheck/v1.0";

/// The header the subscription key travels in.
pub const KEY_HEADER: &str = "API-Subscription-Key";

/// How often the list is asked for when a settings file does not say.
pub const DEFAULT_POLL: Duration = Duration::from_secs(300);

/// The fastest the list is ever asked for, whatever a settings file says.
pub const POLL_FLOOR: Duration = Duration::from_secs(60);

/// How many details are fetched per tick when a settings file does not say.
pub const DEFAULT_DETAILS_PER_TICK: usize = 10;

/// How old a detail may be before it is fetched again.
pub const DETAIL_REFRESH: Duration = Duration::from_secs(1800);

/// How long one tick's batch of detail requests may take.
pub const DETAIL_BUDGET: Duration = Duration::from_secs(5);

/// How long details are left alone after one fails, or is rate-limited
/// without saying for how long.
pub const DETAIL_COOLDOWN: Duration = Duration::from_secs(60);

/// How long the last list that worked is kept while the upstream is failing.
pub const HOLD: Duration = Duration::from_secs(7200);

/// How many refusals in a row stop this source for good.
pub const DENIED_LIMIT: u32 = 3;

const NAME: &str = "ESB PowerCheck";

/// What one request came to.
enum Fetched {
    Body(String),
    Missing,
    Limited(Option<Duration>),
    Denied(StatusCode),
}

#[derive(Debug)]
struct Entry {
    outage: Outage,
    detailed_at: Option<DateTime<Utc>>,
}

/// The PowerCheck API, read over a [`Scope`].
#[derive(Debug)]
pub struct PowerCheckFeed {
    base: String,
    client: reqwest::Client,
    scope: Scope,
    state: SourceState,
    entries: HashMap<String, Entry>,
    details_per_tick: usize,
    details_after: DateTime<Utc>,
    denied: u32,
    stopped: bool,
}

impl PowerCheckFeed {
    /// Builds the client; nothing is requested until the first poll.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error for a key that is empty or cannot
    /// travel in a header.
    pub fn open(
        base_url: &str,
        api_key: &Secret,
        scope: Scope,
        poll: Duration,
        details_per_tick: usize,
    ) -> Result<Self, Error> {
        const ADVICE: &[&str] = &[
            "Open https://powercheck.esbnetworks.ie with your browser's network inspector, and \
             copy the `API-Subscription-Key` header from a request to api.esb.ie.",
            "Set it as `api_key` under [settings.source], preferably as \"${{ env.ESB_API_KEY }}\".",
        ];

        if api_key.expose().trim().is_empty() {
            return Err(human_errors::user(
                "The PowerCheck `api_key` is empty.",
                ADVICE,
            ));
        }

        let mut key = HeaderValue::from_str(api_key.expose().trim()).wrap_user_err(
            "The PowerCheck `api_key` is not something a header can carry.",
            ADVICE,
        )?;
        key.set_sensitive(true);

        let interval = poll.max(POLL_FLOOR);
        if interval > poll {
            info!(
                "`poll` is faster than this source asks ESB; using {}s.",
                interval.as_secs()
            );
        }

        info!(
            poll_s = interval.as_secs(),
            details_per_tick,
            "Reading outages from ESB Networks' PowerCheck, an unofficial API: be a good guest.",
        );

        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            client: http_client(HeaderMap::from_iter([(
                reqwest::header::HeaderName::from_static("api-subscription-key"),
                key,
            )]))?,
            scope,
            state: SourceState::new(NAME, interval),
            entries: HashMap::new(),
            details_per_tick,
            details_after: Utc::now(),
            denied: 0,
            stopped: false,
        })
    }

    /// Whether repeated refusals have stopped this source.
    #[must_use]
    pub const fn stopped(&self) -> bool {
        self.stopped
    }

    async fn get(&self, url: String) -> Result<Fetched, Error> {
        let response = self.client.get(url).send().await.wrap_user_err(
            format!("We could not reach {NAME}."),
            &["Check that this machine can reach api.esb.ie; the service may simply be down."],
        )?;

        match response.status() {
            StatusCode::NOT_FOUND => Ok(Fetched::Missing),
            StatusCode::TOO_MANY_REQUESTS => Ok(Fetched::Limited(retry_after(response.headers()))),
            status @ (StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) => {
                Ok(Fetched::Denied(status))
            }
            status => response
                .error_for_status()
                .wrap_user_err(
                    format!("{NAME} answered {status}."),
                    &["The service is likely overloaded or being maintained; this is retried."],
                )?
                .text()
                .await
                .map(Fetched::Body)
                .wrap_user_err(format!("{NAME} sent a reply we could not read."), &[]),
        }
    }

    /// Asks for the list, and folds it into what is already known.
    async fn list(&mut self) -> Result<(), Error> {
        match self.get(format!("{}/outages", self.base)).await? {
            Fetched::Body(body) => {
                let listing: Listing = serde_json::from_str(&body).wrap_user_err(
                    format!("{NAME} sent something we could not read as a list of outages."),
                    &["The API is unofficial and may have changed shape; check for a newer release."],
                )?;

                self.merge(listing.into_outages());
                self.denied = 0;
                self.state.succeeded();
            }
            Fetched::Limited(asked) => {
                // "Not so fast" is about the key, not about one endpoint.
                let wait = self.state.wait_for(asked);
                self.hold_details(Utc::now(), wait);
            }
            Fetched::Denied(status) => self.refused(status),
            Fetched::Missing => {
                return Err(human_errors::user(
                    format!("{NAME} has no list of outages at '{}'.", self.base),
                    &["Check `base_url` under [settings.source]; the default is usually right."],
                ));
            }
        }

        Ok(())
    }

    /// Keeps what is known about outages that are still listed, forgets the
    /// rest, and marks one whose type has changed as wanting its detail again.
    fn merge(&mut self, listed: Vec<Outage>) {
        let mut entries = HashMap::with_capacity(listed.len());

        for outage in listed
            .into_iter()
            .filter(|outage| self.scope.admits(outage))
        {
            let entry = match self.entries.remove(&outage.id) {
                Some(mut known) if known.outage.kind != outage.kind => {
                    known.outage.kind = outage.kind;
                    known.detailed_at = None;
                    known
                }
                Some(known) => known,
                None => Entry {
                    outage,
                    detailed_at: None,
                },
            };

            entries.insert(entry.outage.id.clone(), entry);
        }

        self.entries = entries;
    }

    /// The outages worth a detail request, most urgent first: never fetched,
    /// then longest since fetched. Final ones are never asked about again.
    fn wanting_detail(&self, now: DateTime<Utc>) -> Vec<String> {
        let refresh = chrono::Duration::from_std(DETAIL_REFRESH).unwrap_or_default();
        let mut wanting: Vec<_> = self
            .entries
            .values()
            .filter(|entry| !entry.outage.is_final())
            .filter(|entry| entry.detailed_at.is_none_or(|at| now - at >= refresh))
            .map(|entry| (entry.detailed_at, entry.outage.id.clone()))
            .collect();

        wanting.sort();
        wanting
            .into_iter()
            .map(|(_, id)| id)
            .take(self.details_per_tick)
            .collect()
    }

    /// One tick's worth of detail requests.
    async fn details(&mut self, now: DateTime<Utc>) {
        let started = Instant::now();

        for id in self.wanting_detail(now) {
            let remaining = DETAIL_BUDGET.saturating_sub(started.elapsed());

            if now < self.details_after || self.stopped || remaining.is_zero() {
                break;
            }

            // The budget bounds the request as well as the batch: one detail
            // that hangs must not hold the tick for the client's own timeout.
            let request = self.get(format!("{}/outages/{id}/", self.base));
            let Ok(fetched) = tokio::time::timeout(remaining, request).await else {
                debug!(id, "A PowerCheck detail outlasted this tick's budget.");
                self.hold_details(now, DETAIL_COOLDOWN);
                break;
            };

            match fetched {
                Ok(Fetched::Body(body)) => match serde_json::from_str::<Detail>(&body) {
                    Ok(detail) => self.detailed(&id, now, Some(detail)),
                    Err(err) => {
                        debug!(id, "A PowerCheck detail was not one we could read: {err}");
                        self.detailed(&id, now, None);
                    }
                },
                // Purged between the list and now; the next list drops it.
                Ok(Fetched::Missing) => self.detailed(&id, now, None),
                Ok(Fetched::Limited(asked)) => {
                    let wait =
                        asked.map_or(DETAIL_COOLDOWN, |stated| stated.min(super::MAX_RETRY_AFTER));
                    debug!(
                        seconds = wait.as_secs(),
                        stated = asked.is_some(),
                        "PowerCheck rate-limited a detail request; holding details back."
                    );
                    self.hold_details(now, wait);
                }
                Ok(Fetched::Denied(status)) => {
                    // A key refused once is not worth a second request this tick.
                    self.refused(status);
                    break;
                }
                Err(err) => {
                    // Without this, the same outage is asked about every tick
                    // for as long as the upstream is struggling.
                    debug!(id, "A PowerCheck detail did not arrive: {err}");
                    self.hold_details(now, DETAIL_COOLDOWN);
                }
            }
        }
    }

    /// Leaves the detail endpoint alone for a while; the top of the batch
    /// loop is what honours it.
    fn hold_details(&mut self, now: DateTime<Utc>, wait: Duration) {
        self.details_after = self.details_after.max(now + wait);
    }

    /// Records an answer about one outage, which is also the key being
    /// accepted: refusals only count when they are consecutive.
    fn detailed(&mut self, id: &str, now: DateTime<Utc>, detail: Option<Detail>) {
        self.denied = 0;

        if let Some(entry) = self.entries.get_mut(id) {
            entry.detailed_at = Some(now);

            if let Some(detail) = detail {
                detail.apply(&mut entry.outage);
            }
        }
    }

    /// Counts a refusal, and stops asking once there have been enough.
    fn refused(&mut self, status: StatusCode) {
        self.denied = self.denied.saturating_add(1);
        self.state.failed(format!(
            "{NAME} refused the subscription key ({status}); ESB may have rotated it.",
        ));

        if self.denied >= DENIED_LIMIT {
            self.stopped = true;

            error!(
                refusals = self.denied,
                "{NAME} has refused every request; the source has stopped. Find the current \
                 `API-Subscription-Key` on powercheck.esbnetworks.ie, set `api_key`, and restart.",
            );
        }
    }

    /// Forgets the last list once the upstream has been failing for [`HOLD`].
    fn release(&mut self, now: DateTime<Utc>) {
        let hold = chrono::Duration::from_std(HOLD).unwrap_or_default();
        let held_too_long = self.state.last_success().is_none_or(|at| now - at > hold);

        if !self.state.is_connected() && held_too_long && !self.entries.is_empty() {
            warn!(
                "{NAME} has not answered for {}h; clearing the map.",
                HOLD.as_secs() / 3600
            );
            self.entries.clear();
        }
    }
}

#[async_trait]
impl OutageFeed for PowerCheckFeed {
    fn name(&self) -> &str {
        NAME
    }

    async fn poll(&mut self) -> Result<Vec<Outage>, Error> {
        let now = Utc::now();

        if !self.stopped
            && self.state.ready()
            && let Err(err) = self.list().await
        {
            self.state.failed(err.to_string());

            return Err(err);
        }

        if self.state.is_connected() {
            self.details(now).await;
        }

        self.release(now);

        Ok(self
            .entries
            .values()
            .map(|entry| entry.outage.clone())
            .collect())
    }

    fn state(&self) -> &SourceState {
        &self.state
    }
}

#[cfg(test)]
#[path = "powercheck_tests.rs"]
mod tests;

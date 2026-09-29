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
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::StatusCode;
use reqwest::header::{HeaderMap, HeaderValue};
use rustak_client::sidecar::async_trait;
use rustak_core::prelude::*;

use super::{OutageFeed, SourceState, http_client, retry_after};
use crate::outage::Outage;
use crate::scope::Scope;
use crate::wire::Listing;

mod details;

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
            "Reading outages from ESB Networks' PowerCheck, an unofficial API: be a good guest. \
             `poll` is the fastest it is asked; a Retry-After it states is waited out above that.",
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
    async fn list(&mut self, now: DateTime<Utc>) -> Result<(), Error> {
        match self.get(format!("{}/outages", self.base)).await? {
            Fetched::Body(body) => {
                let listing: Listing = serde_json::from_str(&body).wrap_user_err(
                    format!("{NAME} sent something we could not read as a list of outages."),
                    &["The API is unofficial and may have changed shape; check for a newer release."],
                )?;

                self.merge(listing.into_outages());
                self.denied = 0;
                self.state.succeeded_at(now);
            }
            Fetched::Limited(asked) => {
                // "Not so fast" is about the key, not about one endpoint.
                let (wait, _) = self.state.wait_for_at(asked, now);
                self.hold_details(now, wait);
            }
            Fetched::Denied(status) => self.refused(status, now),
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

    /// Counts a refusal, and stops asking once there have been enough.
    fn refused(&mut self, status: StatusCode, now: DateTime<Utc>) {
        self.denied = self.denied.saturating_add(1);
        self.state.failed_at(
            format!("{NAME} refused the subscription key ({status}); ESB may have rotated it."),
            now,
        );

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

    /// [`OutageFeed::poll`], at an instant of the caller's choosing: what
    /// decides whether the list is due, and what every schedule is kept
    /// against. The requests themselves still take as long as they take.
    ///
    /// # Errors
    ///
    /// The list request's failure, already recorded in the state.
    pub async fn poll_at(&mut self, now: DateTime<Utc>) -> Result<Vec<Outage>, Error> {
        if !self.stopped
            && self.state.ready_at(now)
            && let Err(err) = self.list(now).await
        {
            self.state.failed_at(err.to_string(), now);

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
}

#[async_trait]
impl OutageFeed for PowerCheckFeed {
    fn name(&self) -> &str {
        NAME
    }

    async fn poll(&mut self) -> Result<Vec<Outage>, Error> {
        self.poll_at(Utc::now()).await
    }

    fn rescope(&mut self, scope: Scope) {
        // What falls outside the new area stops being asked about at once;
        // what is newly inside it arrives with the next list, on the schedule
        // ESB is already being asked on.
        self.entries.retain(|_, entry| scope.admits(&entry.outage));
        self.scope = scope;
    }

    fn state(&self) -> &SourceState {
        &self.state
    }
}

#[cfg(test)]
#[path = "powercheck_tests.rs"]
mod tests;

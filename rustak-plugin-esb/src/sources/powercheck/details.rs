//! The second of PowerCheck's two requests: `GET /outages/<id>/`, a few per
//! tick, for the outages the list says are worth one.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;

use super::{DETAIL_BUDGET, DETAIL_COOLDOWN, DETAIL_REFRESH, Fetched, PowerCheckFeed};
use crate::sources::MAX_RETRY_AFTER;
use crate::wire::Detail;

impl PowerCheckFeed {
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
    pub(super) async fn details(&mut self, now: DateTime<Utc>) {
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
                    let wait = asked.map_or(DETAIL_COOLDOWN, |stated| stated.min(MAX_RETRY_AFTER));
                    debug!(
                        seconds = wait.as_secs(),
                        stated = asked.is_some(),
                        "PowerCheck rate-limited a detail request; holding details back."
                    );
                    self.hold_details(now, wait);
                }
                Ok(Fetched::Denied(status)) => {
                    // A key refused once is not worth a second request this tick.
                    self.refused(status, now);
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
    pub(super) fn hold_details(&mut self, now: DateTime<Utc>, wait: Duration) {
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
}

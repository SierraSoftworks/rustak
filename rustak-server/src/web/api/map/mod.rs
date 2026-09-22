//! `/api/v1/map`: the situation as a map draws it, and as it changes.
//!
//! Two reads. [`features`] is everything current, parsed out of `cot_latest`;
//! [`feed`](feed::feed) is a Server-Sent Events response of what the router
//! relays from then on. They are separate so that the snapshot stays an
//! ordinary cacheable-by-nobody `GET` a script can use, and so that a page can
//! open the feed *first* and lose nothing to the gap between them. A third,
//! [`history`](history::history), is one uid's past — the same features, read
//! back out of the history segments — for a page that wants to draw where
//! something has been.
//!
//! # Who sees what
//!
//! The rule the CoT browser uses, for the reason it uses it: a message is
//! shown to whoever was entitled to receive it — the sender's channels at send
//! time against the reader's — and an administrator sees everything. A map
//! that showed less than the reader's own device would be a worse diagnostic
//! than the list beside it; one that showed more would be a leak.
//!
//! # Reading only, for now
//!
//! Nothing here publishes. When it does, a write will name where it goes — a
//! channel or a mission — and become a CoT event injected through
//! [`Router::handle_inbound`](crate::stream::Router::handle_inbound) as though
//! a client had sent it, so that it is tagged, recorded, fanned out and fed
//! back to every open map by the path everything else already takes.

pub mod feature;
pub mod feed;
pub mod history;
pub mod shape;

use actix_web::web;
use chrono::{Duration, Utc};
use rustak_api::MapFeature;

use crate::cot_store::latest;
use crate::prelude::*;

use super::error::{ApiResult, json_ok};
use super::extract::Authenticated;
use super::subject::failed;

/// How long after going stale a feature is still offered.
///
/// A page dims what is stale and drops it this much later, the way a TAK
/// client does, so that "it stopped reporting a minute ago" is something
/// somebody can see rather than infer from an absence.
pub const LINGER: Duration = Duration::minutes(5);

/// The most features one snapshot carries.
///
/// An ADS-B feed over a busy region is a few thousand aircraft; this is above
/// that and below the point where the response stops being one a browser
/// parses without the page noticing.
const MAX_FEATURES: usize = 10_000;

/// How many rows are read at a time.
const PAGE: u32 = 2_000;

/// Registers the map's routes.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/map/features", web::get().to(features))
        .route(
            "/map/features/{uid}/history",
            web::get().to(history::history),
        )
        .route("/map/events", web::get().to(feed::feed));
}

/// `GET /api/v1/map/features` — everything current, newest relayed first.
///
/// # Errors
///
/// A `500` when a read fails.
pub async fn features(context: web::Data<AppContext>, caller: Authenticated) -> ApiResult {
    let listed = visible(&context, &caller.principal, PAGE, MAX_FEATURES)
        .await
        .map_err(|err| failed(&context, &err))?;

    Ok(json_ok(&listed))
}

/// Up to `most` current features this reader may see, read `page` rows at a
/// time.
///
/// The cap is on what is *answered*, not on what is read: chat, control
/// traffic and other people's channels are all in `cot_latest`, and a limit
/// applied before they are filtered out would spend itself on rows the reader
/// never sees and leave out ones they should. So rows are paged through until
/// there are none left or the answer is full.
///
/// Rows move while this reads — a uid that reports again goes to the front —
/// so a page boundary can repeat a row or step over one. Both are harmless
/// here: a page keeps the newer of two copies, and whatever moved was relayed
/// a moment ago, to a feed the page opened before it asked for this.
async fn visible(
    context: &AppContext,
    reader: &Principal,
    page: u32,
    most: usize,
) -> Result<Vec<MapFeature>, Error> {
    let since = Utc::now() - LINGER;
    let index = context.db().groups().index().await?;

    let mut listed: Vec<MapFeature> = Vec::new();
    let mut offset = 0;

    loop {
        let rows = latest::current(context.db(), since, page, offset).await?;

        listed.extend(
            rows.iter()
                .filter(|row| feature::drawable(&row.kind))
                .filter(|row| reader.is_admin || row.visible_to(&reader.groups))
                .filter_map(|row| feature::from_row(row, &index)),
        );

        if listed.len() >= most {
            warn!(
                most,
                "A map snapshot was cut short: more is current than one carries."
            );
            listed.truncate(most);
            return Ok(listed);
        }
        if rows.len() < page as usize {
            return Ok(listed);
        }

        offset += page;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use chrono::Duration;
    use rustak_cot::codec::EncodedEvent;
    use rustak_cot::{CotTime, Event};

    use super::*;
    use crate::cot_store::CotRecord;
    use crate::testing::TestServer;

    fn sender(bits: &[u32]) -> Principal {
        let mut groups = GroupSet::new();
        for bitpos in bits {
            groups.set(*bitpos, Direction::In);
        }

        Principal::new(
            UserId::from(1),
            Username::parse("grace").unwrap(),
            PrincipalKind::Person,
            AuthMethod::SetupToken,
        )
        .with_groups(Arc::new(groups))
    }

    async fn store(server: &TestServer, uid: &str, kind: &str, bits: &[u32]) {
        let event = Event::builder(kind, uid)
            .point(51.5, -0.12)
            .stale(CotTime::from_datetime(Utc::now() + Duration::minutes(2)))
            .build();

        let mut record = CotRecord::new(Arc::new(EncodedEvent::new(event)), &sender(bits), None);
        record.user_id = None;

        latest::upsert_batch(server.db(), vec![record])
            .await
            .unwrap();
    }

    #[actix_web::test]
    async fn rows_a_reader_never_sees_do_not_use_up_their_snapshot() {
        // Newest first, so the two rows that are not for this reader — chat,
        // and a channel they are not in — are the first two read. A limit
        // applied before the filter would answer nothing.
        let server = TestServer::start().await;
        store(&server, "MINE-1", "a-f-G-U-C", &[2]).await;
        store(&server, "MINE-2", "a-f-G-U-C", &[2]).await;
        store(&server, "THEIRS", "a-f-G-U-C", &[9]).await;
        store(&server, "CHAT", "b-t-f", &[2]).await;

        let mut receives = GroupSet::new();
        // Position zero is reserved, so the channel under test is the second.
        receives.set(2, Direction::Out);
        let reader = sender(&[]).with_groups(Arc::new(receives));

        let listed = visible(&server.context, &reader, 1, 10).await.unwrap();
        let mut uids: Vec<&str> = listed.iter().map(|feature| feature.uid.as_str()).collect();
        uids.sort_unstable();

        assert_eq!(uids, ["MINE-1", "MINE-2"]);
    }

    #[actix_web::test]
    async fn a_snapshot_is_never_longer_than_it_is_allowed_to_be() {
        let server = TestServer::start().await;
        for uid in ["A", "B", "C"] {
            store(&server, uid, "a-f-G-U-C", &[2]).await;
        }

        let everything = visible(&server.context, &sender(&[]).as_admin(), 2, 10).await;
        let capped = visible(&server.context, &sender(&[]).as_admin(), 2, 2).await;

        assert_eq!(everything.unwrap().len(), 3);
        assert_eq!(capped.unwrap().len(), 2);
    }
}

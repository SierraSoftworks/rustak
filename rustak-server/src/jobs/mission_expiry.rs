//! Retiring missions: the expiry a client asked for, then the purge we owe.
//!
//! Two sweeps in one job, because they are two halves of the same life cycle.
//! A mission may carry an `expiration` — an instant after which TAK clients
//! expect it to stop being served — and a mission that has been deleted, by a
//! client or by that expiry, is kept as a tombstone so that `GET /missions/{n}`
//! answers `410 Gone` rather than `404 Not Found`. The first sweep turns an
//! elapsed expiration into a tombstone; the second removes tombstones nobody is
//! still asking about.
//!
//! # Why an expiry is a soft delete rather than a purge
//!
//! `410` is the answer that tells a client holding a Data Sync token to forget
//! it; `404` is the answer that makes it retry with the other spelling of the
//! identifier. Purging an expired mission immediately would give every
//! subscriber the second answer to a question with the first answer, which is
//! how a client ends up retrying for ever. So expiry sets `deleted_at` and the
//! purge horizon — `[retention] missions_purge_after` — decides when the
//! tombstone itself goes.

use chrono::{DateTime, TimeDelta, Utc};

use crate::db::Database;
use crate::db::row::Timestamp;
use crate::prelude::*;
use crate::register_job;

/// The queue partition this job owns.
pub const MISSION_EXPIRY_PARTITION: &str = "housekeeping/mission-expiry";

/// How often missions are retired.
///
/// Hourly: `expiration` is an epoch-second field a client sets deliberately, so
/// an operator who set one for the end of an exercise expects it to take effect
/// that hour rather than the next day. Not configurable — the horizon is.
pub const EXPIRY_INTERVAL: TimeDelta = TimeDelta::hours(1);

/// What one sweep retired.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Retired {
    /// Missions whose `expiration` had passed, now tombstones.
    pub expired: usize,
    /// Tombstones removed for good.
    pub purged: usize,
}

impl Retired {
    /// Whether the sweep found anything at all.
    pub fn is_empty(self) -> bool {
        self.expired == 0 && self.purged == 0
    }
}

/// The message this job runs on. The horizon comes from the configuration at
/// run time, so there is nothing to carry.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionExpiryTask {}

/// Expires missions that have reached their `expiration`, then purges the
/// tombstones older than `[retention] missions_purge_after`.
pub struct MissionExpiryJob;

register_job!(MissionExpiryJob);

impl Job for MissionExpiryJob {
    type JobType = MissionExpiryTask;

    fn partition() -> &'static str {
        MISSION_EXPIRY_PARTITION
    }

    fn propagate_parent() -> bool {
        false
    }

    /// Arms the schedule to run at once.
    ///
    /// Immediately rather than an interval from now, unlike the history sweep:
    /// a server that was down over the moment a mission was due to expire has
    /// been serving it since, and the first thing it should do on the way back
    /// up is stop.
    async fn setup(&self, services: impl Services + Send + Sync + 'static) -> Result<(), Error> {
        Self::dispatch(
            MissionExpiryTask {},
            Some(MISSION_EXPIRY_PARTITION.into()),
            &services,
        )
        .await
    }

    async fn handle(
        &self,
        ctx: JobContext<impl Services + Send + Sync + 'static>,
        _job: &Self::JobType,
    ) -> Result<(), Error> {
        let services = ctx.services();

        // Re-armed before the work, so a sweep that fails is one missed sweep
        // rather than the end of the schedule.
        Self::dispatch_delayed(
            MissionExpiryTask {},
            Some(MISSION_EXPIRY_PARTITION.into()),
            EXPIRY_INTERVAL,
            &services,
        )
        .await?;

        let now = Utc::now();
        let purge_before = now - services.config().retention.missions_purge_after;

        sweep(services.db(), now, purge_before).await?;

        Ok(())
    }
}

/// Expires what is due and purges what is past `purge_before`.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when either statement fails. The two
/// are separate writes on purpose: a purge that cannot run should not roll back
/// an expiry that did, because the expiry is the half that clients can see.
#[instrument("jobs.mission_expiry.sweep", skip_all, err(Display))]
pub async fn sweep(
    db: &Database,
    now: DateTime<Utc>,
    purge_before: DateTime<Utc>,
) -> Result<Retired, Error> {
    let retired = Retired {
        expired: expire_due(db, now).await?,
        purged: purge_tombstones(db, purge_before).await?,
    };

    if !retired.is_empty() {
        info!(
            expired = retired.expired,
            purged = retired.purged,
            "Retired missions that had reached the end of their life."
        );
    }

    Ok(retired)
}

/// Tombstones every live mission whose `expiration` has passed.
///
/// `expiration` is epoch **seconds** and TAK spells "never" as `-1`, so a row
/// with a non-positive value is one nobody asked to expire.
async fn expire_due(db: &Database, now: DateTime<Utc>) -> Result<usize, Error> {
    let seconds = now.timestamp();
    let at = Timestamp::from(now);

    db.write(move |tx| {
        tx.execute(
            "UPDATE missions SET deleted_at = ?1, updated_at = ?1 \
             WHERE deleted_at IS NULL AND expiration IS NOT NULL \
               AND expiration > 0 AND expiration <= ?2",
            rusqlite::params![at, seconds],
        )
    })
    .await
}

/// Removes tombstones older than the purge horizon.
///
/// The child tables go with the row: every one of them declares
/// `ON DELETE CASCADE`, so subscriptions, changes, contents, uids, layers, logs
/// and invitations are removed in the same statement.
async fn purge_tombstones(db: &Database, before: DateTime<Utc>) -> Result<usize, Error> {
    let before = Timestamp::from(before);

    db.write(move |tx| {
        tx.execute(
            "DELETE FROM missions WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
            rusqlite::params![before],
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use crate::db::repos::missions::NewMission;

    use super::*;

    async fn mission(db: &Database, name: &str, expiration: Option<i64>) -> i64 {
        let mut new = NewMission::new(name, "MISSION_SUBSCRIBER");
        new.expiration = expiration;

        db.missions().create(new).await.unwrap().id
    }

    #[test]
    fn the_partition_is_the_one_the_registry_dispatches_on() {
        assert_eq!(MissionExpiryJob::partition(), MISSION_EXPIRY_PARTITION);
        assert!(Retired::default().is_empty());
    }

    #[tokio::test]
    async fn a_mission_past_its_expiration_becomes_a_tombstone_rather_than_disappearing() {
        // A client still holding a token has to be told 410, which means the
        // row has to survive the expiry that made it unreachable.
        let db = Database::open_in_memory().await.unwrap();
        let now = Utc::now();
        let due = mission(&db, "Ended", Some((now - TimeDelta::hours(1)).timestamp())).await;
        let later = mission(&db, "Running", Some((now + TimeDelta::days(1)).timestamp())).await;
        let forever = mission(&db, "Standing", None).await;

        let retired = sweep(&db, now, now - TimeDelta::days(14)).await.unwrap();

        assert_eq!(
            retired,
            Retired {
                expired: 1,
                purged: 0
            }
        );
        assert!(
            db.missions()
                .by_id(due)
                .await
                .unwrap()
                .unwrap()
                .is_deleted()
        );
        assert!(
            !db.missions()
                .by_id(later)
                .await
                .unwrap()
                .unwrap()
                .is_deleted()
        );
        assert!(
            !db.missions()
                .by_id(forever)
                .await
                .unwrap()
                .unwrap()
                .is_deleted()
        );
    }

    #[tokio::test]
    async fn taks_minus_one_means_never_rather_than_long_ago() {
        // `-1` is how TAK spells "no expiration"; read as an instant it is one
        // second before 1970 and would expire every mission on the first sweep.
        let db = Database::open_in_memory().await.unwrap();
        let standing = mission(&db, "Standing", Some(-1)).await;

        let retired = sweep(&db, Utc::now(), Utc::now() - TimeDelta::days(14))
            .await
            .unwrap();

        assert_eq!(retired.expired, 0);
        assert!(
            !db.missions()
                .by_id(standing)
                .await
                .unwrap()
                .unwrap()
                .is_deleted()
        );
    }

    #[tokio::test]
    async fn a_tombstone_past_the_horizon_is_removed_with_everything_under_it() {
        let db = Database::open_in_memory().await.unwrap();
        let now = Utc::now();
        let old = mission(&db, "Ancient", None).await;
        let recent = mission(&db, "Recent", None).await;

        db.missions()
            .soft_delete(old, now - TimeDelta::days(30))
            .await
            .unwrap();
        db.missions().soft_delete(recent, now).await.unwrap();

        let retired = sweep(&db, now, now - TimeDelta::days(14)).await.unwrap();

        assert_eq!(retired.purged, 1);
        assert!(db.missions().by_id(old).await.unwrap().is_none());
        assert!(
            db.missions().by_id(recent).await.unwrap().is_some(),
            "a tombstone inside the horizon still answers 410",
        );
    }

    #[tokio::test]
    async fn a_sweep_with_nothing_to_do_is_not_a_failure() {
        let db = Database::open_in_memory().await.unwrap();

        let retired = sweep(&db, Utc::now(), Utc::now() - TimeDelta::days(14))
            .await
            .unwrap();

        assert!(retired.is_empty());
    }
}

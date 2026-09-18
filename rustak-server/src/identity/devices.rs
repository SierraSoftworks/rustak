//! The clients that have enrolled, keyed by the uid they call themselves.
//!
//! A device is not an identity. It belongs to exactly one account, and one
//! person may carry several — a phone, a laptop, a CloudTAK bridge — so
//! everything joins on the uid the client sends as `clientUid` and puts in the
//! `uid` of every event it emits.
//!
//! # What a client says about itself is not authority
//!
//! The callsign, the platform, the version and the hardware all come out of the
//! `takv` detail or the `version` parameter of an enrolment request. Every one
//! of them is descriptive, optional, and overwritten by whatever the client last
//! said — but a field the client did not send never erases one we already knew,
//! because ATAK, WinTAK, iTAK and CloudTAK each send a different subset.

use std::collections::HashMap;

use rustak_api::Device;
use rustak_core::prelude::*;

use crate::db::{
    Database,
    repos::{DeviceRow, DeviceSeen, Page},
};

/// Records a connection, creating the device the first time we see it.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the write fails.
#[instrument("identity.devices.seen", skip_all, fields(device = %uid), err(Display))]
pub async fn upsert_seen(
    db: &Database,
    uid: &DeviceUid,
    user_id: UserId,
    seen: DeviceSeen,
) -> Result<DeviceRow, Error> {
    let existing = db.devices().get_by_uid(uid).await?;

    let row = db.devices().seen(uid, user_id, seen).await?;

    match existing {
        Some(previous) if previous.user_id != user_id => {
            // A uid that moved between accounts is worth a line: it is either a
            // device handed over deliberately, or a client that has picked
            // somebody else's identifier.
            warn!(
                device = %uid,
                from = %previous.user_id,
                to = %user_id,
                "A device uid changed hands.",
            );
        }
        Some(_) => {}
        None => info!(device = %uid, "Saw a device for the first time."),
    }

    Ok(row)
}

/// Every device, most recently seen first.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the read fails.
pub async fn list(db: &Database, page: Page) -> Result<Vec<DeviceRow>, Error> {
    db.devices().list(page).await
}

/// Every device belonging to one account.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the read fails.
pub async fn list_for_user(db: &Database, user_id: UserId) -> Result<Vec<DeviceRow>, Error> {
    db.devices().list_for_user(user_id).await
}

/// One device, by the uid its client sends.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the read fails.
pub async fn get(db: &Database, uid: &DeviceUid) -> Result<Option<DeviceRow>, Error> {
    db.devices().get_by_uid(uid).await
}

/// Forgets a device and the per-device channel state that went with it.
///
/// The certificates it enrolled with are left alone: they belong to the
/// account, they are what a live connection is authenticated by, and taking
/// them back is [`super::credentials::revoke`]'s job rather than a side effect
/// of tidying a device list.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the write fails.
#[instrument("identity.devices.delete", skip_all, fields(device = %id), err(Display))]
pub async fn delete(db: &Database, id: DeviceId) -> Result<bool, Error> {
    db.devices().delete(id).await
}

/// A device as the API describes it.
pub fn to_dto(row: &DeviceRow, username: Username) -> Device {
    Device {
        id: row.id,
        uid: row.uid.clone(),
        username,
        callsign: row.callsign.clone(),
        platform: row.platform.clone(),
        version: row.version.clone(),
        device_model: row.device_model.clone(),
        first_seen_at: row.first_seen_at,
        last_seen_at: row.last_seen_at,
        last_ip: row.last_ip,
        last_certificate_id: row.last_certificate_id,
    }
}

/// Several devices as the API describes them, resolving the owners in one read.
///
/// A device whose account has gone is dropped rather than rendered without an
/// owner: the foreign key makes that state transient, and a `username` the UI
/// cannot link anywhere is worse than a row that is not there.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the account read fails.
pub async fn to_dtos(db: &Database, rows: &[DeviceRow]) -> Result<Vec<Device>, Error> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let owners = usernames(db).await?;

    Ok(rows
        .iter()
        .filter_map(|row| {
            owners
                .get(&row.user_id)
                .map(|username| to_dto(row, username.clone()))
        })
        .collect())
}

/// Every account's name, by row id.
///
/// One read rather than one per device: an installation's device list is
/// hundreds of rows and its account list is tens.
async fn usernames(db: &Database) -> Result<HashMap<UserId, Username>, Error> {
    let mut owners = HashMap::new();
    let mut page = Page::first(500);

    loop {
        let batch = db.users().list(page).await?;
        let read = batch.len();

        for user in batch {
            owners.insert(user.id, user.username);
        }

        if read < page.limit as usize {
            break;
        }

        page = Page::at(page.offset + page.limit, page.limit);
    }

    Ok(owners)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;
    use crate::db::repos::NewUser;

    async fn fixture() -> (Database, UserId) {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser::person(Username::parse("alice").unwrap()))
            .await
            .unwrap();

        (db, user.id)
    }

    fn uid(value: &str) -> DeviceUid {
        DeviceUid::parse(value).unwrap()
    }

    fn atak() -> DeviceSeen {
        DeviceSeen {
            callsign: Some("ALPHA-1".into()),
            platform: Some("ATAK-CIV".into()),
            version: Some("5.6.0".into()),
            device_model: Some("Samsung SM-G998B".into()),
            os: Some("34".into()),
            last_ip: Some("198.51.100.7".parse::<IpAddr>().unwrap()),
        }
    }

    #[tokio::test]
    async fn a_device_is_created_the_first_time_it_is_seen_and_updated_after() {
        let (db, user) = fixture().await;

        let first = upsert_seen(&db, &uid("ANDROID-1"), user, atak())
            .await
            .unwrap();

        assert_eq!(first.callsign.as_deref(), Some("ALPHA-1"));
        assert_eq!(first.first_seen_at, first.last_seen_at);

        let again = upsert_seen(
            &db,
            &uid("ANDROID-1"),
            user,
            DeviceSeen {
                callsign: Some("ALPHA-2".into()),
                ..DeviceSeen::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(again.id, first.id, "the uid is what a device is");
        assert_eq!(again.callsign.as_deref(), Some("ALPHA-2"));
        assert_eq!(
            again.platform.as_deref(),
            Some("ATAK-CIV"),
            "a field the client did not send this time must not erase what we knew",
        );
    }

    #[tokio::test]
    async fn a_device_reads_back_by_the_uid_its_client_sends() {
        let (db, user) = fixture().await;
        upsert_seen(&db, &uid("ANDROID-1"), user, atak())
            .await
            .unwrap();

        assert!(get(&db, &uid("ANDROID-1")).await.unwrap().is_some());
        assert!(get(&db, &uid("ANDROID-2")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn forgetting_a_device_takes_its_channel_state_and_nothing_else() {
        let (db, user) = fixture().await;
        let device = upsert_seen(&db, &uid("ANDROID-1"), user, atak())
            .await
            .unwrap();

        assert!(delete(&db, device.id).await.unwrap());
        assert!(
            !delete(&db, device.id).await.unwrap(),
            "deleting twice should report that there was nothing there",
        );

        assert!(
            db.users().get(user).await.unwrap().is_some(),
            "a device is not an identity",
        );
    }

    #[tokio::test]
    async fn a_listing_names_each_devices_owner_without_a_read_per_row() {
        let (db, alice) = fixture().await;
        let bob = db
            .users()
            .create(NewUser::person(Username::parse("bob").unwrap()))
            .await
            .unwrap();

        upsert_seen(&db, &uid("ANDROID-1"), alice, atak())
            .await
            .unwrap();
        upsert_seen(&db, &uid("bob (ETL)"), bob.id, DeviceSeen::default())
            .await
            .unwrap();

        let rows = list(&db, Page::default()).await.unwrap();
        let dtos = to_dtos(&db, &rows).await.unwrap();

        assert_eq!(dtos.len(), 2);
        assert!(
            dtos.iter()
                .any(|device| device.uid.as_str() == "bob (ETL)"
                    && device.username.as_str() == "bob"),
        );

        let hers = list_for_user(&db, alice).await.unwrap();
        assert_eq!(hers.len(), 1);
        assert_eq!(hers[0].uid.as_str(), "ANDROID-1");
    }

    #[tokio::test]
    async fn a_device_whose_account_has_gone_is_left_out_of_a_listing() {
        let (db, user) = fixture().await;
        let row = upsert_seen(&db, &uid("ANDROID-1"), user, atak())
            .await
            .unwrap();

        // Rendered from a stale read, which is the only way this happens.
        assert!(
            to_dtos(&db, std::slice::from_ref(&row))
                .await
                .unwrap()
                .len()
                == 1
        );

        db.users().delete(user).await.unwrap();

        assert!(to_dtos(&db, &[row]).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_uid_that_changes_hands_is_recorded_rather_than_refused() {
        // Either a device handed over deliberately or a client claiming
        // somebody else's identifier; both are things an operator should be
        // able to see in the log, and neither is ours to decide here.
        let (db, alice) = fixture().await;
        let bob = db
            .users()
            .create(NewUser::person(Username::parse("bob").unwrap()))
            .await
            .unwrap();

        upsert_seen(&db, &uid("ANDROID-1"), alice, atak())
            .await
            .unwrap();
        let moved = upsert_seen(&db, &uid("ANDROID-1"), bob.id, DeviceSeen::default())
            .await
            .unwrap();

        assert_eq!(moved.user_id, bob.id);
    }

    #[tokio::test]
    async fn the_dto_describes_the_client_as_it_described_itself() {
        let (db, user) = fixture().await;
        let row = upsert_seen(&db, &uid("ANDROID-1"), user, atak())
            .await
            .unwrap();

        let dto = to_dto(&row, Username::parse("alice").unwrap());

        assert_eq!(dto.display(), "ALPHA-1");
        assert_eq!(dto.platform_version().as_deref(), Some("ATAK-CIV 5.6.0"));
        assert_eq!(dto.last_ip, Some("198.51.100.7".parse::<IpAddr>().unwrap()));
    }
}

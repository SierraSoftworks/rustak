//! `devices`: one row per enrolled client, keyed by the `clientUid` it sends.
//!
//! A device belongs to exactly one user. The uid is the client's own
//! identifier, so it is what everything else joins on: the active-channel
//! state, the certificate that was issued for it, and the contact list.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use crate::db::{
    Database,
    repos::Page,
    row::{Timestamp, bool_col, id_col, opt_id_col, ts},
};

/// The columns [`DeviceRow::from_row`] expects, in order.
const COLUMNS: &str = "id, uid, user_id, callsign, platform, version, device_model, os, \
                       incognito, last_certificate_id, first_seen_at, last_seen_at, last_ip";

/// One row of `devices`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: DeviceId,
    pub uid: DeviceUid,
    pub user_id: UserId,
    pub callsign: Option<String>,
    pub platform: Option<String>,
    pub version: Option<String>,
    pub device_model: Option<String>,
    pub os: Option<String>,
    /// The client asked not to appear in contact lists.
    pub incognito: bool,
    pub last_certificate_id: Option<CertificateId>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub last_ip: Option<IpAddr>,
}

impl DeviceRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: id_col(row, 0)?,
            uid: DeviceUid::from_storage(row.get::<_, String>(1)?),
            user_id: id_col(row, 2)?,
            callsign: row.get(3)?,
            platform: row.get(4)?,
            version: row.get(5)?,
            device_model: row.get(6)?,
            os: row.get(7)?,
            incognito: bool_col(row, 8)?,
            last_certificate_id: opt_id_col(row, 9)?,
            first_seen_at: ts(row, 10)?,
            last_seen_at: ts(row, 11)?,
            // An address we cannot parse is a diagnostic rather than a right,
            // so it is dropped rather than failing the whole read.
            last_ip: row
                .get::<_, Option<String>>(12)?
                .and_then(|text| text.parse().ok()),
        })
    }
}

/// A device about to be created.
#[derive(Debug, Clone)]
pub struct NewDevice {
    pub uid: DeviceUid,
    pub user_id: UserId,
    pub callsign: Option<String>,
}

impl NewDevice {
    /// A device we have only just heard of.
    pub fn new(uid: DeviceUid, user_id: UserId) -> Self {
        Self {
            uid,
            user_id,
            callsign: None,
        }
    }
}

/// What a client told us about itself when it connected.
///
/// Every field is optional because ATAK, WinTAK, iTAK and CloudTAK each send a
/// different subset, and a field nobody sent must not overwrite one we already
/// know.
#[derive(Debug, Clone, Default)]
pub struct DeviceSeen {
    pub callsign: Option<String>,
    pub platform: Option<String>,
    pub version: Option<String>,
    pub device_model: Option<String>,
    pub os: Option<String>,
    pub last_ip: Option<IpAddr>,
}

/// Reads and writes `devices`.
pub struct DevicesRepo<'a> {
    db: &'a Database,
}

impl<'a> DevicesRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Creates a device.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when the uid is
    /// already taken or the user does not exist.
    pub async fn create(&self, new: NewDevice) -> Result<DeviceRow, Error> {
        self.db
            .write(move |tx| {
                let now = Timestamp::now();

                tx.query_one(
                    &format!(
                        "INSERT INTO devices (uid, user_id, callsign, first_seen_at, last_seen_at) \
                         VALUES (?1, ?2, ?3, ?4, ?4) RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![new.uid.as_str(), new.user_id.get(), new.callsign, now],
                    DeviceRow::from_row,
                )
            })
            .await
    }

    /// Reads one device by row id.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, id: DeviceId) -> Result<Option<DeviceRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM devices WHERE id = ?1"),
                    [id.get()],
                    DeviceRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Reads one device by the uid its client sends.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_by_uid(&self, uid: &DeviceUid) -> Result<Option<DeviceRow>, Error> {
        let uid = uid.as_str().to_owned();

        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM devices WHERE uid = ?1"),
                    [uid],
                    DeviceRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Records a connection, creating the device if this is the first one.
    ///
    /// Fields the client did not send are left as they were, so a reconnect
    /// that omits the platform does not erase the platform we already knew.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn seen(
        &self,
        uid: &DeviceUid,
        user_id: UserId,
        seen: DeviceSeen,
    ) -> Result<DeviceRow, Error> {
        let uid = uid.as_str().to_owned();

        self.db
            .write(move |tx| {
                let now = Timestamp::now();

                tx.query_one(
                    &format!(
                        "INSERT INTO devices \
                           (uid, user_id, callsign, platform, version, device_model, os, \
                            last_ip, first_seen_at, last_seen_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9) \
                         ON CONFLICT (uid) DO UPDATE SET \
                           user_id = excluded.user_id, \
                           callsign = COALESCE(excluded.callsign, devices.callsign), \
                           platform = COALESCE(excluded.platform, devices.platform), \
                           version = COALESCE(excluded.version, devices.version), \
                           device_model = COALESCE(excluded.device_model, devices.device_model), \
                           os = COALESCE(excluded.os, devices.os), \
                           last_ip = COALESCE(excluded.last_ip, devices.last_ip), \
                           last_seen_at = excluded.last_seen_at \
                         RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        uid,
                        user_id.get(),
                        seen.callsign,
                        seen.platform,
                        seen.version,
                        seen.device_model,
                        seen.os,
                        seen.last_ip.map(|ip| ip.to_string()),
                        now,
                    ],
                    DeviceRow::from_row,
                )
            })
            .await
    }

    /// Every device belonging to a user, most recently seen first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_for_user(&self, user_id: UserId) -> Result<Vec<DeviceRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM devices WHERE user_id = ?1 ORDER BY last_seen_at DESC"
                ))?;

                statement
                    .query_map([user_id.get()], DeviceRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Every device, most recently seen first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(&self, page: Page) -> Result<Vec<DeviceRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM devices ORDER BY last_seen_at DESC LIMIT ?1 OFFSET ?2"
                ))?;

                statement
                    .query_map([page.limit(), page.offset()], DeviceRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Notes which certificate this device most recently presented.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_last_certificate(
        &self,
        id: DeviceId,
        certificate_id: Option<CertificateId>,
    ) -> Result<bool, Error> {
        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE devices SET last_certificate_id = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), certificate_id.map(CertificateId::get)],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Sets whether the device stays out of contact lists.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn set_incognito(&self, id: DeviceId, incognito: bool) -> Result<bool, Error> {
        let changed = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE devices SET incognito = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), i64::from(incognito)],
                )
            })
            .await?;

        Ok(changed > 0)
    }

    /// Deletes a device and its per-device channel state.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn delete(&self, id: DeviceId) -> Result<bool, Error> {
        let deleted = self
            .db
            .write(move |tx| tx.execute("DELETE FROM devices WHERE id = ?1", [id.get()]))
            .await?;

        Ok(deleted > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewUser;

    async fn fixture() -> (Database, UserId) {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser::person(Username::parse("j.smith").unwrap()))
            .await
            .unwrap();

        (db, user.id)
    }

    fn uid(raw: &str) -> DeviceUid {
        DeviceUid::parse(raw).unwrap()
    }

    #[tokio::test]
    async fn a_device_reads_back_as_created() {
        let (db, user) = fixture().await;

        let created = db
            .devices()
            .create(NewDevice {
                callsign: Some("BRAVO".into()),
                ..NewDevice::new(uid("ANDROID-1"), user)
            })
            .await
            .unwrap();

        assert_eq!(created.uid.as_str(), "ANDROID-1");
        assert_eq!(created.first_seen_at, created.last_seen_at);
        assert!(!created.incognito);
        assert_eq!(
            db.devices().get(created.id).await.unwrap().unwrap(),
            created
        );
    }

    #[tokio::test]
    async fn a_uid_belongs_to_one_device() {
        let (db, user) = fixture().await;
        db.devices()
            .create(NewDevice::new(uid("ANDROID-1"), user))
            .await
            .unwrap();

        assert!(
            db.devices()
                .create(NewDevice::new(uid("ANDROID-1"), user))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_device_for_a_user_that_does_not_exist_is_refused() {
        let (db, _user) = fixture().await;

        assert!(
            db.devices()
                .create(NewDevice::new(uid("ANDROID-1"), UserId::new(9999)))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_first_connection_creates_the_device() {
        let (db, user) = fixture().await;

        let seen = db
            .devices()
            .seen(
                &uid("ANDROID-1"),
                user,
                DeviceSeen {
                    callsign: Some("BRAVO".into()),
                    platform: Some("ATAK-CIV".into()),
                    last_ip: Some("10.0.0.4".parse().unwrap()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(seen.callsign.as_deref(), Some("BRAVO"));
        assert_eq!(seen.last_ip, Some("10.0.0.4".parse().unwrap()));
    }

    #[tokio::test]
    async fn a_reconnection_that_says_less_does_not_erase_what_we_knew() {
        let (db, user) = fixture().await;
        db.devices()
            .seen(
                &uid("ANDROID-1"),
                user,
                DeviceSeen {
                    callsign: Some("BRAVO".into()),
                    platform: Some("ATAK-CIV".into()),
                    version: Some("5.5".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let again = db
            .devices()
            .seen(&uid("ANDROID-1"), user, DeviceSeen::default())
            .await
            .unwrap();

        assert_eq!(again.callsign.as_deref(), Some("BRAVO"));
        assert_eq!(again.platform.as_deref(), Some("ATAK-CIV"));
        assert_eq!(again.version.as_deref(), Some("5.5"));
    }

    #[tokio::test]
    async fn a_reconnection_updates_only_the_last_seen_time() {
        let (db, user) = fixture().await;
        let first = db
            .devices()
            .seen(&uid("ANDROID-1"), user, DeviceSeen::default())
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let again = db
            .devices()
            .seen(&uid("ANDROID-1"), user, DeviceSeen::default())
            .await
            .unwrap();

        assert_eq!(again.id, first.id);
        assert_eq!(again.first_seen_at, first.first_seen_at);
        assert!(again.last_seen_at >= first.last_seen_at);
    }

    #[tokio::test]
    async fn deleting_a_user_takes_their_devices_with_them() {
        let (db, user) = fixture().await;
        db.devices()
            .create(NewDevice::new(uid("ANDROID-1"), user))
            .await
            .unwrap();

        db.users().delete(user).await.unwrap();

        assert!(
            db.devices()
                .get_by_uid(&uid("ANDROID-1"))
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn listing_is_most_recently_seen_first() {
        let (db, user) = fixture().await;
        db.devices()
            .seen(&uid("OLD"), user, DeviceSeen::default())
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        db.devices()
            .seen(&uid("NEW"), user, DeviceSeen::default())
            .await
            .unwrap();

        let listed = db.devices().list(Page::default()).await.unwrap();

        assert_eq!(listed[0].uid.as_str(), "NEW");
        assert_eq!(db.devices().list_for_user(user).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn incognito_and_the_last_certificate_can_be_set() {
        let (db, user) = fixture().await;
        let device = db
            .devices()
            .create(NewDevice::new(uid("ANDROID-1"), user))
            .await
            .unwrap();

        assert!(db.devices().set_incognito(device.id, true).await.unwrap());
        assert!(
            db.devices()
                .set_last_certificate(device.id, None)
                .await
                .unwrap()
        );

        let read = db.devices().get(device.id).await.unwrap().unwrap();
        assert!(read.incognito);
        assert_eq!(read.last_certificate_id, None);
        assert!(db.devices().delete(device.id).await.unwrap());
    }
}

//! `certificates`: everything the internal CA has issued, and what has been
//! taken back.
//!
//! The fingerprint is the identity: it is what the TLS handshake computes from
//! the certificate a client presents, and so what revocation and the
//! `require_known_cert` check are both answered by. The serial is unique only
//! within an issuer, which is why the index pairs the two.
//!
//! The narrowed listings the admin API reads are in [`list`], because this file
//! is at `conventions.md`'s length limit and the split falls naturally between
//! recording certificates and answering questions about them.

pub mod list;

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustak_api::{CertificateKind, CertificateSource};
use rustak_core::prelude::*;

use crate::db::{
    Database,
    repos::Page,
    row::{Timestamp, enum_col, id_col, json_col, opt_id_col, opt_ts, to_json, ts},
};

/// The columns [`CertificateRow::from_row`] expects, in order.
const COLUMNS: &str = "id, kind, source, issued_via, serial_hex, fingerprint, subject_cn, san, \
                       user_id, device_id, client_uid, credential_id, issuer_id, der, \
                       key_sealed, not_before, not_after, last_seen_at, revoked_at, \
                       revocation_reason, revoked_by, created_at";

/// One row of `certificates`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateRow {
    pub id: CertificateId,
    pub kind: CertificateKind,
    pub source: CertificateSource,
    /// The endpoint it came out of, for audit. `None` for the CA and for server
    /// certificates, which no client asked for.
    pub issued_via: Option<String>,
    pub serial_hex: String,
    /// Lower-case hex sha256 of the DER, with no colons.
    pub fingerprint: String,
    pub subject_cn: String,
    pub san: Vec<String>,
    pub user_id: Option<UserId>,
    pub device_id: Option<DeviceId>,
    pub client_uid: Option<String>,
    /// The credential that was spent to obtain it, so revoking that credential
    /// can revoke what it bought.
    pub credential_id: Option<CredentialId>,
    pub issuer_id: Option<CertificateId>,
    pub der: Vec<u8>,
    /// The sealed private key, for the certificates we generated the key for.
    pub key_sealed: Option<String>,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub revocation_reason: Option<String>,
    pub revoked_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl CertificateRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: id_col(row, 0)?,
            kind: enum_col(row, 1, CertificateKind::parse)?,
            source: enum_col(row, 2, CertificateSource::parse)?,
            issued_via: row.get(3)?,
            serial_hex: row.get(4)?,
            fingerprint: row.get(5)?,
            subject_cn: row.get(6)?,
            san: json_col(row, 7)?,
            user_id: opt_id_col(row, 8)?,
            device_id: opt_id_col(row, 9)?,
            client_uid: row.get(10)?,
            credential_id: opt_id_col(row, 11)?,
            issuer_id: opt_id_col(row, 12)?,
            der: row.get(13)?,
            key_sealed: row.get(14)?,
            not_before: ts(row, 15)?,
            not_after: ts(row, 16)?,
            last_seen_at: opt_ts(row, 17)?,
            revoked_at: opt_ts(row, 18)?,
            revocation_reason: row.get(19)?,
            revoked_by: row.get(20)?,
            created_at: ts(row, 21)?,
        })
    }

    /// Whether this certificate should be accepted at `now`.
    pub fn is_valid_at(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && self.not_before <= now && now < self.not_after
    }
}

/// A certificate about to be recorded.
#[derive(Debug, Clone)]
pub struct NewCertificate {
    pub kind: CertificateKind,
    pub source: CertificateSource,
    pub issued_via: Option<String>,
    pub serial_hex: String,
    pub fingerprint: String,
    pub subject_cn: String,
    pub san: Vec<String>,
    pub user_id: Option<UserId>,
    pub device_id: Option<DeviceId>,
    pub client_uid: Option<String>,
    pub credential_id: Option<CredentialId>,
    pub issuer_id: Option<CertificateId>,
    pub der: Vec<u8>,
    pub key_sealed: Option<String>,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
}

/// Why a certificate was taken back, and by whom.
#[derive(Debug, Clone)]
pub struct RevocationDetails {
    pub reason: String,
    pub by: Option<Username>,
}

/// Reads and writes `certificates`.
pub struct CertificatesRepo<'a> {
    db: &'a Database,
}

impl<'a> CertificatesRepo<'a> {
    pub(super) fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Records an issued certificate.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error, including when the fingerprint
    /// or the issuer-and-serial pair is already recorded.
    pub async fn create(&self, new: NewCertificate) -> Result<CertificateRow, Error> {
        self.db
            .write(move |tx| {
                tx.query_one(
                    &format!(
                        "INSERT INTO certificates \
                           (kind, source, issued_via, serial_hex, fingerprint, subject_cn, san, \
                            user_id, device_id, client_uid, credential_id, issuer_id, der, \
                            key_sealed, not_before, not_after, created_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                                 ?15, ?16, ?17) RETURNING {COLUMNS}"
                    ),
                    rusqlite::params![
                        new.kind.as_str(),
                        new.source.as_str(),
                        new.issued_via,
                        new.serial_hex,
                        new.fingerprint,
                        new.subject_cn,
                        to_json(&new.san)?,
                        new.user_id.map(UserId::get),
                        new.device_id.map(DeviceId::get),
                        new.client_uid,
                        new.credential_id.map(CredentialId::get),
                        new.issuer_id.map(CertificateId::get),
                        new.der,
                        new.key_sealed,
                        Timestamp::from(new.not_before),
                        Timestamp::from(new.not_after),
                        Timestamp::now(),
                    ],
                    CertificateRow::from_row,
                )
            })
            .await
    }

    /// Reads one certificate by row id.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get(&self, id: CertificateId) -> Result<Option<CertificateRow>, Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM certificates WHERE id = ?1"),
                    [id.get()],
                    CertificateRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Reads the certificate a handshake just presented.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn get_by_fingerprint(
        &self,
        fingerprint: &str,
    ) -> Result<Option<CertificateRow>, Error> {
        let fingerprint = fingerprint.to_owned();

        self.db
            .read(move |c| {
                c.query_one(
                    &format!("SELECT {COLUMNS} FROM certificates WHERE fingerprint = ?1"),
                    [fingerprint],
                    CertificateRow::from_row,
                )
                .optional()
            })
            .await
    }

    /// Every certificate issued to a user, newest first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_for_user(&self, user_id: UserId) -> Result<Vec<CertificateRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM certificates WHERE user_id = ?1 ORDER BY id DESC"
                ))?;

                statement
                    .query_map([user_id.get()], CertificateRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Certificates of one kind, newest first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list_of_kind(
        &self,
        kind: CertificateKind,
        page: Page,
    ) -> Result<Vec<CertificateRow>, Error> {
        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM certificates WHERE kind = ?1 \
                     ORDER BY id DESC LIMIT ?2 OFFSET ?3"
                ))?;
                let rows = statement.query_map(
                    rusqlite::params![kind.as_str(), page.limit(), page.offset()],
                    CertificateRow::from_row,
                )?;

                rows.collect()
            })
            .await
    }

    /// The fingerprints the revocation cache is built from: every live one, and
    /// every revoked one.
    ///
    /// Read as one pair so the cache cannot be assembled from two reads taken
    /// either side of a revocation.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn fingerprints(&self) -> Result<(Vec<String>, Vec<String>), Error> {
        self.db
            .read(|c| {
                let mut statement =
                    c.prepare("SELECT fingerprint, revoked_at IS NOT NULL FROM certificates")?;
                let rows = statement.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?;

                let mut known = Vec::new();
                let mut revoked = Vec::new();
                for row in rows {
                    let (fingerprint, is_revoked) = row?;
                    if is_revoked != 0 {
                        revoked.push(fingerprint.clone());
                    }
                    known.push(fingerprint);
                }

                Ok((known, revoked))
            })
            .await
    }

    /// Certificates of `kind` expiring before `before` and not yet revoked,
    /// which is what the renewal job looks at.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn expiring_before(
        &self,
        kind: CertificateKind,
        before: DateTime<Utc>,
    ) -> Result<Vec<CertificateRow>, Error> {
        let before = Timestamp::from(before);

        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM certificates \
                     WHERE kind = ?1 AND revoked_at IS NULL AND not_after < ?2 \
                     ORDER BY not_after ASC"
                ))?;
                let rows = statement.query_map(
                    rusqlite::params![kind.as_str(), before],
                    CertificateRow::from_row,
                )?;

                rows.collect()
            })
            .await
    }

    /// Revokes one certificate, reporting whether it was live.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn revoke(
        &self,
        id: CertificateId,
        details: RevocationDetails,
    ) -> Result<bool, Error> {
        let revoked = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE certificates \
                     SET revoked_at = ?2, revocation_reason = ?3, revoked_by = ?4 \
                     WHERE id = ?1 AND revoked_at IS NULL",
                    rusqlite::params![
                        id.get(),
                        Timestamp::now(),
                        details.reason,
                        details.by.map(|by| by.into_inner()),
                    ],
                )
            })
            .await?;

        Ok(revoked > 0)
    }

    /// Revokes every live certificate bought with one credential.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn revoke_for_credential(
        &self,
        credential_id: CredentialId,
        details: RevocationDetails,
    ) -> Result<usize, Error> {
        self.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE certificates \
                     SET revoked_at = ?2, revocation_reason = ?3, revoked_by = ?4 \
                     WHERE credential_id = ?1 AND revoked_at IS NULL",
                    rusqlite::params![
                        credential_id.get(),
                        Timestamp::now(),
                        details.reason,
                        details.by.map(|by| by.into_inner()),
                    ],
                )
            })
            .await
    }

    /// Notes that a certificate was presented on a connection.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn touch_last_seen(&self, id: CertificateId) -> Result<(), Error> {
        self.db
            .write(move |tx| {
                tx.execute(
                    "UPDATE certificates SET last_seen_at = ?2 WHERE id = ?1",
                    rusqlite::params![id.get(), Timestamp::now()],
                )
            })
            .await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::{NewDevice, NewUser};

    async fn fixture() -> (Database, UserId, DeviceId) {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser::person(Username::parse("j.smith").unwrap()))
            .await
            .unwrap();
        let device = db
            .devices()
            .create(NewDevice::new(
                DeviceUid::parse("ANDROID-1").unwrap(),
                user.id,
            ))
            .await
            .unwrap();

        (db, user.id, device.id)
    }

    fn issued(user_id: UserId, fingerprint: &str, serial: &str) -> NewCertificate {
        NewCertificate {
            kind: CertificateKind::Client,
            source: CertificateSource::Enrollment,
            issued_via: Some("enroll_v2_json".into()),
            serial_hex: serial.into(),
            fingerprint: fingerprint.into(),
            subject_cn: "j.smith".into(),
            san: vec![],
            user_id: Some(user_id),
            device_id: None,
            client_uid: Some("ANDROID-1".into()),
            credential_id: None,
            issuer_id: None,
            der: b"\x30\x82fake".to_vec(),
            key_sealed: None,
            not_before: Utc::now() - chrono::TimeDelta::minutes(5),
            not_after: Utc::now() + chrono::TimeDelta::days(365),
        }
    }

    #[tokio::test]
    async fn a_certificate_reads_back_as_issued() {
        let (db, user, _device) = fixture().await;

        let created = db
            .certificates()
            .create(issued(user, "aa11", "0102"))
            .await
            .unwrap();

        assert_eq!(created.kind, CertificateKind::Client);
        assert_eq!(created.issued_via.as_deref(), Some("enroll_v2_json"));
        assert_eq!(created.der, b"\x30\x82fake".to_vec());
        assert!(created.is_valid_at(Utc::now()));
        assert_eq!(
            db.certificates().get(created.id).await.unwrap().unwrap(),
            created
        );
    }

    #[tokio::test]
    async fn a_fingerprint_identifies_exactly_one_certificate() {
        let (db, user, _device) = fixture().await;
        db.certificates()
            .create(issued(user, "aa11", "0102"))
            .await
            .unwrap();

        assert!(
            db.certificates()
                .create(issued(user, "aa11", "0304"))
                .await
                .is_err()
        );
        assert!(
            db.certificates()
                .get_by_fingerprint("aa11")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn only_the_sources_the_dto_spells_are_storable() {
        let (db, user, _device) = fixture().await;

        let refused = db
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO certificates \
                       (kind, source, serial_hex, fingerprint, subject_cn, user_id, der, \
                        not_before, not_after, created_at) \
                     VALUES ('client', 'file', '01', 'ff', 'cn', ?1, x'00', \
                             '2026-01-01T00:00:00.000Z', '2027-01-01T00:00:00.000Z', \
                             '2026-01-01T00:00:00.000Z')",
                    [user.get()],
                )
            })
            .await;

        assert!(refused.is_err());
    }

    #[tokio::test]
    async fn revoking_is_recorded_once_with_its_reason() {
        let (db, user, _device) = fixture().await;
        let certificate = db
            .certificates()
            .create(issued(user, "aa11", "0102"))
            .await
            .unwrap();

        let details = RevocationDetails {
            reason: "device-lost".into(),
            by: Some(Username::parse("admin").unwrap()),
        };
        assert!(
            db.certificates()
                .revoke(certificate.id, details.clone())
                .await
                .unwrap()
        );
        assert!(
            !db.certificates()
                .revoke(certificate.id, details)
                .await
                .unwrap()
        );

        let read = db
            .certificates()
            .get(certificate.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.revocation_reason.as_deref(), Some("device-lost"));
        assert_eq!(read.revoked_by.as_deref(), Some("admin"));
        assert!(!read.is_valid_at(Utc::now()));
    }

    #[tokio::test]
    async fn revoking_a_credential_revokes_what_it_bought() {
        let (db, user, _device) = fixture().await;
        let credential = db
            .credentials()
            .create(crate::db::repos::NewCredential {
                user_id: user,
                kind: rustak_api::CredentialKind::EnrollmentToken,
                label: "QR".into(),
                secret_hash: rustak_core::identity::hash("s3cret").unwrap(),
                lookup_hint: rustak_core::identity::lookup_hint("s3cret"),
                expires_at: None,
                max_uses: Some(1),
                created_by: None,
            })
            .await
            .unwrap();

        db.certificates()
            .create(NewCertificate {
                credential_id: Some(credential.id),
                ..issued(user, "aa11", "0102")
            })
            .await
            .unwrap();

        let revoked = db
            .certificates()
            .revoke_for_credential(
                credential.id,
                RevocationDetails {
                    reason: "credential-revoked".into(),
                    by: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(revoked, 1);
        assert!(
            db.certificates()
                .get_by_fingerprint("aa11")
                .await
                .unwrap()
                .unwrap()
                .revoked_at
                .is_some()
        );
    }

    #[tokio::test]
    async fn the_revocation_cache_reads_both_lists_at_once() {
        let (db, user, _device) = fixture().await;
        let live = db
            .certificates()
            .create(issued(user, "aa11", "0102"))
            .await
            .unwrap();
        let gone = db
            .certificates()
            .create(issued(user, "bb22", "0304"))
            .await
            .unwrap();
        db.certificates()
            .revoke(
                gone.id,
                RevocationDetails {
                    reason: "superseded".into(),
                    by: None,
                },
            )
            .await
            .unwrap();

        let (known, revoked) = db.certificates().fingerprints().await.unwrap();

        assert_eq!(known.len(), 2);
        assert_eq!(revoked, vec!["bb22".to_string()]);
        assert!(known.contains(&live.fingerprint));
    }

    #[tokio::test]
    async fn the_renewal_job_sees_only_what_is_expiring_and_live() {
        let (db, user, _device) = fixture().await;
        db.certificates()
            .create(NewCertificate {
                not_after: Utc::now() + chrono::TimeDelta::days(3),
                ..issued(user, "aa11", "0102")
            })
            .await
            .unwrap();
        db.certificates()
            .create(issued(user, "bb22", "0304"))
            .await
            .unwrap();

        let expiring = db
            .certificates()
            .expiring_before(
                CertificateKind::Client,
                Utc::now() + chrono::TimeDelta::days(30),
            )
            .await
            .unwrap();

        assert_eq!(expiring.len(), 1);
        assert_eq!(expiring[0].fingerprint, "aa11");
    }

    #[tokio::test]
    async fn deleting_a_user_keeps_the_certificate_and_forgets_the_owner() {
        let (db, user, _device) = fixture().await;
        db.certificates()
            .create(issued(user, "aa11", "0102"))
            .await
            .unwrap();

        db.users().delete(user).await.unwrap();

        let read = db
            .certificates()
            .get_by_fingerprint("aa11")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.user_id, None, "history outlives the account");
        assert!(
            db.certificates()
                .list_for_user(user)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn listing_by_kind_pages_and_last_seen_can_be_touched() {
        let (db, user, _device) = fixture().await;
        let certificate = db
            .certificates()
            .create(issued(user, "aa11", "0102"))
            .await
            .unwrap();

        db.certificates()
            .touch_last_seen(certificate.id)
            .await
            .unwrap();

        let listed = db
            .certificates()
            .list_of_kind(CertificateKind::Client, Page::first(10))
            .await
            .unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].last_seen_at.is_some());
        assert!(
            db.certificates()
                .list_of_kind(CertificateKind::Ca, Page::first(10))
                .await
                .unwrap()
                .is_empty()
        );
    }
}

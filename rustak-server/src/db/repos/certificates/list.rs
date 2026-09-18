//! The narrowed listings `GET /api/v1/certificates` is built from.
//!
//! A child module rather than more lines in [`super`], because the parent file
//! is already at `conventions.md`'s limit and the split falls naturally: that
//! file records and revokes, this one answers questions about what has been
//! recorded.
//!
//! # Why the state is decided in SQL
//!
//! "Active", "revoked" and "expired" are a function of the row *and of the
//! clock*, so filtering them after a page had been read would return short
//! pages — twenty rows asked for, eleven returned, and no way for the caller to
//! tell that from the end of the list. The predicate therefore goes into the
//! `WHERE` clause, with the instant bound rather than taken from SQLite, for
//! the same reason every other timestamp here is.

use chrono::{DateTime, Utc};
use rustak_api::{CertificateKind, CertificateState};
use rustak_core::prelude::*;

use crate::db::repos::Page;
use crate::db::row::Timestamp;

use super::{COLUMNS, CertificateRow, CertificatesRepo};

/// What a certificate listing may be narrowed by.
///
/// Every field is "and also": an administrator asking for one device's revoked
/// certificates means both, not either.
#[derive(Debug, Clone, Copy, Default)]
pub struct CertificateFilter {
    /// Only certificates of one kind.
    pub kind: Option<CertificateKind>,

    /// Only the ones issued to one account.
    pub user_id: Option<UserId>,

    /// Only the ones issued to one device.
    pub device_id: Option<DeviceId>,

    /// Only the ones in one state, as of the instant passed to
    /// [`CertificatesRepo::list`].
    pub state: Option<CertificateState>,
}

impl CertificateFilter {
    /// Every certificate one account holds.
    pub fn for_user(user_id: UserId) -> Self {
        Self {
            user_id: Some(user_id),
            ..Self::default()
        }
    }

    /// The `WHERE` clause this filter stands for, as SQL.
    ///
    /// `?1`..`?3` are the kind, the account and the device; `?4` is the
    /// instant the state is judged against.
    fn predicate(&self) -> String {
        let mut clauses = vec![
            "(?1 IS NULL OR kind = ?1)".to_string(),
            "(?2 IS NULL OR user_id = ?2)".to_string(),
            "(?3 IS NULL OR device_id = ?3)".to_string(),
        ];

        clauses.push(
            match self.state {
                None => "?4 IS NOT NULL",
                Some(CertificateState::Active) => "(revoked_at IS NULL AND not_after > ?4)",
                Some(CertificateState::Revoked) => "(revoked_at IS NOT NULL)",
                Some(CertificateState::Expired) => "(revoked_at IS NULL AND not_after <= ?4)",
            }
            .to_string(),
        );

        clauses.join(" AND ")
    }
}

impl CertificatesRepo<'_> {
    /// The certificates matching `filter`, newest first.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn list(
        &self,
        filter: CertificateFilter,
        now: DateTime<Utc>,
        page: Page,
    ) -> Result<Vec<CertificateRow>, Error> {
        let sql = format!(
            "SELECT {COLUMNS} FROM certificates WHERE {} ORDER BY id DESC LIMIT ?5 OFFSET ?6",
            filter.predicate()
        );

        self.db
            .read(move |c| {
                let mut statement = c.prepare(&sql)?;

                statement
                    .query_map(
                        rusqlite::params![
                            filter.kind.map(|kind| kind.as_str()),
                            filter.user_id.map(UserId::get),
                            filter.device_id.map(DeviceId::get),
                            Timestamp::from(now),
                            page.limit(),
                            page.offset(),
                        ],
                        CertificateRow::from_row,
                    )?
                    .collect()
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::CertificateSource;

    use crate::db::Database;
    use crate::db::repos::NewCertificate;

    use super::*;

    async fn seeded() -> Database {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(crate::db::repos::NewUser::person(
                Username::parse("grace").unwrap(),
            ))
            .await
            .unwrap();
        let device = db
            .devices()
            .create(crate::db::repos::NewDevice::new(
                DeviceUid::parse("ANDROID-1").unwrap(),
                user.id,
            ))
            .await
            .unwrap();

        for (index, (not_after, revoked)) in [
            ("2030-01-01T00:00:00Z", false),
            ("2020-01-01T00:00:00Z", false),
            ("2030-01-01T00:00:00Z", true),
        ]
        .into_iter()
        .enumerate()
        {
            let row = db
                .certificates()
                .create(NewCertificate {
                    kind: CertificateKind::Client,
                    source: CertificateSource::Enrollment,
                    issued_via: None,
                    serial_hex: format!("{index:032x}"),
                    fingerprint: format!("{index:064x}"),
                    subject_cn: "grace".into(),
                    san: Vec::new(),
                    user_id: Some(user.id),
                    device_id: Some(device.id),
                    client_uid: None,
                    credential_id: None,
                    issuer_id: None,
                    der: vec![index as u8],
                    key_sealed: None,
                    not_before: "2019-01-01T00:00:00Z".parse().unwrap(),
                    not_after: not_after.parse().unwrap(),
                })
                .await
                .unwrap();

            if revoked {
                db.certificates()
                    .revoke(
                        row.id,
                        crate::db::repos::RevocationDetails {
                            reason: "admin_action".into(),
                            by: None,
                        },
                    )
                    .await
                    .unwrap();
            }
        }

        // A certificate belonging to nobody, so the account filter has
        // something to leave out.
        db.certificates()
            .create(NewCertificate {
                kind: CertificateKind::Server,
                source: CertificateSource::Internal,
                issued_via: None,
                serial_hex: "ff".repeat(16),
                fingerprint: "f".repeat(64),
                subject_cn: "localhost".into(),
                san: vec!["localhost".into()],
                user_id: None,
                device_id: None,
                client_uid: None,
                credential_id: None,
                issuer_id: None,
                der: vec![9],
                key_sealed: None,
                not_before: "2019-01-01T00:00:00Z".parse().unwrap(),
                not_after: "2030-01-01T00:00:00Z".parse().unwrap(),
            })
            .await
            .unwrap();

        db
    }

    fn now() -> DateTime<Utc> {
        "2026-01-01T00:00:00Z".parse().unwrap()
    }

    #[tokio::test]
    async fn an_unfiltered_listing_answers_with_everything_newest_first() {
        let db = seeded().await;
        let listed = db
            .certificates()
            .list(CertificateFilter::default(), now(), Page::first(50))
            .await
            .unwrap();

        assert_eq!(listed.len(), 4);
        assert!(listed.windows(2).all(|pair| pair[0].id > pair[1].id));
    }

    #[tokio::test]
    async fn each_state_answers_with_only_its_own() {
        let db = seeded().await;

        for (state, expected) in [
            (CertificateState::Active, 2),
            (CertificateState::Revoked, 1),
            (CertificateState::Expired, 1),
        ] {
            let listed = db
                .certificates()
                .list(
                    CertificateFilter {
                        state: Some(state),
                        ..CertificateFilter::default()
                    },
                    now(),
                    Page::first(50),
                )
                .await
                .unwrap();

            assert_eq!(listed.len(), expected, "{}", state.as_str());
        }
    }

    #[tokio::test]
    async fn a_revoked_certificate_that_has_run_out_is_not_also_expired() {
        // The expired listing excludes the revoked ones outright, so the three
        // states partition the register rather than overlapping.
        let db = seeded().await;
        let later: DateTime<Utc> = "2031-01-01T00:00:00Z".parse().unwrap();

        let expired = db
            .certificates()
            .list(
                CertificateFilter {
                    state: Some(CertificateState::Expired),
                    ..CertificateFilter::default()
                },
                later,
                Page::first(50),
            )
            .await
            .unwrap();

        assert_eq!(expired.len(), 3);
        assert!(expired.iter().all(|row| row.revoked_at.is_none()));
    }

    #[tokio::test]
    async fn the_account_the_device_and_the_kind_all_narrow_and_combine() {
        let db = seeded().await;
        let user = db
            .users()
            .get_by_username(&Username::parse("grace").unwrap())
            .await
            .unwrap()
            .unwrap();
        let device = db
            .devices()
            .get_by_uid(&DeviceUid::parse("ANDROID-1").unwrap())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            db.certificates()
                .list(CertificateFilter::for_user(user.id), now(), Page::first(50))
                .await
                .unwrap()
                .len(),
            3,
        );

        assert_eq!(
            db.certificates()
                .list(
                    CertificateFilter {
                        kind: Some(CertificateKind::Server),
                        ..CertificateFilter::default()
                    },
                    now(),
                    Page::first(50),
                )
                .await
                .unwrap()
                .len(),
            1,
        );

        assert_eq!(
            db.certificates()
                .list(
                    CertificateFilter {
                        device_id: Some(device.id),
                        state: Some(CertificateState::Active),
                        ..CertificateFilter::default()
                    },
                    now(),
                    Page::first(50),
                )
                .await
                .unwrap()
                .len(),
            1,
        );
    }

    #[tokio::test]
    async fn a_page_is_a_window_over_the_same_order() {
        let db = seeded().await;
        let all = db
            .certificates()
            .list(CertificateFilter::default(), now(), Page::first(50))
            .await
            .unwrap();
        let second = db
            .certificates()
            .list(CertificateFilter::default(), now(), Page::at(1, 2))
            .await
            .unwrap();

        assert_eq!(second.len(), 2);
        assert_eq!(second[0].id, all[1].id);
        assert_eq!(second[1].id, all[2].id);
    }
}

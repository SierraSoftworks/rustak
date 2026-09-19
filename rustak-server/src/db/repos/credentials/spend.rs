//! Spending a one-time credential, and what the spend leaves behind.
//!
//! An enrolment token buys exactly one certificate, and
//! [`claim_single_use`](CredentialsRepo::claim_single_use) is both the gate and
//! the spend — one conditional `UPDATE`, so of two concurrent `signClient/v2`
//! posts carrying the same token exactly one changes the row (R-01 M3).
//! [`release_single_use`](CredentialsRepo::release_single_use) is its
//! compensating half, for an issuance that then failed.
//!
//! # Why the row remembers what it bought
//!
//! ATAK's enrolment is three calls with one credential, and the third —
//! `GET /Marti/api/tls/profile/enrollment?clientUid=` — arrives moments after
//! the second has spent it (M2-15). `spent_at` and `spent_uid` are what let
//! [`crate::identity::verify::Grace`] answer that one call for that one device:
//! `revoked_at` cannot, because the claim writes it too, so it says when the
//! row died and not whether it died by being spent or by being taken back.
//!
//! They are therefore written **here and nowhere else**, and cleared both by
//! the release and by `clear`, which every revocation calls. A row a
//! revocation has touched is not returned by
//! [`find_by_hint_including_spent`](CredentialsRepo::find_by_hint_including_spent),
//! so an administrator taking a token back ends its grace window at once.

use chrono::{DateTime, Utc};
use rusqlite::Transaction;
use rustak_core::prelude::*;

use crate::db::row::Timestamp;

use super::{COLUMNS, CredentialRow, CredentialsRepo};

/// Forgets the spend on every row matching `predicate`.
///
/// Unconditional, and separate from the revocation it accompanies: a credential
/// that was already spent is not *live*, so the revoking `UPDATE` changes
/// nothing on it and the grace window would otherwise outlive the revocation.
///
/// # Errors
///
/// Whatever the statement fails with, for the caller's transaction to carry.
pub(super) fn clear<P: rusqlite::Params>(
    tx: &mut Transaction<'_>,
    predicate: &str,
    params: P,
) -> rusqlite::Result<usize> {
    tx.execute(
        &format!("UPDATE credentials SET spent_at = NULL, spent_uid = NULL WHERE {predicate}"),
        params,
    )
}

impl CredentialsRepo<'_> {
    /// The candidate rows for a secret whose hint is `hint`, **including** the
    /// one-time credentials spent since `since`.
    ///
    /// A second query rather than a widened [`find_by_hint`](CredentialsRepo::find_by_hint)
    /// on purpose: a spent row is returned only where a caller has asked for
    /// one by name, so no ordinary authentication path can reach a credential
    /// that has already bought what it was for. `spent_at` is written only by
    /// [`claim_single_use`](Self::claim_single_use) and cleared by a
    /// revocation, so a row a revocation touched is not returned here.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn find_by_hint_including_spent(
        &self,
        hint: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<CredentialRow>, Error> {
        let hint = hint.to_owned();
        let since = Timestamp::from(since);

        self.db
            .read(move |c| {
                let mut statement = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM credentials \
                     WHERE lookup_hint = ?1 \
                       AND (revoked_at IS NULL OR (spent_at IS NOT NULL AND spent_at > ?2)) \
                     ORDER BY id ASC"
                ))?;

                statement
                    .query_map(rusqlite::params![hint, since], CredentialRow::from_row)?
                    .collect()
            })
            .await
    }

    /// Claims a one-time credential: counts the use **and** spends it, in one
    /// conditional write, reporting whether this caller is the one that got it.
    ///
    /// The consumption *is* the gate. Checking usability in one read and
    /// spending it in a later write let two concurrent `signClient/v2` posts
    /// with the same enrolment token both pass and both receive a certificate
    /// (R-01 M3) — the window being one argon2 verification plus a signature.
    /// Here the `WHERE` clause does the checking, so exactly one caller changes
    /// a row.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn claim_single_use(
        &self,
        id: CredentialId,
        client_uid: Option<&str>,
    ) -> Result<bool, Error> {
        let client_uid = client_uid.map(str::to_owned);

        let claimed = self
            .db
            .write(move |tx| {
                let now = Timestamp::now();

                tx.execute(
                    "UPDATE credentials \
                     SET uses = uses + 1, last_used_at = ?2, revoked_at = ?2, \
                         spent_at = ?2, spent_uid = ?3 \
                     WHERE id = ?1 AND revoked_at IS NULL \
                       AND (max_uses IS NULL OR uses < max_uses)",
                    rusqlite::params![id.get(), now, client_uid],
                )
            })
            .await?;

        Ok(claimed > 0)
    }

    /// Puts back a claim whose issuance then failed.
    ///
    /// The compensating half of [`claim_single_use`](Self::claim_single_use):
    /// a token spent for a certificate that was never signed is one somebody
    /// cannot enrol with and cannot get back. Conditional on the row still
    /// being in the state the claim left it, so a release racing anything else
    /// changes nothing.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the write fails.
    pub async fn release_single_use(&self, id: CredentialId) -> Result<bool, Error> {
        let released = self
            .db
            .write(move |tx| {
                tx.execute(
                    "UPDATE credentials \
                     SET uses = uses - 1, revoked_at = NULL, spent_at = NULL, spent_uid = NULL \
                     WHERE id = ?1 AND revoked_at IS NOT NULL AND uses > 0",
                    rusqlite::params![id.get()],
                )
            })
            .await?;

        Ok(released > 0)
    }
}

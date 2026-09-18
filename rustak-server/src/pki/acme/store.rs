//! `acme_certificates`: what an order produced, and what the last one cost.
//!
//! One row per set of names, keyed by the sorted JSON array of them, so that an
//! installation which changes `[acme] domains` orders a new certificate rather
//! than overwriting the one it is still serving on the old names.
//!
//! # Why the row is written twice
//!
//! The private key is sealed against
//! [`crate::crypto::SecretContext::ServerCertKey`],
//! whose identity is the row's own id — which SQLite only assigns once the row
//! exists. So [`reserve`] inserts the row (or finds the existing one) and
//! [`store`] fills it in. A reserved row carries an empty chain, and [`load`]
//! reports that as "no certificate yet" rather than handing a listener a chain
//! with nothing in it.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension as _;
use rustls::sign::CertifiedKey;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};

use rustak_core::prelude::*;

use crate::config::AcmeChallenge;
use crate::crypto::{Sealed, SecretContext, SecretStore};
use crate::db::Database;
use crate::db::row::{Timestamp, json_col, opt_ts, to_json, ts};

/// One row of `acme_certificates`.
#[derive(Debug, Clone)]
pub struct AcmeCertificateRow {
    /// The row id, which is also the sealing context of [`Self::key_sealed`].
    pub id: i64,
    /// The names this certificate covers, sorted.
    pub domains: Vec<String>,
    /// The chain as the authority returned it, leaf first.
    pub chain_pem: String,
    /// The PKCS#8 private key, sealed — absent while the row is only reserved.
    pub key_sealed: Option<Sealed>,
    pub not_before: Option<DateTime<Utc>>,
    pub not_after: Option<DateTime<Utc>>,
    /// How the last successful order was validated.
    pub challenge_type: Option<AcmeChallenge>,
    /// Consecutive failures since the last success, for the back-off.
    pub attempts: i64,
    pub last_attempt_at: Option<DateTime<Utc>>,
    /// The last failure, shown in the admin UI. Never a secret: it is the
    /// authority's own problem document.
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl AcmeCertificateRow {
    /// Whether this row actually holds a certificate, as opposed to being the
    /// placeholder [`reserve`] left behind.
    pub fn is_issued(&self) -> bool {
        !self.chain_pem.trim().is_empty() && self.not_after.is_some() && self.key_sealed.is_some()
    }

    /// The chain and key as rustls wants them.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the key cannot be opened
    /// under this row's context, the chain will not parse, or the two do not
    /// go together.
    pub fn certified(&self, secrets: &SecretStore) -> Result<Arc<CertifiedKey>, Error> {
        let sealed = self.key_sealed.as_ref().ok_or_else(|| {
            human_errors::system(
                "No ACME certificate has been stored for these names yet.",
                ADVICE_REORDER,
            )
        })?;
        let pkcs8 = secrets.open(sealed, self.context())?;

        let chain: Vec<CertificateDer<'static>> =
            CertificateDer::pem_slice_iter(self.chain_pem.as_bytes())
                .collect::<Result<_, _>>()
                .wrap_system_err(
                    "The stored ACME certificate chain could not be read.",
                    ADVICE_REORDER,
                )?;

        if chain.is_empty() {
            return Err(human_errors::system(
                "The stored ACME certificate chain contains no certificates.",
                ADVICE_REORDER,
            ));
        }

        crate::pki::tls::install_crypto_provider();

        CertifiedKey::from_der(
            chain,
            PrivateKeyDer::Pkcs8(pkcs8.into()),
            &rustls::crypto::aws_lc_rs::default_provider(),
        )
        .map(Arc::new)
        .wrap_system_err(
            "The stored ACME certificate and its private key do not go together.",
            ADVICE_REORDER,
        )
    }

    /// The context this row's key is sealed against.
    fn context(&self) -> SecretContext<'static> {
        SecretContext::ServerCertKey {
            certificate: CertificateId::new(self.id),
        }
    }

    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            domains: json_col(row, 1)?,
            chain_pem: row.get(2)?,
            // `reserve` writes `{}` so the column's `json_valid` check passes
            // before there is a key to put there.
            key_sealed: match row.get::<_, String>(3)?.as_str() {
                "{}" | "" => None,
                _ => Some(json_col(row, 3)?),
            },
            not_before: opt_ts(row, 4)?,
            not_after: opt_ts(row, 5)?,
            challenge_type: row
                .get::<_, Option<String>>(6)?
                .and_then(|text| parse_challenge(&text)),
            attempts: row.get(7)?,
            last_attempt_at: opt_ts(row, 8)?,
            last_error: row.get(9)?,
            updated_at: ts(row, 10)?,
        })
    }
}

/// Advice for a stored certificate we can no longer use.
const ADVICE_REORDER: &[&str] = &[
    "Trigger a renewal from the admin API to order a replacement.",
    "If the encryption key was replaced, the stored certificate cannot be recovered and has to be reordered.",
];

/// The columns [`AcmeCertificateRow::from_row`] expects, in order.
const COLUMNS: &str = "id, domains, chain_pem, key_sealed, not_before, not_after, \
                       challenge_type, attempts, last_attempt_at, last_error, updated_at";

/// The names an order is placed for, tidied: trimmed, lower-cased, without the
/// trailing root dot, and without repeats — in the order they were written, so
/// the first name stays the canonical one.
pub fn normalise(domains: &[String]) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(domains.len());

    for domain in domains {
        let name = domain.trim().trim_end_matches('.').to_ascii_lowercase();

        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }

    names
}

/// The `domains` column: the same names, sorted, as a JSON array.
///
/// Sorted so that writing the same names in a different order is the same
/// certificate rather than a second order against the authority's rate limit.
fn storage_key(domains: &[String]) -> rusqlite::Result<String> {
    let mut sorted = normalise(domains);
    sorted.sort();

    to_json(&sorted)
}

/// How a challenge is spelled in the column, matching the schema's `CHECK`.
fn parse_challenge(text: &str) -> Option<AcmeChallenge> {
    match text {
        "tls-alpn-01" => Some(AcmeChallenge::TlsAlpn01),
        "http-01" => Some(AcmeChallenge::Http01),
        _ => None,
    }
}

/// The row for these names, if one has been reserved.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the read or the row mapping
/// fails.
pub async fn load(db: &Database, domains: &[String]) -> Result<Option<AcmeCertificateRow>, Error> {
    let domains = domains.to_vec();

    db.read(move |c| {
        let key = storage_key(&domains)?;

        c.query_one(
            &format!("SELECT {COLUMNS} FROM acme_certificates WHERE domains = ?1"),
            [key],
            AcmeCertificateRow::from_row,
        )
        .optional()
    })
    .await
}

/// The row id for these names, creating an empty row if there is none.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the write fails.
pub async fn reserve(db: &Database, domains: &[String]) -> Result<i64, Error> {
    let domains = domains.to_vec();

    db.write(move |tx| {
        let key = storage_key(&domains)?;
        let now = Timestamp::now();

        tx.execute(
            "INSERT INTO acme_certificates (domains, chain_pem, key_sealed, attempts, created_at, updated_at) \
             VALUES (?1, '', '{}', 0, ?2, ?2) \
             ON CONFLICT (domains) DO NOTHING",
            rusqlite::params![key, now],
        )?;

        tx.query_one(
            "SELECT id FROM acme_certificates WHERE domains = ?1",
            [key],
            |row| row.get(0),
        )
    })
    .await
}

/// Writes what an order produced into the row [`reserve`] handed back.
///
/// Clears the failure state: a success is the end of a back-off.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the key cannot be sealed or the
/// write fails.
pub async fn store(
    db: &Database,
    secrets: &SecretStore,
    id: i64,
    chain_pem: String,
    key_pkcs8: &[u8],
    validity: (DateTime<Utc>, DateTime<Utc>),
    challenge: AcmeChallenge,
) -> Result<(), Error> {
    let sealed = secrets.seal(
        key_pkcs8,
        SecretContext::ServerCertKey {
            certificate: CertificateId::new(id),
        },
    )?;
    let challenge = challenge.as_str().to_string();

    db.write(move |tx| {
        tx.execute(
            "UPDATE acme_certificates SET \
               chain_pem = ?2, key_sealed = ?3, not_before = ?4, not_after = ?5, \
               challenge_type = ?6, attempts = 0, last_error = NULL, \
               last_attempt_at = ?7, updated_at = ?7 \
             WHERE id = ?1",
            rusqlite::params![
                id,
                chain_pem,
                to_json(&sealed)?,
                Timestamp::from(validity.0),
                Timestamp::from(validity.1),
                challenge,
                Timestamp::now(),
            ],
        )
    })
    .await?;

    Ok(())
}

/// Records a failed order, and reports how many have failed in a row.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the write fails.
pub async fn record_failure(db: &Database, id: i64, reason: &str) -> Result<i64, Error> {
    let reason = reason.to_string();

    db.write(move |tx| {
        tx.execute(
            "UPDATE acme_certificates SET \
               attempts = attempts + 1, last_error = ?2, last_attempt_at = ?3, updated_at = ?3 \
             WHERE id = ?1",
            rusqlite::params![id, reason, Timestamp::now()],
        )?;

        tx.query_one(
            "SELECT attempts FROM acme_certificates WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn names() -> Vec<String> {
        vec!["tak.example.com".to_string()]
    }

    #[test]
    fn the_same_names_written_differently_are_one_certificate() {
        assert_eq!(
            storage_key(&["B.example.com".into(), "a.example.com.".into()]).unwrap(),
            storage_key(&["a.example.com".into(), "b.EXAMPLE.com".into()]).unwrap(),
            "case, the root dot and the order are not part of the identity",
        );
    }

    #[test]
    fn the_first_name_written_stays_the_first_name_requested() {
        // The storage key is sorted; what goes into the certificate is not,
        // because the leading name is the one an operator expects to see.
        assert_eq!(
            normalise(&[
                "Z.example.com".into(),
                "a.example.com".into(),
                "z.example.com".into()
            ]),
            vec!["z.example.com".to_string(), "a.example.com".to_string()],
        );
    }

    #[tokio::test]
    async fn nothing_is_stored_until_an_order_finishes() {
        let db = database().await;

        assert!(load(&db, &names()).await.unwrap().is_none());

        let id = reserve(&db, &names()).await.unwrap();
        let reserved = load(&db, &names()).await.unwrap().unwrap();

        assert_eq!(reserved.id, id);
        assert!(
            !reserved.is_issued(),
            "a reserved row is not a certificate a listener may present",
        );
    }

    #[tokio::test]
    async fn reserving_twice_is_the_same_row() {
        let db = database().await;

        let first = reserve(&db, &names()).await.unwrap();
        let second = reserve(&db, &["TAK.example.com.".to_string()])
            .await
            .unwrap();

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn a_stored_certificate_comes_back_as_something_rustls_can_present() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let id = reserve(&db, &names()).await.unwrap();

        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = rcgen::CertificateParams::new(names())
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let now = Utc::now();

        store(
            &db,
            &secrets,
            id,
            certificate.pem(),
            &key.serialize_der(),
            (now, now + chrono::Duration::days(90)),
            AcmeChallenge::Http01,
        )
        .await
        .unwrap();

        let stored = load(&db, &names()).await.unwrap().unwrap();

        assert!(stored.is_issued());
        assert_eq!(stored.challenge_type, Some(AcmeChallenge::Http01));
        assert_eq!(stored.attempts, 0);
        assert!(stored.certified(&secrets).is_ok());
    }

    #[tokio::test]
    async fn a_key_sealed_for_one_row_cannot_be_opened_as_another() {
        // The whole point of binding the context to the row id: a ciphertext
        // moved between rows must not decrypt.
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let id = reserve(&db, &names()).await.unwrap();

        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = rcgen::CertificateParams::new(names())
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let now = Utc::now();

        store(
            &db,
            &secrets,
            id,
            certificate.pem(),
            &key.serialize_der(),
            (now, now + chrono::Duration::days(90)),
            AcmeChallenge::TlsAlpn01,
        )
        .await
        .unwrap();

        let mut relocated = load(&db, &names()).await.unwrap().unwrap();
        relocated.id += 1;

        assert!(relocated.certified(&secrets).is_err());
    }

    #[tokio::test]
    async fn failures_accumulate_and_a_success_clears_them() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let id = reserve(&db, &names()).await.unwrap();

        assert_eq!(
            record_failure(&db, id, "connection refused").await.unwrap(),
            1
        );
        assert_eq!(
            record_failure(&db, id, "connection refused").await.unwrap(),
            2
        );

        let failed = load(&db, &names()).await.unwrap().unwrap();
        assert_eq!(failed.last_error.as_deref(), Some("connection refused"));
        assert!(failed.last_attempt_at.is_some());

        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = rcgen::CertificateParams::new(names())
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let now = Utc::now();

        store(
            &db,
            &secrets,
            id,
            certificate.pem(),
            &key.serialize_der(),
            (now, now + chrono::Duration::days(90)),
            AcmeChallenge::TlsAlpn01,
        )
        .await
        .unwrap();

        let recovered = load(&db, &names()).await.unwrap().unwrap();
        assert_eq!(recovered.attempts, 0);
        assert_eq!(recovered.last_error, None);
    }

    #[tokio::test]
    async fn a_row_never_renders_its_key() {
        let db = database().await;
        let id = reserve(&db, &names()).await.unwrap();
        let row = load(&db, &names()).await.unwrap().unwrap();

        assert_eq!(row.id, id);
        assert!(!format!("{row:?}").contains("BEGIN PRIVATE KEY"));
    }
}

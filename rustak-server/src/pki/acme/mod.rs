//! A publicly trusted certificate for the browser-facing listener.
//!
//! The Marti and stream listeners present certificates from this
//! installation's own authority, because their clients are devices we enrolled
//! and handed a truststore to. The public listener is the one a browser
//! reaches, and a browser trusts what a public authority signed — so `[acme]`
//! obtains one, renews it, and swaps it in without a restart.
//!
//! | File | What it answers |
//! |---|---|
//! | [`transport`] | reaching the directory over the server's own HTTP client |
//! | [`account`] | the account key: registration, reuse, sealing |
//! | [`order`] | one order, from new-order to a chain |
//! | [`challenge`] | proving control of a name, either way it can be proved |
//! | [`store`] | what an order produced, and what the last one cost |
//! | [`renew`] | when to order, and what one run does |
//!
//! # Why the resolver is process-wide
//!
//! The certificate the listener presents lives behind a
//! [`HotSwapCertResolver`], which is built where the listener is built —
//! [`web::tls::resolve`](crate::web::tls::resolve) — and needed where the
//! renewal runs, which is a queue job holding nothing but
//! [`Services`](crate::services::Services). There is exactly one public
//! listener in a process and the alternative was a handle threaded through
//! start-up, the services struct and the job context for the sake of one
//! `Arc`, so it is published here instead. Nothing reads it except the renewal
//! and the admin API, and both treat its absence — a listener that is not in
//! ACME mode — as "no certificate can be swapped in", not as a failure.

pub mod account;
pub mod challenge;
pub mod order;
pub mod renew;
pub mod store;
pub mod transport;

use std::sync::{Arc, RwLock};

use rustls::sign::CertifiedKey;

use rustak_core::prelude::*;

use crate::crypto::SecretStore;
use crate::db::Database;
use crate::pki::tls::HotSwapCertResolver;

pub use challenge::{HTTP01_PREFIX, Responder, routes as http01_routes};
pub use order::IssuedChain;
pub use renew::{CertState, Decision, backoff, decide, next_renewal, run, state, status};
pub use store::{AcmeCertificateRow, normalise};

/// The public listener's certificate resolver, once it has one.
static RESOLVER: RwLock<Option<Arc<HotSwapCertResolver>>> = RwLock::new(None);

/// Publishes the resolver the public listener was built with.
///
/// Called once, by [`web::tls`](crate::web::tls), and only for
/// `[web.public.tls] mode = "acme"`. Replacing it is allowed rather than
/// refused so that a test which builds a second listener is not fighting the
/// first one's leftovers.
pub fn publish_resolver(resolver: Arc<HotSwapCertResolver>) {
    if let Ok(mut held) = RESOLVER.write() {
        *held = Some(resolver);
    }
}

/// The published resolver, or [`None`] when this installation is not in ACME
/// mode.
pub fn resolver() -> Option<Arc<HotSwapCertResolver>> {
    RESOLVER.read().ok().and_then(|held| held.clone())
}

/// The stored certificate for these names, ready for rustls.
///
/// [`None`] when no order has finished yet, which is the state a listener
/// starts in the first time ACME is switched on.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the row cannot be read, or when
/// the stored key cannot be opened or does not match the stored chain.
pub async fn stored_certificate(
    db: &Database,
    secrets: &SecretStore,
    domains: &[String],
) -> Result<Option<Arc<CertifiedKey>>, Error> {
    let Some(row) = store::load(db, domains).await? else {
        return Ok(None);
    };

    if !row.is_issued() {
        return Ok(None);
    }

    row.certified(secrets).map(Some)
}

/// Installs the stored certificate into `resolver`, if both exist.
///
/// Reports whether the swap happened: a renewal that could not reach a
/// resolver has still obtained a certificate, and the next restart will serve
/// it — which is worth a warning rather than an error.
///
/// # Errors
///
/// As [`stored_certificate`].
pub async fn install(
    db: &Database,
    secrets: &SecretStore,
    domains: &[String],
    resolver: Option<&Arc<HotSwapCertResolver>>,
) -> Result<bool, Error> {
    let Some(certified) = stored_certificate(db, secrets, domains).await? else {
        return Ok(false);
    };

    let Some(resolver) = resolver else {
        warn!(
            "A certificate was obtained, but the public listener is not in ACME mode, so it will \
             not be presented until `[web.public.tls] mode = \"acme\"` is set and rustak restarted."
        );

        return Ok(false);
    };

    resolver.install(certified);

    info!(
        domains = ?domains,
        "The public listener is now presenting the renewed certificate."
    );

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::config::AcmeChallenge;

    async fn issued(db: &Database, secrets: &SecretStore, domains: &[String]) {
        let id = store::reserve(db, domains).await.unwrap();
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = rcgen::CertificateParams::new(domains.to_vec())
            .unwrap()
            .self_signed(&key)
            .unwrap();

        store::store(
            db,
            secrets,
            id,
            certificate.pem(),
            &key.serialize_der(),
            (
                chrono::Utc::now(),
                chrono::Utc::now() + chrono::Duration::days(90),
            ),
            AcmeChallenge::TlsAlpn01,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_installation_with_no_order_behind_it_has_nothing_to_present() {
        let db = Database::open_in_memory().await.unwrap();
        let secrets = SecretStore::ephemeral();
        let domains = vec!["tak.example.com".to_string()];

        assert!(
            stored_certificate(&db, &secrets, &domains)
                .await
                .unwrap()
                .is_none()
        );

        // Reserved but never issued: still nothing to present.
        store::reserve(&db, &domains).await.unwrap();
        assert!(
            stored_certificate(&db, &secrets, &domains)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_renewal_swaps_the_listener_over_without_a_restart() {
        let db = Database::open_in_memory().await.unwrap();
        let secrets = SecretStore::ephemeral();
        let domains = vec!["tak.example.com".to_string()];
        let resolver = HotSwapCertResolver::new(None);

        issued(&db, &secrets, &domains).await;

        assert!(!resolver.is_ready());
        assert!(
            install(&db, &secrets, &domains, Some(&resolver))
                .await
                .unwrap()
        );
        assert!(resolver.is_ready());
    }

    #[tokio::test]
    async fn a_certificate_obtained_without_a_listener_to_serve_it_is_not_an_error() {
        let db = Database::open_in_memory().await.unwrap();
        let secrets = SecretStore::ephemeral();
        let domains = vec!["tak.example.com".to_string()];

        issued(&db, &secrets, &domains).await;

        assert!(!install(&db, &secrets, &domains, None).await.unwrap());
    }

    #[test]
    fn the_published_resolver_is_what_the_renewal_finds() {
        let resolver = HotSwapCertResolver::new(None);
        publish_resolver(Arc::clone(&resolver));

        assert!(Arc::ptr_eq(&super::resolver().unwrap(), &resolver));
    }
}

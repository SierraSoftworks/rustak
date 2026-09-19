//! What the public listener presents, and the one way it presents nothing.
//!
//! `[web.public] tls.mode` picks between four answers:
//!
//! | mode | what happens |
//! |---|---|
//! | `internal` | our own authority issues a certificate for the configured host names. The default. |
//! | `files` | a chain and a key are read from disk, behind the same swappable resolver, and re-read when they change. |
//! | `acme` | the last certificate an ACME order produced, behind a resolver that can be swapped without a restart. |
//! | `none` | plaintext, and only when `allow_insecure_http` also says so. |
//!
//! # Why `acme` and `files` start with a certificate nobody trusts
//!
//! The first order cannot be placed until the listener is up: `tls-alpn-01` is
//! answered *by* that listener on :443, and `http-01` is answered by a route on
//! it. So a listener with no ACME certificate yet is bound with an internally
//! issued one — the same certificate `mode = "internal"` would serve — and the
//! renewal job replaces it through the resolver as soon as the order finishes.
//! A browser sees one warning on a brand-new installation for as long as the
//! order takes, rather than a listener that will not start until a certificate
//! it cannot obtain has been obtained.
//!
//! `files` does the same thing for the same reason: the pair is usually
//! rendered by a sidecar that starts alongside rustak, so a listener that
//! refused to bind without it would make every deploy a race. See
//! [`pki::tls::files`](crate::pki::tls::files); `require_files_at_start = true`
//! asks for the fail-fast behaviour instead.
//!
//! The resolver is published on the way out — into the caller's
//! [`AcmeState`] for `acme`, and to [`pki::tls::files`](crate::pki::tls::files)
//! for `files` — because the job which replaces the certificate is not holding
//! one.

use std::sync::Arc;

use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use crate::config::{Config, TlsMode};
use crate::crypto::SecretStore;
use crate::db::Database;
use crate::pki::CaMaterial;
use crate::prelude::*;
use crate::services::AcmeState;

/// What a listener will present, if anything.
pub type PublicTls = Option<Arc<ServerConfig>>;

/// Builds the public listener's TLS configuration.
///
/// `ca` is this installation's authority, which `internal` needs and the other
/// modes do not. `acme` is the context's [`AcmeState`], which `acme` mode
/// publishes its resolver into so that the renewal job can swap a certificate
/// in without a restart; the other modes leave it alone.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error for a configuration we will not serve —
/// plaintext without the explicit opt-in, `acme` with nothing to bootstrap
/// from, `files` pointing at something unreadable — and a
/// [`human_errors::Kind::System`] error when the internal certificate cannot be
/// issued or read back.
#[instrument("web.tls.resolve", skip_all, err(Display))]
pub async fn resolve(
    config: &Config,
    db: &Database,
    secrets: &SecretStore,
    ca: Option<&CaMaterial>,
    acme_state: &AcmeState,
) -> Result<PublicTls, Error> {
    match config.web.public.tls.mode {
        TlsMode::None => plaintext(config).map(|()| None),
        TlsMode::Files => files(config, db, secrets, ca).await.map(Some),
        TlsMode::Acme => acme(config, db, secrets, ca, acme_state).await.map(Some),
        TlsMode::Internal => internal(config, db, secrets, ca).await.map(Some),
    }
}

/// The names the internal certificate should cover.
///
/// `[server] domains` and `[acme] domains` are included automatically, because
/// an operator who has written a host name once should not have to write it in
/// three places to be reachable on it. `stored` carries what the setup wizard
/// recorded, which is the only source an installation configured entirely
/// through the browser has.
pub fn server_names(config: &Config, stored: &[String]) -> Vec<String> {
    let mut names = Vec::new();

    for name in config
        .server
        .domains
        .iter()
        .chain(stored)
        .chain(config.acme.domains.iter())
        .chain(config.pki.server_names.iter())
    {
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();

        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }

    names
}

/// Refuses plaintext unless the operator has said it twice.
fn plaintext(config: &Config) -> Result<(), Error> {
    if config.web.public.allow_insecure_http {
        warn!(
            "The public listener is serving plaintext HTTP. Every token and passkey assertion it \
             carries is readable by anything on the network."
        );

        return Ok(());
    }

    Err(human_errors::user(
        "The public listener is configured for no TLS, which would carry every token and credential in the clear.",
        &[
            "Set [web.public] tls.mode to 'internal' or 'files'.",
            "If this really is a development machine, set [web.public] allow_insecure_http = true as well.",
        ],
    ))
}

/// A certificate obtained from an ACME authority, behind a live resolver.
///
/// The last issued certificate if there is one; otherwise an internally issued
/// one, so that the listener binds and the first order has something to be
/// answered on. Either way the resolver is what the listener consults, and the
/// renewal job swaps the real certificate in through it.
async fn acme(
    config: &Config,
    db: &Database,
    secrets: &SecretStore,
    ca: Option<&CaMaterial>,
    acme_state: &AcmeState,
) -> Result<Arc<ServerConfig>, Error> {
    let domains = crate::pki::acme::normalise(config.acme.domains(&config.server));

    if domains.is_empty() {
        return Err(human_errors::user(
            "ACME has no names to order a certificate for.",
            &[
                "Set [acme] domains, or [server] domains, to the public host names this server answers to.",
            ],
        ));
    }

    let certified = match crate::pki::acme::stored_certificate(db, secrets, &domains).await? {
        Some(certified) => {
            info!(domains = ?domains, "Serving the certificate the last ACME order produced.");

            certified
        }
        None => {
            warn!(
                domains = ?domains,
                "No ACME certificate has been issued yet, so the public listener starts with one \
                 from this installation's own authority. Browsers will warn until the first order \
                 completes; watch GET /api/v1/settings/tls for it."
            );

            bootstrap(config, db, secrets, ca).await?
        }
    };

    let resolver = crate::pki::HotSwapCertResolver::new(Some(certified));
    acme_state.publish_resolver(Arc::clone(&resolver));

    Ok(Arc::new(crate::pki::tls::public_server_config(resolver)))
}

/// The certificate an ACME listener binds with before its first order.
async fn bootstrap(
    config: &Config,
    db: &Database,
    secrets: &SecretStore,
    ca: Option<&CaMaterial>,
) -> Result<Arc<rustls::sign::CertifiedKey>, Error> {
    let certificate = internal_certificate(config, db, secrets, ca).await?;

    install_crypto_provider();

    rustls::sign::CertifiedKey::from_der(
        certificate.chain,
        certificate.key,
        &rustls::crypto::aws_lc_rs::default_provider(),
    )
    .map(Arc::new)
    .or_system_err(&["This is unexpected; please report it with the surrounding log entries."])
}

/// A chain and a key read from disk, behind a resolver the reload job swaps.
///
/// A pair that is not there yet is a warning and a bootstrap certificate, not
/// a refusal to start — see the module documentation — unless
/// `require_files_at_start` says otherwise.
async fn files(
    config: &Config,
    db: &Database,
    secrets: &SecretStore,
    ca: Option<&CaMaterial>,
) -> Result<Arc<ServerConfig>, Error> {
    let tls = &config.web.public.tls;

    let (Some(cert_file), Some(key_file)) = (tls.cert_file.as_ref(), tls.key_file.as_ref()) else {
        return Err(human_errors::user(
            "[web.public] tls.mode is 'files', but cert_file and key_file were not both set.",
            &["Set both keys to the full chain and the private key, in PEM."],
        ));
    };

    install_crypto_provider();

    let (initial, loaded, failure) = match crate::pki::tls::files::load(cert_file, key_file) {
        Ok(pair) => {
            info!(
                certificates = pair.certificates,
                not_after = ?pair.not_after,
                "Loaded the public certificate from disk."
            );

            (Arc::clone(&pair.certified), Some(pair), None)
        }
        Err(err) if tls.require_files_at_start => return Err(err),
        Err(err) => {
            warn!(
                cert_file = %cert_file.display(),
                key_file = %key_file.display(),
                reason = %err.description(),
                interval = ?tls.reload_every(),
                "The public certificate files are not usable yet, so the listener starts with one \
                 from this installation's own authority and will swap them in as soon as they \
                 appear. Watch GET /api/v1/settings/tls for it."
            );

            (
                bootstrap(config, db, secrets, ca).await?,
                None,
                Some(err.description()),
            )
        }
    };

    let resolver = crate::pki::HotSwapCertResolver::new(Some(initial));

    crate::pki::tls::files::publish(crate::pki::tls::files::FilesCertificate::new(
        cert_file.clone(),
        key_file.clone(),
        Arc::clone(&resolver),
        loaded,
        failure,
    ));

    Ok(Arc::new(crate::pki::tls::public_server_config(resolver)))
}

/// A certificate this installation issued itself.
async fn internal(
    config: &Config,
    db: &Database,
    secrets: &SecretStore,
    ca: Option<&CaMaterial>,
) -> Result<Arc<ServerConfig>, Error> {
    let certificate = internal_certificate(config, db, secrets, ca).await?;

    build(certificate.chain, certificate.key)
}

/// The internally issued certificate, which two modes want for two reasons:
/// `internal` serves it, and `acme` binds with it until its first order lands.
async fn internal_certificate(
    config: &Config,
    db: &Database,
    secrets: &SecretStore,
    ca: Option<&CaMaterial>,
) -> Result<crate::pki::ServerCertificate, Error> {
    let ca = ca.ok_or_else(|| {
        human_errors::system(
            "The certificate authority had not been loaded when the public listener was built.",
            &["This is unexpected; please report it with the surrounding log entries."],
        )
    })?;

    let stored = crate::identity::settings::stored(db).await?.domains;
    let names = server_names(config, &stored);

    let certificate = crate::pki::server_cert::load_or_issue(
        db,
        secrets,
        &config.pki,
        ca,
        &names,
        &config.pki.server_ips,
    )
    .await?;

    warn_if_untrusted(&names);

    Ok(certificate)
}

/// Says once, at start-up, what an internally issued certificate means.
fn warn_if_untrusted(names: &[String]) {
    info!(
        names = ?names,
        "The public listener presents a certificate from this installation's own authority; \
         browsers will warn until the authority is installed, and enrolled devices will not."
    );
}

/// Installs rustls' cryptography, once, if start-up has not already.
///
/// `run()` does this before anything else; it is repeated here because
/// `ServerConfig::builder` *panics* without a provider, and a test that builds
/// a listener without going through start-up would otherwise fail in a way that
/// says nothing about what it was testing. Installing twice is a no-op.
fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();

    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

/// One rustls configuration, with client certificates not asked for.
///
/// The public listener is where browsers and enrolling devices arrive, and
/// neither has a certificate yet. Client certificates belong on the Marti and
/// stream listeners, which demand them.
///
/// Only `internal` builds a listener this way. The two modes whose certificate
/// changes while the server runs — `acme` and `files` — go through a resolver
/// instead, because a `ServerConfig` is immutable once built.
fn build(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<ServerConfig>, Error> {
    install_crypto_provider();

    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key)
        .map(Arc::new)
        .wrap_user_err(
            "The certificate this installation issued itself and its private key do not go together.",
            &["This is unexpected; please report it with the surrounding log entries."],
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn config(mode: TlsMode) -> Config {
        let mut config = Config::default();
        config.web.public.tls.mode = mode;
        config.server.domains = vec!["tak.example.com".to_string()];
        config
    }

    #[tokio::test]
    async fn plaintext_takes_a_second_deliberate_statement() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let mut config = config(TlsMode::None);

        let refused = resolve(&config, &db, &secrets, None, &AcmeState::new())
            .await
            .unwrap_err();
        assert!(refused.is(human_errors::Kind::User));

        config.web.public.allow_insecure_http = true;
        assert!(
            resolve(&config, &db, &secrets, None, &AcmeState::new())
                .await
                .unwrap()
                .is_none()
        );
    }

    /// A configuration whose authority can actually be loaded, for the modes
    /// that need one. ECDSA because an RSA authority is a second of bignum
    /// arithmetic per test.
    async fn with_authority(
        db: &Database,
        secrets: &SecretStore,
        mode: TlsMode,
        directory: &std::path::Path,
    ) -> (Config, crate::pki::CaMaterial) {
        let base = config(mode);
        let config = Config {
            pki: crate::config::PkiConfig {
                key_type: crate::config::KeyType::EcdsaP256,
                ..base.pki.clone()
            },
            ..base
        };

        let ca = crate::pki::load_or_create_root_ca(db, secrets, &config.pki, directory)
            .await
            .unwrap();

        (config, ca)
    }

    #[tokio::test]
    async fn an_acme_listener_binds_on_the_internal_certificate_before_its_first_order() {
        // The whole reason the first order is possible at all: :443 has to be
        // answering before the authority can validate anything on it.
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let directory = tempfile::tempdir().unwrap();
        let (mut config, ca) = with_authority(&db, &secrets, TlsMode::Acme, directory.path()).await;
        config.acme.enabled = true;
        config.acme.domains = vec!["tak.example.com".to_string()];

        let state = AcmeState::new();

        assert!(
            resolve(&config, &db, &secrets, Some(&ca), &state)
                .await
                .unwrap()
                .is_some()
        );

        let resolver = state
            .resolver()
            .expect("the renewal has to find the resolver");
        assert!(
            resolver.is_ready(),
            "the listener must present something, or every handshake fails until the order lands",
        );
    }

    #[tokio::test]
    async fn an_acme_listener_serves_the_certificate_the_last_order_produced() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let directory = tempfile::tempdir().unwrap();
        let (mut config, ca) = with_authority(&db, &secrets, TlsMode::Acme, directory.path()).await;
        config.acme.enabled = true;
        config.acme.domains = vec!["acme.example.com".to_string()];

        let domains = vec!["acme.example.com".to_string()];
        let id = crate::pki::acme::store::reserve(&db, &domains)
            .await
            .unwrap();
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let issued = rcgen::CertificateParams::new(domains.clone())
            .unwrap()
            .self_signed(&key)
            .unwrap();

        crate::pki::acme::store::store(
            &db,
            &secrets,
            id,
            issued.pem(),
            &key.serialize_der(),
            (
                chrono::Utc::now(),
                chrono::Utc::now() + chrono::Duration::days(90),
            ),
            crate::config::AcmeChallenge::TlsAlpn01,
        )
        .await
        .unwrap();

        let state = AcmeState::new();

        assert!(
            resolve(&config, &db, &secrets, Some(&ca), &state)
                .await
                .unwrap()
                .is_some()
        );

        let presented = state.resolver().unwrap().current().unwrap();
        assert_eq!(
            presented.end_entity_cert().unwrap().as_ref(),
            issued.der().as_ref(),
            "a restart must not throw away a certificate we already paid rate limit for",
        );
    }

    #[tokio::test]
    async fn an_acme_mode_with_no_names_says_which_keys_to_set() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let mut config = config(TlsMode::Acme);
        config.server.domains = Vec::new();
        config.acme.domains = Vec::new();

        let refused = resolve(&config, &db, &secrets, None, &AcmeState::new())
            .await
            .unwrap_err();

        assert!(refused.is(human_errors::Kind::User));
        assert!(refused.description().contains("names"));
    }

    #[tokio::test]
    async fn the_internal_mode_issues_from_this_installations_own_authority() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let directory = tempfile::tempdir().unwrap();
        let config = config(TlsMode::Internal);
        let pki = crate::config::PkiConfig {
            key_type: crate::config::KeyType::EcdsaP256,
            ..config.pki.clone()
        };
        let config = Config { pki, ..config };

        let ca = crate::pki::load_or_create_root_ca(&db, &secrets, &config.pki, directory.path())
            .await
            .unwrap();

        assert!(
            resolve(&config, &db, &secrets, Some(&ca), &AcmeState::new())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_files_mode_that_names_no_files_says_which_keys_to_set() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();

        let refused = resolve(
            &config(TlsMode::Files),
            &db,
            &secrets,
            None,
            &AcmeState::new(),
        )
        .await
        .unwrap_err();

        assert!(refused.description().contains("cert_file"));
    }

    #[tokio::test]
    async fn a_files_mode_promised_its_files_and_not_given_them_names_the_path() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(TlsMode::Files);

        config.web.public.tls.cert_file = Some(directory.path().join("missing.crt"));
        config.web.public.tls.key_file = Some(directory.path().join("missing.key"));
        config.web.public.tls.require_files_at_start = true;

        let refused = resolve(&config, &db, &secrets, None, &AcmeState::new())
            .await
            .unwrap_err();

        assert!(refused.description().contains("missing.crt"));
    }

    #[tokio::test]
    async fn a_files_mode_whose_pair_has_not_been_written_yet_still_binds() {
        // The Nomad deployment this exists for: the sidecar renders the pair
        // after the task starts, so refusing to bind would make the first
        // deploy a race nobody can win.
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let data = tempfile::tempdir().unwrap();
        let pki = tempfile::tempdir().unwrap();
        let (mut config, ca) = with_authority(&db, &secrets, TlsMode::Files, pki.path()).await;

        let cert_file = data.path().join("fullchain.pem");
        let key_file = data.path().join("privkey.pem");
        config.web.public.tls.cert_file = Some(cert_file.clone());
        config.web.public.tls.key_file = Some(key_file.clone());

        assert!(
            resolve(&config, &db, &secrets, Some(&ca), &AcmeState::new())
                .await
                .unwrap()
                .is_some(),
            "a missing pair must be waited for, not fatal",
        );

        let files =
            crate::pki::tls::files::for_config(&config).expect("the reload job has to find it");
        assert!(
            files.status(&[]).state == rustak_api::TlsCertificateState::Missing,
            "the status has to say the listener is on its bootstrap certificate",
        );

        // And when the sidecar finally writes them, the next look serves them.
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let certificate = rcgen::CertificateParams::new(vec!["tak.example.com".to_string()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        std::fs::write(&cert_file, certificate.pem()).unwrap();
        std::fs::write(&key_file, key.serialize_pem()).unwrap();

        assert!(matches!(
            files.reload(),
            crate::pki::tls::files::Outcome::Swapped { .. }
        ));
        assert_eq!(
            files.status(&[]).state,
            rustak_api::TlsCertificateState::Valid
        );
    }

    #[tokio::test]
    async fn a_chain_and_key_on_disk_are_served() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let directory = tempfile::tempdir().unwrap();

        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params =
            rcgen::CertificateParams::new(vec!["tak.example.com".to_string()]).unwrap();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "tak.example.com");
        let certificate = params.self_signed(&key).unwrap();

        let cert_file = directory.path().join("chain.pem");
        let key_file = directory.path().join("key.pem");
        std::fs::write(&cert_file, certificate.pem()).unwrap();
        std::fs::write(&key_file, key.serialize_pem()).unwrap();

        let mut config = config(TlsMode::Files);
        config.web.public.tls.cert_file = Some(cert_file);
        config.web.public.tls.key_file = Some(key_file);

        assert!(
            resolve(&config, &db, &secrets, None, &AcmeState::new())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn every_place_a_host_name_can_be_written_ends_up_in_the_certificate() {
        let mut config = Config::default();
        config.server.domains = vec!["tak.example.com".to_string()];
        config.acme.domains = vec!["TAK.example.com".to_string()];
        config.pki.server_names = vec!["tak.lan".to_string()];

        assert_eq!(
            server_names(&config, &["wizard.example.com".to_string()]),
            vec![
                "tak.example.com".to_string(),
                "wizard.example.com".to_string(),
                "tak.lan".to_string(),
            ],
            "the same host written twice is one name, and the canonical one comes first",
        );
    }
}

//! What the public listener presents, and the one way it presents nothing.
//!
//! `[web.public] tls.mode` picks between four answers:
//!
//! | mode | what happens |
//! |---|---|
//! | `internal` | our own authority issues a certificate for the configured host names. The default. |
//! | `files` | a chain and a key are read from disk. |
//! | `acme` | refused, with a message saying when it arrives. |
//! | `none` | plaintext, and only when `allow_insecure_http` also says so. |
//!
//! # Why `acme` is an error rather than a fallback
//!
//! Silently serving an internally issued certificate where ACME was asked for
//! would look like it worked: the listener comes up, the browser complains, and
//! the operator spends an afternoon on a certificate authority they did not
//! choose. A start-up failure that names the milestone costs them a minute.

use std::sync::Arc;

use rustls::ServerConfig;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};

use crate::config::{Config, TlsMode};
use crate::crypto::SecretStore;
use crate::db::Database;
use crate::pki::CaMaterial;
use crate::prelude::*;

/// What a listener will present, if anything.
pub type PublicTls = Option<Arc<ServerConfig>>;

/// Builds the public listener's TLS configuration.
///
/// `ca` is this installation's authority, which `internal` needs and the other
/// modes do not.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error for a configuration we will not serve —
/// plaintext without the explicit opt-in, `acme` before M2, `files` pointing at
/// something unreadable — and a [`human_errors::Kind::System`] error when the
/// internal certificate cannot be issued or read back.
#[instrument("web.tls.resolve", skip_all, err(Display))]
pub async fn resolve(
    config: &Config,
    db: &Database,
    secrets: &SecretStore,
    ca: Option<&CaMaterial>,
) -> Result<PublicTls, Error> {
    match config.web.public.tls.mode {
        TlsMode::None => plaintext(config).map(|()| None),
        TlsMode::Files => from_files(config).map(Some),
        TlsMode::Acme => Err(acme_not_yet()),
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

/// The failure for a mode we do not implement yet.
fn acme_not_yet() -> Error {
    human_errors::user(
        "ACME arrives in M2, so this server cannot obtain a public certificate for itself yet.",
        &[
            "Set [web.public] tls.mode to 'internal' to use this installation's own authority.",
            "Set it to 'files' and point cert_file and key_file at a certificate you already hold.",
        ],
    )
}

/// A chain and a key read from disk.
fn from_files(config: &Config) -> Result<Arc<ServerConfig>, Error> {
    let tls = &config.web.public.tls;

    let (Some(cert_file), Some(key_file)) = (tls.cert_file.as_ref(), tls.key_file.as_ref()) else {
        return Err(human_errors::user(
            "[web.public] tls.mode is 'files', but cert_file and key_file were not both set.",
            &["Set both keys to the full chain and the private key, in PEM."],
        ));
    };

    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_file)
        .and_then(|iter| iter.collect())
        .map_err(|err| {
            human_errors::user(
                format!(
                    "We could not read the certificate chain at {}: {err}",
                    cert_file.display()
                ),
                &["Check that the file exists, is readable, and contains PEM certificates."],
            )
        })?;

    if chain.is_empty() {
        return Err(human_errors::user(
            format!("{} contains no certificates.", cert_file.display()),
            &["Point cert_file at the full chain, leaf first, in PEM."],
        ));
    }

    let key = PrivateKeyDer::from_pem_file(key_file).map_err(|err| {
        human_errors::user(
            format!(
                "We could not read the private key at {}: {err}",
                key_file.display()
            ),
            &["Check that the file exists, is readable, and contains a PEM private key."],
        )
    })?;

    info!(
        certificates = chain.len(),
        "Loaded the public certificate from disk."
    );

    build(chain, key)
}

/// A certificate this installation issued itself.
async fn internal(
    config: &Config,
    db: &Database,
    secrets: &SecretStore,
    ca: Option<&CaMaterial>,
) -> Result<Arc<ServerConfig>, Error> {
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

    build(certificate.chain, certificate.key)
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
            "The certificate and private key for the public listener do not go together.",
            &[
                "Check that key_file is the key for cert_file's leaf certificate.",
                "Check that cert_file lists the leaf certificate first.",
            ],
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

        let refused = resolve(&config, &db, &secrets, None).await.unwrap_err();
        assert!(refused.is(human_errors::Kind::User));

        config.web.public.allow_insecure_http = true;
        assert!(
            resolve(&config, &db, &secrets, None)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn asking_for_acme_says_when_it_arrives_rather_than_quietly_doing_something_else() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();

        let refused = resolve(&config(TlsMode::Acme), &db, &secrets, None)
            .await
            .unwrap_err();

        assert!(refused.is(human_errors::Kind::User));
        assert!(refused.description().contains("M2"));
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
            resolve(&config, &db, &secrets, Some(&ca))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn a_files_mode_that_names_no_files_says_which_keys_to_set() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();

        let refused = resolve(&config(TlsMode::Files), &db, &secrets, None)
            .await
            .unwrap_err();

        assert!(refused.description().contains("cert_file"));
    }

    #[tokio::test]
    async fn a_files_mode_pointing_at_nothing_names_the_path() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let directory = tempfile::tempdir().unwrap();
        let mut config = config(TlsMode::Files);

        config.web.public.tls.cert_file = Some(directory.path().join("missing.crt"));
        config.web.public.tls.key_file = Some(directory.path().join("missing.key"));

        let refused = resolve(&config, &db, &secrets, None).await.unwrap_err();

        assert!(refused.description().contains("missing.crt"));
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
            resolve(&config, &db, &secrets, None)
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

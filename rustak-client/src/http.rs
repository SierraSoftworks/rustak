//! The HTTPS half of a sidecar: one `reqwest` client, built from the same
//! identity the CoT stream uses.
//!
//! [`marti`](crate::marti) and [`control`](crate::control) are two APIs on the
//! same server, reached with the same certificate and verified against the same
//! truststore, so they share a client — which also shares its connection pool,
//! its DNS cache and its TLS session tickets.
//!
//! # The truststore replaces the platform's roots rather than joining them
//!
//! For the reason [`TlsIdentity`](crate::stream::TlsIdentity) gives: a TAK
//! deployment's server certificate is issued by the deployment's own CA, and
//! keeping the public roots alongside it would mean trusting every public CA to
//! impersonate the server. A sidecar with no `[service] truststore` gets the
//! platform's roots, which is right for a server behind a public certificate
//! (ACME) and is the only case where they are what was meant.
//!
//! # Errors are the operator's language
//!
//! Everything here answers [`human_errors::Kind::User`] for what an operator can
//! fix — a file that is not there, a URL that will not parse, a handshake that
//! failed — and reserves `Kind::System` for what only we could have got wrong.

use std::path::Path;
use std::time::Duration;

use rustak_core::prelude::*;
use rustak_core::service::ServiceIdentity;
use url::Url;

/// How long any one control or Marti request may take.
///
/// Generous, because a mission package upload over a field link is slow and a
/// sidecar that gave up on one would retry it forever; short enough that a
/// heartbeat cannot pile up behind a wedged connection for a whole tick.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// What a sidecar calls itself to the server it talks to.
const USER_AGENT: &str = concat!("SierraSoftworks/rustak-client/", env!("CARGO_PKG_VERSION"));

/// Advice for a certificate, key or truststore that could not be read.
const ADVICE_MATERIAL: &[&str] = &[
    "Check the paths under [service] in the sidecar's configuration file.",
    "The certificate and key are the ones enrolment wrote; the truststore is the server's CA.",
];

/// Builds the HTTPS client a sidecar reaches both APIs through.
///
/// The client certificate is attached when the identity carries both halves, and
/// left off when it does not — a sidecar that has only a service token can still
/// call the control API, which is the whole reason that token exists.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when a configured file cannot be read or
/// is not the PEM it claims to be, and a [`human_errors::Kind::System`] error
/// when the TLS backend will not initialise.
pub fn client(identity: &ServiceIdentity, timeout: Duration) -> Result<reqwest::Client, Error> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(timeout);

    if let Some(truststore) = identity.truststore() {
        // `tls_certs_only`, not `add_root_certificate`: the configured
        // truststore *replaces* the platform's roots rather than joining them.
        // See the module documentation for why that is the only safe reading of
        // "[service] truststore".
        builder = builder.tls_certs_only(roots(truststore)?);
    }

    if let (Some(certificate), Some(key)) = (identity.certificate(), identity.key()) {
        builder = builder.identity(client_identity(certificate, key)?);
    }

    builder.build().or_system_err(&[
        "This usually means the TLS backend could not be initialised.",
        "Please report this issue to the development team via GitHub.",
    ])
}

/// Every certificate in a truststore file.
fn roots(path: &Path) -> Result<Vec<reqwest::Certificate>, Error> {
    let pem = read(path, "truststore")?;

    let roots = reqwest::Certificate::from_pem_bundle(&pem).map_err(|err| {
        human_errors::user(
            format!(
                "The truststore at '{}' is not a PEM bundle we can read ({err}).",
                path.display()
            ),
            ADVICE_MATERIAL,
        )
    })?;

    if roots.is_empty() {
        return Err(human_errors::user(
            format!(
                "The truststore at '{}' holds no certificates.",
                path.display()
            ),
            ADVICE_MATERIAL,
        ));
    }

    Ok(roots)
}

/// The client certificate and its key, as `reqwest` wants them: one PEM blob.
fn client_identity(certificate: &Path, key: &Path) -> Result<reqwest::Identity, Error> {
    let mut pem = read(certificate, "client certificate")?;
    pem.extend_from_slice(b"\n");
    pem.extend_from_slice(&read(key, "private key")?);

    reqwest::Identity::from_pem(&pem).map_err(|err| {
        human_errors::user(
            format!(
                "The client certificate at '{}' and its key do not make a usable identity ({err}).",
                certificate.display()
            ),
            ADVICE_MATERIAL,
        )
    })
}

/// Reads a file, naming what it was meant to be when it is not there.
fn read(path: &Path, what: &str) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|err| {
        human_errors::user(
            format!("Could not read the {what} at '{}': {err}.", path.display()),
            ADVICE_MATERIAL,
        )
    })
}

/// Parses a base URL and refuses one a request could never be built from.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error for anything that is not an absolute
/// `http`/`https` URL.
pub fn base_url(raw: &str, setting: &str) -> Result<Url, Error> {
    let trimmed = raw.trim().trim_end_matches('/');
    let url = Url::parse(trimmed).map_err(|err| {
        human_errors::user(
            format!("'{raw}' is not a URL we can call ({err})."),
            &[
                "Write it in full, including the scheme and the port.",
                "For example: https://tak.example.com:8443",
            ],
        )
    })?;

    if !matches!(url.scheme(), "http" | "https") {
        return Err(human_errors::user(
            format!("[server] {setting} is '{raw}', which is not an http or https URL."),
            &["For example: https://tak.example.com:8443"],
        ));
    }

    Ok(url)
}

/// Appends a path to a base URL, keeping any path the base already carries.
///
/// `Url::join` would discard it — a base of `https://host/tak` joined with
/// `/Marti/api/version` gives `https://host/Marti/api/version` — and a rustak
/// server behind a reverse proxy with a prefix is an ordinary deployment.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the result will not parse, which
/// would mean `path` was not a path.
pub fn endpoint(base: &Url, path: &str) -> Result<Url, Error> {
    let joined = format!(
        "{}/{}",
        base.as_str().trim_end_matches('/'),
        path.trim_start_matches('/')
    );

    Url::parse(&joined).or_system_err(&["Please report this issue via GitHub."])
}

/// Turns a transport failure into something an operator can act on.
///
/// Kept in one place because the three modules that make requests would
/// otherwise each invent their own phrasing for "the server did not answer".
pub fn transport(err: reqwest::Error, what: &str) -> Error {
    human_errors::user(
        format!("Could not {what}: {err}."),
        &[
            "Check that the server is reachable and that [server] names the right address.",
            "A TLS failure here usually means the truststore is not the server's CA.",
        ],
    )
}

#[cfg(test)]
mod tests {
    use rustak_core::identity::ServiceName;

    use super::*;

    fn identity() -> ServiceIdentity {
        ServiceIdentity::new(ServiceName::parse("weather").unwrap())
    }

    #[test]
    fn a_sidecar_with_only_a_token_still_gets_a_client() {
        // The case the service token exists for: a plugin that has not enrolled
        // yet has no certificate, and must still be able to call the control
        // API to find out what it should be doing.
        assert!(client(&identity(), DEFAULT_TIMEOUT).is_ok());
    }

    #[test]
    fn a_truststore_that_is_not_there_is_the_operators_to_fix() {
        let identity = identity().with_truststore("/nonexistent/truststore.pem");

        let err = client(&identity, DEFAULT_TIMEOUT).unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.description().contains("truststore"), "{err}");
    }

    #[test]
    fn a_truststore_with_nothing_in_it_is_refused_rather_than_ignored() {
        // Silently falling back to the platform's roots would mean trusting
        // every public CA to impersonate the TAK server.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.pem");
        std::fs::write(&path, "# nothing here\n").unwrap();

        let err = client(&identity().with_truststore(&path), DEFAULT_TIMEOUT).unwrap_err();

        assert!(err.description().contains("no certificates"), "{err}");
    }

    #[test]
    fn a_base_url_keeps_the_path_a_reverse_proxy_added() {
        let base = base_url("https://tak.example.com:8443/tak/", "marti").unwrap();

        assert_eq!(
            endpoint(&base, "/Marti/api/version").unwrap().as_str(),
            "https://tak.example.com:8443/tak/Marti/api/version",
        );
    }

    #[test]
    fn a_url_that_is_not_one_names_the_setting_it_came_from() {
        for raw in ["tak.example.com:8443", "ssl://tak.example.com:8089", ""] {
            let err = base_url(raw, "marti").unwrap_err();

            assert!(err.is(human_errors::Kind::User), "{raw}: {err}");
        }
    }
}

//! The HTTPS half of a sidecar: a `reqwest` client per endpoint, built from the
//! same identity the CoT stream uses.
//!
//! # A sidecar's endpoints do not all present the same kind of certificate
//!
//! [`marti`](crate::marti) and [`control`](crate::control) are two APIs, and in
//! a real deployment they are two *listeners* with two different certificates.
//! `plan.md`'s listener table is the authority:
//!
//! | Endpoint | What it presents | [`Trust`] |
//! |---|---|---|
//! | the CoT stream (`:8089`) | the deployment's own CA, always | [`Trust::Internal`] |
//! | `[server] marti`, the mTLS listener (`:8443`) | the deployment's own CA, always | [`Trust::Internal`] |
//! | `[server] control`, the public listener (`:8446`) | ACME, operator-supplied files, **or** the internal CA | [`Trust::Public`] |
//!
//! So there is no single answer to "what does this sidecar trust", and every
//! caller here names its policy rather than taking one it might have got wrong
//! without ever finding out.
//!
//! # `Internal`: the truststore replaces the platform's roots
//!
//! For the reason [`TlsIdentity`](crate::stream::TlsIdentity) gives: a TAK
//! deployment's stream and mTLS certificates are issued by the deployment's own
//! CA, and keeping the public roots alongside them would mean trusting every
//! public CA to impersonate the server. A sidecar with no `[service] truststore`
//! gets the platform's roots, exactly as before.
//!
//! # `Public`: the platform's roots *and* the truststore
//!
//! The public listener is the one an operator is most likely to have put a
//! publicly issued certificate on, and enrolment writes rustak's own CA to
//! `[service] truststore` on the first start. Replacing the platform's roots
//! with that CA is what broke the first live deployment: the certificate,
//! the heartbeat and the token exchange all went to a listener holding a Let's
//! Encrypt certificate that the sidecar had just stopped trusting. Joining the
//! two covers both shapes, and costs nothing an internal-CA deployment cares
//! about — the public listener is the public listener.
//!
//! An operator who wants the public listener pinned to their own PKI, with the
//! platform's roots out of the picture, sets `[service] control_truststore`;
//! that **replaces** the set, and is the only way to say so.
//!
//! # Errors are the operator's language
//!
//! Everything here answers [`human_errors::Kind::User`] for what an operator can
//! fix — a file that is not there, a URL that will not parse, a handshake that
//! failed — and reserves `Kind::System` for what only we could have got wrong.
//! A transport failure is rendered down to its *cause*
//! ([`transport`]): "error sending request" says nothing an operator can act on,
//! and `invalid peer certificate: UnknownIssuer` says everything.

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

/// How long a connection may take to establish, whatever the body does next.
///
/// A long-lived response has no total deadline, so this is the only thing
/// standing between a sidecar and a black-holed SYN: a server that is not there
/// must be found out about in seconds rather than never.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the server-event feed may go without a single byte.
///
/// The feed is not a request: it is a body read for hours, and
/// [`DEFAULT_TIMEOUT`] applied to it cut every stream at thirty seconds —
/// 74 rustak log lines in two and a half minutes from two idle sidecars, and a
/// reopening every 31 seconds that an operator had no way to read as anything
/// but a fault.
///
/// Liveness comes from the server's own SSE keep-alive comment instead, which
/// rustak writes into an idle feed every 20 seconds (`KEEPALIVE`, in the
/// server's `web::api::events`). Three missed keep-alives and a little slack is
/// a feed nothing is coming down, which is the point at which reopening it is
/// better than waiting.
pub const FEED_IDLE_TIMEOUT: Duration = Duration::from_secs(65);

/// What a sidecar calls itself to the server it talks to.
const USER_AGENT: &str = concat!("SierraSoftworks/rustak-client/", env!("CARGO_PKG_VERSION"));

/// How many links of a transport error's cause chain are rendered.
///
/// A handshake failure is three deep; anything longer is a chain that has
/// started repeating itself.
const MAX_CAUSES: usize = 6;

/// Advice for a certificate, key or truststore that could not be read.
const ADVICE_MATERIAL: &[&str] = &[
    "Check the paths under [service] in the sidecar's configuration file.",
    "The certificate and key are the ones enrolment wrote; the truststore is the server's CA.",
];

/// Advice for a request that never reached the server.
const ADVICE_TRANSPORT: &[&str] = &[
    "Check that the server is reachable and that [server] names the right address.",
    "A TLS failure here usually means the truststore is not the server's CA.",
    "[service] truststore verifies the CoT stream and [server] marti; [server] control is verified against the platform's roots *and* that truststore, unless [service] control_truststore pins it to a PKI of your own.",
];

/// Which roots a client verifies one endpoint's certificate against.
///
/// There is deliberately no [`Default`]: the two answers differ, getting it
/// wrong fails only at the handshake, and the module documentation is the table
/// of which endpoint is which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// The CoT stream and `[server] marti` — always the deployment's own CA.
    ///
    /// `[service] truststore` **replaces** the platform's roots.
    Internal,

    /// `[server] control`, the public listener — ACME, operator-supplied files
    /// or the internal CA, and a sidecar cannot know which.
    ///
    /// The platform's roots **and** `[service] truststore`, unless
    /// `[service] control_truststore` replaces both.
    Public,
}

/// The root store a [`Trust`] and an identity add up to.
///
/// Separated from [`client`] so that the decision is a pure function a test can
/// assert on without a TLS backend, a file or a server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Roots<'a> {
    /// Whatever the platform trusts, and nothing else.
    Platform,

    /// Only the certificates in this file.
    Only(&'a Path),

    /// The platform's roots, plus the certificates in this file.
    PlatformAnd(&'a Path),
}

/// What `identity` trusts when it calls an endpoint of this kind.
///
/// See the module documentation for why the two policies differ.
#[must_use]
pub fn roots_for(identity: &ServiceIdentity, trust: Trust) -> Roots<'_> {
    match trust {
        Trust::Internal => match identity.truststore() {
            Some(truststore) => Roots::Only(truststore),
            None => Roots::Platform,
        },
        // An operator who named a control truststore has pinned the public
        // listener deliberately, and "pinned" has to mean the platform's roots
        // are out of it or it pins nothing.
        Trust::Public => match (identity.control_truststore(), identity.truststore()) {
            (Some(pinned), _) => Roots::Only(pinned),
            (None, Some(truststore)) => Roots::PlatformAnd(truststore),
            (None, None) => Roots::Platform,
        },
    }
}

/// Builds the HTTPS client a sidecar reaches one kind of endpoint through.
///
/// `trust` is not optional and has no default: see [`Trust`].
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
pub fn client(
    identity: &ServiceIdentity,
    trust: Trust,
    timeout: Duration,
) -> Result<reqwest::Client, Error> {
    build(
        identity,
        trust,
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(timeout),
    )
}

/// Builds the client a long-lived response body is read through.
///
/// The difference from [`client`] is the *absence* of a total timeout.
/// `reqwest`'s `timeout` is a deadline on the whole exchange, body included, so
/// a client that carries one cannot hold a `text/event-stream` open for longer
/// than it — which is why the server-event feed was reopening every 31 seconds
/// against a 30 second timeout it had inherited from ordinary calls.
///
/// What bounds it instead: [`CONNECT_TIMEOUT`] on getting the connection up,
/// and `idle` — a *read* timeout, which `reqwest` resets on every byte that
/// arrives — on the silence afterwards. `idle` is a parameter rather than a
/// constant so a test can assert the behaviour in milliseconds instead of
/// minutes; production passes [`FEED_IDLE_TIMEOUT`].
///
/// # Errors
///
/// The same as [`client`].
pub fn feed_client(
    identity: &ServiceIdentity,
    trust: Trust,
    idle: Duration,
) -> Result<reqwest::Client, Error> {
    build(
        identity,
        trust,
        reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(idle),
    )
}

/// The half of a client that is the identity rather than the timeouts.
fn build(
    identity: &ServiceIdentity,
    trust: Trust,
    builder: reqwest::ClientBuilder,
) -> Result<reqwest::Client, Error> {
    let mut builder = builder.user_agent(USER_AGENT);

    builder = match roots_for(identity, trust) {
        Roots::Platform => builder,
        // `tls_certs_only`: these certificates *replace* the platform's roots.
        Roots::Only(path) => builder.tls_certs_only(certificates(path)?),
        // `tls_certs_merge`: they join them.
        Roots::PlatformAnd(path) => builder.tls_certs_merge(certificates(path)?),
    };

    if let (Some(certificate), Some(key)) = (identity.certificate(), identity.key()) {
        builder = builder.identity(client_identity(certificate, key)?);
    }

    builder.build().or_system_err(&[
        "This usually means the TLS backend could not be initialised.",
        "Please report this issue to the development team via GitHub.",
    ])
}

/// Every certificate in a truststore file.
fn certificates(path: &Path) -> Result<Vec<reqwest::Certificate>, Error> {
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

/// Whether this is an error [`transport`] built: the server was never reached.
///
/// A refusal carrying a status is the server *answering* — the link is up,
/// however unwelcome the answer — and only a failure to reach it at all is an
/// outage. The harness uses this to decide what a failed call says about the
/// control link as a whole, so that one endpoint answering `404` does not stop
/// a sidecar heartbeating into another that is perfectly well.
///
/// Matched on the advice rather than on the message, because the message names
/// whatever the caller happened to be doing and the advice survives being
/// wrapped by a caller that adds its own.
#[must_use]
pub fn is_transport(err: &Error) -> bool {
    ADVICE_TRANSPORT
        .first()
        .is_some_and(|marker| err.advice().contains(marker))
}

/// Turns a transport failure into something an operator can act on.
///
/// Kept in one place because the modules that make requests would otherwise each
/// invent their own phrasing for "the server did not answer" — and because
/// `reqwest`'s own words for a failed handshake are "error sending request for
/// url (…)", which is the shape a production outage was mistaken for a network
/// problem in. The *cause* is the answer, so the whole chain is rendered.
pub fn transport(err: reqwest::Error, what: &str) -> Error {
    let detail = rendered(err);

    human_errors::user(
        format!("Could not {what}: {detail}{}", full_stop(&detail)),
        ADVICE_TRANSPORT,
    )
}

/// A full stop, unless the sentence already ends in one.
///
/// Every refusal this client renders is "what we were doing" followed by the
/// server's own words, and the server's own words are a sentence. Adding a full
/// stop unconditionally produced
/// `…not one this server accepts for the control API..`, on every refused
/// control-API call, in the first live deployment's log.
pub(crate) fn full_stop(detail: &str) -> &'static str {
    match detail
        .trim_end()
        .ends_with(['.', '!', '?', ':', '\u{2026}'])
    {
        true => "",
        false => ".",
    }
}

/// A transport failure and every cause under it, on one line.
///
/// The URL is rebuilt from [`reqwest::Error::url`] with its userinfo and query
/// removed rather than taken from `reqwest`'s own `Display`, because neither a
/// credential nor a header may reach a log line. Nothing in the chain is a
/// header: the chain below a request error is the connector's, and the deepest
/// link of a handshake failure is `rustls`'s own reason.
fn rendered(err: reqwest::Error) -> String {
    let url = err.url().map(redacted);
    // Rendering our own URL, so `reqwest`'s copy of it would only repeat.
    let err = err.without_url();

    let mut message = match url {
        Some(url) => format!("{err} for url ({url})"),
        None => err.to_string(),
    };

    let mut source = std::error::Error::source(&err);

    for _ in 0..MAX_CAUSES {
        let Some(cause) = source else { break };
        let text = cause.to_string();

        // hyper wraps its connector error in one with the same words.
        if !message.ends_with(&text) {
            message.push_str(": ");
            message.push_str(&text);
        }

        source = cause.source();
    }

    message
}

/// A URL with everything a credential could hide in taken out of it.
fn redacted(url: &Url) -> String {
    let mut url = url.clone();

    // Each answers `Err(())` for a URL that cannot hold the part being cleared,
    // which is the same thing as it already being clear.
    let _ = url.set_password(None);
    let _ = url.set_username("");
    url.set_query(None);
    url.set_fragment(None);

    url.to_string()
}

#[cfg(test)]
mod tests {
    use rustak_core::identity::ServiceName;

    use super::*;

    fn identity() -> ServiceIdentity {
        ServiceIdentity::new(ServiceName::parse("weather").unwrap())
    }

    /// A truststore file holding one real certificate.
    ///
    /// The subject is the file's name so that two of these are two different
    /// authorities: `rcgen`'s default distinguished name is the same string
    /// every time, and `rustls` picks a candidate issuer by name before it
    /// checks a signature — so two identically named roots turn "we do not know
    /// this issuer" into "that signature is wrong".
    fn truststore(directory: &Path, name: &str) -> std::path::PathBuf {
        let key = rcgen::KeyPair::generate().expect("a key");
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("certificate parameters");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, format!("rustak test CA {name}"));
        let certificate = params.self_signed(&key).expect("a self-signed authority");

        let path = directory.join(name);
        std::fs::write(&path, certificate.pem()).expect("the truststore lands");

        path
    }

    #[test]
    fn the_stream_and_marti_replace_the_platforms_roots_with_the_truststore() {
        // The rule `TlsIdentity` argues for, unchanged: these listeners always
        // present the deployment's own CA, and keeping the public roots beside
        // it would let every public CA impersonate the server.
        let identity = identity().with_truststore("/etc/rustak/truststore.pem");

        assert_eq!(
            roots_for(&identity, Trust::Internal),
            Roots::Only(Path::new("/etc/rustak/truststore.pem")),
        );
    }

    #[test]
    fn a_sidecar_with_no_truststore_gets_the_platforms_roots_either_way() {
        assert_eq!(roots_for(&identity(), Trust::Internal), Roots::Platform);
        assert_eq!(roots_for(&identity(), Trust::Public), Roots::Platform);
    }

    #[test]
    fn the_control_endpoint_joins_the_platforms_roots_to_the_truststore() {
        // The production bug: enrolment writes rustak's CA as the truststore,
        // and the public listener presents a Let's Encrypt certificate. Only
        // the join trusts both.
        let identity = identity().with_truststore("/etc/rustak/truststore.pem");

        assert_eq!(
            roots_for(&identity, Trust::Public),
            Roots::PlatformAnd(Path::new("/etc/rustak/truststore.pem")),
        );
    }

    #[test]
    fn a_control_truststore_replaces_the_set_rather_than_joining_it() {
        // Naming one is how an operator pins the public listener to their own
        // PKI, and a pin that left the public roots in place pins nothing.
        let identity = identity()
            .with_truststore("/etc/rustak/truststore.pem")
            .with_control_truststore("/etc/rustak/public-ca.pem");

        assert_eq!(
            roots_for(&identity, Trust::Public),
            Roots::Only(Path::new("/etc/rustak/public-ca.pem")),
        );
        assert_eq!(
            roots_for(&identity, Trust::Internal),
            Roots::Only(Path::new("/etc/rustak/truststore.pem")),
            "it says nothing about the stream or Marti",
        );
    }

    #[test]
    fn a_control_truststore_on_its_own_still_replaces_the_platforms_roots() {
        let identity = identity().with_control_truststore("/etc/rustak/public-ca.pem");

        assert_eq!(
            roots_for(&identity, Trust::Public),
            Roots::Only(Path::new("/etc/rustak/public-ca.pem")),
        );
        assert_eq!(
            roots_for(&identity, Trust::Internal),
            Roots::Platform,
            "and nothing about the stream or Marti",
        );
    }

    #[test]
    fn a_sidecar_with_only_a_token_still_gets_a_client() {
        // The case the service token exists for: a plugin that has not enrolled
        // yet has no certificate, and must still be able to call the control
        // API to find out what it should be doing.
        assert!(client(&identity(), Trust::Public, DEFAULT_TIMEOUT).is_ok());
        assert!(client(&identity(), Trust::Internal, DEFAULT_TIMEOUT).is_ok());
    }

    #[test]
    fn both_policies_build_a_client_from_a_real_truststore() {
        // `tls_certs_merge` is a different code path in reqwest from
        // `tls_certs_only`, and one that answers an error on a platform whose
        // verifier cannot take extra roots. It must not be one of ours.
        let directory = tempfile::tempdir().unwrap();
        let identity = identity()
            .with_truststore(truststore(directory.path(), "internal.pem"))
            .with_control_truststore(truststore(directory.path(), "public.pem"));

        assert!(client(&identity, Trust::Internal, DEFAULT_TIMEOUT).is_ok());
        assert!(client(&identity, Trust::Public, DEFAULT_TIMEOUT).is_ok());
    }

    #[test]
    fn the_feed_client_is_built_from_the_same_identity_and_the_same_roots() {
        // It differs from `client` in its timeouts and in nothing else: a feed
        // that trusted a different set of roots, or presented a different
        // certificate, would be a second security decision nobody made.
        let directory = tempfile::tempdir().unwrap();
        let both = identity()
            .with_truststore(truststore(directory.path(), "internal.pem"))
            .with_control_truststore(truststore(directory.path(), "public.pem"));

        assert!(feed_client(&both, Trust::Public, FEED_IDLE_TIMEOUT).is_ok());
        assert!(feed_client(&both, Trust::Internal, FEED_IDLE_TIMEOUT).is_ok());
        assert!(feed_client(&identity(), Trust::Public, FEED_IDLE_TIMEOUT).is_ok());

        let missing = identity().with_truststore("/nonexistent/truststore.pem");
        let err = feed_client(&missing, Trust::Internal, FEED_IDLE_TIMEOUT).unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
    }

    #[test]
    fn the_feeds_idle_timeout_leaves_room_for_missed_keepalives() {
        // The server writes a keep-alive comment every 20s. An idle timeout at
        // or below that would reopen a perfectly healthy feed on a slow link,
        // which is the bug this was written to fix wearing another number.
        let keepalive = Duration::from_secs(20);

        assert!(
            FEED_IDLE_TIMEOUT >= keepalive * 3,
            "three missed keep-alives is the threshold, not one",
        );
        assert!(
            FEED_IDLE_TIMEOUT < keepalive * 6,
            "and a dead feed still has to be noticed in a couple of minutes",
        );
    }

    #[test]
    fn a_truststore_that_is_not_there_is_the_operators_to_fix() {
        let identity = identity().with_truststore("/nonexistent/truststore.pem");

        let err = client(&identity, Trust::Internal, DEFAULT_TIMEOUT).unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.description().contains("truststore"), "{err}");
    }

    #[test]
    fn a_control_truststore_that_is_not_there_is_the_operators_to_fix() {
        let identity = identity().with_control_truststore("/nonexistent/public-ca.pem");

        let err = client(&identity, Trust::Public, DEFAULT_TIMEOUT).unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.description().contains("public-ca.pem"), "{err}");
    }

    #[test]
    fn a_truststore_with_nothing_in_it_is_refused_rather_than_ignored() {
        // Silently falling back to the platform's roots would mean trusting
        // every public CA to impersonate the TAK server.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("empty.pem");
        std::fs::write(&path, "# nothing here\n").unwrap();

        let err = client(
            &identity().with_truststore(&path),
            Trust::Internal,
            DEFAULT_TIMEOUT,
        )
        .unwrap_err();

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

    /// A TLS server on an ephemeral port, presenting a certificate signed by an
    /// authority nobody was told about.
    async fn untrusted_server() -> (String, tokio::task::JoinHandle<()>) {
        let key = rcgen::KeyPair::generate().expect("a key");
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .expect("certificate parameters");
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "rustak test server");
        let certificate = params.self_signed(&key).expect("a self-signed certificate");

        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.der().clone()],
                rustls_pki_types::PrivateKeyDer::try_from(key.serialize_der())
                    .expect("a usable key"),
            )
            .expect("a server configuration");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral port");
        let address = listener.local_addr().expect("the bound address");
        let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(config));

        let handle = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                // The handshake is the whole test; whatever it answers, the
                // client has already decided.
                tokio::spawn(async move {
                    let _ = acceptor.accept(stream).await;
                });
            }
        });

        (format!("https://localhost:{}", address.port()), handle)
    }

    #[tokio::test]
    async fn a_handshake_failure_names_the_rustls_reason_rather_than_reqwests_wrapper() {
        // The production finding: "error sending request for url (…)" was read
        // as a network problem for an hour. The cause is the answer.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let (base, server) = untrusted_server().await;
        let directory = tempfile::tempdir().unwrap();
        let identity = identity().with_truststore(truststore(directory.path(), "other-ca.pem"));

        let client = client(&identity, Trust::Internal, DEFAULT_TIMEOUT).expect("a client");
        let err = client
            .get(&base)
            .send()
            .await
            .map(drop)
            .expect_err("a certificate from an authority we do not hold");
        let rendered = transport(err, "register the service 'weather'");

        server.abort();

        let described = rendered.description();
        assert!(
            described.contains("UnknownIssuer"),
            "the rustls reason has to reach the operator: {described}",
        );
        assert!(
            described.contains("register the service 'weather'"),
            "and so does what was being attempted: {described}",
        );
        assert!(
            rendered
                .advice()
                .iter()
                .any(|line| line.contains("control_truststore")),
            "the advice names the way out",
        );
    }

    #[test]
    fn only_a_failure_to_reach_the_server_counts_as_one() {
        // What the harness reads to decide whether the control link is down: a
        // refusal is the server answering, and an answer is not an outage.
        let refused = human_errors::user(
            "Could not register the service 'weather': 409 Conflict.",
            &["Another account already holds that service name. Choose another."],
        );

        assert!(!is_transport(&refused), "a status is an answer");
    }

    #[test]
    fn a_wrapped_transport_failure_is_still_one() {
        // `enrolment::ensure` wraps what `enroll` answered; the classification
        // has to survive that or a wrapped outage reads as a refusal.
        let inner = human_errors::user("Could not reach it.", ADVICE_TRANSPORT);
        let wrapped = human_errors::wrap_user(
            inner,
            "Could not enrol 'svc.weather'.",
            &["The sidecar does not start without an identity, so this is fatal."],
        );

        assert!(is_transport(&wrapped));
    }

    #[test]
    fn a_rendered_url_carries_no_credential_and_no_query() {
        // `reqwest`'s own Display prints the URL verbatim, userinfo and all.
        let url = Url::parse("https://svc:hunter2@tak.example.com:8446/oauth/token?assertion=ey.J")
            .unwrap();

        let rendered = redacted(&url);

        assert_eq!(rendered, "https://tak.example.com:8446/oauth/token");
    }
}

//! The rustls configurations rustak's three listeners are built from.
//!
//! | Listener | Client certificate | ALPN |
//! |---|---|---|
//! | public (`:8446`, `:443`) | never asked for | `h2`, `http/1.1`, `acme-tls/1` |
//! | Marti (`:8443`) | asked for, required by configuration | `h2`, `http/1.1` |
//! | stream (`:8089`) | required | none |
//!
//! # TLS 1.3 is not optional
//!
//! ATAK sets an OpenSSL cipher list of `DEFAULT:!ECDH` on its mission-package
//! transfers, which removes every ECDHE suite. rustls offers *only* ECDHE
//! suites in TLS 1.2 — it has no RSA key exchange and no finite-field
//! Diffie-Hellman — so for that client a TLS 1.2 handshake has nothing to
//! agree on and the connection is TLS 1.3 or nothing. A `set_cipher_list` does
//! not touch TLS 1.3 suites, so 1.3 works.
//!
//! TLS 1.2 is still offered, because CloudTAK, WinTAK and iTAK use it and none
//! of them restricts the suite list that way.
//!
//! # No ALPN on the stream
//!
//! commoncommo's streaming client sends neither ALPN nor server name
//! indication. Advertising a protocol list there would make rustls refuse a
//! handshake that offered none, which is every ATAK connection.

pub mod client_verifier;
pub mod peer;
pub mod resolver;

use std::sync::Arc;

use rustak_core::prelude::*;
use rustls::ServerConfig;
use rustls::server::WantsServerCert;
use rustls::server::danger::ClientCertVerifier;

pub use client_verifier::RustakClientVerifier;
pub use peer::{PeerCertificate, common_name, from_tokio_rustls, on_connect_capture};
pub use resolver::{ACME_TLS_ALPN, HotSwapCertResolver, TlsAlpnChallenge};

/// HTTP/2, then HTTP/1.1: what a browser and CloudTAK both offer.
const HTTP_ALPN: &[&[u8]] = &[b"h2", b"http/1.1"];

/// Which listener a configuration is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerKind {
    /// The browser and enrolment listener. No client certificate.
    Public,

    /// The Marti API. A client certificate is asked for.
    Marti,

    /// The CoT stream. A client certificate is required.
    Stream,
}

impl ListenerKind {
    /// How the listener is named in a log line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Marti => "marti",
            Self::Stream => "stream",
        }
    }
}

/// The public listener: no client certificate, HTTP and ACME validation.
///
/// `acme-tls/1` is advertised so that a validation handshake is not refused
/// for offering a protocol the server does not know; the resolver decides
/// whether there is actually a challenge to answer.
pub fn public_server_config(resolver: Arc<HotSwapCertResolver>) -> ServerConfig {
    let mut config = base().with_no_client_auth().with_cert_resolver(resolver);

    config.alpn_protocols = HTTP_ALPN
        .iter()
        .map(|protocol| protocol.to_vec())
        .chain(std::iter::once(ACME_TLS_ALPN.to_vec()))
        .collect();

    config
}

/// The Marti listener: a client certificate is asked for, and whether one is
/// required is [`RustakClientVerifier`]'s business.
///
/// The same port serves `/oauth/token` and the enrolment routes, which a device
/// reaches before it holds a certificate — so a verifier built with
/// `mandatory = false` is what lets a device enrol at all.
pub fn marti_server_config(
    resolver: Arc<HotSwapCertResolver>,
    verifier: Arc<RustakClientVerifier>,
) -> ServerConfig {
    let mut config = with_verifier(verifier).with_cert_resolver(resolver);

    config.alpn_protocols = HTTP_ALPN.iter().map(|protocol| protocol.to_vec()).collect();

    config
}

/// The streaming listener: a client certificate is the authentication, so the
/// verifier it is given must be a mandatory one.
///
/// No ALPN, and no server name is expected; see the module documentation.
pub fn stream_server_config(
    resolver: Arc<HotSwapCertResolver>,
    verifier: Arc<RustakClientVerifier>,
) -> ServerConfig {
    if !verifier.is_mandatory() {
        // Not an error: the listener still works and every connection is still
        // authenticated by the application. But a stream that accepted an
        // anonymous connection would be a security posture nobody chose, so it
        // is said loudly rather than discovered later.
        warn!(
            "The streaming listener was built with an optional client certificate verifier; \
             connections without a certificate will reach the application."
        );
    }

    with_verifier(verifier).with_cert_resolver(resolver)
}

/// The configuration a listener is built for, by kind.
pub fn server_config(
    kind: ListenerKind,
    resolver: Arc<HotSwapCertResolver>,
    verifier: Option<Arc<RustakClientVerifier>>,
) -> Result<ServerConfig, Error> {
    match (kind, verifier) {
        (ListenerKind::Public, _) => Ok(public_server_config(resolver)),
        (ListenerKind::Marti, Some(verifier)) => Ok(marti_server_config(resolver, verifier)),
        (ListenerKind::Stream, Some(verifier)) => Ok(stream_server_config(resolver, verifier)),
        (kind, None) => Err(human_errors::system(
            format!(
                "The {} listener needs a client certificate verifier and was not given one.",
                kind.as_str()
            ),
            &["This is unexpected; please report it with the surrounding log entries."],
        )),
    }
}

/// Every configuration starts here: TLS 1.3 and 1.2, aws-lc-rs.
fn base() -> rustls::ConfigBuilder<ServerConfig, rustls::WantsVerifier> {
    install_crypto_provider();

    ServerConfig::builder_with_protocol_versions(&[
        &rustls::version::TLS13,
        &rustls::version::TLS12,
    ])
}

/// The half of a builder that has been told how to check client certificates.
fn with_verifier(
    verifier: Arc<RustakClientVerifier>,
) -> rustls::ConfigBuilder<ServerConfig, WantsServerCert> {
    base().with_client_cert_verifier(verifier as Arc<dyn ClientCertVerifier>)
}

/// Installs rustls' cryptography, once, if start-up has not already.
///
/// `run()` does this before anything else; it is repeated here because
/// `ServerConfig::builder` *panics* without a provider, and a test that builds
/// a listener without going through start-up would otherwise fail in a way that
/// says nothing about what it was testing. Installing twice is a no-op.
pub fn install_crypto_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();

    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use rustls::pki_types::{CertificateDer, ServerName};
    use rustls::sign::CertifiedKey;
    use rustls::{ClientConfig, RootCertStore};

    use super::*;
    use crate::pki::keys::{KeyType, generate_key};
    use crate::pki::revoke::RevocationCache;

    /// How long either end of a test handshake waits before giving up.
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// What a server that completed a handshake sends.
    const GREETING: &str = "connected";
    use crate::pki::testing::{TestAuthority, TestClient};

    /// The server's own certificate, self-signed for `localhost`, and the root
    /// the test client trusts it by.
    fn server_identity() -> (Arc<CertifiedKey>, CertificateDer<'static>) {
        install_crypto_provider();

        let key = generate_key(KeyType::EcdsaP256).unwrap();
        let params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        let certificate = params.self_signed(&key).unwrap();
        let der = CertificateDer::from(certificate.der().to_vec());

        let certified = CertifiedKey::from_der(
            vec![der.clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            &rustls::crypto::aws_lc_rs::default_provider(),
        )
        .unwrap();

        (Arc::new(certified), der)
    }

    /// Runs one real handshake against `config`, offering `client` if given.
    ///
    /// Returns the bytes the server sent, or the error the client saw — which
    /// for a refused certificate is a TLS alert rather than anything at the
    /// application layer.
    fn handshake(
        config: ServerConfig,
        server_root: CertificateDer<'static>,
        client: Option<&TestClient>,
    ) -> Result<String, String> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            // Timeouts on both ends: a handshake the server refuses can leave
            // either side waiting for a flight that is never sent, and a test
            // that hangs says far less than one that fails.
            socket.set_read_timeout(Some(TIMEOUT)).unwrap();
            socket.set_write_timeout(Some(TIMEOUT)).unwrap();

            let mut connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
            let mut socket = &socket;
            let mut stream = rustls::Stream::new(&mut connection, &mut socket);

            let _ = stream.write_all(GREETING.as_bytes());
            let _ = stream.flush();
            connection.send_close_notify();
            let _ = connection.write_tls(&mut socket);
        });

        let mut roots = RootCertStore::empty();
        roots.add(server_root).unwrap();

        let builder = ClientConfig::builder().with_root_certificates(roots);
        let client_config = match client {
            Some(client) => builder
                .with_client_auth_cert(vec![client.der.clone()], client.private_key())
                .unwrap(),
            None => builder.with_no_client_auth(),
        };

        let name = ServerName::try_from("localhost").unwrap();
        let mut connection = rustls::ClientConnection::new(Arc::new(client_config), name).unwrap();
        let mut socket = std::net::TcpStream::connect(address).unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        socket.set_write_timeout(Some(TIMEOUT)).unwrap();

        let mut stream = rustls::Stream::new(&mut connection, &mut socket);
        let mut buffer = vec![0u8; GREETING.len()];

        // `read_exact` rather than reading to the end of the stream: what is
        // being tested is whether the handshake completed, and waiting for the
        // close would turn a missing `close_notify` into a failure of its own.
        let outcome = stream
            .read_exact(&mut buffer)
            .map_err(|err| err.to_string())
            .map(|()| String::from_utf8_lossy(&buffer).into_owned());

        let _ = server.join();

        outcome
    }

    fn verifier(
        authority: &TestAuthority,
        cache: Arc<RevocationCache>,
        mandatory: bool,
    ) -> Arc<RustakClientVerifier> {
        RustakClientVerifier::new(&[authority.certificate()], cache, mandatory).unwrap()
    }

    #[tokio::test]
    async fn an_enrolled_client_completes_the_stream_handshake() {
        let authority = TestAuthority::new().await;
        let client = authority.issue("alice");
        let cache = RevocationCache::new(true);
        cache.note_issued(&client.fingerprint);

        let (certified, root) = server_identity();
        let config = stream_server_config(
            HotSwapCertResolver::new(Some(certified)),
            verifier(&authority, cache, true),
        );

        assert_eq!(handshake(config, root, Some(&client)).unwrap(), "connected");
    }

    #[tokio::test]
    async fn a_revoked_client_is_dropped_at_the_stream_handshake() {
        let authority = TestAuthority::new().await;
        let client = authority.issue("alice");
        let cache = RevocationCache::new(true);
        cache.note_issued(&client.fingerprint);
        cache.note_revoked(&client.fingerprint);

        let (certified, root) = server_identity();
        let config = stream_server_config(
            HotSwapCertResolver::new(Some(certified)),
            verifier(&authority, cache, true),
        );

        let error = handshake(config, root, Some(&client))
            .expect_err("a revoked certificate must not complete a handshake");

        assert!(
            error.to_lowercase().contains("revoked") || error.contains("certificate"),
            "the failure should name the certificate: {error}"
        );
    }

    #[tokio::test]
    async fn a_client_we_have_no_record_of_is_dropped_at_the_stream_handshake() {
        let authority = TestAuthority::new().await;
        let client = authority.issue("alice");

        // Never noted as issued: this is the restored-backup case.
        let (certified, root) = server_identity();
        let config = stream_server_config(
            HotSwapCertResolver::new(Some(certified)),
            verifier(&authority, RevocationCache::new(true), true),
        );

        assert!(handshake(config, root, Some(&client)).is_err());
    }

    #[tokio::test]
    async fn a_client_from_another_authority_is_dropped_at_the_stream_handshake() {
        let ours = TestAuthority::new().await;
        let theirs = TestAuthority::new().await;
        let foreign = theirs.issue("alice");

        // Known and unrevoked, so only the chain check can refuse it.
        let cache = RevocationCache::new(true);
        cache.note_issued(&foreign.fingerprint);

        let (certified, root) = server_identity();
        let config = stream_server_config(
            HotSwapCertResolver::new(Some(certified)),
            verifier(&ours, cache, true),
        );

        assert!(handshake(config, root, Some(&foreign)).is_err());
    }

    #[tokio::test]
    async fn a_client_with_no_certificate_is_dropped_at_the_stream_handshake() {
        let authority = TestAuthority::new().await;
        let (certified, root) = server_identity();
        let config = stream_server_config(
            HotSwapCertResolver::new(Some(certified)),
            verifier(&authority, RevocationCache::new(false), true),
        );

        assert!(
            handshake(config, root, None).is_err(),
            "the stream authenticates by certificate and nothing else"
        );
    }

    #[tokio::test]
    async fn the_marti_listener_lets_a_device_without_a_certificate_enrol() {
        let authority = TestAuthority::new().await;
        let (certified, root) = server_identity();
        let config = marti_server_config(
            HotSwapCertResolver::new(Some(certified)),
            verifier(&authority, RevocationCache::new(true), false),
        );

        assert_eq!(handshake(config, root, None).unwrap(), "connected");
    }

    #[tokio::test]
    async fn the_marti_listener_still_refuses_a_revoked_certificate() {
        let authority = TestAuthority::new().await;
        let client = authority.issue("alice");
        let cache = RevocationCache::new(false);
        cache.note_revoked(&client.fingerprint);

        let (certified, root) = server_identity();
        let config = marti_server_config(
            HotSwapCertResolver::new(Some(certified)),
            verifier(&authority, cache, false),
        );

        assert!(
            handshake(config, root, Some(&client)).is_err(),
            "optional means 'may be absent', not 'may be invalid'"
        );
    }

    #[tokio::test]
    async fn the_public_listener_never_asks_for_a_certificate() {
        let (certified, root) = server_identity();
        let config = public_server_config(HotSwapCertResolver::new(Some(certified)));

        assert_eq!(handshake(config, root, None).unwrap(), "connected");
    }

    #[test]
    fn the_listeners_advertise_the_protocols_their_clients_speak() {
        let (certified, _) = server_identity();
        let resolver = HotSwapCertResolver::new(Some(certified));

        let public = public_server_config(Arc::clone(&resolver));

        assert_eq!(
            public.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec(), ACME_TLS_ALPN.to_vec()]
        );
    }

    #[tokio::test]
    async fn the_stream_advertises_nothing_because_its_client_offers_nothing() {
        let authority = TestAuthority::new().await;
        let (certified, _) = server_identity();
        let resolver = HotSwapCertResolver::new(Some(certified));
        let verifier = verifier(&authority, RevocationCache::new(false), true);

        let stream = stream_server_config(Arc::clone(&resolver), Arc::clone(&verifier));
        let marti = marti_server_config(resolver, verifier);

        assert!(stream.alpn_protocols.is_empty());
        assert_eq!(
            marti.alpn_protocols,
            vec![b"h2".to_vec(), b"http/1.1".to_vec()]
        );
    }

    #[test]
    fn both_versions_a_tak_client_might_speak_are_accepted() {
        // rustls exposes no accessor for a built configuration's versions, and
        // the property that matters is behavioural anyway: a client pinned to
        // each version has to complete a handshake.
        for version in [&rustls::version::TLS13, &rustls::version::TLS12] {
            let (certified, root) = server_identity();
            let config = public_server_config(HotSwapCertResolver::new(Some(certified)));

            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(TIMEOUT)).unwrap();
                socket.set_write_timeout(Some(TIMEOUT)).unwrap();

                let mut connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
                let mut socket = &socket;
                let mut stream = rustls::Stream::new(&mut connection, &mut socket);

                let _ = stream.write_all(GREETING.as_bytes());
                let _ = stream.flush();

                connection.protocol_version()
            });

            let mut roots = RootCertStore::empty();
            roots.add(root).unwrap();

            let client_config = ClientConfig::builder_with_protocol_versions(&[version])
                .with_root_certificates(roots)
                .with_no_client_auth();

            let name = ServerName::try_from("localhost").unwrap();
            let mut connection =
                rustls::ClientConnection::new(Arc::new(client_config), name).unwrap();
            let mut socket = std::net::TcpStream::connect(address).unwrap();
            socket.set_read_timeout(Some(TIMEOUT)).unwrap();
            socket.set_write_timeout(Some(TIMEOUT)).unwrap();

            let mut stream = rustls::Stream::new(&mut connection, &mut socket);
            let mut buffer = vec![0u8; GREETING.len()];

            stream
                .read_exact(&mut buffer)
                .unwrap_or_else(|err| panic!("{version:?} must be accepted: {err}"));

            assert_eq!(String::from_utf8_lossy(&buffer), GREETING);
            assert_eq!(
                server.join().unwrap(),
                Some(version.version),
                "the handshake must settle on the version the client pinned"
            );
        }
    }

    #[tokio::test]
    async fn a_listener_built_by_kind_needs_a_verifier_where_one_is_required() {
        let authority = TestAuthority::new().await;
        let (certified, _) = server_identity();
        let resolver = HotSwapCertResolver::new(Some(certified));
        let verifier = verifier(&authority, RevocationCache::new(false), true);

        assert!(
            server_config(ListenerKind::Public, Arc::clone(&resolver), None).is_ok(),
            "the public listener has nothing to verify"
        );
        assert!(server_config(ListenerKind::Marti, Arc::clone(&resolver), None).is_err());
        assert!(server_config(ListenerKind::Stream, Arc::clone(&resolver), None).is_err());
        assert!(server_config(ListenerKind::Stream, resolver, Some(verifier)).is_ok());

        assert_eq!(ListenerKind::Public.as_str(), "public");
        assert_eq!(ListenerKind::Marti.as_str(), "marti");
        assert_eq!(ListenerKind::Stream.as_str(), "stream");
    }
}

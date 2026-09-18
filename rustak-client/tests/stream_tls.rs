//! The handshake the default build is made of.
//!
//! Everything else in this crate's tests runs over a duplex or a plain socket,
//! because that is what makes them quick and deterministic. This one exists
//! because those are exactly the transports a shipped rustak client cannot use:
//! `:8089` is mutually authenticated TLS, and a `TlsIdentity` that loads
//! cleanly but cannot complete a handshake would pass every other test here.
//!
//! The authority is thrown away with the test. `rustak-server`'s own test CA
//! (`pki::testing`) is the one the integration suites use; this one only has to
//! produce three PEM files.

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
};
use rustak_client::stream::{Endpoint, StreamConfig, TlsIdentity, connect};
use rustak_cot::codec::{EncodedEvent, Frame, Mode, TakCodec};
use rustak_cot::detail::Contact;
use rustak_cot::{Event, xml};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use rustls::{RootCertStore, ServerConfig};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_util::codec::Framed;

/// The PEM files a mutually authenticated connection needs at each end.
struct Authority {
    directory: tempfile::TempDir,
    server_chain: Vec<CertificateDer<'static>>,
    server_key: PrivateKeyDer<'static>,
    roots: RootCertStore,
}

impl Authority {
    fn new() -> Self {
        let ca_key = KeyPair::generate().expect("a CA key");
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "rustak-client test CA");
        let ca = ca_params.self_signed(&ca_key).expect("a self-signed CA");
        let issuer = Issuer::new(ca_params, ca_key);

        let server = leaf("localhost", ExtendedKeyUsagePurpose::ServerAuth, &issuer);
        let client = leaf("SERVICE-adsb", ExtendedKeyUsagePurpose::ClientAuth, &issuer);

        let directory = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(directory.path().join("ca.pem"), ca.pem()).unwrap();
        std::fs::write(directory.path().join("client.pem"), client.0).unwrap();
        std::fs::write(directory.path().join("client.key"), client.1).unwrap();

        let mut roots = RootCertStore::empty();
        roots.add(ca.der().clone()).expect("the CA is usable");

        Self {
            directory,
            server_chain: vec![
                CertificateDer::from_pem_slice(server.0.as_bytes()).expect("the server chain"),
            ],
            server_key: PrivateKeyDer::from_pem_slice(server.1.as_bytes()).expect("the server key"),
            roots,
        }
    }

    fn client_identity(&self) -> TlsIdentity {
        TlsIdentity::from_pem_files(
            self.directory.path().join("ca.pem"),
            self.directory.path().join("client.pem"),
            self.directory.path().join("client.key"),
        )
        .expect("the client material loads")
    }

    fn acceptor(self) -> TlsAcceptor {
        let provider = rustak_client::stream::tls::provider();
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(self.roots),
            provider.clone(),
        )
        .build()
        .expect("a client verifier");
        let config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("the provider supports the default versions")
            .with_client_cert_verifier(verifier)
            .with_single_cert(self.server_chain, self.server_key)
            .expect("a server configuration");

        TlsAcceptor::from(Arc::new(config))
    }
}

/// One certificate and its key, in PEM.
fn leaf(
    name: &str,
    usage: ExtendedKeyUsagePurpose,
    issuer: &Issuer<'_, KeyPair>,
) -> (String, String) {
    let key = KeyPair::generate().expect("a leaf key");
    let mut params = CertificateParams::new(vec![name.to_string()]).expect("leaf parameters");
    params.distinguished_name.push(DnType::CommonName, name);
    params.extended_key_usages = vec![usage];

    let certificate = params.signed_by(&key, issuer).expect("a signed leaf");

    (certificate.pem(), key.serialize_pem())
}

fn sa(uid: &str, callsign: &str) -> Event {
    Event::builder("a-f-G-U-C", uid)
        .how("m-g")
        .point(51.5074, -0.1278)
        .typed(&Contact::new(callsign).with_endpoint("*:-1:stcp"))
        .build()
}

#[tokio::test]
async fn a_client_certificate_completes_the_handshake_and_carries_cot_both_ways() {
    let authority = Authority::new();
    let identity = authority.client_identity();
    assert_eq!(identity.roots(), 1);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let acceptor = authority.acceptor();

    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("a connection");
        let session = acceptor.accept(socket).await.expect("a TLS handshake");

        // The client's certificate is what a real listener maps to a principal.
        let (_, connection) = session.get_ref();
        let peer = connection
            .peer_certificates()
            .expect("the client authenticated")
            .len();

        let mut framed = Framed::new(session, TakCodec::new(Mode::Xml));
        framed
            .send(&EncodedEvent::new(sa("SERVER-1", "SERVER")))
            .await
            .expect("the server can write");

        let Some(Ok(Frame::Xml(bytes))) = framed.next().await else {
            panic!("the client should introduce itself");
        };

        (peer, xml::parse(&bytes).expect("parseable XML").uid)
    });

    let config = StreamConfig::new(Endpoint::tls("localhost", port), "SERVICE-adsb")
        .with_tls(identity)
        .with_connect_timeout(Duration::from_secs(5));

    let mut client = connect(&config).await.expect("the handshake completes");

    let greeting = client
        .next()
        .await
        .expect("the server's message arrives")
        .expect("and it parses");
    assert_eq!(greeting.uid, "SERVER-1");

    client
        .send(sa("SERVICE-adsb", "ADSB"))
        .await
        .expect("the client can write");

    let (certificates, seen) = server.await.expect("the server task finishes");

    assert_eq!(certificates, 1, "the client presented its certificate");
    assert_eq!(seen, "SERVICE-adsb");
}

#[tokio::test]
async fn a_server_the_truststore_does_not_know_is_refused() {
    // The property a pinned truststore exists for: another CA's certificate,
    // however valid, is not this deployment's server.
    let ours = Authority::new();
    let theirs = Authority::new();

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a free port");
    let port = listener.local_addr().unwrap().port();
    let acceptor = theirs.acceptor();

    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.expect("a connection");

        // The handshake is expected to fail; what matters is that the client
        // is the one that refuses it.
        let _ = acceptor.accept(socket).await;
    });

    let config = StreamConfig::new(Endpoint::tls("localhost", port), "SERVICE-adsb")
        .with_tls(ours.client_identity())
        .with_connect_timeout(Duration::from_secs(5));

    let error = connect(&config)
        .await
        .expect_err("an unknown CA is refused");

    assert!(
        error.to_string().contains("connection"),
        "the failure should read as a connection failure: {error}",
    );

    server.await.expect("the server task finishes");
}

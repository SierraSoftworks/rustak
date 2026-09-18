//! Choosing which certificate a listener presents, and swapping it live.
//!
//! # Why a resolver rather than a fixed certificate
//!
//! A `rustls::ServerConfig` is immutable once built and an actix listener holds
//! one for its lifetime, so `with_single_cert` would mean restarting the server
//! to install a renewed certificate. A [`ResolvesServerCert`] is consulted on
//! every handshake, which makes replacing the certificate an atomic write to a
//! lock instead — which is what ACME renewal, the daily internal-certificate
//! rotation and the `files` mode's reload all need.
//!
//! # The ACME challenge slot
//!
//! TLS-ALPN-01 proves control of a name by answering one handshake — ALPN
//! `acme-tls/1`, SNI the name being validated — with a throwaway self-signed
//! certificate carrying the challenge digest. That has to happen on port 443
//! while the listener is serving ordinary traffic, so the challenge lives
//! beside the real certificate here and is picked only for handshakes that ask
//! for it by ALPN *and* name.

use std::sync::{Arc, RwLock};

use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;

/// The ALPN protocol an ACME validation handshake asks for.
pub const ACME_TLS_ALPN: &[u8] = b"acme-tls/1";

/// A throwaway certificate answering one ACME validation handshake.
pub struct TlsAlpnChallenge {
    /// The name being validated. Compared to the handshake's SNI.
    pub sni: String,

    /// The self-signed certificate carrying the challenge digest.
    pub certified: Arc<CertifiedKey>,
}

impl std::fmt::Debug for TlsAlpnChallenge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsAlpnChallenge")
            .field("sni", &self.sni)
            .finish_non_exhaustive()
    }
}

/// The certificate a listener presents, replaceable while it is running.
#[derive(Debug, Default)]
pub struct HotSwapCertResolver {
    current: RwLock<Option<Arc<CertifiedKey>>>,
    challenge: RwLock<Option<TlsAlpnChallenge>>,
}

impl HotSwapCertResolver {
    /// A resolver presenting `initial`, or nothing until one is installed.
    ///
    /// Nothing is a legitimate state: an ACME installation binds :443 before it
    /// holds a certificate, so that the challenge can be answered on it. Those
    /// handshakes fail until a certificate arrives, which is a listener that
    /// says "not yet" rather than one that never starts.
    pub fn new(initial: Option<Arc<CertifiedKey>>) -> Arc<Self> {
        Arc::new(Self {
            current: RwLock::new(initial),
            challenge: RwLock::new(None),
        })
    }

    /// Replaces the certificate every subsequent handshake will present.
    ///
    /// Connections already established keep the one they negotiated with; TLS
    /// has no way to change a certificate mid-connection, and they are valid
    /// until they close.
    pub fn install(&self, certified: Arc<CertifiedKey>) {
        if let Ok(mut current) = self.current.write() {
            *current = Some(certified);
        }
    }

    /// The certificate currently installed, if any.
    pub fn current(&self) -> Option<Arc<CertifiedKey>> {
        self.current.read().ok().and_then(|held| held.clone())
    }

    /// Whether a certificate has been installed at all.
    pub fn is_ready(&self) -> bool {
        self.current().is_some()
    }

    /// Arms the ACME validation answer for one name.
    pub fn set_challenge(&self, challenge: TlsAlpnChallenge) {
        if let Ok(mut held) = self.challenge.write() {
            *held = Some(challenge);
        }
    }

    /// Disarms it, which the order does as soon as validation finishes —
    /// leaving it armed would answer `acme-tls/1` handshakes indefinitely.
    pub fn clear_challenge(&self) {
        if let Ok(mut held) = self.challenge.write() {
            *held = None;
        }
    }

    /// The challenge certificate for a handshake asking for this name.
    fn challenge_for(&self, server_name: Option<&str>) -> Option<Arc<CertifiedKey>> {
        let held = self.challenge.read().ok()?;
        let challenge = held.as_ref()?;
        let requested = server_name?;

        challenge
            .sni
            .eq_ignore_ascii_case(requested)
            .then(|| Arc::clone(&challenge.certified))
    }
}

impl ResolvesServerCert for HotSwapCertResolver {
    /// Answers with the challenge certificate for an ACME validation, and with
    /// the installed certificate for everything else.
    ///
    /// Server name indication is *not* required for an ordinary handshake:
    /// ATAK's streaming client sends none, so demanding one would break the
    /// listener it matters most on.
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let wants_acme = hello
            .alpn()
            .is_some_and(|mut offered| offered.any(|protocol| protocol == ACME_TLS_ALPN));

        if wants_acme {
            // Deliberately not falling back to the real certificate: a client
            // that asked for `acme-tls/1` and got a normal certificate would
            // report a confusing validation failure, and no real client offers
            // that protocol by accident.
            return self.challenge_for(hello.server_name());
        }

        self.current()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpStream;

    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, RootCertStore};

    use super::*;
    use crate::pki::keys::{KeyType, generate_key};

    /// How long either end of a test handshake waits before giving up.
    const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// A self-signed certificate for `name`, as both a resolver entry and a
    /// root a test client can trust.
    fn certified(
        name: &str,
    ) -> (
        Arc<CertifiedKey>,
        rustls::pki_types::CertificateDer<'static>,
    ) {
        let key = generate_key(KeyType::EcdsaP256).unwrap();
        let params = rcgen::CertificateParams::new(vec![name.to_owned()]).unwrap();
        let certificate = params.self_signed(&key).unwrap();
        let der = rustls::pki_types::CertificateDer::from(certificate.der().to_vec());

        let certified = CertifiedKey::from_der(
            vec![der.clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            &rustls::crypto::aws_lc_rs::default_provider(),
        )
        .unwrap();

        (Arc::new(certified), der)
    }

    /// Drives a real handshake against the resolver and reports which
    /// certificate the server presented, because `ClientHello` cannot be
    /// constructed outside rustls.
    fn handshake(
        resolver: Arc<HotSwapCertResolver>,
        server_name: Option<&str>,
        alpn: &[&[u8]],
        trusted: &[rustls::pki_types::CertificateDer<'static>],
    ) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, String> {
        crate::pki::tls::install_crypto_provider();

        let mut server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_cert_resolver(resolver);
        server_config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();

        let server = std::thread::spawn(move || {
            let (socket, _) = listener.accept().unwrap();
            // Timeouts on both ends: a handshake the server refuses can leave
            // either side waiting for a flight that is never sent, and a test
            // that hangs says far less than one that fails.
            socket.set_read_timeout(Some(TIMEOUT)).unwrap();
            socket.set_write_timeout(Some(TIMEOUT)).unwrap();

            let mut connection = rustls::ServerConnection::new(Arc::new(server_config)).unwrap();
            let mut socket = &socket;
            let mut stream = rustls::Stream::new(&mut connection, &mut socket);

            let _ = stream.write_all(b"hello");
            let _ = stream.flush();
            connection.send_close_notify();
            let _ = connection.write_tls(&mut socket);
        });

        let mut roots = RootCertStore::empty();
        for certificate in trusted {
            roots.add(certificate.clone()).unwrap();
        }

        let mut client_config = ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client_config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();

        let name = ServerName::try_from(server_name.unwrap_or("127.0.0.1").to_owned()).unwrap();
        let mut connection = rustls::ClientConnection::new(Arc::new(client_config), name).unwrap();
        let mut socket = TcpStream::connect(address).unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        socket.set_write_timeout(Some(TIMEOUT)).unwrap();
        let mut stream = rustls::Stream::new(&mut connection, &mut socket);

        let mut buffer = [0u8; 5];
        let outcome = stream
            .read_exact(&mut buffer)
            .map_err(|err| err.to_string())
            .map(|()| {
                connection
                    .peer_certificates()
                    .map(<[_]>::to_vec)
                    .unwrap_or_default()
            });

        let _ = server.join();

        outcome
    }

    #[test]
    fn an_ordinary_handshake_gets_the_installed_certificate_without_naming_a_host() {
        let (certified, der) = certified("tak.example.com");
        let resolver = HotSwapCertResolver::new(Some(certified));

        // No server name: the streaming client sends none, which must work.
        let presented = handshake(
            resolver,
            Some("tak.example.com"),
            &[],
            std::slice::from_ref(&der),
        )
        .unwrap();

        assert_eq!(presented.first(), Some(&der));
    }

    #[test]
    fn a_resolver_with_nothing_installed_presents_nothing() {
        let resolver = HotSwapCertResolver::new(None);
        let (_, der) = certified("tak.example.com");

        assert!(!resolver.is_ready());
        assert!(handshake(resolver, Some("tak.example.com"), &[], &[der]).is_err());
    }

    #[test]
    fn installing_a_certificate_replaces_the_one_presented() {
        let (first, first_der) = certified("tak.example.com");
        let (second, second_der) = certified("tak.example.com");
        let resolver = HotSwapCertResolver::new(Some(first));

        let before = handshake(
            Arc::clone(&resolver),
            Some("tak.example.com"),
            &[],
            &[first_der.clone(), second_der.clone()],
        )
        .unwrap();

        resolver.install(second);

        let after = handshake(
            resolver,
            Some("tak.example.com"),
            &[],
            &[first_der.clone(), second_der.clone()],
        )
        .unwrap();

        assert_eq!(before.first(), Some(&first_der));
        assert_eq!(after.first(), Some(&second_der));
    }

    #[test]
    fn an_acme_handshake_for_the_right_name_gets_the_challenge_certificate() {
        let (installed, installed_der) = certified("tak.example.com");
        let (challenge, challenge_der) = certified("tak.example.com");
        let resolver = HotSwapCertResolver::new(Some(installed));

        resolver.set_challenge(TlsAlpnChallenge {
            sni: "tak.example.com".to_owned(),
            certified: challenge,
        });

        let presented = handshake(
            resolver,
            Some("tak.example.com"),
            &[ACME_TLS_ALPN],
            &[installed_der, challenge_der.clone()],
        )
        .unwrap();

        assert_eq!(presented.first(), Some(&challenge_der));
    }

    #[test]
    fn an_acme_handshake_for_another_name_is_refused_rather_than_answered() {
        let (installed, installed_der) = certified("tak.example.com");
        let (challenge, challenge_der) = certified("other.example.com");
        let resolver = HotSwapCertResolver::new(Some(installed));

        resolver.set_challenge(TlsAlpnChallenge {
            sni: "other.example.com".to_owned(),
            certified: challenge,
        });

        assert!(
            handshake(
                resolver,
                Some("tak.example.com"),
                &[ACME_TLS_ALPN],
                &[installed_der, challenge_der],
            )
            .is_err(),
            "the real certificate must not answer an acme-tls/1 handshake"
        );
    }

    #[test]
    fn clearing_the_challenge_stops_it_being_served() {
        let (installed, installed_der) = certified("tak.example.com");
        let (challenge, challenge_der) = certified("tak.example.com");
        let resolver = HotSwapCertResolver::new(Some(installed));

        resolver.set_challenge(TlsAlpnChallenge {
            sni: "tak.example.com".to_owned(),
            certified: challenge,
        });
        resolver.clear_challenge();

        assert!(
            handshake(
                Arc::clone(&resolver),
                Some("tak.example.com"),
                &[ACME_TLS_ALPN],
                &[installed_der.clone(), challenge_der],
            )
            .is_err(),
            "a cleared challenge answers nothing"
        );

        let ordinary = handshake(
            resolver,
            Some("tak.example.com"),
            &[],
            std::slice::from_ref(&installed_der),
        )
        .expect("ordinary traffic is unaffected");

        assert_eq!(ordinary.first(), Some(&installed_der));
    }

    #[test]
    fn a_challenge_never_renders_its_key() {
        let (certified, _) = certified("tak.example.com");
        let challenge = TlsAlpnChallenge {
            sni: "tak.example.com".to_owned(),
            certified,
        };

        assert!(format!("{challenge:?}").contains("tak.example.com"));
        assert!(!format!("{challenge:?}").contains("key"));
    }
}

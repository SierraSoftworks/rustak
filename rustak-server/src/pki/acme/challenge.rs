//! Answering the authority's question: "do you control this name?"
//!
//! Two ways, and rustak implements both because each needs a port the other
//! does not.
//!
//! | Challenge | Answered by | Needs |
//! |---|---|---|
//! | `tls-alpn-01` | a throwaway certificate served for ALPN `acme-tls/1` | a TLS listener on :443 |
//! | `http-01` | a plaintext body under `/.well-known/acme-challenge/` | port 80 reaching the public listener |
//!
//! # Why the token map is a static
//!
//! actix builds one `App` per worker thread, and the handler is a plain
//! function with nothing but its request: there is no per-application state a
//! challenge published by a background job could reach. There is exactly one
//! public listener per process and exactly one order in flight at a time, so
//! the map is process-wide — and it is empty except during the seconds an
//! authority is being answered, because [`withdraw`] runs as soon as
//! validation finishes.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use actix_web::{HttpResponse, web};
use instant_acme::ChallengeType;
use sha2::{Digest as _, Sha256};

use rustak_core::prelude::*;

use crate::config::AcmeChallenge;
use crate::pki::keys::{KeyType, generate_key};
use crate::pki::tls::{HotSwapCertResolver, TlsAlpnChallenge};

/// The path an authority fetches an `http-01` answer from.
pub const HTTP01_PREFIX: &str = "/.well-known/acme-challenge";

/// Tokens we are currently prepared to answer, and what to answer with.
///
/// A `BTreeMap` rather than a hash map: it holds at most one entry per name in
/// one order, and a deterministic iteration order makes a test's failure
/// message the same every time.
static TOKENS: RwLock<BTreeMap<String, String>> = RwLock::new(BTreeMap::new());

/// Arms the `http-01` answer for one token.
pub fn publish(token: &str, key_authorization: &str) {
    if let Ok(mut tokens) = TOKENS.write() {
        tokens.insert(token.to_owned(), key_authorization.to_owned());
    }
}

/// Disarms it. Runs as soon as validation finishes, whether it succeeded or
/// not: an answer left armed is a path that serves a secret indefinitely.
pub fn withdraw(token: &str) {
    if let Ok(mut tokens) = TOKENS.write() {
        tokens.remove(token);
    }
}

/// What to answer a request for `token`, if anything.
///
/// Public so that a test can check what is armed at the moment an authority
/// would fetch it, which is the only moment it matters.
pub fn answer(token: &str) -> Option<String> {
    TOKENS.read().ok()?.get(token).cloned()
}

/// Registers `GET /.well-known/acme-challenge/{token}` on a listener.
///
/// Always mounted, and always a `404` unless an order is in flight, so that an
/// installation that switches ACME on does not need a restart for the path to
/// exist.
pub fn routes(config: &mut web::ServiceConfig) {
    config.route(
        &format!("{HTTP01_PREFIX}/{{token}}"),
        web::get().to(respond),
    );
}

/// Answers one `http-01` validation request.
async fn respond(token: web::Path<String>) -> HttpResponse {
    match answer(&token) {
        // RFC 8555 §8.3: the body is the key authorization, and the content
        // type is either `application/octet-stream` or absent. `text/plain`
        // is what every other implementation sends and what the authority
        // reads; the body is compared byte for byte, so nothing may be added.
        Some(key_authorization) => HttpResponse::Ok()
            .content_type("text/plain; charset=utf-8")
            .body(key_authorization),
        None => HttpResponse::NotFound().finish(),
    }
}

/// Publishes and withdraws challenge answers for one order.
///
/// Holds the listener's certificate resolver when there is one, which is what
/// decides whether `tls-alpn-01` can be answered at all: a resolver is only
/// installed for `[web.public.tls] mode = "acme"`.
pub struct Responder {
    resolver: Option<Arc<HotSwapCertResolver>>,
}

impl Responder {
    /// A responder that can answer `tls-alpn-01` through `resolver`, and
    /// `http-01` through the process-wide token map either way.
    pub fn new(resolver: Option<Arc<HotSwapCertResolver>>) -> Self {
        Self { resolver }
    }

    /// Whether this responder could answer a challenge of this kind.
    pub fn can_answer(&self, kind: AcmeChallenge) -> bool {
        match kind {
            AcmeChallenge::TlsAlpn01 => self.resolver.is_some(),
            AcmeChallenge::Http01 => true,
        }
    }

    /// Arms the answer for one authorisation.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the `tls-alpn-01` challenge
    /// certificate cannot be generated, or when the kind cannot be answered at
    /// all — which [`can_answer`](Self::can_answer) is asked first to avoid.
    pub fn publish(
        &self,
        kind: AcmeChallenge,
        domain: &str,
        token: &str,
        key_authorization: &str,
    ) -> Result<(), Error> {
        match kind {
            AcmeChallenge::Http01 => {
                publish(token, key_authorization);

                Ok(())
            }
            AcmeChallenge::TlsAlpn01 => {
                let resolver = self.resolver.as_ref().ok_or_else(|| {
                    human_errors::system(
                        "A tls-alpn-01 challenge cannot be answered without the public listener's certificate resolver.",
                        &["Set `[web.public.tls] mode = \"acme\"`, or use `challenge = \"http-01\"`."],
                    )
                })?;

                resolver.set_challenge(TlsAlpnChallenge {
                    sni: domain.to_ascii_lowercase(),
                    certified: challenge_certificate(domain, key_authorization)?,
                });

                Ok(())
            }
        }
    }

    /// Disarms it, whatever kind it was.
    pub fn withdraw(&self, kind: AcmeChallenge, token: &str) {
        match kind {
            AcmeChallenge::Http01 => withdraw(token),
            AcmeChallenge::TlsAlpn01 => {
                if let Some(resolver) = &self.resolver {
                    resolver.clear_challenge();
                }
            }
        }
    }
}

/// The throwaway certificate a `tls-alpn-01` handshake is answered with.
///
/// Self-signed for the name being validated, carrying the SHA-256 of the key
/// authorization in the `acmeIdentifier` extension — which is the whole proof.
/// It is never presented to an ordinary client: the resolver hands it out only
/// for a handshake that asked for `acme-tls/1` by ALPN *and* for this name.
fn challenge_certificate(
    domain: &str,
    key_authorization: &str,
) -> Result<Arc<rustls::sign::CertifiedKey>, Error> {
    // ECDSA whatever `[pki] key_type` says: nothing verifies this certificate's
    // signature — the authority reads one extension out of it — and generating
    // an RSA key would put hundreds of milliseconds inside a challenge window.
    let key = generate_key(KeyType::EcdsaP256)?;

    let mut params =
        rcgen::CertificateParams::new(vec![domain.to_owned()]).or_system_err(ADVICE_CHALLENGE)?;
    // RFC 8737 §3: the extension value is the SHA-256 of the key
    // authorization. `KeyAuthorization::digest` computes the same bytes; it is
    // done here so that a test can arm a challenge without an order in flight.
    params.custom_extensions = vec![rcgen::CustomExtension::new_acme_identifier(
        &Sha256::digest(key_authorization.as_bytes()),
    )];

    let certificate = params.self_signed(&key).or_system_err(ADVICE_CHALLENGE)?;

    crate::pki::tls::install_crypto_provider();

    // `CertifiedKey::from_der` checks that the key matches the certificate by
    // parsing the certificate with webpki, which refuses a critical extension
    // it does not know — and `acmeIdentifier` is precisely that. The key is
    // loaded and paired directly instead; it was generated four lines above,
    // so there is nothing for the check to find.
    let signing_key = rustls::crypto::aws_lc_rs::default_provider()
        .key_provider
        .load_private_key(rustls_pki_types::PrivateKeyDer::Pkcs8(
            key.serialize_der().into(),
        ))
        .or_system_err(ADVICE_CHALLENGE)?;

    Ok(Arc::new(rustls::sign::CertifiedKey::new(
        vec![certificate.der().clone()],
        signing_key,
    )))
}

/// Advice for a challenge we could not prepare.
const ADVICE_CHALLENGE: &[&str] =
    &["This is unexpected; please report it with the surrounding log entries."];

/// How a challenge kind is named in the protocol.
pub fn wire_type(kind: AcmeChallenge) -> ChallengeType {
    match kind {
        AcmeChallenge::TlsAlpn01 => ChallengeType::TlsAlpn01,
        AcmeChallenge::Http01 => ChallengeType::Http01,
    }
}

/// The kind a protocol challenge type corresponds to, where we answer it.
pub fn from_wire(kind: &ChallengeType) -> Option<AcmeChallenge> {
    match kind {
        ChallengeType::TlsAlpn01 => Some(AcmeChallenge::TlsAlpn01),
        ChallengeType::Http01 => Some(AcmeChallenge::Http01),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};

    use super::*;

    /// A key authorization is `<token>.<account key thumbprint>`; nothing here
    /// parses it, so any stable string exercises the same paths.
    fn authorization(token: &str) -> String {
        format!("{token}.thumbprint")
    }

    #[actix_web::test]
    async fn a_token_nobody_published_is_not_found() {
        let app = test::init_service(App::new().configure(routes)).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/.well-known/acme-challenge/never-published")
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_published_token_is_answered_with_exactly_the_key_authorization() {
        let token = "published-token-for-the-answer-test";
        publish(token, &authorization(token));

        let app = test::init_service(App::new().configure(routes)).await;
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!("{HTTP01_PREFIX}/{token}"))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);

        let body = test::read_body(response).await;
        assert_eq!(
            body,
            authorization(token).as_bytes(),
            "the authority compares the body byte for byte",
        );

        withdraw(token);
    }

    #[actix_web::test]
    async fn a_withdrawn_token_stops_being_answered() {
        let token = "withdrawn-token-for-the-withdraw-test";
        publish(token, &authorization(token));
        withdraw(token);

        let app = test::init_service(App::new().configure(routes)).await;
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!("{HTTP01_PREFIX}/{token}"))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[actix_web::test]
    async fn a_responder_without_a_resolver_cannot_answer_tls_alpn_01() {
        let without = Responder::new(None);
        assert!(!without.can_answer(AcmeChallenge::TlsAlpn01));
        assert!(without.can_answer(AcmeChallenge::Http01));

        let with = Responder::new(Some(HotSwapCertResolver::new(None)));
        assert!(with.can_answer(AcmeChallenge::TlsAlpn01));
    }

    #[actix_web::test]
    async fn publishing_a_tls_alpn_challenge_arms_the_resolver_and_withdrawing_disarms_it() {
        let resolver = HotSwapCertResolver::new(None);
        let responder = Responder::new(Some(Arc::clone(&resolver)));
        responder
            .publish(
                AcmeChallenge::TlsAlpn01,
                "TAK.example.com",
                "token",
                "token.thumbprint",
            )
            .unwrap();

        // The resolver's own tests cover which handshake it answers; what
        // matters here is that something was armed, under the lower-cased name.
        assert!(format!("{resolver:?}").contains("tak.example.com"));

        responder.withdraw(AcmeChallenge::TlsAlpn01, "token");
        assert!(!format!("{resolver:?}").contains("tak.example.com"));
    }

    #[actix_web::test]
    async fn a_tls_alpn_challenge_certificate_carries_the_acme_identifier_extension() {
        let certified = challenge_certificate("tak.example.com", "token.thumbprint").unwrap();
        let (_, parsed) =
            x509_parser::parse_x509_certificate(certified.end_entity_cert().unwrap()).unwrap();

        // 1.3.6.1.5.5.7.1.31 — the `acmeIdentifier` extension RFC 8737 defines.
        let oid = x509_parser::oid_registry::Oid::from(&[1, 3, 6, 1, 5, 5, 7, 1, 31]).unwrap();

        let extension = parsed
            .get_extension_unique(&oid)
            .unwrap()
            .expect("the proof is the extension; without it the challenge fails");
        assert!(extension.critical, "RFC 8737 requires it to be critical");
    }

    #[actix_web::test]
    async fn every_challenge_we_offer_survives_the_round_trip_to_the_wire() {
        for kind in [AcmeChallenge::TlsAlpn01, AcmeChallenge::Http01] {
            assert_eq!(from_wire(&wire_type(kind)), Some(kind));
        }

        assert_eq!(from_wire(&ChallengeType::Dns01), None);
    }
}

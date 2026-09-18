//! One order, from "I would like a certificate" to a chain on disk.
//!
//! RFC 8555 orders run through a small state machine, and the whole of it is
//! [`place`]:
//!
//! 1. **new-order** with one identifier per name;
//! 2. one **authorisation** per identifier, each already `valid` (the authority
//!    remembers a recent proof) or `pending`;
//! 3. for each pending one, a **challenge** is armed, marked ready, and the
//!    order polled until it is `ready`;
//! 4. the order is **finalised** with a CSR over a key we generate;
//! 5. the **chain** is downloaded.
//!
//! # Why the challenge is chosen per authorisation
//!
//! `[acme] challenge` is a preference, not a demand: an authority may not offer
//! it for a particular name, and the other one may still be answerable. The
//! preference is tried first, the other is tried second, and an authorisation
//! offering neither is the error — which names the authorisation rather than
//! failing later with "order invalid".

use chrono::{DateTime, TimeZone as _, Utc};
use instant_acme::{Account, AuthorizationStatus, Identifier, NewOrder, OrderStatus, RetryPolicy};

use rustak_core::prelude::*;

use crate::config::{AcmeChallenge, KeyType};
use crate::pki::keys::generate_key;

use super::account::failed;
use super::challenge::{Responder, from_wire, wire_type};

/// Advice for an order that did not finish.
const ADVICE_ORDER: &[&str] = &[
    "Check that every name in `[acme] domains` resolves to this server from the public internet.",
    "Check that the port the challenge is answered on is reachable: 443 for tls-alpn-01, 80 for http-01.",
    "Test against `directory = \"letsencrypt-staging\"` first; production rate limits are unforgiving.",
];

/// What an order produced.
pub struct IssuedChain {
    /// The chain as the authority returned it, leaf first.
    pub chain_pem: String,
    /// The PKCS#8 private key we generated for it.
    pub key_pkcs8: Vec<u8>,
    pub not_before: DateTime<Utc>,
    pub not_after: DateTime<Utc>,
    /// How the last authorisation was validated, for the record.
    pub challenge: AcmeChallenge,
}

impl std::fmt::Debug for IssuedChain {
    /// Written out so that the private key cannot reach a log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssuedChain")
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

/// Places one order and returns the chain it produced.
///
/// `preference` is `[acme] challenge`; `responder` decides what can actually be
/// answered and arms it.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error carrying the authority's own words
/// when the order is refused, no offered challenge can be answered, validation
/// does not complete, or the chain will not parse.
#[instrument("pki.acme.order", skip_all, fields(domains = domains.len()), err(Display))]
pub async fn place(
    account: &Account,
    domains: &[String],
    preference: AcmeChallenge,
    responder: &Responder,
    key_type: KeyType,
) -> Result<IssuedChain, Error> {
    let identifiers: Vec<Identifier> = domains.iter().cloned().map(Identifier::Dns).collect();

    let mut order = account
        .new_order(&NewOrder::new(&identifiers))
        .await
        .map_err(|err| failed("The certificate authority refused the order.", &err))?;

    // Everything from here on may leave a challenge armed, so the withdrawal
    // is unconditional and the failure is reported after it. A `?` in the
    // middle of the loop would otherwise leave a path serving a secret, or the
    // resolver answering `acme-tls/1` handshakes, until the next order.
    let mut armed: Vec<(AcmeChallenge, String)> = Vec::new();
    let outcome = answer_and_finish(
        &mut order, domains, preference, responder, key_type, &mut armed,
    )
    .await;

    for (kind, token) in &armed {
        responder.withdraw(*kind, token);
    }

    let (chain_pem, key_pkcs8, used) = outcome?;
    let (not_before, not_after) = validity(&chain_pem)?;

    info!(
        not_after = %not_after,
        challenge = used.as_str(),
        "An ACME certificate was issued."
    );

    Ok(IssuedChain {
        chain_pem,
        key_pkcs8,
        not_before,
        not_after,
        challenge: used,
    })
}

/// Answers every pending authorisation, then finalises and downloads.
///
/// Records what it armed in `armed` as it goes, so that [`place`] can disarm
/// it whether this succeeded or not.
async fn answer_and_finish(
    order: &mut instant_acme::Order,
    domains: &[String],
    preference: AcmeChallenge,
    responder: &Responder,
    key_type: KeyType,
    armed: &mut Vec<(AcmeChallenge, String)>,
) -> Result<(String, Vec<u8>, AcmeChallenge), Error> {
    let mut used = preference;

    {
        let mut authorizations = order.authorizations();

        while let Some(result) = authorizations.next().await {
            let mut authorization =
                result.map_err(|err| failed("An ACME authorisation could not be read.", &err))?;

            if authorization.status == AuthorizationStatus::Valid {
                continue;
            }

            if authorization.status != AuthorizationStatus::Pending {
                return Err(human_errors::system(
                    format!(
                        "The certificate authority reported an authorisation as {:?}, which cannot be answered.",
                        authorization.status
                    ),
                    ADVICE_ORDER,
                ));
            }

            let domain = authorization.identifier().to_string();
            let kind = choose(&authorization, preference, responder, &domain)?;
            let mut challenge = authorization.challenge(wire_type(kind)).ok_or_else(|| {
                human_errors::system(
                    format!(
                        "The {} challenge for {domain} disappeared between being offered and being asked for.",
                        kind.as_str()
                    ),
                    ADVICE_ORDER,
                )
            })?;

            let token = challenge.token.clone();
            let key_authorization = challenge.key_authorization();

            responder.publish(kind, &domain, &token, key_authorization.as_str())?;
            armed.push((kind, token));
            used = kind;

            challenge.set_ready().await.map_err(|err| {
                failed("The authority would not accept our challenge answer.", &err)
            })?;
        }
    }

    let (chain_pem, key_pkcs8) = finish(order, domains, key_type).await?;

    Ok((chain_pem, key_pkcs8, used))
}

/// Which challenge to answer for one authorisation.
///
/// The preference first, then the other, then an error naming the name — an
/// authority that offers only `dns-01` for a wildcard is a real case, and
/// "order invalid" three polls later is not an answer anybody can act on.
fn choose(
    authorization: &instant_acme::AuthorizationHandle<'_>,
    preference: AcmeChallenge,
    responder: &Responder,
    domain: &str,
) -> Result<AcmeChallenge, Error> {
    let offered: Vec<AcmeChallenge> = authorization
        .challenges
        .iter()
        .filter_map(|challenge| from_wire(&challenge.r#type))
        .collect();

    let other = match preference {
        AcmeChallenge::TlsAlpn01 => AcmeChallenge::Http01,
        AcmeChallenge::Http01 => AcmeChallenge::TlsAlpn01,
    };

    for kind in [preference, other] {
        if offered.contains(&kind) && responder.can_answer(kind) {
            if kind != preference {
                warn!(
                    domain,
                    preferred = preference.as_str(),
                    using = kind.as_str(),
                    "The authority does not offer the configured ACME challenge for this name; falling back."
                );
            }

            return Ok(kind);
        }
    }

    Err(human_errors::system(
        format!(
            "The authority offers no challenge for {domain} that this server can answer (it offered {}).",
            offered
                .iter()
                .map(|kind| kind.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        ADVICE_ORDER,
    ))
}

/// Waits for validation, finalises with our own CSR, and downloads the chain.
async fn finish(
    order: &mut instant_acme::Order,
    domains: &[String],
    key_type: KeyType,
) -> Result<(String, Vec<u8>), Error> {
    let status = order
        .poll_ready(&RetryPolicy::default())
        .await
        .map_err(|err| failed("The authority did not validate our answer.", &err))?;

    if status != OrderStatus::Ready {
        return Err(human_errors::system(
            format!("The ACME order ended as {status:?} rather than ready."),
            ADVICE_ORDER,
        ));
    }

    // Our own CSR rather than `Order::finalize`: that needs instant-acme's
    // `rcgen` feature, and it would choose the key type instead of `[pki]`.
    let names = domains.to_vec();
    let key = tokio::task::spawn_blocking(move || {
        let key = generate_key(key_type)?;
        let mut params = rcgen::CertificateParams::new(names).or_system_err(ADVICE_ORDER)?;
        // The subject is left empty: a public authority ignores what a CSR
        // asks for beyond the public key and the names, and CA/Browser Forum
        // rules forbid a common name it did not put there itself.
        params.distinguished_name = rcgen::DistinguishedName::new();

        let csr = params.serialize_request(&key).or_system_err(ADVICE_ORDER)?;

        Ok::<_, Error>((key.serialize_der(), csr.der().to_vec()))
    })
    .await
    .or_system_err(ADVICE_ORDER)??;

    order
        .finalize_csr(&key.1)
        .await
        .map_err(|err| failed("The authority refused our signing request.", &err))?;

    let chain = order
        .poll_certificate(&RetryPolicy::default())
        .await
        .map_err(|err| failed("The issued certificate could not be downloaded.", &err))?;

    Ok((chain, key.0))
}

/// The leaf's validity window, which is what the renewal decision is made on.
fn validity(chain_pem: &str) -> Result<(DateTime<Utc>, DateTime<Utc>), Error> {
    let leaf = pem::parse_many(chain_pem)
        .or_system_err(ADVICE_ORDER)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            human_errors::system(
                "The authority returned a certificate chain with nothing in it.",
                ADVICE_ORDER,
            )
        })?;

    let (_, parsed) =
        x509_parser::parse_x509_certificate(leaf.contents()).or_system_err(ADVICE_ORDER)?;

    let at = |time: x509_parser::time::ASN1Time| {
        Utc.timestamp_opt(time.timestamp(), 0)
            .single()
            .ok_or_else(|| {
                human_errors::system(
                    "The issued certificate carries a validity date we cannot represent.",
                    ADVICE_ORDER,
                )
            })
    };

    Ok((
        at(parsed.validity().not_before)?,
        at(parsed.validity().not_after)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chain whose leaf is valid for `days`, as the authority would return.
    fn chain(days: i64) -> String {
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params =
            rcgen::CertificateParams::new(vec!["tak.example.com".to_string()]).unwrap();
        let now = Utc::now();
        params.not_before = rcgen::date_time_ymd(now.year(), now.month() as u8, now.day() as u8);
        let end = now + chrono::Duration::days(days);
        params.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);

        params.self_signed(&key).unwrap().pem()
    }

    use chrono::Datelike as _;

    #[test]
    fn the_leaf_decides_the_validity_window() {
        let (not_before, not_after) = validity(&chain(90)).unwrap();

        assert!(not_before <= Utc::now());
        assert!(not_after > Utc::now() + chrono::Duration::days(88));
    }

    #[test]
    fn a_chain_with_nothing_in_it_is_refused_rather_than_stored() {
        assert!(validity("").is_err());
        assert!(
            validity("-----BEGIN CERTIFICATE-----\nnot base64\n-----END CERTIFICATE-----").is_err()
        );
    }

    #[test]
    fn the_first_certificate_is_the_one_read() {
        // A chain is leaf first; reading the issuer's dates instead would put
        // the renewal years away.
        let leaf = chain(30);
        let issuer = chain(3650);
        let (_, not_after) = validity(&format!("{leaf}{issuer}")).unwrap();

        assert!(not_after < Utc::now() + chrono::Duration::days(60));
    }
}

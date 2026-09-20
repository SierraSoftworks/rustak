//! Deciding whether an assertion really came from an orchestrator we trust.
//!
//! The order is the whole of the security argument, and it is the order below:
//! **algorithm, key, signature, `iss`, `aud`, `exp`, `nbf`, `iat` — and only
//! then any other claim.** Nothing reads `nomad_job_id` or
//! `kubernetes.io/namespace` until the bytes carrying them have been proved to
//! be the orchestrator's, because until then they are a string somebody sent
//! us.
//!
//! Four refusals matter, and each is a different attack:
//!
//! 1. **An algorithm the token chose.** A key set is public, so a token signed
//!    `HS256` with a published modulus as the secret would verify if the token
//!    got to pick. The issuer's `algorithms` is an allow-list and the header is
//!    checked against it before a key is even looked up.
//! 2. **No `kid`.** It is how we decide which key to check against; a token
//!    without one could only be accepted unverified.
//! 3. **A missing `aud` or `iss`.** `jsonwebtoken` compares those *only against
//!    a token that carries them*, so leaving one out is accepted where naming
//!    the wrong one is refused — unless they are required by name, which they
//!    are here.
//! 4. **An `iss` an issuer entry did not ask for.** An entry with no `issuer`
//!    exists for the one orchestrator that publishes none (a Nomad cluster
//!    without `oidc_issuer`), and a token that *does* carry one is refused by
//!    it rather than matched loosely.
//!
//! # Which issuer a token is judged against
//!
//! Its unverified `iss`, used as a **routing hint only**: it selects the
//! candidate entries, and the entry then re-checks `iss` after the signature.
//! A hint that is a lie selects an issuer whose keys will not verify the token,
//! which is a refusal either way.

use base64::Engine as _;
use jsonwebtoken::jwk::JwkSet;

use crate::config::{WorkloadConfig, WorkloadIssuer};
use crate::prelude::*;

use super::claims::Claims;
use super::rules::{Binding, RuleRefusal, bind};
use super::{Assertion, Refusal};

/// The base64 variant a JWT's segments are encoded with.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Verifies an assertion and answers the account it speaks for.
///
/// # Errors
///
/// A [`Refusal`] naming what was wrong, which the caller records and does not
/// repeat to whoever presented the token.
#[instrument("auth.workload.verify", skip_all, err(Debug))]
pub async fn verify<S: Services>(services: &S, token: &str) -> Result<Assertion, Refusal> {
    let config = services.config();
    let workload = &config.auth.workload;

    if !workload.is_enabled() {
        return Err(Refusal::NotConfigured);
    }

    let header = jsonwebtoken::decode_header(token).map_err(|_| Refusal::Malformed)?;
    let kid = header.kid.clone().ok_or(Refusal::Malformed)?;
    let presented = unverified_issuer(token);

    let candidates: Vec<&WorkloadIssuer> = workload
        .issuers
        .iter()
        .filter(|issuer| issuer.issuer.as_deref() == presented.as_deref())
        .collect();

    if candidates.is_empty() {
        debug!(
            issuer = ?presented,
            "Refused an assertion whose issuer no `[auth.workload]` entry claims.",
        );

        return Err(Refusal::UnknownIssuer);
    }

    let mut refusal = Refusal::UnknownIssuer;

    for issuer in candidates {
        match against(services, workload, issuer, &kid, header.alg, token).await {
            Ok(assertion) => return Ok(assertion),
            Err(Refusal::Unavailable(err)) => return Err(Refusal::Unavailable(err)),
            Err(found) => refusal = found,
        }
    }

    Err(refusal)
}

/// One issuer's answer about one token.
async fn against<S: Services>(
    services: &S,
    workload: &WorkloadConfig,
    issuer: &WorkloadIssuer,
    kid: &str,
    algorithm: jsonwebtoken::Algorithm,
    token: &str,
) -> Result<Assertion, Refusal> {
    if !issuer
        .algorithms
        .iter()
        .any(|allowed| allowed.algorithm() == algorithm)
    {
        warn!(
            issuer = %issuer.name,
            algorithm = ?algorithm,
            "Refused an assertion signed with an algorithm this issuer does not use.",
        );

        return Err(Refusal::Algorithm);
    }

    let keys = super::keys::key_set(services, issuer)
        .await
        .map_err(Refusal::Unavailable)?;

    // A token naming a key we have not seen may mean the orchestrator rotated
    // since we cached; refetch once — throttled — before refusing it.
    let keys = match keys.find(kid) {
        Some(_) => keys,
        None => match super::keys::refreshed(services, issuer)
            .await
            .map_err(Refusal::Unavailable)?
        {
            Some(refetched) => refetched,
            None => return Err(Refusal::UnknownKey),
        },
    };

    let claims = decode(issuer, &keys, kid, algorithm, token)?;

    fresh_enough(issuer, &claims)?;

    let Binding {
        account,
        namespace,
        subject,
    } = bind(workload, &issuer.name, &claims).map_err(Refusal::Rule)?;

    Ok(Assertion {
        issuer_name: issuer.name.clone(),
        issuer: issuer.issuer.clone(),
        account,
        namespace,
        subject,
        jti: string(&claims, "jti"),
        sub: string(&claims, "sub"),
        claims,
    })
}

/// The signature, the audience, the issuer and the two time windows.
fn decode(
    issuer: &WorkloadIssuer,
    keys: &JwkSet,
    kid: &str,
    algorithm: jsonwebtoken::Algorithm,
    token: &str,
) -> Result<Claims, Refusal> {
    let jwk = keys.find(kid).ok_or(Refusal::UnknownKey)?;
    let key = jsonwebtoken::DecodingKey::from_jwk(jwk).map_err(|err| {
        Refusal::Unavailable(human_errors::system(
            format!(
                "We could not build a verification key from the `{}` key set ({err}).",
                issuer.name
            ),
            &["This usually means the orchestrator published a key in a form we do not support."],
        ))
    })?;

    let mut validation = jsonwebtoken::Validation::new(algorithm);
    validation.algorithms = vec![algorithm];
    validation.leeway = issuer.clock_skew.num_seconds().max(0).unsigned_abs();
    validation.validate_exp = true;
    validation.validate_nbf = true;
    validation.validate_aud = true;
    validation.set_audience(&[issuer.audience.as_str()]);

    // `aud` and `iss` are compared only against a token that carries them, so
    // a token that simply leaves one out is accepted unless it is required by
    // name. See the module documentation.
    let mut required = vec!["exp", "aud"];

    if let Some(expected) = &issuer.issuer {
        validation.set_issuer(&[expected.as_str()]);
        required.push("iss");
    }

    validation.set_required_spec_claims(&required);

    jsonwebtoken::decode::<Claims>(token, &key, &validation)
        .map(|data| data.claims)
        .map_err(|err| {
            debug!(issuer = %issuer.name, error = %err, "Refused a workload assertion.");

            Refusal::Claims
        })
}

/// `iat` must be present and must not be in the future.
///
/// `jsonwebtoken` validates `exp` and `nbf` and leaves `iat` alone, which would
/// leave a token claiming to have been issued next week indistinguishable from
/// one issued a second ago — and `iat` is what an operator reads out of the
/// audit trail to work out which run of a task enrolled.
fn fresh_enough(issuer: &WorkloadIssuer, claims: &Claims) -> Result<(), Refusal> {
    let Some(issued) = claims.get("iat").and_then(serde_json::Value::as_i64) else {
        debug!(issuer = %issuer.name, "Refused a workload assertion that says nothing about when it was issued.");

        return Err(Refusal::Claims);
    };

    let ceiling = chrono::Utc::now().timestamp() + issuer.clock_skew.num_seconds().max(0);

    if issued > ceiling {
        debug!(issuer = %issuer.name, "Refused a workload assertion issued in the future.");

        return Err(Refusal::Claims);
    }

    Ok(())
}

/// The `iss` a token claims, read **without** verifying anything.
///
/// A routing hint and nothing else; see the [module documentation](self).
/// Answers [`None`] for a token we cannot take apart at all, which selects the
/// entries that expect no `iss` and is refused by them a moment later.
fn unverified_issuer(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let decoded = B64.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;

    claims.get("iss")?.as_str().map(str::to_owned)
}

/// A string claim, for the two we copy onto the assertion.
fn string(claims: &Claims, key: &str) -> Option<String> {
    claims
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// Whether a rule refusal is one the caller should report as ambiguous.
pub(super) fn is_ambiguous(refusal: &Refusal) -> bool {
    matches!(refusal, Refusal::Rule(RuleRefusal::Ambiguous { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_issuer_a_token_claims_is_read_without_verifying_it() {
        let token = format!(
            "{}.{}.not-a-signature",
            B64.encode(br#"{"alg":"RS256","kid":"k1"}"#),
            B64.encode(br#"{"iss":"https://nomad.example.com","sub":"x"}"#),
        );

        assert_eq!(
            unverified_issuer(&token).as_deref(),
            Some("https://nomad.example.com"),
        );
    }

    #[test]
    fn a_token_with_no_issuer_or_no_shape_at_all_reads_as_none() {
        let without = format!(
            "{}.{}.sig",
            B64.encode(br#"{"alg":"RS256"}"#),
            B64.encode(br#"{"sub":"x"}"#),
        );

        assert_eq!(unverified_issuer(&without), None);
        assert_eq!(unverified_issuer("not-a-jwt"), None);
        assert_eq!(unverified_issuer(""), None);
        assert_eq!(unverified_issuer("a.!!!.c"), None);
    }

    #[test]
    fn an_assertion_issued_in_the_future_is_refused_and_one_inside_the_skew_is_not() {
        let issuer = crate::config::WorkloadIssuer {
            name: "nomad".to_string(),
            issuer: None,
            jwks_url: None,
            discovery_url: None,
            jwks_file: None,
            audience: "rustak".to_string(),
            algorithms: Vec::new(),
            clock_skew: chrono::Duration::seconds(30),
            jwks_refresh: chrono::Duration::hours(1),
            allow_insecure_jwks: false,
        };

        let at = |offset: i64| {
            serde_json::json!({ "iat": chrono::Utc::now().timestamp() + offset })
                .as_object()
                .unwrap()
                .clone()
        };

        assert!(fresh_enough(&issuer, &at(-5)).is_ok());
        assert!(fresh_enough(&issuer, &at(10)).is_ok(), "inside the skew");
        assert!(fresh_enough(&issuer, &at(600)).is_err());
        assert!(
            fresh_enough(&issuer, serde_json::json!({}).as_object().unwrap()).is_err(),
            "a token that says nothing about when it was issued is not one we date",
        );
    }
}

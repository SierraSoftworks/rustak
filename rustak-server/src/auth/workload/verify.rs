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
use super::rules::{Binding, bind};
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
#[instrument("auth.workload.verify", skip_all, err(level = "debug", Debug))]
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
            None => return Err(unknown_key(issuer, kid)),
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
    let jwk = keys.find(kid).ok_or_else(|| unknown_key(issuer, kid))?;
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
        .map_err(|err| Refusal::Claims(explain(issuer, &err, token)))
}

/// Which check a token failed, and by how much — with nothing of the token in
/// it.
///
/// An operator reading `error=Claims` learns that something about a token was
/// wrong and nothing else; the first live deployment spent two hours not
/// knowing that "something" was an `exp` an hour in the past. So the check is
/// named, and the distance is measured — and the token, its signature and every
/// claim that is not one of these is left out, because this is written to a log
/// and handed to whoever presented the credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaimRefusal {
    check: &'static str,
    detail: String,
}

impl ClaimRefusal {
    /// A refusal of `check`, described by `detail`.
    pub(super) fn new(check: &'static str, detail: impl Into<String>) -> Self {
        Self {
            check,
            detail: detail.into(),
        }
    }

    /// The claim or check that failed, for the log's `reason` field.
    pub fn check(&self) -> &'static str {
        self.check
    }
}

impl std::fmt::Display for ClaimRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.check, self.detail)
    }
}

/// A key the issuer does not publish, named.
///
/// The `kid` came off the wire, so it is tidied before it is repeated: it
/// reaches a log line and an `error_description`, and neither is a place for
/// whatever bytes a caller chose.
fn unknown_key(issuer: &WorkloadIssuer, kid: &str) -> Refusal {
    Refusal::UnknownKey {
        kid: tidy(kid),
        issuer: issuer.name.clone(),
    }
}

/// What `jsonwebtoken` refused a token for, in the words an operator reads.
///
/// Every branch here reads the payload again. That is safe *because of where it
/// is*: `jsonwebtoken::decode` verifies the signature before it validates a
/// single claim, so a token that reached a claim error is one the orchestrator
/// really signed. A signature failure gets no detail at all, for the same
/// reason.
fn explain(
    issuer: &WorkloadIssuer,
    err: &jsonwebtoken::errors::Error,
    token: &str,
) -> ClaimRefusal {
    use jsonwebtoken::errors::ErrorKind;

    let now = chrono::Utc::now().timestamp();

    match err.kind() {
        ErrorKind::ExpiredSignature => match number(token, "exp") {
            Some(exp) => ClaimRefusal::new("exp", format!("expired {} ago", span(now - exp))),
            None => ClaimRefusal::new("exp", "expired"),
        },
        ErrorKind::ImmatureSignature => match number(token, "nbf") {
            Some(nbf) => {
                ClaimRefusal::new("nbf", format!("not valid for another {}", span(nbf - now)))
            }
            None => ClaimRefusal::new("nbf", "not valid yet"),
        },
        ErrorKind::InvalidAudience => ClaimRefusal::new(
            "aud",
            format!(
                "expected {}, token names [{}]",
                tidy(&issuer.audience),
                audience(token).join(", "),
            ),
        ),
        ErrorKind::InvalidIssuer => ClaimRefusal::new(
            "iss",
            format!(
                "the `iss` is not the one the `{}` entry expects",
                issuer.name
            ),
        ),
        ErrorKind::MissingRequiredClaim(name) => {
            ClaimRefusal::new("claims", format!("the token carries no {}", tidy(name)))
        }
        ErrorKind::InvalidClaimFormat(name) => ClaimRefusal::new(
            "claims",
            format!("{} is not the shape a JWT gives it", tidy(name)),
        ),
        ErrorKind::InvalidSignature => ClaimRefusal::new(
            "signature",
            format!(
                "the signature is not one the key it names in the `{}` key set verifies",
                issuer.name
            ),
        ),
        ErrorKind::InvalidAlgorithm => ClaimRefusal::new(
            "alg",
            format!(
                "that algorithm is not one the `{}` entry allows",
                issuer.name
            ),
        ),
        _ => ClaimRefusal::new(
            "claims",
            "the claims are not ones this issuer's tokens carry",
        ),
    }
}

/// A numeric claim, read from a token whose signature has already verified.
fn number(token: &str, name: &str) -> Option<i64> {
    claims_of(token)?.get(name)?.as_i64()
}

/// The audiences a token names, tidied for a log line.
fn audience(token: &str) -> Vec<String> {
    match claims_of(token)
        .as_ref()
        .and_then(|claims| claims.get("aud"))
    {
        Some(serde_json::Value::String(one)) => vec![tidy(one)],
        Some(serde_json::Value::Array(many)) => many
            .iter()
            .filter_map(serde_json::Value::as_str)
            .take(4)
            .map(tidy)
            .collect(),
        _ => Vec::new(),
    }
}

/// A token's payload, decoded and not verified.
fn claims_of(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;

    serde_json::from_slice(&B64.decode(payload).ok()?).ok()
}

/// A value off the wire, made safe to repeat in a log line.
///
/// Quoted, escaped, stripped of control characters and cut short: a refusal
/// message is written to a log and handed back to whoever presented the token,
/// and neither is a place for arbitrary bytes.
fn tidy(value: impl AsRef<str>) -> String {
    let cleaned: String = value
        .as_ref()
        .chars()
        .filter(|character| !character.is_control())
        .take(64)
        .collect();

    format!("{cleaned:?}")
}

/// A number of seconds, as an operator reads it: "40s", "58m12s", "1h02m".
fn span(seconds: i64) -> String {
    let seconds = seconds.max(0);

    match (seconds / 3600, (seconds % 3600) / 60, seconds % 60) {
        (0, 0, seconds) => format!("{seconds}s"),
        (0, minutes, seconds) => format!("{minutes}m{seconds:02}s"),
        (hours, minutes, _) => format!("{hours}h{minutes:02}m"),
    }
}

/// `iat` must be present and must not be in the future.
///
/// `jsonwebtoken` validates `exp` and `nbf` and leaves `iat` alone, which would
/// leave a token claiming to have been issued next week indistinguishable from
/// one issued a second ago — and `iat` is what an operator reads out of the
/// audit trail to work out which run of a task enrolled.
fn fresh_enough(issuer: &WorkloadIssuer, claims: &Claims) -> Result<(), Refusal> {
    let Some(issued) = claims.get("iat").and_then(serde_json::Value::as_i64) else {
        return Err(Refusal::Claims(ClaimRefusal::new(
            "iat",
            "the token says nothing about when it was issued",
        )));
    };

    let now = chrono::Utc::now().timestamp();
    let ceiling = now + issuer.clock_skew.num_seconds().max(0);

    if issued > ceiling {
        return Err(Refusal::Claims(ClaimRefusal::new(
            "iat",
            format!(
                "issued {} in the future, which is more than the {} this issuer allows",
                span(issued - now),
                span(issuer.clock_skew.num_seconds()),
            ),
        )));
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

    /// A token carrying `claims`, signed with nothing: `explain` is only ever
    /// reached for a token whose signature has already verified, so what these
    /// exercise is the reading, not the trusting.
    fn token(claims: serde_json::Value) -> String {
        format!(
            "{}.{}.a-signature-this-test-does-not-check",
            B64.encode(br#"{"alg":"RS256","kid":"rustak-workload-1"}"#),
            B64.encode(claims.to_string().as_bytes()),
        )
    }

    /// The issuer entry the reference deployment writes.
    fn nomad() -> crate::config::WorkloadIssuer {
        crate::config::WorkloadIssuer {
            name: "nomad".to_string(),
            issuer: Some("https://nomad.example.com".to_string()),
            jwks_url: None,
            discovery_url: None,
            jwks_file: None,
            audience: "rustak".to_string(),
            algorithms: Vec::new(),
            clock_skew: chrono::Duration::seconds(30),
            jwks_refresh: chrono::Duration::hours(1),
            allow_insecure_jwks: false,
        }
    }

    #[test]
    fn an_expired_token_is_refused_by_name_and_by_how_much() {
        // The production defect said `error=Claims` and nothing else, for two
        // hours, five seconds apart. This is the line that would have ended it
        // in one read.
        let now = chrono::Utc::now().timestamp();
        let token = token(serde_json::json!({ "exp": now - 3_492, "aud": ["rustak"] }));

        let refusal = explain(
            &nomad(),
            &jsonwebtoken::errors::ErrorKind::ExpiredSignature.into(),
            &token,
        );

        assert_eq!(refusal.check(), "exp");
        assert_eq!(refusal.to_string(), "exp: expired 58m12s ago");
        assert!(
            !refusal.to_string().contains(&token),
            "and no part of the token is in it",
        );
    }

    #[test]
    fn a_token_that_is_not_valid_yet_says_how_long_it_will_be() {
        let now = chrono::Utc::now().timestamp();
        let token = token(serde_json::json!({ "nbf": now + 40 }));

        let refusal = explain(
            &nomad(),
            &jsonwebtoken::errors::ErrorKind::ImmatureSignature.into(),
            &token,
        );

        assert_eq!(refusal.check(), "nbf");
        assert_eq!(refusal.to_string(), "nbf: not valid for another 40s");
    }

    #[test]
    fn a_token_for_another_audience_names_both_audiences_and_nothing_else() {
        let token = token(serde_json::json!({ "aud": ["vault", "consul"] }));

        let refusal = explain(
            &nomad(),
            &jsonwebtoken::errors::ErrorKind::InvalidAudience.into(),
            &token,
        );

        assert_eq!(refusal.check(), "aud");
        assert_eq!(
            refusal.to_string(),
            r#"aud: expected "rustak", token names ["vault", "consul"]"#,
        );
    }

    #[test]
    fn a_signature_that_does_not_verify_is_told_nothing_about_itself() {
        // The one refusal that gets no detail: the bytes are not the
        // orchestrator's, so nothing in them is worth repeating.
        let token = token(serde_json::json!({ "exp": 1, "aud": ["rustak"] }));

        let refusal = explain(
            &nomad(),
            &jsonwebtoken::errors::ErrorKind::InvalidSignature.into(),
            &token,
        );

        assert_eq!(refusal.check(), "signature");
        assert!(!refusal.to_string().contains('1'), "{refusal}");
        assert!(!refusal.to_string().contains(&token));
    }

    #[test]
    fn a_missing_claim_is_named_and_a_hostile_one_cannot_carry_the_line_away() {
        let refusal = explain(
            &nomad(),
            &jsonwebtoken::errors::ErrorKind::MissingRequiredClaim("exp".to_string()).into(),
            &token(serde_json::json!({})),
        );

        assert_eq!(refusal.to_string(), r#"claims: the token carries no "exp""#);

        let hostile = explain(
            &nomad(),
            &jsonwebtoken::errors::ErrorKind::MissingRequiredClaim(format!(
                "a\nERROR everything is fine{}",
                "b".repeat(200)
            ))
            .into(),
            &token(serde_json::json!({})),
        );

        assert!(!hostile.to_string().contains('\n'), "{hostile}");
        assert!(hostile.to_string().len() < 120, "{hostile}");
    }

    #[test]
    fn a_duration_reads_the_way_an_operator_would_say_it() {
        assert_eq!(span(40), "40s");
        assert_eq!(span(3_492), "58m12s");
        assert_eq!(span(3_720), "1h02m");
        assert_eq!(span(-5), "0s", "a clock that stepped is not a negative age");
    }

    #[test]
    fn an_assertion_issued_in_the_future_is_refused_and_one_inside_the_skew_is_not() {
        let issuer = nomad();

        let at = |offset: i64| {
            serde_json::json!({ "iat": chrono::Utc::now().timestamp() + offset })
                .as_object()
                .unwrap()
                .clone()
        };

        assert!(fresh_enough(&issuer, &at(-5)).is_ok());
        assert!(fresh_enough(&issuer, &at(10)).is_ok(), "inside the skew");

        let refusal = fresh_enough(&issuer, &at(600)).unwrap_err();

        assert_eq!(refusal.reason(), "iat");
        assert!(
            refusal.sentence().contains("in the future"),
            "{}",
            refusal.sentence(),
        );
        assert_eq!(
            fresh_enough(&issuer, serde_json::json!({}).as_object().unwrap())
                .unwrap_err()
                .sentence(),
            "iat: the token says nothing about when it was issued",
            "a token that says nothing about when it was issued is not one we date",
        );
    }
}

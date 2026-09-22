//! What this sidecar's own workload assertion says, and whether it is still
//! worth presenting.
//!
//! Nothing here is a security decision. A server decides whether to believe a
//! token; this is the *holder* reading its own, so that it can say which
//! credential it is using, when that credential runs out, and — the thing this
//! module exists for — notice that what it just read from disk is already dead
//! before presenting it to a server that will rightly refuse it.
//!
//! # Why the holder checks its own expiry
//!
//! Nomad renews a workload identity by **rewriting the file**; the environment
//! variable it also offers is frozen at process start. A sidecar that reads its
//! token and presents it blindly therefore cannot tell "the server is
//! misconfigured" from "my copy of the token is an hour old", and the first
//! live deployment spent two hours failing every five seconds without either
//! end saying which. One unverified `exp` read is the difference.
//!
//! # And why it reads twice
//!
//! A renewal is not atomic from the outside: a read that lands while the
//! orchestrator is writing sees a truncated file, an empty one, or the old
//! token. [`read_fresh`] therefore gives it a moment and looks again before
//! deciding, and prefers whichever read answered something worth presenting.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use rustak_core::prelude::*;

/// How close to its expiry a token stops being worth presenting.
///
/// The same margin the access-token cache uses, and for the same reason: a
/// token that expires between the check and the request is a refusal that costs
/// a whole retry cycle to recover from.
pub(crate) const FRESH_WITHIN: Duration = Duration::seconds(60);

/// The clock, injected so that tests decide what "now" is.
///
/// A token's whole behaviour here is a comparison against the current instant,
/// and a test that arranges one by sleeping is a test that fails on a busy
/// machine. Nothing in production ever constructs anything but the default.
#[derive(Clone)]
pub(crate) struct Clock(Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>);

impl Default for Clock {
    fn default() -> Self {
        Self(Arc::new(Utc::now))
    }
}

impl std::fmt::Debug for Clock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Clock")
    }
}

impl Clock {
    /// What this clock says the time is.
    pub(crate) fn now(&self) -> DateTime<Utc> {
        (self.0)()
    }

    /// A clock that always answers the same instant.
    #[cfg(test)]
    pub(crate) fn fixed(at: DateTime<Utc>) -> Self {
        Self(Arc::new(move || at))
    }
}

/// One workload assertion, as this sidecar reads its own.
///
/// Holds the token beside the two things the holder needs to know about it:
/// whether it could be taken apart at all, and when it stops being valid.
#[derive(Clone, Debug)]
pub struct Assertion {
    token: Secret,
    expires_at: Option<DateTime<Utc>>,
    issuer: Option<String>,
    readable: bool,
}

impl Assertion {
    /// Reads a token, **without** verifying anything.
    pub fn read(token: Secret) -> Self {
        let claims = payload(&token);

        Self {
            expires_at: claims
                .as_ref()
                .and_then(|claims| claims.get("exp"))
                .and_then(serde_json::Value::as_i64)
                .and_then(|seconds| DateTime::from_timestamp(seconds, 0)),
            issuer: claims
                .as_ref()
                .and_then(|claims| claims.get("iss"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            readable: claims.is_some(),
            token,
        }
    }

    /// The token itself, to present.
    pub fn token(&self) -> &Secret {
        &self.token
    }

    /// When it stops being valid, when it says.
    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        self.expires_at
    }

    /// The `iss` it claims, for the identity line.
    pub fn issuer(&self) -> Option<&str> {
        self.issuer.as_deref()
    }

    /// Whether this is a JWT we could take apart at all.
    ///
    /// [`false`] is a file the orchestrator is part way through writing far
    /// more often than it is a deployment holding rubbish, so it is a reason to
    /// look again rather than a reason to fail.
    pub fn is_readable(&self) -> bool {
        self.readable
    }

    /// How long ago it expired, when it has.
    pub fn expired_for(&self, now: DateTime<Utc>) -> Option<Duration> {
        let expires_at = self.expires_at?;

        (now > expires_at).then(|| now - expires_at)
    }

    /// Whether presenting it now is worth doing.
    ///
    /// A token whose expiry we cannot read is presented: the server is the one
    /// that decides, and refusing to try would turn an unreadable claim into an
    /// outage of our own making.
    pub fn is_fresh_at(&self, now: DateTime<Utc>) -> bool {
        if !self.readable {
            return false;
        }

        self.expires_at
            .is_none_or(|expires_at| now + FRESH_WITHIN < expires_at)
    }
}

/// Reads an assertion, giving the orchestrator a moment when what is there is
/// not worth presenting.
///
/// `read` is the source, read afresh each time it is called; `settle` is how
/// long to wait before looking again. The second read wins whenever it answers
/// something better than the first — which is what a renewal in progress looks
/// like from here.
///
/// # Errors
///
/// Whatever `read` answered, when neither read produced a token at all.
pub(crate) async fn read_fresh<R>(
    mut read: R,
    clock: &Clock,
    settle: std::time::Duration,
) -> Result<Assertion, Error>
where
    R: FnMut() -> Result<Secret, Error>,
{
    let first = read().map(Assertion::read);

    if first
        .as_ref()
        .is_ok_and(|assertion| assertion.is_fresh_at(clock.now()))
    {
        return first;
    }

    tracing::debug!(
        "The workload identity that was read is not one worth presenting; looking again in case the orchestrator is renewing it.",
    );

    tokio::time::sleep(settle).await;

    match (first, read().map(Assertion::read)) {
        // A second read that is worth presenting always wins; so does any
        // second read at all when the first answered nothing.
        (Err(_), second) => second,
        (Ok(first), Ok(second)) if better(&first, &second, clock.now()) => Ok(second),
        (Ok(first), _) => Ok(first),
    }
}

/// Whether the second read answered something worth preferring.
fn better(first: &Assertion, second: &Assertion, now: DateTime<Utc>) -> bool {
    if second.is_fresh_at(now) {
        return true;
    }

    match (first.expires_at, second.expires_at) {
        (Some(before), Some(after)) => after > before,
        // Anything we can take apart beats something we cannot.
        (_, None) => false,
        (None, Some(_)) => second.is_readable(),
    }
}

/// The `iss` a token claims, read **without** verifying anything.
pub fn unverified_issuer(token: &Secret) -> Option<String> {
    unverified_claim(token, "iss")
}

/// The `sub` a token claims, read **without** verifying anything.
///
/// For the identity line, which has to name the account this sidecar is
/// *actually* acting as rather than the one it asked to be: rustak puts the
/// username it resolved into the access token it issues, so this is the
/// server's own answer read back.
pub fn unverified_subject(token: &Secret) -> Option<String> {
    unverified_claim(token, "sub")
}

/// One claim of a JWT payload, decoded and **not** verified.
fn unverified_claim(token: &Secret, name: &str) -> Option<String> {
    payload(token)?.get(name)?.as_str().map(str::to_owned)
}

/// A JWT's payload, decoded and **not** verified.
fn payload(token: &Secret) -> Option<serde_json::Map<String, serde_json::Value>> {
    use base64::Engine as _;

    let payload = token.expose().split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;

    match serde_json::from_slice(&decoded).ok()? {
        serde_json::Value::Object(claims) => Some(claims),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The instant these tests judge against; nothing here reads a wall clock.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    /// A token whose `exp` is the given instant.
    fn token(expires_at: Option<DateTime<Utc>>) -> Secret {
        use base64::Engine as _;

        let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let claims = match expires_at {
            Some(at) => serde_json::json!({
                "iss": "https://nomad.example.com",
                "sub": "global:default:rustak-plugin-ais:sidecar:ais:rustak",
                "exp": at.timestamp(),
            }),
            None => serde_json::json!({ "iss": "https://nomad.example.com" }),
        };

        Secret::new(format!(
            "{}.{}.not-a-signature",
            encoder.encode(br#"{"alg":"RS256","kid":"k1"}"#),
            encoder.encode(claims.to_string().as_bytes()),
        ))
    }

    #[test]
    fn a_sidecar_reads_its_own_expiry_and_issuer_without_verifying_anything() {
        let assertion = Assertion::read(token(Some(at(3600))));

        assert_eq!(assertion.expires_at(), Some(at(3600)));
        assert_eq!(assertion.issuer(), Some("https://nomad.example.com"));
        assert!(assertion.is_readable());
    }

    #[test]
    fn a_token_that_is_not_a_token_is_not_readable_and_is_not_fresh() {
        // What a read that landed while Nomad was writing the file looks like.
        for raw in ["", "eyJhbGciOiJSUzI1NiJ9", "not-a-jwt", "a.!!!.c"] {
            let assertion = Assertion::read(Secret::new(raw));

            assert!(!assertion.is_readable(), "{raw}");
            assert!(!assertion.is_fresh_at(at(0)), "{raw}");
            assert_eq!(assertion.expired_for(at(0)), None, "{raw}");
        }
    }

    #[test]
    fn a_token_is_stale_a_minute_before_it_expires_and_expired_after_it() {
        let assertion = Assertion::read(token(Some(at(3600))));

        assert!(assertion.is_fresh_at(at(3_539)), "61s to go");
        assert!(!assertion.is_fresh_at(at(3_540)), "exactly the margin");
        assert!(!assertion.is_fresh_at(at(3_600)));
        assert_eq!(assertion.expired_for(at(3_600)), None, "not yet expired");
        assert_eq!(
            assertion.expired_for(at(7_080)),
            Some(Duration::seconds(3_480)),
            "58 minutes, which is what the production defect produced",
        );
    }

    #[test]
    fn a_token_that_says_nothing_about_its_expiry_is_presented_anyway() {
        // The server is the one that decides; refusing to try would turn a
        // claim we could not read into an outage of our own making.
        let assertion = Assertion::read(token(None));

        assert!(assertion.is_fresh_at(at(0)));
        assert_eq!(assertion.expired_for(at(0)), None);
    }

    #[tokio::test]
    async fn a_fresh_token_is_read_once_and_presented() {
        let mut reads = 0;
        let assertion = read_fresh(
            || {
                reads += 1;

                Ok(token(Some(at(3_600))))
            },
            &Clock::fixed(at(0)),
            std::time::Duration::ZERO,
        )
        .await
        .expect("a token");

        assert_eq!(assertion.expires_at(), Some(at(3_600)));
        assert_eq!(reads, 1, "nothing waits for a token that is already good");
    }

    #[tokio::test]
    async fn a_token_being_rewritten_is_read_again_and_the_new_one_is_used() {
        // The renewal in progress: the first read lands on the old token (or on
        // a half-written file), and the one a moment later is the new one.
        let answers = [token(Some(at(30))), token(Some(at(3_600)))];
        let mut reads = 0;

        let assertion = read_fresh(
            || {
                let answer = answers[reads.min(1)].clone();
                reads += 1;

                Ok(answer)
            },
            &Clock::fixed(at(0)),
            std::time::Duration::ZERO,
        )
        .await
        .expect("a token");

        assert_eq!(reads, 2);
        assert_eq!(
            assertion.expires_at(),
            Some(at(3_600)),
            "the token the second read found is the one presented",
        );
    }

    #[tokio::test]
    async fn a_file_that_is_not_there_yet_is_read_again_rather_than_failed() {
        let mut reads = 0;

        let assertion = read_fresh(
            || {
                reads += 1;

                match reads {
                    1 => Err(human_errors::user("no such file", &[])),
                    _ => Ok(token(Some(at(3_600)))),
                }
            },
            &Clock::fixed(at(0)),
            std::time::Duration::ZERO,
        )
        .await
        .expect("a token");

        assert_eq!(reads, 2);
        assert_eq!(assertion.expires_at(), Some(at(3_600)));
    }

    #[tokio::test]
    async fn an_expired_token_that_does_not_improve_is_still_the_one_presented() {
        // There is nothing better to present, and the server's refusal — with
        // the expiry this end can now name — is more use than a silent skip.
        let assertion = read_fresh(
            || Ok(token(Some(at(-3_600)))),
            &Clock::fixed(at(0)),
            std::time::Duration::ZERO,
        )
        .await
        .expect("a token");

        assert_eq!(
            assertion.expired_for(at(0)),
            Some(Duration::seconds(3_600)),
            "and it knows exactly how dead it is",
        );
    }

    #[tokio::test]
    async fn a_source_that_never_answers_is_the_error_it_last_gave() {
        let failed = read_fresh(
            || Err(human_errors::user("the file is empty", &[])),
            &Clock::fixed(at(0)),
            std::time::Duration::ZERO,
        )
        .await;

        assert!(failed.is_err());
    }

    #[test]
    fn the_account_a_token_was_issued_to_is_read_back_from_it() {
        assert_eq!(
            unverified_subject(&token(Some(at(3_600)))).as_deref(),
            Some("global:default:rustak-plugin-ais:sidecar:ais:rustak"),
        );
        assert_eq!(
            unverified_issuer(&token(None)).as_deref(),
            Some("https://nomad.example.com"),
        );
        assert_eq!(unverified_issuer(&Secret::new("not-a-jwt")), None);
        assert_eq!(unverified_subject(&Secret::new("not-a-jwt")), None);
    }
}

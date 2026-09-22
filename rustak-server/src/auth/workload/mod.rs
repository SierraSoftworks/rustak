//! An orchestrator's workload identity, used as a rustak credential.
//!
//! A task running under Nomad or Kubernetes is already holding a short-lived,
//! signed statement of what it is — Nomad's `identity` block, Kubernetes'
//! projected service-account token. This module is what turns one of those into
//! "you are the account `ais`", so that a deployment holds **no rustak secret
//! at all**: no enrolment token to mint and hand over, no service token to copy
//! into a file that gets committed.
//!
//! # What it is not
//!
//! It is not a second way to be an administrator, and it is not a way to become
//! an account that does not already exist. An assertion reaches exactly the
//! account a binding rule names, that account must already exist as a
//! `service`, must be enabled, and must satisfy `[auth] user_acl` — and
//! `users::principal` is told that this is not an administrative grant, so a
//! service account somebody once flagged as an administrator does not become
//! one by presenting a job token.
//!
//! # The order of the checks is the design
//!
//! [`verify`] proves the bytes are the orchestrator's before reading any claim
//! that decides anything — see that module. [`resolve`] then does the part that
//! is about *us*: the account, its kind, whether it is switched off, and the
//! access-control expression. Rate limiting belongs to the caller, because it
//! is keyed on the account this answers with.
//!
//! # A token is never logged
//!
//! Not at `debug`, not in an error, not in the audit trail. What is recorded is
//! the issuer, the namespace, the subject and the `jti` — enough to find the
//! run of the task that enrolled, and useless to replay.

pub mod claims;
pub mod grant;
pub mod keys;
mod refusals;
pub mod request;
pub mod rules;
mod verify;

use std::net::IpAddr;

use rustak_api::UserKind;
use rustak_core::identity::{AuthMethod, Username};

use crate::auth::acl::{AuthRequestFilter, evaluate};
use crate::auth::ratelimit::RateLimiter;
use crate::auth::resolve::{AuthFailure, RequestFacts, Resolved};
use crate::identity::users;
use crate::prelude::*;

/// The rate-limiter subject an assertion is counted against before it has
/// named an account.
///
/// Two keys rather than one. The account is what the brief asks for and what
/// stops one workload's broken deployment costing another one its enrolments;
/// this one bounds what an address can spend on *verification* before any
/// account has been named, because a caller who sends us rubbish never reaches
/// the other key at all.
pub const RATE_LIMIT_SUBJECT: &str = "workload-identity";

pub use claims::Claims;
pub use grant::{GRANT_TYPE, jwt_bearer};
pub use keys::warn_about_insecure_issuers;
pub use refusals::Presented;
pub use request::{assertion_of, from_request};
pub use rules::RuleRefusal;
pub use verify::{ClaimRefusal, verify};

/// An assertion this server has verified, and the account it speaks for.
///
/// Held in the request's extensions by
/// [`resolve_principal`](crate::auth::resolve::resolve_principal), so that the
/// enrolment handler can audit *which* workload enrolled without re-reading a
/// credential it has already spent its checks on.
#[derive(Debug, Clone)]
pub struct Assertion {
    /// The `[auth.workload]` issuer that verified it.
    pub issuer_name: String,

    /// The `iss` the token carried, when it carried one.
    pub issuer: Option<String>,

    /// The account the binding rules named.
    pub account: Username,

    /// The namespace the rule matched.
    pub namespace: String,

    /// The subject the rule matched — a Nomad job id, a Kubernetes service
    /// account name.
    pub subject: String,

    /// The token's `jti`, which is what an operator matches against the
    /// orchestrator's own record of the run.
    pub jti: Option<String>,

    /// The token's `sub`, which for Nomad is
    /// `<region>:<namespace>:<job>:<group>:<task>:<identity>`.
    pub sub: Option<String>,

    /// Every claim the token carried, for a rule's `match` expression and for
    /// the access-control expression.
    pub claims: Claims,
}

impl Assertion {
    /// What the audit trail records about this assertion.
    ///
    /// Deliberately not the token, and deliberately not every claim: the four
    /// fields here are what identifies the run of the task that enrolled.
    pub fn audit_detail(&self) -> serde_json::Value {
        serde_json::json!({
            "issuer": self.issuer_name,
            "iss": self.issuer,
            "namespace": self.namespace,
            "subject": self.subject,
            "jti": self.jti,
            "sub": self.sub,
        })
    }
}

/// Why an assertion was not accepted.
///
/// Distinguished here so the log can say what happened; the caller tells the
/// client only that the credential was refused, because the difference between
/// "no rule matched" and "the signature is wrong" is an oracle.
#[derive(Debug)]
pub enum Refusal {
    /// No `[auth.workload]` issuer is configured at all.
    NotConfigured,
    /// The token is not a JWT we can take apart, or names no key.
    Malformed,
    /// No issuer entry expects the `iss` the token carries.
    UnknownIssuer,
    /// The header named an algorithm this issuer does not sign with.
    Algorithm,
    /// The token names a key the issuer does not publish, and refetching did
    /// not produce one.
    UnknownKey {
        /// The `kid` the token named, tidied — it came off the wire.
        kid: String,
        /// The `[auth.workload]` entry that does not publish it.
        issuer: String,
    },
    /// The signature, audience, issuer or time window was not acceptable, and
    /// which of those it was.
    Claims(ClaimRefusal),
    /// The token verified and no single account could be named.
    Rule(RuleRefusal),
    /// Something of ours failed.
    Unavailable(Error),
}

impl Refusal {
    /// A short word naming the refusal, for the log line.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotConfigured => "not-configured",
            Self::Malformed => "malformed",
            Self::UnknownIssuer => "unknown-issuer",
            Self::Algorithm => "algorithm",
            Self::UnknownKey { .. } => "unknown-key",
            Self::Claims(refusal) => refusal.check(),
            Self::Rule(refusal) => refusal.reason(),
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// The sentence an operator reads, and the one whoever presented the
    /// credential is given back.
    ///
    /// It names the check that failed and by how much, and **no token
    /// material**: the caller is the token's holder, so telling them that what
    /// they sent expired 58 minutes ago is not an oracle — it is the one fact
    /// that turns a two-hour outage into a one-line diagnosis. What stays
    /// secret is everything about *this server*: which account a rule would
    /// have named, whether that account exists, whether it is switched off.
    pub fn sentence(&self) -> String {
        match self {
            Self::NotConfigured => {
                "This server is not configured to accept workload identities.".to_string()
            }
            Self::Malformed => {
                "That is not a JWT whose header names the key it was signed with.".to_string()
            }
            Self::UnknownIssuer => {
                "No `[auth.workload]` issuer on this server claims the `iss` that token carries."
                    .to_string()
            }
            Self::Algorithm => {
                "That token is signed with an algorithm its issuer does not use.".to_string()
            }
            Self::UnknownKey { kid, issuer } => {
                format!("kid {kid} is not published by issuer {issuer}")
            }
            Self::Claims(refusal) => refusal.to_string(),
            Self::Rule(refusal) => refusal.sentence(),
            Self::Unavailable(_) => {
                "This server could not check that assertion just now.".to_string()
            }
        }
    }
}

/// Why a caller was not let in.
///
/// Two halves, because they are answered differently. A [`Refusal`] is
/// something about the *credential*, and its sentence goes back to the caller
/// in `error_description` — they are holding the token, so they are the one
/// person it tells nothing new. Everything else is about *this installation* —
/// which accounts exist, which are switched off, what `user_acl` says — and is
/// answered with one word.
#[derive(Debug)]
pub enum Denied {
    /// A credential this server would not take, and what was wrong with it.
    Refused(Refusal),

    /// Everything else: a rate limit, a read that failed, an account that may
    /// not do this.
    Failed(AuthFailure),
}

impl Denied {
    /// The sentence to hand back to whoever presented the credential.
    pub fn description(&self) -> Option<String> {
        match self {
            Self::Refused(refusal) => Some(refusal.sentence()),
            Self::Failed(_) => None,
        }
    }
}

impl From<Denied> for AuthFailure {
    fn from(denied: Denied) -> Self {
        match denied {
            Denied::Refused(refusal) => refusal.into(),
            Denied::Failed(failure) => failure,
        }
    }
}

impl From<Refusal> for AuthFailure {
    /// Every refusal a caller can provoke looks the same on the wire.
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Unavailable(err) => Self::Unavailable(err),
            _ => Self::Rejected,
        }
    }
}

/// [`resolve`], behind the same limiter every credential endpoint goes through.
///
/// This is what a route calls. Two keys are checked: the address on its own
/// before anything is verified, and the account once one has been named — see
/// [`RATE_LIMIT_SUBJECT`].
///
/// # Errors
///
/// As [`resolve`], plus [`AuthFailure::RateLimited`] when the caller has been
/// refused too often.
pub async fn resolve_limited<S: Services>(
    services: &S,
    limiter: &RateLimiter,
    address: Option<IpAddr>,
    token: &str,
    facts: &RequestFacts<'_>,
    presented: Presented,
) -> Result<(Resolved, Assertion), Denied> {
    limiter
        .check(address, RATE_LIMIT_SUBJECT)
        .map_err(|retry_after| {
            refusals::throttled(token, retry_after);

            Denied::Failed(AuthFailure::RateLimited(retry_after))
        })?;

    let assertion = match verified(services, token, presented).await {
        Ok(assertion) => assertion,
        Err(Refusal::Unavailable(err)) => {
            return Err(Denied::Failed(AuthFailure::Unavailable(err)));
        }
        Err(refusal) => {
            limiter.record_failure(address, RATE_LIMIT_SUBJECT);

            return Err(Denied::Refused(refusal));
        }
    };

    limiter
        .check(address, assertion.account.as_str())
        .map_err(|retry_after| {
            refusals::throttled(token, retry_after);

            Denied::Failed(AuthFailure::RateLimited(retry_after))
        })?;

    match account(services, &assertion, facts).await {
        Ok(resolved) => {
            limiter.record_success(address, RATE_LIMIT_SUBJECT);
            limiter.record_success(address, assertion.account.as_str());

            announce(&assertion);

            Ok((resolved, assertion))
        }
        Err(AuthFailure::Unavailable(err)) => Err(Denied::Failed(AuthFailure::Unavailable(err))),
        Err(failure) => {
            limiter.record_failure(address, RATE_LIMIT_SUBJECT);
            limiter.record_failure(address, assertion.account.as_str());

            Err(Denied::Failed(failure))
        }
    }
}

/// Verifies an assertion and resolves the account it speaks for.
///
/// The [`Assertion`] comes back beside the [`Resolved`] so that the caller can
/// keep it for the audit trail; nothing else needs it.
///
/// # Errors
///
/// [`AuthFailure::Rejected`] for every refusal the presenter could provoke;
/// [`AuthFailure::Forbidden`] when the account exists and the answer will not
/// change by presenting another token; [`AuthFailure::Unavailable`] when a read
/// fails.
#[instrument("auth.workload.resolve", skip_all, err(level = "debug", Debug))]
pub async fn resolve<S: Services>(
    services: &S,
    token: &str,
    facts: &RequestFacts<'_>,
) -> Result<(Resolved, Assertion), AuthFailure> {
    let assertion = verified(services, token, Presented::Deliberately)
        .await
        .map_err(AuthFailure::from)?;
    let resolved = account(services, &assertion, facts).await?;

    announce(&assertion);

    Ok((resolved, assertion))
}

/// [`verify`], with the refusal recorded in the words an operator reads.
///
/// The recording is [`refusals::announce`]'s: `warn` once per
/// `(issuer, subject, reason)` per five minutes, `debug` in between, never
/// `ERROR` and never with the request's headers attached. See that module for
/// what this used to do instead.
async fn verified<S: Services>(
    services: &S,
    token: &str,
    presented: Presented,
) -> Result<Assertion, Refusal> {
    match verify(services, token).await {
        Ok(assertion) => Ok(assertion),
        Err(refusal) => {
            refusals::announce(token, &refusal, presented);

            Err(refusal)
        }
    }
}

/// The line an operator greps for when a first run misbehaves.
fn announce(assertion: &Assertion) {
    info!(
        issuer = %assertion.issuer_name,
        namespace = %assertion.namespace,
        subject = %assertion.subject,
        account = %assertion.account,
        "A workload identity authenticated.",
    );
}

/// The account half: it has to exist, be a service, be on, and be allowed.
async fn account<S: Services>(
    services: &S,
    assertion: &Assertion,
    facts: &RequestFacts<'_>,
) -> Result<Resolved, AuthFailure> {
    let db = services.db();
    let config = services.config();

    let Some(user) = db.users().get_by_username(&assertion.account).await? else {
        info!(
            account = %assertion.account,
            issuer = %assertion.issuer_name,
            subject = %assertion.subject,
            "A workload identity resolved to an account that does not exist.",
        );

        return Err(AuthFailure::Rejected);
    };

    if user.disabled {
        info!(account = %user.username, "Refused a workload identity for an account that is switched off.");

        return Err(AuthFailure::Rejected);
    }

    if user.kind != UserKind::Service {
        // A person's account is not something a job may become, whatever the
        // rules say: the binding is machinery, and a person's account is the
        // one place a machine's mistake reaches a human being's rights.
        warn!(
            account = %user.username,
            issuer = %assertion.issuer_name,
            "Refused a workload identity that named an account which is not a service.",
        );

        return Err(AuthFailure::Forbidden(
            "A workload identity may only act as a service account.",
        ));
    }

    // The provider claims here are the assertion's own, so an expression such
    // as `claims.nomad_namespace == "default"` is judged against the token in
    // front of us rather than against something a sign-in recorded.
    let filter = AuthRequestFilter {
        method: facts.method,
        path: facts.path,
        client_ip: facts.client_ip.clone(),
        headers: facts.headers,
        claims: Some(&assertion.claims),
        username: user.username.as_str(),
        source: user.source.as_str(),
    };

    if config.auth.user_acl.is_some() && !evaluate(&config.auth, &filter).allowed {
        // Loud on purpose: an installation whose `user_acl` was written for a
        // directory will refuse every workload enrolment, and this one line is
        // what makes that a five-second diagnosis instead of an afternoon.
        warn!(
            account = %user.username,
            issuer = %assertion.issuer_name,
            path = facts.path,
            acl = %config.auth.user_acl().to_string(),
            "`[auth] user_acl` refused a workload identity. Widen it to admit service accounts.",
        );

        return Err(AuthFailure::Forbidden(
            "Your account is not permitted to use this.",
        ));
    }

    let via = AuthMethod::Workload {
        issuer: assertion.issuer_name.clone(),
        subject: assertion.subject.clone(),
    };

    Ok(Resolved {
        principal: users::principal(db, &user, via, false).await?,
        user,
        // No `AccessClaims`: this is not one of our tokens, and putting a
        // foreign `jti` where sign-out looks for one of ours would be a
        // revocation that could never match.
        claims: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One of each refusal, for the tests that hold them all to a rule.
    fn each() -> Vec<Refusal> {
        vec![
            Refusal::NotConfigured,
            Refusal::Malformed,
            Refusal::UnknownIssuer,
            Refusal::Algorithm,
            Refusal::UnknownKey {
                kid: "\"rustak-workload-9\"".to_string(),
                issuer: "nomad".to_string(),
            },
            Refusal::Claims(ClaimRefusal::new("exp", "expired 58m12s ago")),
            Refusal::Claims(ClaimRefusal::new("aud", "expected \"rustak\"")),
            Refusal::Rule(RuleRefusal::NoRule),
            Refusal::Rule(RuleRefusal::Ambiguous {
                accounts: vec!["a".into(), "b".into()],
            }),
        ]
    }

    #[test]
    fn every_refusal_has_a_word_the_log_can_carry() {
        let words: std::collections::HashSet<&str> = each().iter().map(Refusal::reason).collect();

        assert!(
            words.contains("exp"),
            "a claim refusal is named by its claim"
        );
        assert!(words.contains("aud"));
        assert!(words.contains("unknown-key"));
        assert_eq!(
            words.len(),
            each().len(),
            "every refusal an operator can meet has a word of its own",
        );
    }

    #[test]
    fn every_refusal_says_what_was_wrong_and_names_no_account() {
        // The sentence goes into `error_description` and into the log, so it
        // has to be worth reading — and it must not answer questions about
        // this installation that the caller did not get to ask.
        for refusal in each() {
            let sentence = refusal.sentence();

            assert!(sentence.len() > 20, "{sentence}");
            assert!(
                !sentence.contains("ais") && !sentence.contains("account is"),
                "no account may be named: {sentence}",
            );
        }

        assert_eq!(
            Refusal::Claims(ClaimRefusal::new("exp", "expired 58m12s ago")).sentence(),
            "exp: expired 58m12s ago",
        );
        assert_eq!(
            Refusal::UnknownKey {
                kid: "\"k9\"".to_string(),
                issuer: "nomad".to_string(),
            }
            .sentence(),
            "kid \"k9\" is not published by issuer nomad",
        );
    }

    #[test]
    fn every_refusal_a_caller_can_provoke_looks_the_same_on_the_wire() {
        for refusal in each() {
            assert!(
                matches!(AuthFailure::from(refusal), AuthFailure::Rejected),
                "the difference between these is an oracle",
            );
        }

        assert!(matches!(
            AuthFailure::from(Refusal::Unavailable(human_errors::system("x", &[]))),
            AuthFailure::Unavailable(_),
        ));
    }

    #[test]
    fn only_a_refused_credential_is_described_back_to_its_holder() {
        // A credential's own faults are the holder's to know. Everything
        // else — whether an account exists, whether it is switched off — is
        // answered with one word, because the difference is an oracle.
        assert_eq!(
            Denied::Refused(Refusal::Claims(ClaimRefusal::new("exp", "expired 1s ago")))
                .description()
                .as_deref(),
            Some("exp: expired 1s ago"),
        );
        assert_eq!(Denied::Failed(AuthFailure::Rejected).description(), None);
        assert_eq!(
            Denied::Failed(AuthFailure::Forbidden("no")).description(),
            None,
        );
    }

    #[test]
    fn the_audit_detail_names_the_run_and_never_the_token() {
        let assertion = Assertion {
            issuer_name: "nomad".to_string(),
            issuer: Some("https://nomad.example.com".to_string()),
            account: Username::parse("ais").unwrap(),
            namespace: "default".to_string(),
            subject: "rustak-plugin-ais".to_string(),
            jti: Some("a-jti".to_string()),
            sub: Some("global:default:rustak-plugin-ais:sidecar:ais:rustak".to_string()),
            claims: serde_json::json!({ "nomad_task": "ais" })
                .as_object()
                .unwrap()
                .clone(),
        };

        let detail = assertion.audit_detail().to_string();

        assert!(detail.contains("nomad"), "{detail}");
        assert!(detail.contains("rustak-plugin-ais"), "{detail}");
        assert!(detail.contains("a-jti"), "{detail}");
        assert!(
            !detail.contains("nomad_task"),
            "the claims are not the audit trail's business: {detail}",
        );
    }
}

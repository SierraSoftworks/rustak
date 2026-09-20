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
pub use request::{assertion_of, from_request};
pub use rules::RuleRefusal;
pub use verify::verify;

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
    UnknownKey,
    /// The signature, audience, issuer or time window was not acceptable.
    Claims,
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
            Self::UnknownKey => "unknown-key",
            Self::Claims => "claims",
            Self::Rule(refusal) => refusal.reason(),
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// Whether this refusal is worth an operator's attention rather than being
    /// the ordinary noise of somebody presenting the wrong thing.
    ///
    /// An ambiguous binding is a configuration mistake that `--check` could not
    /// see, so it is a `warn` where everything else is a `debug`.
    fn is_notable(&self) -> bool {
        verify::is_ambiguous(self)
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
) -> Result<(Resolved, Assertion), AuthFailure> {
    limiter
        .check(address, RATE_LIMIT_SUBJECT)
        .map_err(AuthFailure::RateLimited)?;

    let assertion = match verified(services, token).await {
        Ok(assertion) => assertion,
        Err(AuthFailure::Unavailable(err)) => return Err(AuthFailure::Unavailable(err)),
        Err(failure) => {
            limiter.record_failure(address, RATE_LIMIT_SUBJECT);

            return Err(failure);
        }
    };

    limiter
        .check(address, assertion.account.as_str())
        .map_err(AuthFailure::RateLimited)?;

    match account(services, &assertion, facts).await {
        Ok(resolved) => {
            limiter.record_success(address, RATE_LIMIT_SUBJECT);
            limiter.record_success(address, assertion.account.as_str());

            announce(&assertion);

            Ok((resolved, assertion))
        }
        Err(AuthFailure::Unavailable(err)) => Err(AuthFailure::Unavailable(err)),
        Err(failure) => {
            limiter.record_failure(address, RATE_LIMIT_SUBJECT);
            limiter.record_failure(address, assertion.account.as_str());

            Err(failure)
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
#[instrument("auth.workload.resolve", skip_all, err(Debug))]
pub async fn resolve<S: Services>(
    services: &S,
    token: &str,
    facts: &RequestFacts<'_>,
) -> Result<(Resolved, Assertion), AuthFailure> {
    let assertion = verified(services, token).await?;
    let resolved = account(services, &assertion, facts).await?;

    announce(&assertion);

    Ok((resolved, assertion))
}

/// [`verify`], with the refusal recorded in the words an operator reads.
async fn verified<S: Services>(services: &S, token: &str) -> Result<Assertion, AuthFailure> {
    match verify(services, token).await {
        Ok(assertion) => Ok(assertion),
        Err(refusal) => {
            if refusal.is_notable() {
                warn!(
                    reason = refusal.reason(),
                    "Refused a workload assertion that two binding rules disagreed about.",
                );
            } else {
                debug!(reason = refusal.reason(), "Refused a workload assertion.");
            }

            Err(refusal.into())
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

    #[test]
    fn every_refusal_has_a_word_the_log_can_carry() {
        let refusals = [
            Refusal::NotConfigured,
            Refusal::Malformed,
            Refusal::UnknownIssuer,
            Refusal::Algorithm,
            Refusal::UnknownKey,
            Refusal::Claims,
            Refusal::Rule(RuleRefusal::NoRule),
        ];

        let words: std::collections::HashSet<&str> = refusals.iter().map(Refusal::reason).collect();

        assert_eq!(words.len(), refusals.len(), "the words must be distinct");
    }

    #[test]
    fn only_an_ambiguous_binding_is_worth_shouting_about() {
        assert!(
            Refusal::Rule(RuleRefusal::Ambiguous {
                accounts: vec!["a".into(), "b".into()],
            })
            .is_notable()
        );
        assert!(!Refusal::Rule(RuleRefusal::NoRule).is_notable());
        assert!(!Refusal::Claims.is_notable());
    }

    #[test]
    fn every_refusal_a_caller_can_provoke_looks_the_same_on_the_wire() {
        for refusal in [
            Refusal::NotConfigured,
            Refusal::Malformed,
            Refusal::UnknownIssuer,
            Refusal::Algorithm,
            Refusal::UnknownKey,
            Refusal::Claims,
            Refusal::Rule(RuleRefusal::NoRule),
        ] {
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

//! `[auth.workload]` — the orchestrators whose workload identity we accept.
//!
//! A task running under Nomad or Kubernetes is already holding a short-lived,
//! signed statement of what it is: Nomad's `identity` block and Kubernetes'
//! projected service-account token. `[auth.workload]` is what turns one of
//! those into a rustak credential, so a deployment holds no rustak secret at
//! all — no enrolment token to mint, no service token to copy into a file.
//!
//! # Two halves, and they are deliberately separate
//!
//! An [`WorkloadIssuer`] says whose signatures we trust and where the keys come
//! from. A [`WorkloadRule`] says which **account** a token signed by that issuer
//! speaks for. Neither decides on its own: an issuer with no rule admits
//! nobody, and a rule naming an issuer that does not exist is refused when the
//! file is read.
//!
//! The split matters for a second reason. `issuer` is the `iss` string a token
//! must carry; `jwks_url` is where this server fetches the keys. They are
//! *different settings* because they are genuinely different addresses in the
//! first deployment this was written for — the Nomad servers publish an
//! `oidc_issuer` on their public name while rustak fetches the key set from the
//! same node's tailnet address over plain HTTP, because the TLS-terminated name
//! hairpins unreliably from inside a container there.
//!
//! # Nothing here is a wildcard
//!
//! A rule matches one namespace exactly and one subject prefix exactly, and the
//! account it produces is either the remainder of that prefix or a name written
//! out in full. A token that satisfies two rules which disagree about the
//! account is refused rather than resolved, and the obvious cases of that are
//! caught when the file is read; see [`WorkloadConfig::validate`].

mod validate;

use std::path::PathBuf;

use filt_rs::Filter;
use rustak_core::prelude::Username;
use serde::{Deserialize, Serialize};

/// The literal `account` value that means "the subject with its prefix
/// removed".
///
/// Reserved: an installation whose service account is genuinely called
/// `strip-prefix` has to reach it from a rule with a different prefix.
pub const STRIP_PREFIX: &str = "strip-prefix";

fn default_algorithms() -> Vec<WorkloadAlgorithm> {
    vec![
        WorkloadAlgorithm::Rs256,
        WorkloadAlgorithm::Es256,
        WorkloadAlgorithm::EdDsa,
    ]
}

fn default_clock_skew() -> chrono::Duration {
    chrono::Duration::seconds(30)
}

fn default_jwks_refresh() -> chrono::Duration {
    chrono::Duration::hours(1)
}

fn default_true() -> bool {
    true
}

/// `[auth.workload]`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadConfig {
    /// The orchestrators whose signatures are accepted. Empty — the default —
    /// switches workload identity off entirely.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issuers: Vec<WorkloadIssuer>,

    /// Which account a token from one of those issuers speaks for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<WorkloadRule>,

    /// Whether issuing a certificate to a workload takes back the ones it was
    /// issued before.
    ///
    /// On by default, and the reason is rescheduling: an orchestrator that
    /// moves a task to another node leaves the old node's volume holding a
    /// perfectly valid certificate for the same account. Superseding on every
    /// issue means the identity a deployment holds is the one it last asked
    /// for, and nothing else.
    #[serde(default = "default_true")]
    pub revoke_previous: bool,
}

impl Default for WorkloadConfig {
    /// Written out rather than derived: serde's `default = "…"` applies only
    /// when deserialising, so a derived `Default` would leave
    /// `revoke_previous` off while an empty file turned it on — and the two
    /// disagreeing about *that* is a rescheduled task leaving a live
    /// certificate behind.
    fn default() -> Self {
        Self {
            issuers: Vec::new(),
            rules: Vec::new(),
            revoke_previous: default_true(),
        }
    }
}

impl std::fmt::Debug for WorkloadConfig {
    /// Written out because [`Filter`] has no `Debug`; see
    /// [`WorkloadRule`]'s own.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkloadConfig")
            .field("issuers", &self.issuers)
            .field("rules", &self.rules)
            .field("revoke_previous", &self.revoke_previous)
            .finish()
    }
}

impl WorkloadConfig {
    /// Whether anything is configured to be accepted at all.
    pub fn is_enabled(&self) -> bool {
        !self.issuers.is_empty()
    }

    /// Whether this section says nothing the defaults do not already say.
    ///
    /// What `skip_serializing_if` asks, so that a configuration nobody wrote a
    /// `[auth.workload]` for round-trips without growing an empty one.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The issuer registered under `name`.
    pub fn issuer(&self, name: &str) -> Option<&WorkloadIssuer> {
        self.issuers.iter().find(|issuer| issuer.name == name)
    }

    /// The rules belonging to one issuer, in the order they were written.
    pub fn rules_for<'a>(&'a self, issuer: &'a str) -> impl Iterator<Item = &'a WorkloadRule> {
        self.rules.iter().filter(move |rule| rule.issuer == issuer)
    }
}

/// One orchestrator's signing authority.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadIssuer {
    /// What this installation calls it. Named by every rule, and recorded in
    /// the audit trail of every enrolment it authorises.
    pub name: String,

    /// The `iss` claim a token must carry, checked after the signature.
    ///
    /// Required in practice: a Nomad cluster with `oidc_issuer` set and every
    /// Kubernetes API server publish one. [`None`] is allowed for the one case
    /// that has none — a Nomad cluster with no `oidc_issuer` — and then the key
    /// set is the only thing binding a token to this issuer, so a token that
    /// *does* carry an `iss` is refused rather than matched loosely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,

    /// Where the key set is published, fetched directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwks_url: Option<String>,

    /// An OpenID discovery document to read `jwks_uri` out of, instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_url: Option<String>,

    /// A key set on disk, for an air-gapped installation or one that pins the
    /// keys itself. Re-read when a token names a `kid` we do not hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jwks_file: Option<PathBuf>,

    /// The `aud` a token must name. Required: an audience is what stops a token
    /// minted for some other consumer of the same orchestrator being replayed
    /// here.
    pub audience: String,

    /// The signature algorithms accepted, as an allow-list.
    #[serde(default = "default_algorithms")]
    pub algorithms: Vec<WorkloadAlgorithm>,

    /// How far a token's `exp`, `nbf` and `iat` may be out before it is
    /// refused.
    #[serde(
        default = "default_clock_skew",
        with = "rustak_core::config::duration::humane"
    )]
    pub clock_skew: chrono::Duration,

    /// How long a fetched key set is kept before it is fetched again.
    ///
    /// A rotation is normally noticed sooner than this: a token naming a `kid`
    /// we do not hold refetches on the spot, at most once a minute.
    #[serde(
        default = "default_jwks_refresh",
        with = "rustak_core::config::duration::humane"
    )]
    pub jwks_refresh: chrono::Duration,

    /// Whether `jwks_url` may be a plain `http://` address to a host that is
    /// not a loopback one.
    ///
    /// Off by default. Turning it on logs a warning at every start-up, because
    /// whoever can answer that request decides which signatures this server
    /// accepts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_insecure_jwks: bool,
}

/// A signature algorithm an assertion may be signed with.
///
/// An allow-list rather than "whatever the token says": the key set is public,
/// so a token that chose its own algorithm could choose a symmetric one and
/// sign itself with a published modulus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkloadAlgorithm {
    #[serde(rename = "RS256")]
    Rs256,
    #[serde(rename = "RS384")]
    Rs384,
    #[serde(rename = "RS512")]
    Rs512,
    #[serde(rename = "PS256")]
    Ps256,
    #[serde(rename = "PS384")]
    Ps384,
    #[serde(rename = "PS512")]
    Ps512,
    #[serde(rename = "ES256")]
    Es256,
    #[serde(rename = "ES384")]
    Es384,
    #[serde(rename = "EdDSA")]
    EdDsa,
}

impl WorkloadAlgorithm {
    /// The `jsonwebtoken` algorithm this names.
    pub fn algorithm(self) -> jsonwebtoken::Algorithm {
        use jsonwebtoken::Algorithm as A;

        match self {
            Self::Rs256 => A::RS256,
            Self::Rs384 => A::RS384,
            Self::Rs512 => A::RS512,
            Self::Ps256 => A::PS256,
            Self::Ps384 => A::PS384,
            Self::Ps512 => A::PS512,
            Self::Es256 => A::ES256,
            Self::Es384 => A::ES384,
            Self::EdDsa => A::EdDSA,
        }
    }
}

/// Which account a token from one issuer speaks for.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadRule {
    /// The [`WorkloadIssuer::name`] this rule belongs to.
    pub issuer: String,

    /// The claim carrying the workload's namespace — `nomad_namespace`, or
    /// `kubernetes.io.namespace` for the nested Kubernetes object. Dots address
    /// nested objects.
    pub namespace_claim: String,

    /// The value that claim must have, compared exactly.
    pub namespace: String,

    /// The claim carrying the workload's own name — `nomad_job_id`, or
    /// `kubernetes.io.serviceaccount.name`.
    pub subject_claim: String,

    /// The prefix that claim must start with, compared exactly. Required when
    /// `account` strips it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_prefix: Option<String>,

    /// The account this rule maps to.
    pub account: WorkloadAccount,

    /// A further constraint over the token's claims, as a [`filt_rs`]
    /// expression — `claims.nomad_task == "ais"`, say. The namespace and prefix
    /// above are checked first and this narrows what is left; it can never
    /// widen a rule.
    #[serde(default, rename = "match", skip_serializing_if = "Option::is_none")]
    pub match_claims: Option<Filter>,
}

impl std::fmt::Debug for WorkloadRule {
    /// Written out because [`Filter`] has no `Debug`, and because the
    /// expression is the first thing anybody debugging a refused enrolment
    /// asks for.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkloadRule")
            .field("issuer", &self.issuer)
            .field("namespace_claim", &self.namespace_claim)
            .field("namespace", &self.namespace)
            .field("subject_claim", &self.subject_claim)
            .field("subject_prefix", &self.subject_prefix)
            .field("account", &self.account)
            .field(
                "match",
                &self
                    .match_claims
                    .as_ref()
                    .map(std::string::ToString::to_string),
            )
            .finish()
    }
}

impl WorkloadRule {
    /// The prefix a subject must start with, as a string.
    pub fn prefix(&self) -> &str {
        self.subject_prefix.as_deref().unwrap_or_default()
    }

    /// The account `subject` maps to under this rule, if it maps at all.
    ///
    /// [`None`] means the remainder after the prefix is not a usable account
    /// name, which is a refusal rather than a fallback: inventing a name for a
    /// job somebody mistyped is how one deployment enrols as another.
    pub fn account_for(&self, subject: &str) -> Option<Username> {
        match &self.account {
            WorkloadAccount::StripPrefix => {
                Username::parse(subject.strip_prefix(self.prefix())?).ok()
            }
            WorkloadAccount::Fixed(name) => Some(name.clone()),
        }
    }
}

/// What a rule does with the subject it matched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkloadAccount {
    /// The account is the subject with [`WorkloadRule::subject_prefix`]
    /// removed: `rustak-plugin-ais` becomes `ais`.
    StripPrefix,

    /// The account is this name, whatever the subject was. One job, one
    /// account.
    Fixed(Username),
}

impl Serialize for WorkloadAccount {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::StripPrefix => serializer.serialize_str(STRIP_PREFIX),
            Self::Fixed(name) => serializer.serialize_str(name.as_str()),
        }
    }
}

impl<'de> Deserialize<'de> for WorkloadAccount {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;

        if raw == STRIP_PREFIX {
            return Ok(Self::StripPrefix);
        }

        Username::parse(&raw)
            .map(Self::Fixed)
            .map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> WorkloadConfig {
        toml::from_str(text).expect("the fragment should parse")
    }

    const NOMAD: &str = r#"
        [[issuers]]
        name = "nomad"
        issuer = "https://nomad.example.com"
        jwks_url = "https://nomad.example.com:4646/.well-known/jwks.json"
        audience = "rustak"

        [[rules]]
        issuer = "nomad"
        namespace_claim = "nomad_namespace"
        namespace = "default"
        subject_claim = "nomad_job_id"
        subject_prefix = "rustak-plugin-"
        account = "strip-prefix"
    "#;

    #[test]
    fn an_empty_section_switches_the_whole_thing_off() {
        let parsed = parse("");

        assert_eq!(parsed, WorkloadConfig::default());
        assert!(!parsed.is_enabled());
        assert!(
            parsed.revoke_previous,
            "superseding is the default, so a rescheduled task leaves nothing live",
        );
    }

    #[test]
    fn the_reference_rule_maps_a_job_to_the_account_named_after_it() {
        let parsed = parse(NOMAD);
        let rule = &parsed.rules[0];

        assert_eq!(
            rule.account_for("rustak-plugin-ais").map(|a| a.to_string()),
            Some("ais".to_string()),
        );
        assert_eq!(rule.account_for("something-else"), None);
        assert_eq!(
            rule.account_for("rustak-plugin-"),
            None,
            "an empty remainder is not an account",
        );
    }

    #[test]
    fn a_fixed_account_ignores_the_subject_it_matched() {
        let parsed = parse(
            r#"
            [[issuers]]
            name = "k8s"
            issuer = "https://kubernetes.default.svc"
            jwks_url = "https://kubernetes.default.svc/openid/v1/jwks"
            audience = "rustak"

            [[rules]]
            issuer = "k8s"
            namespace_claim = "kubernetes.io.namespace"
            namespace = "tak"
            subject_claim = "kubernetes.io.serviceaccount.name"
            account = "svc.ais"
            "#,
        );

        assert_eq!(
            parsed.rules[0]
                .account_for("anything-at-all")
                .map(|a| a.to_string()),
            Some("svc.ais".to_string()),
        );
    }

    #[test]
    fn the_defaults_are_the_ones_both_orchestrators_sign_with() {
        let issuer = &parse(NOMAD).issuers[0];

        assert_eq!(
            issuer.algorithms,
            vec![
                WorkloadAlgorithm::Rs256,
                WorkloadAlgorithm::Es256,
                WorkloadAlgorithm::EdDsa
            ],
        );
        assert_eq!(issuer.clock_skew, chrono::Duration::seconds(30));
        assert_eq!(issuer.jwks_refresh, chrono::Duration::hours(1));
        assert!(!issuer.allow_insecure_jwks);
    }

    #[test]
    fn every_algorithm_name_is_the_one_a_discovery_document_prints() {
        // Nomad 2.0's document lists RS256 and EdDSA; a name that did not round
        // trip would be a file an operator copied from it and we refused.
        let parsed = parse(
            r#"
            [[issuers]]
            name = "nomad"
            jwks_url = "https://nomad.example.com/.well-known/jwks.json"
            audience = "rustak"
            algorithms = ["RS256", "EdDSA", "ES384", "PS512"]
            "#,
        );

        assert_eq!(
            parsed.issuers[0].algorithms,
            vec![
                WorkloadAlgorithm::Rs256,
                WorkloadAlgorithm::EdDsa,
                WorkloadAlgorithm::Es384,
                WorkloadAlgorithm::Ps512,
            ],
        );
        assert_eq!(
            WorkloadAlgorithm::EdDsa.algorithm(),
            jsonwebtoken::Algorithm::EdDSA,
        );
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<WorkloadConfig>(
            "[[issuers]]\nname = \"nomad\"\naudience = \"rustak\"\njwks_uri = \"https://x/jwks\"\n",
        ) else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("jwks_uri"), "{err}");
    }

    #[test]
    fn an_account_that_is_not_a_username_is_refused_when_the_file_is_read() {
        let Err(err) = toml::from_str::<WorkloadConfig>(
            "[[rules]]\nissuer = \"nomad\"\nnamespace_claim = \"n\"\nnamespace = \"default\"\nsubject_claim = \"s\"\naccount = \"not a username\"\n",
        ) else {
            panic!("a malformed account should be refused");
        };

        assert!(!err.to_string().is_empty(), "{err}");
    }

    #[test]
    fn a_rule_renders_its_expression_in_a_debug_dump() {
        let parsed = parse(&format!(
            "{NOMAD}\nmatch = 'claims.nomad_task == \"ais\"'\n"
        ));

        let rendered = format!("{:?}", parsed.rules[0]);

        assert!(rendered.contains("nomad_task"), "{rendered}");
    }

    #[test]
    fn the_rules_of_one_issuer_can_be_picked_out_from_the_rest() {
        let parsed = parse(&format!(
            r#"{NOMAD}

            [[rules]]
            issuer = "somewhere-else"
            namespace_claim = "nomad_namespace"
            namespace = "default"
            subject_claim = "nomad_job_id"
            account = "svc.other"
            "#
        ));

        assert_eq!(parsed.rules_for("nomad").count(), 1);
        assert_eq!(parsed.rules_for("somewhere-else").count(), 1);
        assert_eq!(parsed.rules_for("nobody").count(), 0);
        assert!(parsed.issuer("nomad").is_some());
        assert!(parsed.issuer("nobody").is_none());
    }
}

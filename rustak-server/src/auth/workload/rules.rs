//! Which account an assertion speaks for — exactly one, or none.
//!
//! The rules are evaluated in the order they are written and **all** of them
//! are evaluated: the first match does not win. That is deliberate. A token
//! that satisfies two rules naming different accounts is ambiguous, and an
//! engine that stopped at the first match would resolve it silently, by
//! configuration order, in whichever direction the file happened to be in.
//! `--check` catches the shapes of that an operator writes by accident
//! (`config::workload::validate`); this catches the rest, with a token in
//! front of it.
//!
//! Two rules landing on the *same* account is not ambiguous and is not a
//! refusal — an installation may well describe one account twice.
//!
//! # Every comparison here is exact and case sensitive
//!
//! A namespace equals; a subject starts with a prefix. Neither is a pattern,
//! neither folds case, and there is no wildcard — `filt_rs`'s `==` is
//! case-insensitive and is therefore *not* what these use, however tempting the
//! shared vocabulary is. What an operator can reach for when exactness is not
//! enough is the rule's own `match` expression, which narrows and can never
//! widen.

use rustak_core::prelude::Username;

use crate::config::{WorkloadConfig, WorkloadRule};

use super::claims::{Claims, ClaimsFilter, string_at};

/// The account an assertion resolves to, and what it was matched on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The account the token speaks for.
    pub account: Username,
    /// The namespace the rule matched, for the audit trail.
    pub namespace: String,
    /// The subject the rule matched, for the audit trail.
    pub subject: String,
}

/// Why no single account could be named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleRefusal {
    /// No rule for this issuer matched the token at all.
    NoRule,
    /// Two rules matched and named different accounts.
    Ambiguous {
        /// The two accounts, in the order the rules are written.
        accounts: Vec<String>,
    },
    /// A rule matched and the name it produced is not a usable account —
    /// an empty remainder, or characters a username may not carry.
    UnusableAccount {
        /// The subject the rule was trying to strip.
        subject: String,
    },
}

impl RuleRefusal {
    /// A short word naming the refusal, for the log line.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NoRule => "no-rule",
            Self::Ambiguous { .. } => "ambiguous",
            Self::UnusableAccount { .. } => "unusable-account",
        }
    }
}

/// The account `claims` resolves to under `issuer`'s rules.
///
/// # Errors
///
/// A [`RuleRefusal`] when no rule matched, when two disagreed, or when the
/// account a rule produced is not one that could exist.
pub fn bind(
    workload: &WorkloadConfig,
    issuer: &str,
    claims: &Claims,
) -> Result<Binding, RuleRefusal> {
    let mut found: Option<Binding> = None;
    let mut unusable: Option<RuleRefusal> = None;

    for rule in workload.rules_for(issuer) {
        let Some((namespace, subject)) = matches(rule, issuer, claims) else {
            continue;
        };

        let Some(account) = rule.account_for(subject) else {
            unusable = Some(RuleRefusal::UnusableAccount {
                subject: subject.to_owned(),
            });
            continue;
        };

        let binding = Binding {
            account,
            namespace: namespace.to_owned(),
            subject: subject.to_owned(),
        };

        match &found {
            // Two rules describing the same account is an installation
            // saying one thing twice, which is untidy rather than unsafe.
            Some(earlier) if earlier.account == binding.account => {}
            Some(earlier) => {
                return Err(RuleRefusal::Ambiguous {
                    accounts: vec![
                        earlier.account.as_str().to_owned(),
                        binding.account.as_str().to_owned(),
                    ],
                });
            }
            None => found = Some(binding),
        }
    }

    // A rule that matched but could not name an account is a better answer than
    // "nothing matched": it is what an operator has to fix.
    found.ok_or_else(|| unusable.unwrap_or(RuleRefusal::NoRule))
}

/// Whether one rule claims this token, and the namespace and subject it read.
fn matches<'a>(
    rule: &WorkloadRule,
    issuer: &str,
    claims: &'a Claims,
) -> Option<(&'a str, &'a str)> {
    let namespace = string_at(claims, &rule.namespace_claim)?;
    let subject = string_at(claims, &rule.subject_claim)?;

    if namespace != rule.namespace || !subject.starts_with(rule.prefix()) {
        return None;
    }

    if let Some(expression) = &rule.match_claims {
        let filter = ClaimsFilter {
            issuer,
            namespace,
            subject,
            claims,
        };

        // An expression that cannot be evaluated counts as not matching, so a
        // mistake in one narrows rather than widens — the same direction
        // `auth::acl` takes for the same reason.
        if !expression.matches(&filter).unwrap_or(false) {
            return None;
        }
    }

    Some((namespace, subject))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workload(text: &str) -> WorkloadConfig {
        toml::from_str(text).expect("the fragment should parse")
    }

    fn nomad_claims(job: &str, namespace: &str) -> Claims {
        serde_json::json!({
            "nomad_namespace": namespace,
            "nomad_job_id": job,
            "nomad_task": "ais",
        })
        .as_object()
        .unwrap()
        .clone()
    }

    /// The reference rule the documentation shows.
    const REFERENCE: &str = r#"
        [[rules]]
        issuer = "nomad"
        namespace_claim = "nomad_namespace"
        namespace = "default"
        subject_claim = "nomad_job_id"
        subject_prefix = "rustak-plugin-"
        account = "strip-prefix"
    "#;

    #[test]
    fn the_reference_rule_maps_the_job_to_the_account_named_after_it() {
        let bound = bind(
            &workload(REFERENCE),
            "nomad",
            &nomad_claims("rustak-plugin-ais", "default"),
        )
        .expect("the reference rule matches");

        assert_eq!(bound.account.as_str(), "ais");
        assert_eq!(bound.namespace, "default");
        assert_eq!(bound.subject, "rustak-plugin-ais");
    }

    #[test]
    fn a_token_from_another_namespace_matches_nothing() {
        // The namespace is the boundary an operator controls with Nomad's own
        // `submit-job` capability, so it has to be the thing that decides.
        let refused = bind(
            &workload(REFERENCE),
            "nomad",
            &nomad_claims("rustak-plugin-ais", "staging"),
        )
        .unwrap_err();

        assert_eq!(refused, RuleRefusal::NoRule);
    }

    #[test]
    fn a_job_outside_the_prefix_matches_nothing() {
        let refused = bind(
            &workload(REFERENCE),
            "nomad",
            &nomad_claims("somebody-elses-job", "default"),
        )
        .unwrap_err();

        assert_eq!(refused.reason(), "no-rule");
    }

    #[test]
    fn a_prefix_with_nothing_after_it_is_not_an_account() {
        let refused = bind(
            &workload(REFERENCE),
            "nomad",
            &nomad_claims("rustak-plugin-", "default"),
        )
        .unwrap_err();

        assert_eq!(refused.reason(), "unusable-account");
    }

    #[test]
    fn the_rules_of_another_issuer_do_not_apply() {
        let refused = bind(
            &workload(REFERENCE),
            "kubernetes",
            &nomad_claims("rustak-plugin-ais", "default"),
        )
        .unwrap_err();

        assert_eq!(refused, RuleRefusal::NoRule);
    }

    #[test]
    fn a_kubernetes_rule_reads_the_nested_claims() {
        let claims = serde_json::json!({
            "kubernetes.io": {
                "namespace": "tak",
                "serviceaccount": { "name": "rustak-plugin-adsb" },
            },
        })
        .as_object()
        .unwrap()
        .clone();

        let bound = bind(
            &workload(
                r#"
                [[rules]]
                issuer = "kubernetes"
                namespace_claim = "kubernetes.io.namespace"
                namespace = "tak"
                subject_claim = "kubernetes.io.serviceaccount.name"
                subject_prefix = "rustak-plugin-"
                account = "strip-prefix"
                "#,
            ),
            "kubernetes",
            &claims,
        )
        .expect("the nested claims are read the same way");

        assert_eq!(bound.account.as_str(), "adsb");
    }

    #[test]
    fn two_rules_that_disagree_refuse_rather_than_pick_one() {
        // The property the whole module is arranged around: order in the file
        // must not silently decide which account a workload becomes.
        let refused = bind(
            &workload(&format!(
                r#"{REFERENCE}

                [[rules]]
                issuer = "nomad"
                namespace_claim = "nomad_namespace"
                namespace = "default"
                subject_claim = "nomad_job_id"
                subject_prefix = "rustak-"
                account = "svc.everything"
                "#
            )),
            "nomad",
            &nomad_claims("rustak-plugin-ais", "default"),
        )
        .unwrap_err();

        let RuleRefusal::Ambiguous { accounts } = &refused else {
            panic!("expected an ambiguity: {refused:?}");
        };
        assert_eq!(accounts, &["ais".to_string(), "svc.everything".to_string()]);
    }

    #[test]
    fn two_rules_that_agree_are_an_installation_saying_one_thing_twice() {
        let bound = bind(
            &workload(
                r#"
                [[rules]]
                issuer = "nomad"
                namespace_claim = "nomad_namespace"
                namespace = "default"
                subject_claim = "nomad_job_id"
                subject_prefix = "rustak-plugin-"
                account = "svc.ais"

                [[rules]]
                issuer = "nomad"
                namespace_claim = "nomad_namespace"
                namespace = "default"
                subject_claim = "nomad_job_id"
                subject_prefix = "rustak-"
                account = "svc.ais"
                "#,
            ),
            "nomad",
            &nomad_claims("rustak-plugin-ais", "default"),
        )
        .expect("both rules name the same account");

        assert_eq!(bound.account.as_str(), "svc.ais");
    }

    #[test]
    fn a_match_expression_narrows_a_rule_and_never_widens_it() {
        let config = workload(&format!(
            "{REFERENCE}match = 'claims.nomad_task == \"ais\"'\n"
        ));

        assert!(
            bind(
                &config,
                "nomad",
                &nomad_claims("rustak-plugin-ais", "default")
            )
            .is_ok(),
            "the task is the one the expression names",
        );

        let mut other = nomad_claims("rustak-plugin-ais", "default");
        other.insert("nomad_task".to_string(), serde_json::json!("adsb"));

        assert_eq!(
            bind(&config, "nomad", &other).unwrap_err(),
            RuleRefusal::NoRule,
            "and an expression that does not hold takes the rule away",
        );

        // Even with the expression satisfied, the namespace still decides.
        assert_eq!(
            bind(
                &config,
                "nomad",
                &nomad_claims("rustak-plugin-ais", "staging")
            )
            .unwrap_err(),
            RuleRefusal::NoRule,
        );
    }

    #[test]
    fn a_claim_the_token_does_not_carry_matches_nothing() {
        // A Kubernetes token presented to a Nomad rule, and the reverse.
        let refused = bind(
            &workload(REFERENCE),
            "nomad",
            serde_json::json!({ "sub": "somebody" })
                .as_object()
                .unwrap(),
        )
        .unwrap_err();

        assert_eq!(refused, RuleRefusal::NoRule);
    }

    #[test]
    fn the_comparison_is_case_sensitive_even_though_the_filter_language_is_not() {
        // `filt_rs`'s `==` folds case; these comparisons deliberately do not,
        // because `Default` and `default` are two namespaces on a real cluster.
        for (job, namespace) in [
            ("RUSTAK-PLUGIN-ais", "default"),
            ("rustak-plugin-ais", "Default"),
        ] {
            assert_eq!(
                bind(&workload(REFERENCE), "nomad", &nomad_claims(job, namespace)).unwrap_err(),
                RuleRefusal::NoRule,
                "{job} in {namespace}",
            );
        }
    }
}

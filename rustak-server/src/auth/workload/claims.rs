//! Reading a claim out of an assertion, and letting an expression do the same.
//!
//! # Why a dotted path is not a split on `.`
//!
//! Kubernetes puts everything it says about a pod inside one claim, and that
//! claim is called **`kubernetes.io`** — a dot in the key itself:
//!
//! ```json
//! { "kubernetes.io": { "namespace": "tak", "serviceaccount": { "name": "ais" } } }
//! ```
//!
//! So `kubernetes.io.namespace` cannot be read by splitting on `.` and walking
//! three levels; there are two. [`claim`] therefore tries the whole path as a
//! literal key first and then the **longest** head that exists as one, which
//! reads `kubernetes.io.serviceaccount.name` and plain `nomad_job_id` with the
//! same rule and no special case for either orchestrator.
//!
//! # What a `match` expression can see
//!
//! [`ClaimsFilter`] exposes `claims.<path>` over exactly the same lookup, plus
//! the three facts a rule has already established — `issuer`, `namespace` and
//! `subject` — so an operator can narrow a rule by a claim the rule itself does
//! not read (`claims.nomad_task == "ais"`) without learning a second syntax.

use filt_rs::{FilterValue, Filterable};
use serde_json::{Map, Value};

use crate::auth::acl::json_to_filter_value;

/// The prefix that addresses a claim in a `match` expression.
const CLAIMS: &str = "claims.";

/// The decoded claims of an assertion.
pub type Claims = Map<String, Value>;

/// The claim at `path`, where `path` may address a nested object with dots.
///
/// The whole path is tried as a literal key first, then the longest head that
/// exists as one; see the [module documentation](self).
pub fn claim<'a>(claims: &'a Claims, path: &str) -> Option<&'a Value> {
    if let Some(found) = claims.get(path) {
        return Some(found);
    }

    // From the right, so that the longest head is tried first: with a
    // `kubernetes.io` key present, `kubernetes.io.namespace` must not be read
    // as a `kubernetes` object.
    for (index, _) in path.rmatch_indices('.') {
        let (head, tail) = path.split_at(index);

        if let Some(child) = claims.get(head).and_then(Value::as_object)
            && let Some(found) = claim(child, &tail[1..])
        {
            return Some(found);
        }
    }

    None
}

/// The claim at `path`, when there is one and it is a string.
///
/// A claim that is a number or an object is [`None`] rather than rendered:
/// every claim a binding rule reads is a name, and comparing a name against a
/// stringified object is how a rule matches something nobody meant.
pub fn string_at<'a>(claims: &'a Claims, path: &str) -> Option<&'a str> {
    claim(claims, path).and_then(Value::as_str)
}

/// What a rule's `match` expression can see.
pub struct ClaimsFilter<'a> {
    /// The configured name of the issuer that signed the assertion.
    pub issuer: &'a str,
    /// The namespace the rule matched.
    pub namespace: &'a str,
    /// The subject the rule matched.
    pub subject: &'a str,
    /// Every claim the assertion carries.
    pub claims: &'a Claims,
}

impl Filterable for ClaimsFilter<'_> {
    fn get(&self, key: &str) -> FilterValue<'_> {
        match key {
            "issuer" => self.issuer.into(),
            "namespace" => self.namespace.into(),
            "subject" => self.subject.into(),
            key if key.starts_with(CLAIMS) => claim(self.claims, &key[CLAIMS.len()..])
                .map_or(FilterValue::Null, json_to_filter_value),
            _ => FilterValue::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kubernetes() -> Claims {
        serde_json::json!({
            "iss": "https://kubernetes.default.svc",
            "sub": "system:serviceaccount:tak:rustak-plugin-ais",
            "kubernetes.io": {
                "namespace": "tak",
                "pod": { "name": "ais-7c9", "uid": "8f0" },
                "serviceaccount": { "name": "rustak-plugin-ais", "uid": "1b2" },
            },
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn nomad() -> Claims {
        serde_json::json!({
            "iss": "https://nomad.example.com",
            "sub": "global:default:rustak-plugin-ais:sidecar:ais:rustak",
            "nomad_namespace": "default",
            "nomad_job_id": "rustak-plugin-ais",
            "nomad_task": "ais",
            "nomad_allocation_id": "3f1",
        })
        .as_object()
        .unwrap()
        .clone()
    }

    #[test]
    fn a_claim_whose_own_name_contains_a_dot_is_read_as_one_key() {
        // The whole reason this module exists: `kubernetes.io` is the key, not
        // a `kubernetes` object with an `io` field.
        let claims = kubernetes();

        assert_eq!(string_at(&claims, "kubernetes.io.namespace"), Some("tak"));
        assert_eq!(
            string_at(&claims, "kubernetes.io.serviceaccount.name"),
            Some("rustak-plugin-ais"),
        );
        assert_eq!(
            string_at(&claims, "kubernetes.io.pod.name"),
            Some("ais-7c9"),
        );
    }

    #[test]
    fn a_flat_claim_is_read_without_any_of_that() {
        let claims = nomad();

        assert_eq!(string_at(&claims, "nomad_namespace"), Some("default"));
        assert_eq!(
            string_at(&claims, "nomad_job_id"),
            Some("rustak-plugin-ais"),
        );
        assert_eq!(string_at(&claims, "nomad_task"), Some("ais"));
    }

    #[test]
    fn a_claim_that_is_not_there_is_none_rather_than_an_error() {
        let claims = nomad();

        assert_eq!(string_at(&claims, "kubernetes.io.namespace"), None);
        assert_eq!(string_at(&claims, "nomad_namespace.extra"), None);
        assert_eq!(string_at(&claims, ""), None);
    }

    #[test]
    fn a_claim_that_is_not_a_string_does_not_become_one() {
        // A rule comparing a namespace against a rendered object would match
        // something nobody meant.
        let claims = kubernetes();

        assert_eq!(string_at(&claims, "kubernetes.io"), None);
        assert!(claim(&claims, "kubernetes.io").is_some());
    }

    #[test]
    fn an_expression_sees_the_claims_and_what_the_rule_already_decided() {
        let claims = nomad();
        let filter = ClaimsFilter {
            issuer: "nomad",
            namespace: "default",
            subject: "rustak-plugin-ais",
            claims: &claims,
        };

        for expression in [
            r#"issuer == "nomad""#,
            r#"namespace == "default""#,
            r#"subject startswith_cs "rustak-plugin-""#,
            r#"claims.nomad_task == "ais""#,
            r#"claims.nomad_allocation_id == "3f1""#,
        ] {
            assert!(
                filt_rs::Filter::new(expression)
                    .unwrap()
                    .matches(&filter)
                    .unwrap(),
                "{expression}",
            );
        }

        assert!(
            !filt_rs::Filter::new(r#"claims.nomad_task == "adsb""#)
                .unwrap()
                .matches(&filter)
                .unwrap()
        );
        assert_eq!(filter.get("claims.nothing"), FilterValue::Null);
        assert_eq!(filter.get("whatever"), FilterValue::Null);
    }

    #[test]
    fn an_expression_reaches_a_nested_kubernetes_claim_too() {
        let claims = kubernetes();
        let filter = ClaimsFilter {
            issuer: "kubernetes",
            namespace: "tak",
            subject: "rustak-plugin-ais",
            claims: &claims,
        };

        assert!(
            filt_rs::Filter::new(r#"claims.kubernetes.io.pod.name startswith_cs "ais-""#)
                .unwrap()
                .matches(&filter)
                .unwrap()
        );
    }
}

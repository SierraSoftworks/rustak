//! The rules an `[auth.workload]` section has to satisfy beyond parsing.
//!
//! Each of these is a file that is individually well formed and jointly
//! impossible or dangerous: an issuer with no key set to fetch, a rule naming
//! an issuer nobody registered, a prefix rule with no prefix, two rules that
//! could hand one token to two accounts. Catching them when the file is read is
//! what makes `rustak --check` worth running before a deployment goes out — the
//! alternative is a server that starts, reports itself healthy, and refuses the
//! first enrolment.

use human_errors::Error;

use super::{WorkloadAccount, WorkloadConfig, WorkloadIssuer, WorkloadRule};
use crate::config::validate::positive;

/// Advice for a section the example file shows the right form of.
const ADVICE_EXAMPLE: &[&str] = &[
    "Compare the section against config.example.toml, which documents every key with its default.",
    "Run `rustak --config <file> --check` to validate a file without starting the server.",
];

/// Advice for a workload issuer that publishes its keys over plain HTTP.
const ADVICE_INSECURE_JWKS: &[&str] = &[
    "Use an https:// URL: whoever can answer that request decides which signatures this server accepts.",
    "A loopback address is accepted as it is, because nothing crosses a network to reach it.",
    "Set `allow_insecure_jwks = true` on the issuer to accept the risk deliberately; it is logged at every start-up.",
];

/// Advice for two rules that could hand one token to two accounts.
const ADVICE_AMBIGUOUS: &[&str] = &[
    "Give the rules different namespaces, or prefixes that cannot both match one subject.",
    "Add a `match` expression to one of them so that a token satisfies exactly one.",
    "A token that maps to two accounts is refused at runtime; this is the same refusal, found earlier.",
];

impl WorkloadConfig {
    /// Checks everything a `[auth.workload]` section has to satisfy.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error naming the issuer or the rule that
    /// is wrong, and what to write instead.
    pub fn validate(&self) -> Result<(), Error> {
        issuers(self)?;

        for rule in &self.rules {
            rule_is_usable(self, rule)?;
        }

        overlaps(self)
    }
}

/// Every issuer has to be one we could fetch keys from, under a name of its
/// own.
fn issuers(workload: &WorkloadConfig) -> Result<(), Error> {
    for (index, issuer) in workload.issuers.iter().enumerate() {
        issuer_is_usable(issuer)?;

        if workload.issuers[..index]
            .iter()
            .any(|earlier| earlier.name == issuer.name)
        {
            return Err(human_errors::user(
                format!(
                    "`[auth.workload]` registers two issuers called `{}`, and a rule naming it could mean either.",
                    issuer.name
                ),
                ADVICE_EXAMPLE,
            ));
        }
    }

    Ok(())
}

/// One issuer: a name, an audience, exactly one key source, usable windows.
fn issuer_is_usable(issuer: &WorkloadIssuer) -> Result<(), Error> {
    let name = &issuer.name;

    if name.trim().is_empty() {
        return Err(human_errors::user(
            "An issuer under `[auth.workload]` has no `name`, and a rule has nothing to refer to it by.",
            ADVICE_EXAMPLE,
        ));
    }

    if issuer.audience.trim().is_empty() {
        return Err(human_errors::user(
            format!(
                "The workload issuer `{name}` sets no `audience`, so a token minted for anything else at the same orchestrator would be accepted here."
            ),
            &[
                "Set `audience` to the value the workload's own configuration asks for — `aud = [\"rustak\"]` in a Nomad `identity` block, or the projected token's `audience`.",
                "There is no default: an audience nobody chose is one nobody can rely on.",
            ],
        ));
    }

    let sources = [
        issuer.jwks_url.is_some(),
        issuer.discovery_url.is_some(),
        issuer.jwks_file.is_some(),
    ]
    .into_iter()
    .filter(|set| *set)
    .count();

    if sources != 1 {
        return Err(human_errors::user(
            format!(
                "The workload issuer `{name}` names {sources} places to get its signing keys from, and it needs exactly one."
            ),
            &[
                "Set `jwks_url` to the key set — Nomad publishes it at /.well-known/jwks.json on its HTTP API, Kubernetes at /openid/v1/jwks.",
                "Or set `discovery_url` to an OpenID discovery document and let rustak read `jwks_uri` out of it.",
                "Or set `jwks_file` to a key set on disk, for an installation that pins the keys itself.",
            ],
        ));
    }

    if issuer.algorithms.is_empty() {
        return Err(human_errors::user(
            format!(
                "The workload issuer `{name}` accepts no signature algorithms, so no token could be verified."
            ),
            ADVICE_EXAMPLE,
        ));
    }

    positive(
        issuer.clock_skew,
        &format!("[auth.workload] {name} clock_skew"),
    )?;
    positive(
        issuer.jwks_refresh,
        &format!("[auth.workload] {name} jwks_refresh"),
    )?;

    transport_is_safe(issuer)
}

/// A key set fetched over plain HTTP from somewhere else on the network is one
/// anybody on that network can replace.
fn transport_is_safe(issuer: &WorkloadIssuer) -> Result<(), Error> {
    let Some(url) = issuer
        .jwks_url
        .as_deref()
        .or(issuer.discovery_url.as_deref())
    else {
        return Ok(());
    };

    let parsed = url::Url::parse(url).map_err(|err| {
        human_errors::user(
            format!(
                "The workload issuer `{}` names `{url}`, which is not a URL rustak can fetch ({err}).",
                issuer.name
            ),
            ADVICE_EXAMPLE,
        )
    })?;

    if parsed.scheme() != "http" || issuer.allow_insecure_jwks || is_loopback(&parsed) {
        return Ok(());
    }

    Err(human_errors::user(
        format!(
            "The workload issuer `{}` fetches its signing keys over plain HTTP from `{}`.",
            issuer.name,
            parsed.host_str().unwrap_or_default(),
        ),
        ADVICE_INSECURE_JWKS,
    ))
}

/// Whether a URL's host is this machine, in which case nothing crosses a
/// network to reach it.
fn is_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        None => false,
    }
}

/// One rule: an issuer that exists, claims to read, and an account to land on.
fn rule_is_usable(workload: &WorkloadConfig, rule: &WorkloadRule) -> Result<(), Error> {
    if workload.issuer(&rule.issuer).is_none() {
        return Err(human_errors::user(
            format!(
                "A `[auth.workload]` rule names the issuer `{}`, and no issuer by that name is registered.",
                rule.issuer
            ),
            ADVICE_EXAMPLE,
        ));
    }

    for (value, key) in [
        (&rule.namespace_claim, "namespace_claim"),
        (&rule.namespace, "namespace"),
        (&rule.subject_claim, "subject_claim"),
    ] {
        if value.trim().is_empty() {
            return Err(human_errors::user(
                format!(
                    "A `[auth.workload]` rule for `{}` leaves `{key}` empty, so it would match every token the issuer signs.",
                    rule.issuer
                ),
                ADVICE_EXAMPLE,
            ));
        }
    }

    if rule.account == WorkloadAccount::StripPrefix && rule.prefix().is_empty() {
        return Err(human_errors::user(
            format!(
                "A `[auth.workload]` rule for `{}` strips a prefix it does not set, so every job in `{}` would enrol as itself.",
                rule.issuer, rule.namespace,
            ),
            &[
                "Set `subject_prefix` to the prefix your deployments share, for example \"rustak-plugin-\".",
                "Or set `account` to the one account this rule is for, written out in full.",
            ],
        ));
    }

    Ok(())
}

/// Two rules that could hand one token to two different accounts.
///
/// The obvious cases only, and deliberately so: a `match` expression is an
/// arbitrary predicate and deciding whether two of them can both hold is not
/// something a configuration check can do. What is caught here is the shape an
/// operator actually writes by accident — two rules over the same claims in the
/// same namespace whose prefixes overlap. Anything subtler is refused at
/// runtime instead, where both rules have a token in front of them.
fn overlaps(workload: &WorkloadConfig) -> Result<(), Error> {
    for (index, rule) in workload.rules.iter().enumerate() {
        for earlier in &workload.rules[..index] {
            if !overlapping(earlier, rule) {
                continue;
            }

            return Err(human_errors::user(
                format!(
                    "Two `[auth.workload]` rules for `{}` in `{}` both match a subject starting `{}`, and they name different accounts.",
                    rule.issuer,
                    rule.namespace,
                    longest_prefix(earlier, rule),
                ),
                ADVICE_AMBIGUOUS,
            ));
        }
    }

    Ok(())
}

/// Whether two rules could both claim one token, for different accounts.
fn overlapping(first: &WorkloadRule, second: &WorkloadRule) -> bool {
    if first == second {
        // The same rule written twice lands on the same account, which is
        // untidy rather than ambiguous.
        return false;
    }

    // A `match` is an arbitrary predicate; two rules carrying one may well be
    // disjoint, and guessing otherwise would refuse a file that works.
    if first.match_claims.is_some() || second.match_claims.is_some() {
        return false;
    }

    let same_question = first.issuer == second.issuer
        && first.namespace_claim == second.namespace_claim
        && first.namespace == second.namespace
        && first.subject_claim == second.subject_claim;

    let prefixes_overlap =
        first.prefix().starts_with(second.prefix()) || second.prefix().starts_with(first.prefix());

    same_question && prefixes_overlap && !same_account(first, second)
}

/// Whether two rules would always land on the same account anyway.
fn same_account(first: &WorkloadRule, second: &WorkloadRule) -> bool {
    match (&first.account, &second.account) {
        (WorkloadAccount::Fixed(left), WorkloadAccount::Fixed(right)) => left == right,
        // Two strip-prefix rules with different prefixes produce different
        // remainders for the same subject, which is the case this exists for.
        _ => false,
    }
}

/// The longer of two overlapping prefixes, for the message that names them.
fn longest_prefix<'a>(first: &'a WorkloadRule, second: &'a WorkloadRule) -> &'a str {
    match first.prefix().len() >= second.prefix().len() {
        true => first.prefix(),
        false => second.prefix(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete, valid section, with `{extra}` appended.
    fn section(extra: &str) -> WorkloadConfig {
        toml::from_str(&format!(
            r#"
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
            {extra}
            "#
        ))
        .expect("the fragment should parse")
    }

    fn refusal(extra: &str) -> String {
        let Err(err) = section(extra).validate() else {
            panic!("this section should not validate:\n{extra}");
        };

        assert!(err.is(human_errors::Kind::User), "{err}");
        err.to_string()
    }

    #[test]
    fn the_reference_section_is_one_check_accepts() {
        section("")
            .validate()
            .expect("the reference section is valid");
    }

    #[test]
    fn an_empty_section_is_valid_and_admits_nobody() {
        WorkloadConfig::default()
            .validate()
            .expect("configuring nothing is not an error");
    }

    #[test]
    fn an_issuer_with_no_audience_is_refused_by_name() {
        // The whole point of the audience: a Nomad cluster mints identities for
        // Vault and Consul too, and one of those must not enrol here.
        let message = refusal(
            r#"
            [[issuers]]
            name = "other"
            jwks_url = "https://other.example.com/jwks"
            audience = ""
            "#,
        );

        assert!(message.contains("audience"), "{message}");
        assert!(message.contains("other"), "{message}");
    }

    #[test]
    fn an_issuer_needs_exactly_one_place_to_get_its_keys() {
        for sources in [
            "",
            "jwks_url = \"https://a/jwks\"\ndiscovery_url = \"https://a/.well-known/openid-configuration\"",
        ] {
            let message = refusal(&format!(
                "[[issuers]]\nname = \"other\"\naudience = \"rustak\"\n{sources}\n"
            ));

            assert!(message.contains("signing keys"), "{message}");
        }
    }

    #[test]
    fn two_issuers_of_one_name_are_refused_because_a_rule_could_mean_either() {
        let message = refusal(
            r#"
            [[issuers]]
            name = "nomad"
            jwks_url = "https://elsewhere.example.com/jwks"
            audience = "rustak"
            "#,
        );

        assert!(message.contains("two issuers"), "{message}");
    }

    #[test]
    fn a_rule_naming_an_issuer_nobody_registered_is_refused() {
        let message = refusal(
            r#"
            [[rules]]
            issuer = "kubernetes"
            namespace_claim = "kubernetes.io.namespace"
            namespace = "tak"
            subject_claim = "kubernetes.io.serviceaccount.name"
            account = "svc.ais"
            "#,
        );

        assert!(message.contains("kubernetes"), "{message}");
    }

    #[test]
    fn a_prefix_rule_with_no_prefix_would_enrol_every_job_as_itself() {
        let message = refusal(
            r#"
            [[rules]]
            issuer = "nomad"
            namespace_claim = "nomad_namespace"
            namespace = "other"
            subject_claim = "nomad_job_id"
            account = "strip-prefix"
            "#,
        );

        assert!(message.contains("prefix"), "{message}");
    }

    #[test]
    fn two_rules_that_could_hand_one_token_to_two_accounts_are_refused() {
        // `rustak-plugin-ais` satisfies both prefixes and strips to two
        // different names, which is exactly the accident this catches.
        let message = refusal(
            r#"
            [[rules]]
            issuer = "nomad"
            namespace_claim = "nomad_namespace"
            namespace = "default"
            subject_claim = "nomad_job_id"
            subject_prefix = "rustak-"
            account = "strip-prefix"
            "#,
        );

        assert!(message.contains("different accounts"), "{message}");
        assert!(message.contains("rustak-plugin-"), "{message}");
    }

    #[test]
    fn rules_that_cannot_both_match_are_left_alone() {
        // Different namespaces, disjoint prefixes, and a narrowing expression
        // are each enough to make two rules a deliberate pair rather than a
        // mistake.
        for extra in [
            r#"
            [[rules]]
            issuer = "nomad"
            namespace_claim = "nomad_namespace"
            namespace = "staging"
            subject_claim = "nomad_job_id"
            subject_prefix = "rustak-plugin-"
            account = "strip-prefix"
            "#,
            r#"
            [[rules]]
            issuer = "nomad"
            namespace_claim = "nomad_namespace"
            namespace = "default"
            subject_claim = "nomad_job_id"
            subject_prefix = "feed-"
            account = "strip-prefix"
            "#,
            r#"
            [[rules]]
            issuer = "nomad"
            namespace_claim = "nomad_namespace"
            namespace = "default"
            subject_claim = "nomad_job_id"
            subject_prefix = "rustak-"
            account = "strip-prefix"
            match = 'claims.nomad_task == "ais"'
            "#,
        ] {
            section(extra)
                .validate()
                .unwrap_or_else(|err| panic!("this pair is deliberate: {err}\n{extra}"));
        }
    }

    #[test]
    fn a_key_set_fetched_over_plain_http_needs_saying_so_out_loud() {
        // The first deployment fetches from a tailnet address over plain HTTP
        // because the TLS name hairpins badly from inside a container; that has
        // to be a decision somebody wrote down, not a default.
        let message = refusal(
            r#"
            [[issuers]]
            name = "nomad-plain"
            jwks_url = "http://100.64.0.1:4646/.well-known/jwks.json"
            audience = "rustak"
            "#,
        );

        assert!(message.contains("plain HTTP"), "{message}");
        assert!(message.contains("100.64.0.1"), "{message}");

        section(
            r#"
            [[issuers]]
            name = "nomad-plain"
            jwks_url = "http://100.64.0.1:4646/.well-known/jwks.json"
            audience = "rustak"
            allow_insecure_jwks = true
            "#,
        )
        .validate()
        .expect("the opt-in is what makes it allowed");
    }

    #[test]
    fn a_loopback_key_set_needs_no_opt_in_because_nothing_crosses_a_network() {
        for host in ["127.0.0.1:4646", "localhost:4646", "[::1]:4646"] {
            section(&format!(
                "[[issuers]]\nname = \"local\"\njwks_url = \"http://{host}/jwks\"\naudience = \"rustak\"\n"
            ))
            .validate()
            .unwrap_or_else(|err| panic!("{host} should need no opt-in: {err}"));
        }
    }

    #[test]
    fn a_window_of_zero_is_refused_the_way_every_other_one_is() {
        let message = refusal(
            r#"
            [[issuers]]
            name = "other"
            jwks_url = "https://other.example.com/jwks"
            audience = "rustak"
            clock_skew = "0s"
            "#,
        );

        assert!(message.contains("clock_skew"), "{message}");
    }
}

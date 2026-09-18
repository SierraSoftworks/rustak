//! Turning a verified claim set into somebody.
//!
//! Two decisions live here and they are deliberately separate. *Who is this?* —
//! the account name everything they own is stored under, which must not change
//! because a directory let them edit their display name. *What do we call
//! them?* — the name on the screen, which may change freely.

use rustak_api::Username;
use rustak_core::prelude::*;

use crate::config::OidcConfig;
use crate::identity::VerifiedIdentity;

/// Claims that carry protocol meaning rather than anything about the person.
///
/// Hidden from the access-control expressions so that an operator writing
/// `claims.*` sees a directory's attributes and not the mechanics of the token
/// that carried them.
const PROTOCOL_CLAIMS: &[&str] = &[
    "exp",
    "nbf",
    "iat",
    "iss",
    "aud",
    "jti",
    "nonce",
    "at_hash",
    "c_hash",
    "azp",
    "auth_time",
];

/// The claim consulted when `username_claim` names nothing usable.
const FALLBACK_USERNAME_CLAIM: &str = "preferred_username";

/// A non-empty string claim.
fn string_claim(claims: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    claims
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Who this claim set says signed in.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the provider sent no usable
/// account name, naming the claim it was asked for so the operator can point
/// `username_claim` somewhere else.
pub fn identity_from_claims(
    oidc: &OidcConfig,
    issuer: &str,
    claims: &serde_json::Map<String, serde_json::Value>,
) -> Result<VerifiedIdentity, Error> {
    let subject = string_claim(claims, "sub").ok_or_else(|| {
        human_errors::user(
            "Your identity provider did not say who signed in.",
            &["Every OpenID Connect provider must include a 'sub' claim; check its configuration."],
        )
    })?;

    // `sub` is the last resort rather than the default: it is usually an opaque
    // identifier, and an account name is something people have to recognise,
    // type into a client and see in the audit log.
    let claim = &oidc.username_claim;
    let raw = string_claim(claims, claim)
        .or_else(|| string_claim(claims, FALLBACK_USERNAME_CLAIM))
        .or_else(|| string_claim(claims, "sub"))
        .ok_or_else(|| missing_username(claim))?;

    let username = Username::parse(&raw).map_err(|err| {
        human_errors::user(
            format!("Your identity provider's '{claim}' claim is not a usable account name: {err}"),
            &["Point 'username_claim' under [auth.oidc] at a claim carrying a plain account name."],
        )
    })?;

    Ok(VerifiedIdentity {
        issuer: issuer.to_string(),
        subject,
        username,
        display_name: string_claim(claims, "name")
            .or_else(|| string_claim(claims, FALLBACK_USERNAME_CLAIM)),
        email: string_claim(claims, "email"),
        groups: groups_from_claims(oidc, claims),
    })
}

/// The failure for a provider that sent no account name.
fn missing_username(claim: &str) -> Error {
    human_errors::user(
        format!(
            "Your identity provider did not send a '{claim}' claim, so we cannot tell which account you are signing in to."
        ),
        &[
            "Configure your identity provider to include that claim in its ID tokens.",
            "Alternatively, set 'username_claim' under [auth.oidc] to a claim it does send.",
        ],
    )
}

/// The groups claim, as a list of strings.
///
/// A provider that sends a single string rather than an array is common enough
/// to be worth accepting; anything else is ignored rather than refused, because
/// a claim we cannot read is not a reason to refuse somebody's sign-in.
pub fn groups_from_claims(
    oidc: &OidcConfig,
    claims: &serde_json::Map<String, serde_json::Value>,
) -> Vec<String> {
    match claims.get(&oidc.groups_claim) {
        Some(serde_json::Value::Array(values)) => values
            .iter()
            .filter_map(|value| value.as_str())
            .map(str::to_string)
            .collect(),
        Some(serde_json::Value::String(value)) => vec![value.clone()],
        _ => Vec::new(),
    }
}

/// The claims an access-control expression may see.
pub fn filterable_claims(
    claims: &serde_json::Map<String, serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    claims
        .iter()
        .filter(|(key, _)| !PROTOCOL_CLAIMS.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oidc() -> OidcConfig {
        OidcConfig {
            endpoint: "https://id.example.com".to_string(),
            client_id: "rustak".to_string(),
            client_secret: "secret".to_string(),
            scopes: Vec::new(),
            username_claim: "preferred_username".to_string(),
            groups_claim: "groups".to_string(),
            group_prefix: String::new(),
            strip_group_prefix: true,
            read_suffix: "_READ".to_string(),
            write_suffix: "_WRITE".to_string(),
            read_only_group: None,
            auto_create_groups: true,
            link_by_username: false,
            display_name: None,
        }
    }

    fn claims(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn the_account_name_comes_from_the_configured_claim() {
        let identity = identity_from_claims(
            &oidc(),
            "https://id.example.com",
            &claims(serde_json::json!({
                "sub": "opaque-subject-id",
                "preferred_username": "ada",
                "name": "Ada Lovelace",
                "email": "ada@example.com",
            })),
        )
        .unwrap();

        assert_eq!(identity.username.as_str(), "ada");
        assert_eq!(
            identity.subject, "opaque-subject-id",
            "the subject is the identity, the username is only a label on it",
        );
        assert_eq!(identity.display_name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(identity.email.as_deref(), Some("ada@example.com"));
    }

    #[test]
    fn a_provider_that_sends_only_a_subject_is_still_usable() {
        let identity = identity_from_claims(
            &oidc(),
            "https://id.example.com",
            &claims(serde_json::json!({ "sub": "ada" })),
        )
        .unwrap();

        assert_eq!(identity.username.as_str(), "ada");
        assert_eq!(identity.display_name, None);
    }

    #[test]
    fn a_configured_claim_that_is_absent_falls_back_before_it_fails() {
        let oidc = OidcConfig {
            username_claim: "upn".to_string(),
            ..oidc()
        };

        let identity = identity_from_claims(
            &oidc,
            "https://id.example.com",
            &claims(serde_json::json!({ "sub": "s", "preferred_username": "ada" })),
        )
        .unwrap();

        assert_eq!(identity.username.as_str(), "ada");
    }

    #[test]
    fn a_token_with_no_subject_is_not_a_sign_in() {
        assert!(
            identity_from_claims(
                &oidc(),
                "https://id.example.com",
                &claims(serde_json::json!({ "preferred_username": "ada" })),
            )
            .is_err()
        );
    }

    #[test]
    fn an_unusable_account_name_says_which_claim_to_point_elsewhere() {
        let failed = identity_from_claims(
            &oidc(),
            "https://id.example.com",
            &claims(serde_json::json!({ "sub": "s", "preferred_username": "not a username!" })),
        )
        .unwrap_err();

        assert!(failed.is(human_errors::Kind::User));
        assert!(failed.description().contains("preferred_username"));
    }

    #[test]
    fn a_groups_claim_is_read_as_an_array_or_as_one_string() {
        let oidc = oidc();

        assert_eq!(
            groups_from_claims(&oidc, &claims(serde_json::json!({ "groups": ["a", "b"] }))),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            groups_from_claims(&oidc, &claims(serde_json::json!({ "groups": "a" }))),
            vec!["a".to_string()]
        );
        assert!(groups_from_claims(&oidc, &claims(serde_json::json!({ "groups": 7 }))).is_empty());
        assert!(groups_from_claims(&oidc, &claims(serde_json::json!({}))).is_empty());
    }

    #[test]
    fn the_mechanics_of_the_token_are_hidden_from_the_access_control_rules() {
        let filtered = filterable_claims(&claims(serde_json::json!({
            "sub": "ada",
            "email": "ada@example.com",
            "groups": ["admins"],
            "exp": 1,
            "iss": "https://id.example.com",
            "aud": "rustak",
            "nonce": "abc",
        })));

        assert!(filtered.contains_key("sub"));
        assert!(filtered.contains_key("email"));
        assert!(filtered.contains_key("groups"));

        for hidden in ["exp", "iss", "aud", "nonce"] {
            assert!(!filtered.contains_key(hidden), "{hidden} should be hidden");
        }
    }
}

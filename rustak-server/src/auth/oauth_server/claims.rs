//! What this server is willing to say about an account, and to whom.
//!
//! One function builds the claim set, and both places that release claims — the
//! ID token ([`super::id_token`]) and `/oauth/userinfo`
//! ([`mod@super::userinfo`]) —
//! go through it, so the two can never disagree about who somebody is. A
//! relying party that read one identity from the token and another from
//! userinfo would have two accounts for one person, and the certificate
//! enrolled afterwards would carry whichever of them happened to win.
//!
//! # The identity is one string
//!
//! `sub` and `preferred_username` are both the rustak username, which is also
//! the `sub` of the access token and the common name of the certificate
//! enrolment issues. Anything else would mean CloudTAK's profile key, rustak's
//! account and the certificate on the device were three different names for the
//! same person.
//!
//! # What each scope releases
//!
//! | Scope | Claim | Source |
//! |---|---|---|
//! | always | `sub`, `preferred_username` | `users.username` |
//! | `profile` | `name` | `users.display_name`, omitted when unset |
//! | `email` | `email` | `users.email`, **omitted when unset** |
//! | `groups` | `groups` | the channels held, plus the administrator marker |
//!
//! An account with no email address has no `email` claim. Synthesising one from
//! the username would be a plausible-looking address that belongs to somebody
//! else, and a relying party that matches accounts by email would hand them
//! this one.

use crate::config::OAuthServerConfig;
use crate::db::Database;
use crate::db::repos::UserRow;
use crate::prelude::*;

use super::scopes;

/// The claims released for an account, narrowed to `granted`.
///
/// `granted` of [`None`] releases the full set. That is the password grant's
/// token asking about itself: it was issued with rustak's own `api` scope,
/// which already reaches every endpoint these facts come from, so withholding
/// them would be a formality rather than a control.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the channel read fails.
pub async fn released(
    db: &Database,
    oauth: &OAuthServerConfig,
    user: &UserRow,
    is_admin: bool,
    granted: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, Error> {
    let allows = |scope: &str| granted.is_none_or(|granted| scopes::grants(granted, scope));
    let mut claims = serde_json::Map::new();

    claims.insert("sub".to_string(), user.username.as_str().into());
    claims.insert(
        "preferred_username".to_string(),
        user.username.as_str().into(),
    );

    if allows(scopes::PROFILE)
        && let Some(name) = user.display_name.as_deref()
    {
        claims.insert("name".to_string(), name.into());
    }

    // Never invented; see the module documentation.
    if allows(scopes::EMAIL)
        && let Some(email) = user.email.as_deref()
    {
        claims.insert("email".to_string(), email.into());
    }

    if allows(scopes::GROUPS) {
        claims.insert(
            "groups".to_string(),
            groups(db, oauth, user, is_admin).await?.into(),
        );
    }

    Ok(claims)
}

/// The group names an account is reported in.
///
/// The **channels** it holds — a membership, in either direction, rather than
/// whatever a device has switched on right now — deduplicated, because a
/// channel held `IN` and `OUT` is one channel and a relying party mapping roles
/// from this list should not see it twice. `[auth.oauth] admin_group` is
/// appended for an administrator, so that a relying party can map admin rights
/// by group without a claim a standard library would ignore.
async fn groups(
    db: &Database,
    oauth: &OAuthServerConfig,
    user: &UserRow,
    is_admin: bool,
) -> Result<Vec<String>, Error> {
    let held = crate::identity::members::grants_for_user(db, user.id).await?;
    let mut names: Vec<String> = Vec::with_capacity(held.len() + 1);

    for grant in &held {
        let name = grant.group.as_str().to_string();

        if !names.contains(&name) {
            names.push(name);
        }
    }

    if is_admin && !names.contains(&oauth.admin_group) {
        names.push(oauth.admin_group.clone());
    }

    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewUser;

    /// An account with a display name, an email address and a channel.
    async fn fixture(email: Option<&str>) -> (Database, UserRow) {
        let db = Database::open_in_memory().await.unwrap();
        let user = db
            .users()
            .create(NewUser {
                display_name: Some("Ada Lovelace".to_string()),
                email: email.map(str::to_string),
                ..NewUser::person(Username::parse("ada").unwrap())
            })
            .await
            .unwrap();

        crate::identity::groups::join_default(&db, user.id)
            .await
            .unwrap();

        (db, user)
    }

    fn oauth() -> OAuthServerConfig {
        OAuthServerConfig::default()
    }

    #[tokio::test]
    async fn the_identity_is_one_string_whatever_was_asked_for() {
        let (db, user) = fixture(Some("ada@example.com")).await;

        for granted in [None, Some("openid"), Some("openid profile email groups")] {
            let claims = released(&db, &oauth(), &user, false, granted)
                .await
                .unwrap();

            assert_eq!(claims["sub"], "ada", "{granted:?}");
            assert_eq!(claims["preferred_username"], "ada", "{granted:?}");
        }
    }

    #[tokio::test]
    async fn a_scope_that_was_not_granted_releases_nothing() {
        let (db, user) = fixture(Some("ada@example.com")).await;
        let claims = released(&db, &oauth(), &user, true, Some("openid"))
            .await
            .unwrap();

        assert!(!claims.contains_key("name"), "{claims:?}");
        assert!(!claims.contains_key("email"), "{claims:?}");
        assert!(!claims.contains_key("groups"), "{claims:?}");
    }

    #[tokio::test]
    async fn each_scope_releases_exactly_its_own_claim() {
        let (db, user) = fixture(Some("ada@example.com")).await;

        let profile = released(&db, &oauth(), &user, false, Some("openid profile"))
            .await
            .unwrap();

        assert_eq!(profile["name"], "Ada Lovelace");
        assert!(!profile.contains_key("email"));

        let email = released(&db, &oauth(), &user, false, Some("openid email"))
            .await
            .unwrap();

        assert_eq!(email["email"], "ada@example.com");
        assert!(!email.contains_key("name"));
    }

    #[tokio::test]
    async fn an_account_with_no_email_address_has_no_email_claim() {
        // Rather than one synthesised from the username, which would be a
        // plausible address belonging to somebody else.
        let (db, user) = fixture(None).await;
        let claims = released(&db, &oauth(), &user, false, Some("openid email"))
            .await
            .unwrap();

        assert!(!claims.contains_key("email"), "{claims:?}");
    }

    #[tokio::test]
    async fn the_groups_claim_is_the_channels_held_and_nothing_is_listed_twice() {
        let (db, user) = fixture(None).await;
        let claims = released(&db, &oauth(), &user, false, Some("groups"))
            .await
            .unwrap();

        let names: Vec<&str> = claims["groups"]
            .as_array()
            .expect("a flat array of strings")
            .iter()
            .map(|name| name.as_str().expect("a string"))
            .collect();

        assert!(names.contains(&"__ANON__"), "{names:?}");
        assert_eq!(
            names.len(),
            1,
            "a channel held in both directions is one channel: {names:?}",
        );
        assert!(!names.contains(&"admin"), "{names:?}");
    }

    #[tokio::test]
    async fn an_administrator_carries_the_marker_group_a_relying_party_maps() {
        let (db, user) = fixture(None).await;
        let claims = released(&db, &oauth(), &user, true, Some("groups"))
            .await
            .unwrap();

        assert!(
            claims["groups"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == "admin"),
            "{claims:?}",
        );
    }

    #[tokio::test]
    async fn the_marker_group_is_whatever_the_installation_named_it() {
        let (db, user) = fixture(None).await;
        let oauth = OAuthServerConfig {
            admin_group: "tak-admins".to_string(),
            ..OAuthServerConfig::default()
        };
        let claims = released(&db, &oauth, &user, true, Some("groups"))
            .await
            .unwrap();
        let names = claims["groups"].as_array().unwrap();

        assert!(names.iter().any(|name| name == "tak-admins"), "{names:?}");
        assert!(!names.iter().any(|name| name == "admin"), "{names:?}");
    }
}

//! `[auth.oidc]` — federating with an identity provider.
//!
//! Two things are configured here, and they are worth separating in your head.
//! The first four keys are the OAuth2 client registration: where the provider
//! is, who we are to it, and what we ask for. Everything after that is **group
//! mapping** — how a `groups` claim becomes membership of rustak channels.
//!
//! # The suffix convention
//!
//! TAK deployments conventionally encode a channel's direction in the group
//! name: `ops_READ` grants read (OUT), `ops_WRITE` grants write (IN), and a
//! bare `ops` grants both. rustak follows that convention because it is what an
//! existing directory is already populated with, and makes both suffixes
//! configurable because not every directory uses those exact words. Set
//! `read_only_group` to a group whose members never get write access, whatever
//! the suffix says.
//!
//! # Secrets
//!
//! `client_secret` is written as `"${{ env.RUSTAK_OIDC_CLIENT_SECRET }}"` and
//! supplied through the environment; the loader substitutes it before parsing.
//! It is redacted from the `Debug` rendering of this struct.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What a redacted secret renders as in a `Debug` dump.
const REDACTED: &str = "<redacted>";

/// The scope every OIDC request carries, whether or not it is configured.
const OPENID: &str = "openid";

/// The scopes requested when none are configured.
///
/// `offline_access` is included because without a refresh token the admin UI
/// cannot renew a session in the background and has to interrupt whoever is
/// using it. Add `groups` when your provider gates the groups claim behind one.
fn default_scopes() -> Vec<String> {
    vec![
        "profile".to_string(),
        "email".to_string(),
        "offline_access".to_string(),
    ]
}

/// The claim naming the account, falling back to `sub` when it is absent.
fn default_username_claim() -> String {
    "preferred_username".to_string()
}

/// The claim listing the groups an account belongs to.
fn default_groups_claim() -> String {
    "groups".to_string()
}

fn default_read_suffix() -> String {
    "_READ".to_string()
}

fn default_write_suffix() -> String {
    "_WRITE".to_string()
}

fn default_true() -> bool {
    true
}

/// `[auth.oidc]`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfig {
    /// The provider's issuer URL. Its endpoints are discovered from
    /// `{endpoint}/.well-known/openid-configuration`.
    pub endpoint: String,

    /// The client ID registered with the provider, and the audience we require
    /// of its ID tokens.
    pub client_id: String,

    /// The client secret registered with the provider.
    pub client_secret: String,

    /// The scopes requested. `openid` is always added.
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,

    /// The claim identifying the account, which becomes the rustak username.
    ///
    /// Choose one that does not change: renaming an account is a migration, not
    /// something that should happen because somebody changed their email.
    #[serde(default = "default_username_claim")]
    pub username_claim: String,

    /// The claim listing group memberships.
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,

    /// Only groups starting with this prefix are considered. Empty considers
    /// all of them.
    #[serde(default)]
    pub group_prefix: String,

    /// Whether `group_prefix` is removed from the resulting channel name, so
    /// that a directory group `tak-ops` becomes the channel `ops`.
    #[serde(default = "default_true")]
    pub strip_group_prefix: bool,

    /// The suffix marking a read-only (OUT) membership.
    #[serde(default = "default_read_suffix")]
    pub read_suffix: String,

    /// The suffix marking a write (IN) membership.
    #[serde(default = "default_write_suffix")]
    pub write_suffix: String,

    /// A group whose members never receive write access, whatever suffix their
    /// other groups carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_only_group: Option<String>,

    /// Whether a claimed group that does not exist yet is created.
    ///
    /// On, because the alternative is an administrator hand-creating a channel
    /// before anybody in it can sign in. Off when the channel list is meant to
    /// be curated here rather than in the directory.
    #[serde(default = "default_true")]
    pub auto_create_groups: bool,

    /// Whether an account whose username matches an existing local user is
    /// linked to it rather than refused.
    ///
    /// Off by default: turning it on means whoever controls the provider can
    /// take over an existing account by claiming its username.
    #[serde(default)]
    pub link_by_username: bool,

    /// The name of the sign-in button, shown on `/login/authserver` and in the
    /// admin UI. Defaults to the provider's host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

impl fmt::Debug for OidcConfig {
    /// Written out so that a configuration dump in a log or a bug report does
    /// not carry the client secret.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OidcConfig")
            .field("endpoint", &self.endpoint)
            .field("client_id", &self.client_id)
            .field("client_secret", &REDACTED)
            .field("scopes", &self.scopes)
            .field("username_claim", &self.username_claim)
            .field("groups_claim", &self.groups_claim)
            .field("group_prefix", &self.group_prefix)
            .field("strip_group_prefix", &self.strip_group_prefix)
            .field("read_suffix", &self.read_suffix)
            .field("write_suffix", &self.write_suffix)
            .field("read_only_group", &self.read_only_group)
            .field("auto_create_groups", &self.auto_create_groups)
            .field("link_by_username", &self.link_by_username)
            .field("display_name", &self.display_name)
            .finish()
    }
}

impl OidcConfig {
    /// The scopes to request, with `openid` guaranteed.
    ///
    /// A provider is entitled to reject the whole request without it, and
    /// leaving it out of the configured list is an easy thing to do — so it is
    /// added here rather than documented as a requirement.
    pub fn scopes(&self) -> Vec<String> {
        let mut scopes = Vec::with_capacity(self.scopes.len() + 1);
        scopes.push(OPENID.to_string());
        scopes.extend(
            self.scopes
                .iter()
                .filter(|scope| scope.as_str() != OPENID)
                .cloned(),
        );
        scopes
    }

    /// The name to show on the sign-in button.
    pub fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three keys a provider cannot be reached without.
    const MINIMAL: &str = r#"
        endpoint = "https://id.example.com"
        client_id = "rustak"
        client_secret = "shh"
    "#;

    #[test]
    fn the_group_mapping_keys_all_have_defaults() {
        let parsed: OidcConfig = toml::from_str(MINIMAL).unwrap();

        assert_eq!(parsed.username_claim, "preferred_username");
        assert_eq!(parsed.groups_claim, "groups");
        assert_eq!(parsed.group_prefix, "");
        assert!(parsed.strip_group_prefix);
        assert_eq!(parsed.read_suffix, "_READ");
        assert_eq!(parsed.write_suffix, "_WRITE");
        assert_eq!(parsed.read_only_group, None);
        assert!(parsed.auto_create_groups);
        assert!(!parsed.link_by_username);
    }

    #[test]
    fn linking_by_username_is_off_because_it_is_an_account_takeover() {
        // Whoever controls the provider could otherwise claim an existing
        // rustak username and inherit that account's channels.
        let parsed: OidcConfig = toml::from_str(MINIMAL).unwrap();

        assert!(!parsed.link_by_username);
    }

    #[test]
    fn a_provider_cannot_be_half_configured() {
        // No defaults for the three keys that identify us to the provider: a
        // missing client secret has to be a load failure, not a sign-in that
        // fails later with an opaque provider error.
        let Err(err) = toml::from_str::<OidcConfig>(
            r#"
            endpoint = "https://id.example.com"
            client_id = "rustak"
            "#,
        ) else {
            panic!("an OIDC section without a client secret should be refused");
        };

        assert!(err.to_string().contains("client_secret"), "{err}");
    }

    #[test]
    fn openid_is_always_requested_and_never_twice() {
        let defaults: OidcConfig = toml::from_str(MINIMAL).unwrap();
        assert_eq!(
            defaults.scopes(),
            vec!["openid", "profile", "email", "offline_access"]
        );

        let configured: OidcConfig =
            toml::from_str(&format!("{MINIMAL}\nscopes = [\"openid\", \"groups\"]")).unwrap();
        assert_eq!(configured.scopes(), vec!["openid", "groups"]);
    }

    #[test]
    fn the_sign_in_button_falls_back_to_the_providers_url() {
        let parsed: OidcConfig = toml::from_str(MINIMAL).unwrap();
        assert_eq!(parsed.display_name(), "https://id.example.com");

        let named: OidcConfig =
            toml::from_str(&format!("{MINIMAL}\ndisplay_name = \"Home SSO\"")).unwrap();
        assert_eq!(named.display_name(), "Home SSO");
    }

    #[test]
    fn the_client_secret_never_appears_in_a_debug_dump() {
        let parsed: OidcConfig = toml::from_str(MINIMAL).unwrap();

        let rendered = format!("{parsed:?}");

        assert!(!rendered.contains("shh"), "{rendered}");
        assert!(rendered.contains(REDACTED), "{rendered}");
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<OidcConfig>(&format!("{MINIMAL}\ngroup_claim = \"g\""))
        else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("group_claim"), "{err}");
    }
}

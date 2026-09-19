//! Signing in to the admin API.
//!
//! However somebody signs in — through the identity provider, or with a passkey
//! — the server issues its own RS256 bearer token and a rotating refresh token.
//! That keeps one token format everywhere: the same one CloudTAK receives from
//! `/oauth/token` and the same one a sidecar presents to the control API.
//!
//! There is no password sign-in. A person either federates to an identity
//! provider or registers a passkey, so there is no reusable human secret for
//! this server to store, rate-limit or leak.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::group::GroupMembership;
use crate::identity::Username;
use crate::user::{UserKind, UserSource};

/// How this installation expects people to sign in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthMode {
    /// Federated to an identity provider. The UI opens
    /// [`AuthMode::Oidc::authorization_endpoint`] in a popup, gets a code back,
    /// and exchanges it here for our own token.
    Oidc {
        authorization_endpoint: String,
        client_id: String,

        #[serde(default)]
        scopes: Vec<String>,

        /// Whether the flow requires proof key for code exchange. Always true;
        /// carried explicitly so the UI never has to assume it.
        #[serde(default = "default_true")]
        pkce: bool,
    },

    /// No identity provider is configured, so people sign in with a passkey
    /// registered on this server.
    Passkey,
}

fn default_true() -> bool {
    true
}

/// What the login page needs to know before it can draw itself.
///
/// Public, because it is fetched before anybody has signed in — so it says how
/// to start, and nothing about who exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthMetadata {
    pub mode: AuthMode,

    /// Whether passkeys are also accepted.
    ///
    /// Always true when [`AuthMetadata::mode`] is [`AuthMode::Passkey`]. It can
    /// also be true alongside an identity provider, which is how an
    /// administrator keeps a way in if the provider is unreachable, and it is
    /// why the login page can show two buttons.
    #[serde(default)]
    pub passkeys_enabled: bool,
}

impl AuthMetadata {
    /// An installation with no identity provider.
    pub fn passkeys_only() -> Self {
        Self {
            mode: AuthMode::Passkey,
            passkeys_enabled: true,
        }
    }
}

/// The identity provider's authorization code, sent back for our own token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenExchangeRequest {
    pub code: String,

    /// The redirect URI the code was issued for, which the provider checks
    /// again on exchange.
    pub redirect_uri: String,

    /// The proof key for code exchange verifier the UI generated before it sent
    /// the user to the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_verifier: Option<String>,
}

/// A request to trade a refresh token for a fresh pair.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenRefreshRequest {
    pub refresh_token: String,
}

impl fmt::Debug for TokenRefreshRequest {
    /// Redacts the token, which is a credential.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenRefreshRequest")
            .field("refresh_token", &"***")
            .finish()
    }
}

/// The tokens a successful sign-in returns.
///
/// Refresh tokens rotate: using one invalidates it and issues another, and
/// using a token that has already been spent revokes the whole family, on the
/// assumption that the only way that happens is that one of them was stolen.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenResponse {
    /// The bearer token, which is our own RS256 JWT.
    pub token: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,

    /// How long the bearer token lasts, in seconds.
    pub expires_in: u64,

    /// Always `Bearer`; carried because OAuth 2.0 clients expect the field.
    #[serde(default = "default_token_type")]
    pub token_type: String,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

impl TokenResponse {
    /// A response carrying both halves of a rotating pair.
    pub fn new(
        token: impl Into<String>,
        refresh_token: impl Into<String>,
        expires_in: u64,
    ) -> Self {
        Self {
            token: token.into(),
            refresh_token: Some(refresh_token.into()),
            expires_in,
            token_type: default_token_type(),
        }
    }
}

impl fmt::Debug for TokenResponse {
    /// Redacts both tokens, so that logging a response cannot hand somebody a
    /// session.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenResponse")
            .field("token", &"***")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "***"))
            .field("expires_in", &self.expires_in)
            .field("token_type", &self.token_type)
            .finish()
    }
}

/// How the current request proved who it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthVia {
    /// Our own JWT in an `Authorization: Bearer` header. This is what a
    /// sign-in of any kind ends up as.
    Bearer,

    /// A client certificate presented during the TLS handshake, which is how
    /// enrolled devices and sidecars identify themselves.
    ClientCert,

    /// Basic authentication, accepted only on enrolment and the password grant.
    Basic,

    /// A cookie, which exists only on the `/login/*` pages that federate to an
    /// identity provider.
    Session,
}

impl AuthVia {
    /// Every method, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::Bearer, Self::ClientCert, Self::Basic, Self::Session];

    /// The value carried on the wire.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Bearer => "bearer",
            Self::ClientCert => "client_cert",
            Self::Basic => "basic",
            Self::Session => "session",
        }
    }

    /// A short phrase naming the method for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Bearer => "Bearer token",
            Self::ClientCert => "Client certificate",
            Self::Basic => "Basic authentication",
            Self::Session => "Session cookie",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|via| via.as_str() == value)
    }
}

/// Who the caller is, as the admin UI sees itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub username: Username,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    pub kind: UserKind,

    #[serde(default)]
    pub is_admin: bool,

    /// How this request authenticated.
    pub via: AuthVia,

    /// Where the account came from: made here, or by the identity provider.
    #[serde(default)]
    pub source: UserSource,

    /// The issuer of the identity provider this account is linked to, when it
    /// is linked to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_provider: Option<String>,

    /// The channels this identity may use.
    ///
    /// Looked up per request rather than carried in the token, so that removing
    /// somebody from a channel takes effect at once rather than when their
    /// token expires.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<GroupMembership>,
}

impl Me {
    /// What to call this person in the UI.
    pub fn display(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.username.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Direction, GroupName};

    #[test]
    fn a_federated_installation_describes_its_provider() {
        let metadata = AuthMetadata {
            mode: AuthMode::Oidc {
                authorization_endpoint: "https://id.example.com/authorize".into(),
                client_id: "rustak".into(),
                scopes: vec!["openid".into(), "profile".into()],
                pkce: true,
            },
            passkeys_enabled: true,
        };

        let json = serde_json::to_string(&metadata).unwrap();
        assert!(json.contains(r#""kind":"oidc""#));
        assert_eq!(
            serde_json::from_str::<AuthMetadata>(&json).unwrap(),
            metadata
        );
    }

    #[test]
    fn an_installation_with_no_provider_offers_passkeys() {
        let metadata = AuthMetadata::passkeys_only();
        let json = serde_json::to_string(&metadata).unwrap();

        assert_eq!(
            json,
            r#"{"mode":{"kind":"passkey"},"passkeys_enabled":true}"#
        );
        assert_eq!(
            serde_json::from_str::<AuthMetadata>(&json).unwrap(),
            metadata
        );
    }

    #[test]
    fn there_is_no_password_sign_in_mode() {
        // Passwords were deliberately removed: a mode named for one must fail
        // to parse rather than quietly deserialise into something else.
        assert!(serde_json::from_str::<AuthMode>(r#"{"kind":"local"}"#).is_err());
        assert!(serde_json::from_str::<AuthMode>(r#"{"kind":"password"}"#).is_err());
    }

    #[test]
    fn tokens_round_trip_and_are_redacted_in_debug_output() {
        let response = TokenResponse::new("header.payload.signature", "refresh-secret", 3600);

        let json = serde_json::to_string(&response).unwrap();
        assert_eq!(
            serde_json::from_str::<TokenResponse>(&json).unwrap(),
            response
        );

        let debug = format!("{response:?}");
        assert!(!debug.contains("signature"), "the token leaked: {debug}");
        assert!(
            !debug.contains("refresh-secret"),
            "the token leaked: {debug}"
        );
        assert!(debug.contains("3600"));

        let request = TokenRefreshRequest {
            refresh_token: "refresh-secret".into(),
        };
        assert!(!format!("{request:?}").contains("refresh-secret"));
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<TokenRefreshRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn a_token_response_defaults_its_type_to_bearer() {
        let response: TokenResponse = serde_json::from_value(serde_json::json!({
            "token": "header.payload.signature",
            "expires_in": 900,
        }))
        .unwrap();

        assert_eq!(response.token_type, "Bearer");
        assert_eq!(response.refresh_token, None);
    }

    #[test]
    fn an_exchange_request_round_trips_through_serde() {
        let request = TokenExchangeRequest {
            code: "abc".into(),
            redirect_uri: "https://tak.example.com/auth/callback".into(),
            code_verifier: Some("verifier".into()),
        };

        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<TokenExchangeRequest>(&json).unwrap(),
            request
        );
    }

    #[test]
    fn me_round_trips_through_serde() {
        let me = Me {
            username: Username::parse("alice").unwrap(),
            display_name: Some("Alice Smith".into()),
            email: Some("alice@example.com".into()),
            kind: UserKind::Person,
            is_admin: true,
            via: AuthVia::Bearer,
            source: UserSource::Oidc,
            identity_provider: Some("https://id.example.com".into()),
            groups: vec![GroupMembership::new(GroupName::anon(), Direction::Both)],
        };

        let json = serde_json::to_string(&me).unwrap();
        assert_eq!(serde_json::from_str::<Me>(&json).unwrap(), me);
        assert_eq!(me.display(), "Alice Smith");
    }

    #[test]
    fn a_caller_with_no_details_still_deserialises() {
        let me: Me = serde_json::from_value(serde_json::json!({
            "username": "bob",
            "kind": "person",
            "via": "client_cert",
        }))
        .unwrap();

        assert!(!me.is_admin);
        assert!(me.groups.is_empty());
        assert_eq!(me.display(), "bob");
    }

    #[test]
    fn methods_round_trip_through_their_wire_form() {
        for via in AuthVia::ALL.iter().copied() {
            let json = serde_json::to_string(&via).unwrap();

            assert_eq!(json, format!("\"{}\"", via.as_str()));
            assert_eq!(serde_json::from_str::<AuthVia>(&json).unwrap(), via);
            assert_eq!(AuthVia::parse(via.as_str()), Some(via));
        }

        // There is no anonymous principal on this server.
        assert_eq!(AuthVia::parse("anonymous"), None);
    }
}

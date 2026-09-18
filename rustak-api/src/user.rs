//! The people and services that hold an identity on this server.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{UserId, Username};

/// Whether an identity belongs to a person or to a machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserKind {
    /// Somebody who signs in.
    #[default]
    Person,

    /// A sidecar, which authenticates with a certificate and a service token
    /// and never sees a sign-in page.
    Service,
}

impl UserKind {
    /// Every kind, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::Person, Self::Service];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Person => "person",
            Self::Service => "service",
        }
    }

    /// A short phrase naming the kind for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Person => "Person",
            Self::Service => "Service",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }
}

/// Where an identity came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserSource {
    /// Created on this server, by the first-run wizard or an administrator.
    /// Signs in with a passkey; there are no local passwords.
    #[default]
    Local,

    /// Created the first time somebody signed in through the identity
    /// provider. Display name, email and channel memberships are refreshed
    /// from the provider's claims on every sign-in.
    Oidc,

    /// Created for a sidecar when it was registered.
    Service,
}

impl UserSource {
    /// Every source, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[Self::Local, Self::Oidc, Self::Service];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Oidc => "oidc",
            Self::Service => "service",
        }
    }

    /// A short phrase naming the source for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local => "Local",
            Self::Oidc => "Single sign-on",
            Self::Service => "Service",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|source| source.as_str() == value)
    }

    /// Whether this server owns the account's details, or the identity
    /// provider does.
    pub fn is_managed_here(&self) -> bool {
        !matches!(self, Self::Oidc)
    }
}

/// An identity as described to the admin UI.
///
/// Carries no credential of any kind — not a hash, not a hint, not a length.
/// Credentials are listed separately, by their own endpoint, and even there
/// only their metadata is returned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: UserId,
    pub username: Username,
    pub kind: UserKind,
    pub source: UserSource,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,

    /// Whether this identity currently has administrative access, whether that
    /// came from the access-control rules or from
    /// [`User::admin_override`].
    #[serde(default)]
    pub is_admin: bool,

    /// An administrator's explicit decision about administrative access, which
    /// wins over the access-control rules.
    ///
    /// Absent when nobody has overridden anything, which is the normal case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_override: Option<bool>,

    /// Whether the account is switched off. Disabling revokes the account's
    /// certificates and tokens as well as refusing new sign-ins.
    #[serde(default)]
    pub disabled: bool,

    pub created_at: DateTime<Utc>,

    /// The last time we saw this identity: a sign-in, an enrolment, or a client
    /// connecting to the stream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<DateTime<Utc>>,
}

impl User {
    /// What to call this identity in the UI.
    pub fn display(&self) -> &str {
        self.display_name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| self.username.as_str())
    }
}

/// The changes an administrator may make to an identity.
///
/// Every field is optional and means "leave this alone" when absent. To clear
/// [`UserPatch::display_name`], send an empty string rather than `null`, so
/// that the difference between "unchanged" and "cleared" does not depend on
/// telling an absent field from a null one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UserPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// Sets [`User::admin_override`], granting or refusing administrative
    /// access regardless of what the access-control rules say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_admin: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

impl UserPatch {
    /// Whether this patch would change anything.
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none() && self.is_admin.is_none() && self.disabled.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_user() -> User {
        User {
            id: UserId::new(1),
            username: Username::parse("alice").unwrap(),
            kind: UserKind::Person,
            source: UserSource::Oidc,
            display_name: Some("Alice Smith".into()),
            email: Some("alice@example.com".into()),
            is_admin: true,
            admin_override: Some(true),
            disabled: false,
            created_at: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            last_seen_at: Some("2026-09-18T13:00:00.250Z".parse().unwrap()),
        }
    }

    #[test]
    fn a_user_round_trips_through_serde() {
        let user = a_user();
        let json = serde_json::to_string(&user).unwrap();

        assert_eq!(serde_json::from_str::<User>(&json).unwrap(), user);
    }

    #[test]
    fn a_user_never_carries_a_credential() {
        // The guard is the type: there is nowhere on a user to put one. This
        // test exists so that adding such a field is a deliberate act that
        // breaks a test named after the reason not to.
        let serde_json::Value::Object(rendered) = serde_json::to_value(a_user()).unwrap() else {
            panic!("a user should serialise to an object");
        };

        let mut fields: Vec<&str> = rendered.keys().map(String::as_str).collect();
        fields.sort();

        assert_eq!(
            fields,
            vec![
                "admin_override",
                "created_at",
                "disabled",
                "display_name",
                "email",
                "id",
                "is_admin",
                "kind",
                "last_seen_at",
                "source",
                "username",
            ],
            "a field was added to the user DTO; check it cannot carry a credential"
        );
    }

    #[test]
    fn a_minimal_user_omits_everything_it_does_not_know() {
        let user: User = serde_json::from_value(serde_json::json!({
            "id": 2,
            "username": "bob",
            "kind": "person",
            "source": "local",
            "created_at": "2026-09-18T12:00:00.500Z",
        }))
        .unwrap();

        assert!(!user.is_admin);
        assert!(!user.disabled);
        assert_eq!(user.admin_override, None);
        assert_eq!(user.display(), "bob");
    }

    #[test]
    fn a_display_name_that_is_only_spaces_falls_back_to_the_username() {
        let mut user = a_user();
        user.display_name = Some("   ".into());

        assert_eq!(user.display(), "alice");
    }

    #[test]
    fn kinds_and_sources_round_trip_through_their_wire_form() {
        for kind in UserKind::ALL.iter().copied() {
            let json = serde_json::to_string(&kind).unwrap();

            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(serde_json::from_str::<UserKind>(&json).unwrap(), kind);
            assert_eq!(UserKind::parse(kind.as_str()), Some(kind));
        }

        for source in UserSource::ALL.iter().copied() {
            let json = serde_json::to_string(&source).unwrap();

            assert_eq!(json, format!("\"{}\"", source.as_str()));
            assert_eq!(serde_json::from_str::<UserSource>(&json).unwrap(), source);
            assert_eq!(UserSource::parse(source.as_str()), Some(source));
        }

        assert!(!UserSource::Oidc.is_managed_here());
        assert!(UserSource::Local.is_managed_here());
    }

    #[test]
    fn an_empty_patch_serialises_to_an_empty_object() {
        let patch = UserPatch::default();

        assert!(patch.is_empty());
        assert_eq!(serde_json::to_string(&patch).unwrap(), "{}");

        let patch = UserPatch {
            disabled: Some(true),
            ..UserPatch::default()
        };

        assert!(!patch.is_empty());
        let json = serde_json::to_string(&patch).unwrap();
        assert_eq!(json, r#"{"disabled":true}"#);
        assert_eq!(serde_json::from_str::<UserPatch>(&json).unwrap(), patch);
    }
}

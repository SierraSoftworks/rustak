//! The secrets a person can mint, and what the server says about them
//! afterwards.
//!
//! Every credential here is stored as an argon2id hash, shown exactly once at
//! the moment it is minted, labelled so its owner can tell two of them apart,
//! audited, and revocable. There is no local password: a person signs in to the
//! admin UI with a passkey or through the identity provider, and the
//! credentials below exist only for the clients that cannot do either.

use core::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{CredentialId, Username};

/// What a credential may be used for.
///
/// The list is short on purpose, and each entry names the one thing it is
/// accepted for. A credential presented anywhere else is refused even when it
/// is otherwise valid, so widening what a secret can do is a change to this
/// enum rather than an accident of which endpoint was called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    /// A one-time, short-lived token that enrols one client.
    ///
    /// This is the recommended way to bring a device on: it goes into the QR
    /// code an ATAK user scans, it is spent the moment a certificate is issued,
    /// and it is worthless afterwards. Default lifetime is fifteen minutes.
    EnrollmentToken,

    /// A reusable, expiring password, for clients that can do nothing better.
    ///
    /// Opt-in and deliberately awkward: it is accepted only by the password
    /// grant on `/oauth/token` and by Basic authentication on the `Marti`
    /// enrolment endpoints, because CloudTAK needs both. It is never accepted
    /// for the admin UI, for the rest of the Marti API, or on the stream.
    /// Default lifetime is ninety days.
    ClientPassword,

    /// The bearer token a sidecar presents to the service control API.
    ///
    /// A sidecar's primary identity is its client certificate; this is what it
    /// registers and reports health with before it has one.
    ServiceToken,
}

impl CredentialKind {
    /// Every kind, in the order a reader is offered them.
    pub const ALL: &'static [Self] = &[
        Self::EnrollmentToken,
        Self::ClientPassword,
        Self::ServiceToken,
    ];

    /// The value carried on the wire and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::EnrollmentToken => "enrollment_token",
            Self::ClientPassword => "client_password",
            Self::ServiceToken => "service_token",
        }
    }

    /// A short phrase naming the kind for somebody reading the UI.
    pub fn label(&self) -> &'static str {
        match self {
            Self::EnrollmentToken => "Enrolment token",
            Self::ClientPassword => "Client password",
            Self::ServiceToken => "Service token",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|kind| kind.as_str() == value)
    }

    /// Whether the credential is spent the first time it works.
    pub fn is_single_use(&self) -> bool {
        matches!(self, Self::EnrollmentToken)
    }

    /// Whether the UI should warn that this is a compatibility credential
    /// rather than something to reach for.
    pub fn is_compatibility_only(&self) -> bool {
        matches!(self, Self::ClientPassword)
    }
}

/// A credential as described to its owner or to an administrator.
///
/// The secret is not here, and neither is its hash, its hint or its length. The
/// only moment a secret exists outside the client that will use it is the
/// [`CredentialCreated`] that minting returns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Credential {
    pub id: CredentialId,
    pub kind: CredentialKind,

    /// What its owner called it, so two of them can be told apart. Usually the
    /// name of the device it is for.
    pub label: String,

    /// Whose credential this is. Present when an administrator is listing
    /// somebody else's; absent when a person is listing their own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<Username>,

    pub created_at: DateTime<Utc>,

    /// Who minted it, which is not always its owner: an administrator can mint
    /// an enrolment token on somebody's behalf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Username>,

    /// When it stops working. Absent means it does not expire on its own, which
    /// is only ever true of a service token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    /// How many times it may be used in total.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,

    /// How many times it has been used.
    #[serde(default)]
    pub uses: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,

    /// When it was revoked. Revoking a credential also revokes the certificates
    /// that were issued with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Credential {
    /// Whether it has been revoked by hand.
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// Whether every use it was given has been taken.
    pub fn is_exhausted(&self) -> bool {
        self.max_uses.is_some_and(|max| self.uses >= max)
    }

    /// Whether it would still be accepted at `now`.
    pub fn is_usable_at(&self, now: DateTime<Utc>) -> bool {
        !self.is_revoked()
            && !self.is_exhausted()
            && self.expires_at.is_none_or(|expires_at| now < expires_at)
    }
}

/// A request to mint a credential.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CreateCredentialRequest {
    pub kind: CredentialKind,

    /// What to call it. Usually the name of the device it is for.
    pub label: String,

    /// Whose credential to mint. Absent means the caller's own; only an
    /// administrator may name somebody else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<Username>,

    /// How long it should last. Absent means the default for its kind, which is
    /// short for an enrolment token and ninety days for a client password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<u32>,

    /// How many times it may be used. Absent means the default for its kind,
    /// which is once for an enrolment token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_uses: Option<u32>,
}

/// What minting returns: the only time the secret exists outside the client
/// that will use it.
///
/// This is not stored, not logged and not audited — the audit entry names the
/// credential, never its secret. [`CredentialCreated`] redacts the secret in
/// its `Debug` rendering so that a stray `{:?}` in a handler cannot put it in a
/// log line.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialCreated {
    pub credential: Credential,

    /// The secret, shown once.
    pub secret: String,

    /// For an enrolment token, the `tak://` URL that an ATAK user scans as a QR
    /// code. It carries the secret, so it is as sensitive as the secret is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enroll_url: Option<String>,
}

impl fmt::Debug for CredentialCreated {
    /// Redacts the secret and the URL that embeds it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialCreated")
            .field("credential", &self.credential)
            .field("secret", &"***")
            .field("enroll_url", &self.enroll_url.as_ref().map(|_| "***"))
            .finish()
    }
}

/// The pieces an enrolment QR code is composed from, without the secret.
///
/// ATAK reads `tak://com.atakmap.app/enroll?host=<host>&username=<user>&token=
/// <token>`, and the token is the credential's secret — which exists outside
/// the server exactly once, in the [`CredentialCreated`] that minting returned.
/// So this endpoint hands back everything *except* the token, and the UI
/// composes the URL from whichever mint response it is still holding. A server
/// that could re-emit the URL would be a server that stored the secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollTemplate {
    pub credential: CredentialId,

    /// The host an enrolling client is pointed at, which is the server's
    /// canonical domain rather than whatever `Host` header the caller sent.
    pub host: String,

    pub username: Username,

    /// The URL with `{token}` where the secret goes, so the UI does not have to
    /// know the scheme.
    pub url_template: String,
}

/// The URL an ATAK client reads out of an enrolment QR code.
///
/// `{host}`, `{username}` and `{token}` are filled in; all three parameters are
/// required, and ATAK refuses the link without them.
pub const ENROLL_URL: &str =
    "tak://com.atakmap.app/enroll?host={host}&username={username}&token={token}";

#[cfg(test)]
mod tests {
    use super::*;

    fn a_credential() -> Credential {
        Credential {
            id: CredentialId::new(1),
            kind: CredentialKind::EnrollmentToken,
            label: "Alice's phone".into(),
            username: Some(Username::parse("alice").unwrap()),
            created_at: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            created_by: Some(Username::parse("alice").unwrap()),
            expires_at: Some("2026-09-18T12:15:00.500Z".parse().unwrap()),
            max_uses: Some(1),
            uses: 0,
            last_used_at: None,
            revoked_at: None,
        }
    }

    #[test]
    fn a_credential_round_trips_through_serde() {
        let credential = a_credential();
        let json = serde_json::to_string(&credential).unwrap();

        assert_eq!(
            serde_json::from_str::<Credential>(&json).unwrap(),
            credential
        );
    }

    #[test]
    fn a_credential_never_carries_its_secret() {
        // Not the secret, not its hash, not the hint the server looks it up by,
        // not its length. This test exists so that adding such a field breaks a
        // test named after the reason not to.
        let serde_json::Value::Object(rendered) = serde_json::to_value(a_credential()).unwrap()
        else {
            panic!("a credential should serialise to an object");
        };

        let mut fields: Vec<&str> = rendered.keys().map(String::as_str).collect();
        fields.sort();

        assert_eq!(
            fields,
            vec![
                "created_at",
                "created_by",
                "expires_at",
                "id",
                "kind",
                "label",
                "max_uses",
                "username",
                "uses",
            ],
            "a field was added to the credential DTO; check it cannot carry a secret"
        );
    }

    #[test]
    fn the_minted_secret_is_redacted_in_debug_output() {
        // A handler that logs its response, or an error that captures it, must
        // not be how a one-time secret reaches a log file.
        let created = CredentialCreated {
            credential: a_credential(),
            secret: "s3cr3t-token-value".into(),
            enroll_url: Some(
                "tak://com.atakmap.app/enroll?host=tak.example.com&username=alice&token=s3cr3t-token-value"
                    .into(),
            ),
        };

        let debug = format!("{created:?}");

        assert!(
            !debug.contains("s3cr3t"),
            "the secret leaked into Debug: {debug}"
        );
        assert!(debug.contains("***"));

        // It does still have to reach the browser once.
        let json = serde_json::to_string(&created).unwrap();
        assert!(json.contains("s3cr3t-token-value"));
        assert_eq!(
            serde_json::from_str::<CredentialCreated>(&json).unwrap(),
            created
        );
    }

    #[test]
    fn a_spent_or_expired_credential_is_no_longer_usable() {
        let now: DateTime<Utc> = "2026-09-18T12:05:00Z".parse().unwrap();
        let credential = a_credential();
        assert!(credential.is_usable_at(now));

        let spent = Credential {
            uses: 1,
            ..a_credential()
        };
        assert!(spent.is_exhausted());
        assert!(!spent.is_usable_at(now));

        let expired: DateTime<Utc> = "2026-09-18T13:00:00Z".parse().unwrap();
        assert!(!credential.is_usable_at(expired));

        let revoked = Credential {
            revoked_at: Some("2026-09-18T12:01:00Z".parse().unwrap()),
            ..a_credential()
        };
        assert!(revoked.is_revoked());
        assert!(!revoked.is_usable_at(now));
    }

    #[test]
    fn a_request_to_mint_leaves_the_defaults_to_the_server() {
        let request: CreateCredentialRequest = serde_json::from_value(serde_json::json!({
            "kind": "enrollment_token",
            "label": "Alice's phone",
        }))
        .unwrap();

        assert_eq!(request.expires_in_days, None);
        assert_eq!(request.max_uses, None);
        assert_eq!(request.username, None);
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"kind":"enrollment_token","label":"Alice's phone"}"#
        );
    }

    #[test]
    fn kinds_round_trip_through_their_wire_form() {
        for kind in CredentialKind::ALL.iter().copied() {
            let json = serde_json::to_string(&kind).unwrap();

            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(serde_json::from_str::<CredentialKind>(&json).unwrap(), kind);
            assert_eq!(CredentialKind::parse(kind.as_str()), Some(kind));
        }

        // The kinds this server deliberately does not have.
        assert_eq!(CredentialKind::parse("local_password"), None);
        assert_eq!(CredentialKind::parse("device_password"), None);

        assert!(CredentialKind::EnrollmentToken.is_single_use());
        assert!(!CredentialKind::ClientPassword.is_single_use());
        assert!(CredentialKind::ClientPassword.is_compatibility_only());
        assert!(!CredentialKind::EnrollmentToken.is_compatibility_only());
    }
    #[test]
    fn the_enrolment_template_carries_everything_except_the_secret() {
        // The secret exists outside the server once, in the mint response. A
        // template that could be turned into a working URL on its own would
        // mean the server had kept it.
        let template = EnrollTemplate {
            credential: CredentialId::new(1),
            host: "tak.example.com".into(),
            username: Username::parse("alice").unwrap(),
            url_template: ENROLL_URL
                .replace("{host}", "tak.example.com")
                .replace("{username}", "alice"),
        };

        let json = serde_json::to_string(&template).unwrap();
        assert_eq!(
            serde_json::from_str::<EnrollTemplate>(&json).unwrap(),
            template
        );
        assert!(template.url_template.contains("{token}"));
        assert!(
            template
                .url_template
                .starts_with("tak://com.atakmap.app/enroll?")
        );
    }
}

//! Signing in with a passkey.
//!
//! Passkeys are how somebody signs in to the admin UI when there is no identity
//! provider, and how the first administrator is bootstrapped. Because they
//! replace passwords entirely, there is no reusable human secret for this
//! server to store or for anyone to phish.
//!
//! # Why the ceremony payloads are opaque
//!
//! The two blobs below — the options the server sends and the credential the
//! browser returns — are WebAuthn's own structures. They are produced and
//! consumed by `webauthn-rs` on one side and by `navigator.credentials` on the
//! other, and neither end asks this crate to understand them. Modelling them
//! here would be a second, less careful transcription of a specification that
//! is already implemented on both sides, and every field added to WebAuthn
//! would be a field this crate had to learn. So they travel as
//! [`serde_json::Value`] and the parts we do own — which challenge this is, who
//! it is for, what the passkey is called — are typed.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{PasskeyId, Username};

/// A request to begin registering a new passkey.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasskeyRegistrationStart {
    /// What to call it, so somebody with a phone and a security key can tell
    /// which is which when it comes time to remove one.
    pub label: String,

    /// The short-lived token the first-run wizard hands out after it creates
    /// the first administrator.
    ///
    /// Absent for anybody who is already signed in, which is every other
    /// registration. It exists only because the first administrator has no way
    /// to authenticate yet — that is the passkey they are about to register.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registration_token: Option<String>,
}

/// A request to begin signing in with a passkey.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PasskeyLoginStart {
    /// Who is signing in.
    ///
    /// Absent for a discoverable credential, where the authenticator itself
    /// says which account it holds. Leaving it out is the better flow: naming
    /// an account before authenticating is what lets somebody ask the server
    /// which accounts exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<Username>,
}

/// What the server sends back to start either ceremony.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PasskeyChallenge {
    /// Identifies the challenge the server is holding for this ceremony.
    ///
    /// The state itself stays on the server, is good for one attempt, and
    /// expires in minutes; this is only the handle to it.
    pub challenge_id: String,

    /// The WebAuthn options, passed to `navigator.credentials` unchanged.
    pub options: serde_json::Value,
}

/// The browser's answer to a registration challenge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PasskeyRegistrationFinish {
    pub challenge_id: String,

    /// The `PublicKeyCredential` the browser produced, passed to `webauthn-rs`
    /// unchanged.
    pub credential: serde_json::Value,

    /// What to call the passkey, when the name was not settled at the start of
    /// the ceremony.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The browser's answer to a sign-in challenge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PasskeyLoginFinish {
    pub challenge_id: String,

    /// The `PublicKeyCredential` the browser produced, passed to `webauthn-rs`
    /// unchanged.
    pub credential: serde_json::Value,
}

/// A registered passkey, as listed in the UI.
///
/// What is stored for a passkey is a public key, so there is nothing secret to
/// withhold here — but there is also nothing useful to show beyond which one it
/// is and whether it is still being used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PasskeySummary {
    pub id: PasskeyId,

    /// What its owner called it.
    pub label: String,

    pub created_at: DateTime<Utc>,

    /// When it was last used to sign in. Absent for one that never has been,
    /// which is worth noticing before removing the only other one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registration_ceremony_round_trips_through_serde() {
        let start = PasskeyRegistrationStart {
            label: "Alice's phone".into(),
            registration_token: None,
        };
        let json = serde_json::to_string(&start).unwrap();
        assert_eq!(json, r#"{"label":"Alice's phone"}"#);
        assert_eq!(
            serde_json::from_str::<PasskeyRegistrationStart>(&json).unwrap(),
            start
        );

        let challenge = PasskeyChallenge {
            challenge_id: "0d4f1a6e".into(),
            options: serde_json::json!({
                "publicKey": { "challenge": "c2FsdA", "rp": { "id": "tak.example.com" } }
            }),
        };
        let json = serde_json::to_string(&challenge).unwrap();
        assert_eq!(
            serde_json::from_str::<PasskeyChallenge>(&json).unwrap(),
            challenge
        );

        let finish = PasskeyRegistrationFinish {
            challenge_id: "0d4f1a6e".into(),
            credential: serde_json::json!({ "id": "abc", "type": "public-key" }),
            label: Some("Alice's phone".into()),
        };
        let json = serde_json::to_string(&finish).unwrap();
        assert_eq!(
            serde_json::from_str::<PasskeyRegistrationFinish>(&json).unwrap(),
            finish
        );
    }

    #[test]
    fn the_ceremony_payload_survives_verbatim() {
        // `webauthn-rs` and the browser both need every field they put in this
        // object, including ones this crate has never heard of, so it has to
        // pass through untouched.
        let original = serde_json::json!({
            "id": "abc",
            "rawId": "abc",
            "type": "public-key",
            "response": { "clientDataJSON": "e30", "attestationObject": "o2M" },
            "clientExtensionResults": { "credProps": { "rk": true } },
            "somethingWebauthnAddedLater": [1, 2, 3],
        });

        let finish = PasskeyLoginFinish {
            challenge_id: "0d4f1a6e".into(),
            credential: original.clone(),
        };

        let json = serde_json::to_string(&finish).unwrap();
        let parsed: PasskeyLoginFinish = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.credential, original);
    }

    #[test]
    fn a_discoverable_sign_in_names_nobody() {
        // Naming an account before authenticating is how somebody finds out
        // which accounts exist, so the username has to be optional.
        let start = PasskeyLoginStart::default();

        assert_eq!(serde_json::to_string(&start).unwrap(), "{}");
        assert_eq!(
            serde_json::from_str::<PasskeyLoginStart>("{}").unwrap(),
            start
        );

        let named = PasskeyLoginStart {
            username: Some(Username::parse("alice").unwrap()),
        };
        let json = serde_json::to_string(&named).unwrap();
        assert_eq!(json, r#"{"username":"alice"}"#);
        assert_eq!(
            serde_json::from_str::<PasskeyLoginStart>(&json).unwrap(),
            named
        );
    }

    #[test]
    fn a_summary_round_trips_through_serde() {
        let summary = PasskeySummary {
            id: PasskeyId::new(1),
            label: "Alice's phone".into(),
            created_at: "2026-09-18T12:00:00.500Z".parse().unwrap(),
            last_used_at: None,
        };

        let json = serde_json::to_string(&summary).unwrap();
        assert_eq!(
            json,
            r#"{"id":1,"label":"Alice's phone","created_at":"2026-09-18T12:00:00.500Z"}"#
        );
        assert_eq!(
            serde_json::from_str::<PasskeySummary>(&json).unwrap(),
            summary
        );
    }
}

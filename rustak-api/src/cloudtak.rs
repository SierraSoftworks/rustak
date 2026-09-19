//! Onboarding CloudTAK: the one hand-over in which this server holds a
//! client's private key.
//!
//! CloudTAK's *Configure Server* page will not save without an administrator
//! client certificate uploaded as a `.p12`, **and** a username and password for
//! the same account — it validates the pair over mTLS, then runs the password
//! grant and enrols a certificate of its own, and the first account to get
//! through becomes CloudTAK's system administrator. Assembling that by hand
//! means an `openssl` signing request, the enrolment endpoint and a legacy
//! `pkcs12 -export`, because rustak issues certificates against a request the
//! device made and never sees the key.
//!
//! [`CloudTakOnboarding`] is the deliberate exception. The key is generated for
//! one download, sealed at rest, handed over once and deleted; everything else
//! about the certificate — the subject, the record, the revocation path — is
//! the ordinary one. The response is therefore the most sensitive body this API
//! emits, which is why both secrets on it redact themselves in [`fmt::Debug`]
//! and why nothing here is ever re-emitted: a second call mints a second
//! hand-over rather than repeating the first.

use core::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{CertificateId, CredentialId, Username};

/// What a minted CloudTAK client password is called when the caller says
/// nothing, so that it is recognisable in the credential list later.
pub const DEFAULT_CREDENTIAL_LABEL: &str = "CloudTAK";

/// The value recorded as a certificate's `issued_via`, which is also how the
/// certificate list tells a CloudTAK hand-over from an ordinary enrolment.
pub const ISSUED_VIA: &str = "cloudtak_onboarding";

/// How long a prepared bundle may sit waiting to be fetched.
///
/// The download is the next click after the response is rendered, so the window
/// exists for somebody reading the page rather than for anything a client
/// legitimately waits on — and a sealed private key at rest is exactly what
/// this feature is trying not to leave behind.
pub const BUNDLE_TTL_MINUTES: i64 = 10;

/// Which client password the hand-over should carry.
///
/// `"mint"` makes a new one; `{"existing": 12}` reuses one the account already
/// has, for an operator who has already given CloudTAK a password and only
/// needs the certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnboardingCredential {
    /// Mint a fresh client password and show it once.
    #[default]
    Mint,

    /// Use the client password with this identifier, which must belong to the
    /// account being onboarded and must still be live.
    Existing(CredentialId),
}

impl OnboardingCredential {
    /// The credential to reuse, where the caller named one.
    pub fn existing(&self) -> Option<CredentialId> {
        match self {
            Self::Mint => None,
            Self::Existing(id) => Some(*id),
        }
    }
}

/// The ports the three URLs should carry, where the deployment does not use the
/// ones this server binds.
///
/// A container published on other host ports is the common case: rustak listens
/// on `8089`/`8443`/`8446` inside and the operator forwards `28089`/`28443`/
/// `28446` outside, and CloudTAK has to be told the outside ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CloudTakPorts {
    /// The CoT stream port, which becomes `ssl://host:<port>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<u16>,

    /// The mutually authenticated Marti port, which becomes CloudTAK's `api`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marti: Option<u16>,

    /// The browser-facing port, which becomes CloudTAK's `webtak`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public: Option<u16>,
}

impl CloudTakPorts {
    /// Whether the caller overrode nothing, so the configured ports stand.
    pub fn is_empty(&self) -> bool {
        self.stream.is_none() && self.marti.is_none() && self.public.is_none()
    }
}

/// What `POST /api/v1/users/{username}/cloudtak-onboarding` carries.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CloudTakOnboardingRequest {
    /// Mint a client password, or reuse one of the account's.
    #[serde(default)]
    pub credential: OnboardingCredential,

    /// What to call a minted credential. Ignored when one is reused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// The host name the three URLs should use, where it is not the one this
    /// installation calls itself by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,

    /// Port overrides, each independent of the others.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ports: Option<CloudTakPorts>,
}

/// The three base URLs CloudTAK stores for one server.
///
/// It does not assume they share a host or a port, and neither does this: they
/// are composed separately so that a deployment publishing rustak on three
/// different ports is described exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudTakUrls {
    /// `ssl://host:8089` — the CoT stream.
    pub stream: String,

    /// `https://host:8443` — Marti over mTLS, which CloudTAK calls `api`.
    pub api: String,

    /// `https://host:8446` — the OAuth and enrolment surface, which CloudTAK
    /// calls `webtak` and reaches with full system-CA verification.
    pub webtak: String,
}

/// Everything CloudTAK's server setup needs, produced by one action.
///
/// The two secrets exist here and nowhere else: neither is stored in a form
/// this server can read back, and asking again produces a new hand-over rather
/// than the same one.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudTakOnboarding {
    /// The account CloudTAK will sign in as and become system administrator of.
    pub username: Username,

    /// The client password, shown once.
    ///
    /// Absent when the caller reused a credential the account already had: the
    /// server keeps only an argon2id hash of a client password, so there is
    /// nothing left to re-emit and the operator who minted it already has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,

    /// Where to fetch the PKCS#12 bundle, which works exactly once.
    pub p12_download_url: String,

    /// The passphrase that bundle is sealed with, shown once.
    pub p12_password: String,

    /// The three URLs to paste into CloudTAK.
    pub urls: CloudTakUrls,

    /// The certificate that was issued, so it can be listed and revoked like
    /// any other.
    pub certificate_id: CertificateId,

    /// The client password's row, so revoking it revokes the certificate too.
    pub credential_id: CredentialId,

    /// When the download stops working, whether or not it was used.
    pub expires_at: DateTime<Utc>,
}

impl fmt::Debug for CloudTakOnboarding {
    /// Redacts both secrets, so that a stray `{:?}` in a handler cannot put an
    /// administrator's password or a private key's passphrase in a log file.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CloudTakOnboarding")
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "***"))
            .field("p12_download_url", &self.p12_download_url)
            .field("p12_password", &"***")
            .field("urls", &self.urls)
            .field("certificate_id", &self.certificate_id)
            .field("credential_id", &self.credential_id)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn onboarding() -> CloudTakOnboarding {
        CloudTakOnboarding {
            username: Username::parse("ada").unwrap(),
            password: Some("TOP-SECRET-PASSWORD".to_string()),
            p12_download_url: "/api/v1/cloudtak-onboarding/abc.p12".to_string(),
            p12_password: "TOPSECRETPASSPHR".to_string(),
            urls: CloudTakUrls {
                stream: "ssl://tak.example:8089".to_string(),
                api: "https://tak.example:8443".to_string(),
                webtak: "https://tak.example:8446".to_string(),
            },
            certificate_id: CertificateId::new(7),
            credential_id: CredentialId::new(9),
            expires_at: Utc::now(),
        }
    }

    #[test]
    fn minting_is_the_word_mint_and_reuse_names_a_row() {
        assert_eq!(
            serde_json::to_string(&OnboardingCredential::Mint).unwrap(),
            r#""mint""#,
        );
        assert_eq!(
            serde_json::to_string(&OnboardingCredential::Existing(CredentialId::new(12))).unwrap(),
            r#"{"existing":12}"#,
        );
    }

    #[test]
    fn a_request_naming_only_the_credential_parses() {
        let parsed: CloudTakOnboardingRequest =
            serde_json::from_str(r#"{"credential":"mint"}"#).unwrap();

        assert_eq!(parsed.credential, OnboardingCredential::Mint);
        assert_eq!(parsed.credential.existing(), None);
        assert!(parsed.host.is_none());
        assert!(parsed.ports.is_none());
    }

    #[test]
    fn an_empty_request_means_mint_with_every_default() {
        let parsed: CloudTakOnboardingRequest = serde_json::from_str("{}").unwrap();

        assert_eq!(parsed, CloudTakOnboardingRequest::default());
        assert_eq!(parsed.credential, OnboardingCredential::Mint);
    }

    #[test]
    fn a_reused_credential_is_read_back_as_its_row() {
        let parsed: CloudTakOnboardingRequest =
            serde_json::from_str(r#"{"credential":{"existing":12}}"#).unwrap();

        assert_eq!(parsed.credential.existing(), Some(CredentialId::new(12)));
    }

    #[test]
    fn ports_may_be_overridden_one_at_a_time() {
        let parsed: CloudTakOnboardingRequest =
            serde_json::from_str(r#"{"ports":{"stream":28089}}"#).unwrap();
        let ports = parsed.ports.unwrap();

        assert_eq!(ports.stream, Some(28089));
        assert_eq!(ports.marti, None);
        assert!(!ports.is_empty());
        assert!(CloudTakPorts::default().is_empty());
    }

    #[test]
    fn neither_secret_appears_in_a_debug_rendering() {
        // The whole reason this type has a hand-written `Debug`: this body is
        // the most sensitive one the API emits, and a handler logging its
        // response must not be the way it escapes.
        let rendered = format!("{:?}", onboarding());

        assert!(!rendered.contains("TOP-SECRET-PASSWORD"), "{rendered}");
        assert!(!rendered.contains("TOPSECRETPASSPHR"), "{rendered}");
        assert!(rendered.contains("***"), "{rendered}");
    }

    #[test]
    fn a_reused_credential_leaves_the_password_out_rather_than_inventing_one() {
        let mut reused = onboarding();
        reused.password = None;

        let value = serde_json::to_value(&reused).unwrap();

        assert!(value.get("password").is_none(), "{value}");
        assert!(!format!("{reused:?}").contains("TOP-SECRET"));
    }

    #[test]
    fn the_response_carries_no_field_that_could_hold_key_material() {
        // Named after the reason: the private key is handed over through the
        // one-shot download and must never be reachable from the JSON body, so
        // adding a field for it breaks this test.
        let value = serde_json::to_value(onboarding()).unwrap();
        let mut fields: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();

        assert_eq!(
            fields,
            [
                "certificate_id",
                "credential_id",
                "expires_at",
                "p12_download_url",
                "p12_password",
                "password",
                "urls",
                "username",
            ],
        );
    }
}

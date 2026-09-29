//! Adding a service: one administrative action that produces everything a
//! sidecar deployment needs to paste in.
//!
//! Deploying a sidecar that has no orchestrator identity used to be three
//! separate actions — create the service account, mint its service token, mint
//! a one-time enrolment token — against an enrolment token that expires fifteen
//! minutes after it is minted. `POST /api/v1/service-onboarding` does all three
//! at once and answers with the two secrets, the `[service]` configuration
//! fragment and the environment lines, each of which is shown exactly once.
//!
//! # Why a service token is part of it
//!
//! The client certificate a sidecar enrols for authenticates the CoT stream and
//! the Marti API. It does **not** reach the control API: `/api/v1` is served
//! only by the public listener, which asks for no client certificate, so a
//! sidecar that registers, heartbeats or reads its configuration needs the
//! service token as well. The action mints one when the account holds none,
//! and leaves a live one alone unless it is asked, explicitly, to replace it.
//!
//! # What is not here
//!
//! The secrets are stored nowhere this crate could describe: the server keeps
//! argon2id hashes. [`ServiceOnboarding`] redacts both in [`fmt::Debug`], so a
//! logged response cannot carry one.

use core::fmt;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{CredentialId, ServiceName, Username};

/// The environment variable a sidecar reads its one-time enrolment token from.
pub const ENROLLMENT_TOKEN_ENV: &str = "RUSTAK_ENROLLMENT_TOKEN";

/// The environment variable the fragment's `[service] token` names.
pub const SERVICE_TOKEN_ENV: &str = "RUSTAK_SERVICE_TOKEN";

/// What `POST /api/v1/service-onboarding` carries.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ServiceOnboardingRequest {
    /// The service's name, which is what `[service] name` will say:
    /// lower-case letters, digits and hyphens.
    pub name: String,

    /// The account it signs in as, where that is not the service's own name
    /// (`svc.adsb` for the service `adsb`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,

    /// Replace the service token the account already holds, revoking it.
    ///
    /// Off by default because revoking it stops whatever is running with it
    /// from reaching the control API; an operator re-deploying a sidecar that
    /// still holds its token needs only the fresh enrolment token.
    #[serde(default, skip_serializing_if = "core::ops::Not::not")]
    pub rotate_service_token: bool,
}

/// What happened to the account's service token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceTokenOutcome {
    /// The account held none, so one was minted and is in this response.
    Minted,

    /// The account already held a live one, which was left alone; nothing was
    /// minted and nothing was revoked.
    Kept,

    /// A new one was minted and the ones it replaces were revoked, because
    /// the request asked for exactly that.
    Rotated,
}

/// Everything one deployment of a sidecar needs, produced by one action.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceOnboarding {
    /// The service, as `[service] name` says it.
    pub name: ServiceName,

    /// The account it enrols and signs in as.
    pub account: Username,

    /// Whether this action created the account, or found it already there.
    pub account_created: bool,

    /// The one-time enrolment token, shown once.
    pub enrollment_token: String,

    /// The enrolment token's row, so it can be revoked before it is used.
    pub enrollment_token_id: CredentialId,

    /// When the enrolment token stops working whether or not it was used.
    pub enrollment_expires_at: DateTime<Utc>,

    /// What happened to the service token.
    pub service_token_outcome: ServiceTokenOutcome,

    /// The service token, shown once. Absent when a live one was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_token: Option<String>,

    /// The new service token's row, when one was minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_token_id: Option<CredentialId>,

    /// The service tokens a rotation revoked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revoked_service_tokens: Vec<CredentialId>,

    /// The `[service]` section to put in the sidecar's configuration file. It
    /// names the service token by its environment variable, never by value.
    pub config_fragment: String,

    /// The environment lines that carry the secrets, with the expiry stated.
    pub environment: String,

    /// What the action did, in sentences an operator reads before pasting.
    pub notes: Vec<String>,
}

impl fmt::Debug for ServiceOnboarding {
    /// Redacts both secrets and the environment lines that carry them.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceOnboarding")
            .field("name", &self.name)
            .field("account", &self.account)
            .field("account_created", &self.account_created)
            .field("enrollment_token", &"***")
            .field("enrollment_token_id", &self.enrollment_token_id)
            .field("enrollment_expires_at", &self.enrollment_expires_at)
            .field("service_token_outcome", &self.service_token_outcome)
            .field("service_token", &self.service_token.as_ref().map(|_| "***"))
            .field("service_token_id", &self.service_token_id)
            .field("revoked_service_tokens", &self.revoked_service_tokens)
            .field("config_fragment", &self.config_fragment)
            .field("environment", &"***")
            .field("notes", &self.notes)
            .finish()
    }
}

/// The `[service]` section a deployment of `name` pastes in.
///
/// `account` is written only when it differs from the name, because the
/// sidecar's own default is the name. The token is an `${{ env.… }}`
/// expression so that the file — the part of a deployment that gets copied
/// around — never holds the secret.
pub fn config_fragment(name: &ServiceName, account: &Username) -> String {
    let mut fragment = format!("[service]\nname = \"{name}\"\n");

    if account.as_str() != name.as_str() {
        fragment.push_str(&format!("account = \"{account}\"\n"));
    }

    fragment.push_str(&format!(
        "token = \"${{{{ env.{SERVICE_TOKEN_ENV} }}}}\"\n\
         # The first start enrols and writes the certificate, key and truststore\n\
         # here; the default is the directory this file is in.\n\
         # pki_dir = \"/data\"\n"
    ));

    fragment
}

/// The environment lines carrying the secrets, each with what it is for.
///
/// `service_token` is [`None`] when a live one was kept: the server holds only
/// its hash, and the deployment already has it.
pub fn environment(
    enrollment_token: &str,
    expires_at: DateTime<Utc>,
    service_token: Option<&str>,
) -> String {
    let mut lines = format!(
        "# One-time: spent by the first start, and refused after {}.\n\
         {ENROLLMENT_TOKEN_ENV}={enrollment_token}\n",
        expires_at.to_rfc3339_opts(SecondsFormat::Secs, true),
    );

    match service_token {
        Some(token) => lines.push_str(&format!(
            "# The control API credential. Keep it; it is not shown again.\n\
             {SERVICE_TOKEN_ENV}={token}\n"
        )),
        None => lines.push_str(&format!(
            "# {SERVICE_TOKEN_ENV}: keep the one this deployment already has.\n"
        )),
    }

    lines
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    fn name(value: &str) -> ServiceName {
        ServiceName::parse(value).unwrap()
    }

    fn account(value: &str) -> Username {
        Username::parse(value).unwrap()
    }

    #[test]
    fn a_request_naming_only_the_service_parses_with_every_default() {
        let parsed: ServiceOnboardingRequest = serde_json::from_str(r#"{"name":"adsb"}"#).unwrap();

        assert_eq!(parsed.name, "adsb");
        assert_eq!(parsed.account, None);
        assert!(!parsed.rotate_service_token);
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            r#"{"name":"adsb"}"#,
            "the defaults are left off the wire",
        );
    }

    #[test]
    fn the_outcome_is_a_lower_case_word() {
        assert_eq!(
            serde_json::to_string(&ServiceTokenOutcome::Rotated).unwrap(),
            r#""rotated""#
        );
    }

    #[test]
    fn the_fragment_names_the_account_only_when_it_differs() {
        let same = config_fragment(&name("adsb"), &account("adsb"));
        assert!(same.starts_with("[service]\nname = \"adsb\"\n"), "{same}");
        assert!(!same.contains("account ="), "{same}");

        let other = config_fragment(&name("adsb"), &account("svc.adsb"));
        assert!(other.contains("account = \"svc.adsb\"\n"), "{other}");
    }

    #[test]
    fn the_fragment_names_the_token_by_its_variable_and_never_its_value() {
        let fragment = config_fragment(&name("adsb"), &account("svc.adsb"));

        assert!(
            fragment.contains(r#"token = "${{ env.RUSTAK_SERVICE_TOKEN }}""#),
            "{fragment}"
        );

        // `rustak-server`'s suite parses it with the sidecar's own loader; here
        // it is enough that the only settings are the three it means to make.
        let settings: Vec<&str> = fragment
            .lines()
            .filter(|line| line.contains(" = ") && !line.starts_with('#'))
            .collect();
        assert_eq!(settings.len(), 3, "{fragment}");
    }

    #[test]
    fn the_environment_states_the_expiry_and_each_secret_once() {
        let expires = Utc.with_ymd_and_hms(2026, 9, 29, 12, 15, 0).unwrap();
        let lines = environment("enrol-me", expires, Some("rsk_token"));

        assert!(lines.contains("2026-09-29T12:15:00Z"), "{lines}");
        assert!(
            lines.contains("\nRUSTAK_ENROLLMENT_TOKEN=enrol-me\n"),
            "{lines}"
        );
        assert!(
            lines.contains("\nRUSTAK_SERVICE_TOKEN=rsk_token\n"),
            "{lines}"
        );
        assert_eq!(lines.matches("enrol-me").count(), 1);
    }

    #[test]
    fn a_kept_service_token_is_named_and_not_invented() {
        let lines = environment("enrol-me", Utc::now(), None);

        assert!(!lines.contains("RUSTAK_SERVICE_TOKEN="), "{lines}");
        assert!(lines.contains("# RUSTAK_SERVICE_TOKEN: keep"), "{lines}");
    }

    #[test]
    fn debug_redacts_every_secret() {
        let onboarding = ServiceOnboarding {
            name: name("adsb"),
            account: account("svc.adsb"),
            account_created: true,
            enrollment_token: "ENROLMENT-SECRET".to_string(),
            enrollment_token_id: CredentialId::new(1),
            enrollment_expires_at: Utc::now(),
            service_token_outcome: ServiceTokenOutcome::Minted,
            service_token: Some("rsk_SERVICE-SECRET".to_string()),
            service_token_id: Some(CredentialId::new(2)),
            revoked_service_tokens: Vec::new(),
            config_fragment: config_fragment(&name("adsb"), &account("svc.adsb")),
            environment: environment("ENROLMENT-SECRET", Utc::now(), Some("rsk_SERVICE-SECRET")),
            notes: vec!["Created the account.".to_string()],
        };

        let rendered = format!("{onboarding:?}");
        assert!(!rendered.contains("ENROLMENT-SECRET"), "{rendered}");
        assert!(!rendered.contains("SERVICE-SECRET"), "{rendered}");
        assert!(rendered.contains("svc.adsb"), "{rendered}");
    }
}

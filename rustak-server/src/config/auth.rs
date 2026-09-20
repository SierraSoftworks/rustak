//! `[auth]` — tokens, credentials and who is allowed in.
//!
//! Two kinds of setting live here. The first is our own token issuance: the
//! issuer and audience our RS256 JWTs carry, how long an access token and a
//! refresh token last, and the key that seals everything else at rest. The
//! second is credential policy: how long an enrollment token is good for,
//! whether the compatibility client passwords exist at all, and the rate limit
//! that applies to every endpoint accepting a secret.
//!
//! # Access is denied by default
//!
//! `user_acl` and `admin_acl` are [`filt_rs`] expressions over the request and
//! the identity provider's claims, and both default to `false`. An
//! installation with an identity provider configured and no ACL lets nobody in,
//! which is the right way round: the alternative is an installation that
//! accidentally admits an entire directory. The first-run wizard's
//! passkey-registered admin is authorised by the `users.is_admin` column
//! instead, so a fresh installation is administrable without writing a filter.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use filt_rs::Filter;
use serde::{Deserialize, Serialize};

use super::{OAuthServerConfig, OidcConfig, WorkloadConfig};

/// What a redacted secret renders as in a `Debug` dump.
const REDACTED: &str = "<redacted>";

/// The setup token file name under `data_dir`.
const SETUP_TOKEN_NAME: &str = "setup-token";

/// The audience our own JWTs carry, and the one we require when verifying one.
fn default_audience() -> String {
    "rustak".to_string()
}

fn default_access_token_ttl() -> chrono::Duration {
    chrono::Duration::hours(1)
}

fn default_refresh_token_ttl() -> chrono::Duration {
    chrono::Duration::days(30)
}

fn default_enrollment_token_ttl() -> chrono::Duration {
    chrono::Duration::minutes(15)
}

fn default_client_password_ttl() -> chrono::Duration {
    chrono::Duration::days(90)
}

/// Ten minutes: ATAK fetches its enrolment profile within a second of the
/// certificate, and the margin is for a device that lost its link in between
/// rather than for anything that could be planned around.
fn default_enrollment_grace() -> chrono::Duration {
    chrono::Duration::minutes(10)
}

fn default_true() -> bool {
    true
}

/// The ACL used when none is configured: nobody.
fn deny_everybody() -> Filter {
    // Provably infallible: `false` is a literal in the filter grammar, and the
    // expression is a constant rather than anything an operator supplied.
    Filter::new("false").expect("the literal `false` filter is always valid")
}

/// The shared deny-everybody ACL, so that a request handler can hold a
/// `&Filter` without one being rebuilt per call.
static DENY: LazyLock<Filter> = LazyLock::new(deny_everybody);

/// `[auth]`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthConfig {
    /// The `iss` claim of the tokens we issue. Defaults to
    /// [`ServerConfig::base_url`](super::ServerConfig::base_url).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,

    /// The `aud` claim we issue and require.
    #[serde(default = "default_audience")]
    pub audience: String,

    /// How long an access token is valid.
    #[serde(
        default = "default_access_token_ttl",
        with = "rustak_core::config::duration::humane"
    )]
    pub access_token_ttl: chrono::Duration,

    /// How long a refresh token is valid. Refresh tokens rotate, and reuse of a
    /// consumed one revokes the whole family.
    #[serde(
        default = "default_refresh_token_ttl",
        with = "rustak_core::config::duration::humane"
    )]
    pub refresh_token_ttl: chrono::Duration,

    /// Who may sign in at all. Denies everybody when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_acl: Option<Filter>,

    /// Who is an administrator, in addition to anybody carrying the
    /// `users.is_admin` flag. Denies everybody when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admin_acl: Option<Filter>,

    /// Whether a new user joins the `__ANON__` group, the default channel every
    /// TAK client expects to be able to talk on.
    #[serde(default = "default_true")]
    pub anon_group_default: bool,

    /// How long a one-time enrollment token may be used before it expires.
    ///
    /// Short on purpose: the token is typed into a device or scanned from a QR
    /// code within a minute or two of being minted, and it is consumed by the
    /// certificate signing it authorises.
    #[serde(
        default = "default_enrollment_token_ttl",
        with = "rustak_core::config::duration::humane"
    )]
    pub enrollment_token_ttl: chrono::Duration,

    /// How long a **spent** enrolment token may still fetch the enrolment
    /// device profile.
    ///
    /// ATAK's enrolment is three calls with one token: `tls/config`,
    /// `signClient/v2` — which spends it — and then
    /// `GET /Marti/api/tls/profile/enrollment?clientUid=`, unconditionally and
    /// with the same credential (research 07 §1.5). Within this window that
    /// third call is answered, **only** for the two `/Marti/api/tls/profile`
    /// routes and **only** for the `clientUid` the token was spent by. It buys
    /// nothing else: no second certificate, no `/oauth/token`, no Marti API.
    ///
    /// Set it to `0s` to refuse the profile fetch outright, at the cost of the
    /// "Failed to get profile: Enrollment (401)" ATAK reports when it fails.
    #[serde(
        default = "default_enrollment_grace",
        with = "rustak_core::config::duration::humane"
    )]
    pub enrollment_grace: chrono::Duration,

    /// Whether long-lived client passwords may be minted at all.
    ///
    /// On by default purely for compatibility: CloudTAK authenticates with a
    /// username and password against `/oauth/token` and has no other option
    /// today. They are accepted only there and on `/Marti/api/tls/*`, never for
    /// the admin UI or the Marti API, and the UI labels them as a compatibility
    /// credential. An installation with no CloudTAK should turn this off.
    #[serde(default = "default_true")]
    pub client_passwords_enabled: bool,

    /// How long a client password lasts.
    ///
    /// There is no "never expires": a credential that can be replayed forever
    /// is the thing enrollment certificates exist to avoid.
    #[serde(
        default = "default_client_password_ttl",
        with = "rustak_core::config::duration::humane"
    )]
    pub client_password_ttl: chrono::Duration,

    /// Whether `/token/access` may hand a caller its own access token.
    ///
    /// CloudTAK uses it to obtain a token it can present to other services.
    #[serde(default = "default_true")]
    pub allow_access_token_retrieval: bool,

    /// Where the first-run setup token is written. Defaults to
    /// `<data_dir>/setup-token`.
    ///
    /// The file is created with mode 0600 on the first start of an
    /// installation with no administrator, and deleted once the wizard
    /// completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_token_file: Option<PathBuf>,

    /// The AES-256 key sealing stored secrets, as base64 or hexadecimal.
    ///
    /// Generated into a file beside the database when absent. Set it to keep
    /// the key in your own secret management, as
    /// `"${{ env.RUSTAK_SECRET_KEY }}"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_key: Option<String>,

    /// Keys previously used to seal secrets.
    ///
    /// Values are written with the current key and read with whichever key
    /// sealed them, so a rotated key stays listed until every record that used
    /// it has been rewritten.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub previous_secret_keys: Vec<String>,

    /// The rate limit applied to every endpoint that accepts a secret.
    #[serde(default)]
    pub rate_limit: RateLimitConfig,

    /// The clients our own `/oauth/authorize` will issue codes to.
    #[serde(default)]
    pub oauth: OAuthServerConfig,

    /// The identity provider to federate with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oidc: Option<OidcConfig>,

    /// The orchestrators whose workload identity is accepted as a credential.
    ///
    /// Empty by default, which switches the whole path off: a token nobody has
    /// registered an issuer for is not a credential here, whoever signed it.
    #[serde(default, skip_serializing_if = "WorkloadConfig::is_empty")]
    pub workload: WorkloadConfig,
}

impl Default for AuthConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            issuer: None,
            audience: default_audience(),
            access_token_ttl: default_access_token_ttl(),
            refresh_token_ttl: default_refresh_token_ttl(),
            user_acl: None,
            admin_acl: None,
            anon_group_default: true,
            enrollment_token_ttl: default_enrollment_token_ttl(),
            enrollment_grace: default_enrollment_grace(),
            client_passwords_enabled: true,
            client_password_ttl: default_client_password_ttl(),
            allow_access_token_retrieval: true,
            oauth: OAuthServerConfig::default(),
            setup_token_file: None,
            secret_key: None,
            previous_secret_keys: Vec::new(),
            rate_limit: RateLimitConfig::default(),
            oidc: None,
            workload: WorkloadConfig::default(),
        }
    }
}

impl fmt::Debug for AuthConfig {
    /// Written out because [`Filter`] has no `Debug`, and because this struct
    /// holds key material: a configuration dump in a log or a bug report must
    /// not carry the key that seals every stored secret.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthConfig")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("access_token_ttl", &self.access_token_ttl)
            .field("refresh_token_ttl", &self.refresh_token_ttl)
            .field("user_acl", &self.user_acl().to_string())
            .field("admin_acl", &self.admin_acl().to_string())
            .field("anon_group_default", &self.anon_group_default)
            .field("enrollment_token_ttl", &self.enrollment_token_ttl)
            .field("enrollment_grace", &self.enrollment_grace)
            .field("client_passwords_enabled", &self.client_passwords_enabled)
            .field("client_password_ttl", &self.client_password_ttl)
            .field(
                "allow_access_token_retrieval",
                &self.allow_access_token_retrieval,
            )
            .field("setup_token_file", &self.setup_token_file)
            .field("secret_key", &self.secret_key.as_ref().map(|_| REDACTED))
            .field("previous_secret_keys", &self.previous_secret_keys.len())
            .field("rate_limit", &self.rate_limit)
            .field("oauth", &self.oauth)
            .field("oidc", &self.oidc)
            .field("workload", &self.workload)
            .finish()
    }
}

impl AuthConfig {
    /// The filter deciding who may sign in, denying everybody when unset.
    pub fn user_acl(&self) -> &Filter {
        self.user_acl.as_ref().unwrap_or(&DENY)
    }

    /// The filter deciding who is an administrator, denying everybody when
    /// unset. The `users.is_admin` column grants it independently of this.
    pub fn admin_acl(&self) -> &Filter {
        self.admin_acl.as_ref().unwrap_or(&DENY)
    }

    /// Where the first-run setup token is written, resolved against the data
    /// directory.
    pub fn setup_token_file(&self, data_dir: &Path) -> PathBuf {
        match self.setup_token_file.as_deref() {
            Some(path) if path.is_absolute() => path.to_path_buf(),
            Some(path) => data_dir.join(path),
            None => data_dir.join(SETUP_TOKEN_NAME),
        }
    }
}

/// `[auth.rate_limit]` — the limiter on every endpoint that accepts a secret.
///
/// Keyed on the source address and the identity being attempted, so that one
/// client cannot lock another out by guessing at their username.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitConfig {
    /// Failures allowed within `window` before `lockout` applies.
    #[serde(default = "default_attempts")]
    pub attempts: u32,

    /// The window failures are counted over.
    #[serde(
        default = "default_window",
        with = "rustak_core::config::duration::humane"
    )]
    pub window: chrono::Duration,

    /// How long the key is refused once `attempts` is exceeded.
    #[serde(
        default = "default_lockout",
        with = "rustak_core::config::duration::humane"
    )]
    pub lockout: chrono::Duration,
}

fn default_attempts() -> u32 {
    10
}

fn default_window() -> chrono::Duration {
    chrono::Duration::minutes(1)
}

fn default_lockout() -> chrono::Duration {
    chrono::Duration::minutes(15)
}

impl Default for RateLimitConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            attempts: default_attempts(),
            window: default_window(),
            lockout: default_lockout(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        let parsed: AuthConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, AuthConfig::default());
        assert_eq!(parsed.audience, "rustak");
        assert_eq!(parsed.access_token_ttl, chrono::Duration::hours(1));
        assert_eq!(parsed.refresh_token_ttl, chrono::Duration::days(30));
        assert_eq!(parsed.enrollment_token_ttl, chrono::Duration::minutes(15));
        assert_eq!(parsed.enrollment_grace, chrono::Duration::minutes(10));
        assert_eq!(parsed.client_password_ttl, chrono::Duration::days(90));
        assert!(parsed.client_passwords_enabled);
        assert!(parsed.anon_group_default);
        assert!(parsed.allow_access_token_retrieval);
    }

    #[test]
    fn access_is_denied_when_no_acl_is_configured() {
        // The whole point of the `Option`: an installation that configures an
        // identity provider and forgets the ACL admits nobody, rather than
        // admitting the provider's entire directory.
        let config = AuthConfig::default();

        assert_eq!(config.user_acl().to_string(), "false");
        assert_eq!(config.admin_acl().to_string(), "false");
    }

    #[test]
    fn an_acl_is_parsed_when_the_file_is_loaded_not_when_a_request_arrives() {
        let parsed: AuthConfig = toml::from_str(
            r#"
            user_acl = 'true'
            admin_acl = 'claims.groups contains "tak-admins"'
            "#,
        )
        .unwrap();

        assert_eq!(parsed.user_acl().to_string(), "true");
        assert_eq!(
            parsed.admin_acl().to_string(),
            r#"claims.groups contains "tak-admins""#
        );
    }

    #[test]
    fn a_malformed_acl_is_refused_at_load_time() {
        // A filter that only fails when somebody tries to sign in is a filter
        // that fails at three in the morning.
        let Err(err) = toml::from_str::<AuthConfig>("admin_acl = 'claims.groups contains'") else {
            panic!("a malformed filter should be refused");
        };

        assert!(!err.to_string().is_empty(), "{err}");
    }

    #[test]
    fn the_rate_limit_has_a_default_an_installation_can_live_with() {
        let parsed: AuthConfig = toml::from_str("").unwrap();

        assert_eq!(parsed.rate_limit.attempts, 10);
        assert_eq!(parsed.rate_limit.window, chrono::Duration::minutes(1));
        assert_eq!(parsed.rate_limit.lockout, chrono::Duration::minutes(15));
    }

    #[test]
    fn the_rate_limit_is_an_inline_table_as_the_example_file_writes_it() {
        let parsed: AuthConfig =
            toml::from_str(r#"rate_limit = { attempts = 3, window = "5m", lockout = "1h" }"#)
                .unwrap();

        assert_eq!(parsed.rate_limit.attempts, 3);
        assert_eq!(parsed.rate_limit.window, chrono::Duration::minutes(5));
        assert_eq!(parsed.rate_limit.lockout, chrono::Duration::hours(1));
    }

    #[test]
    fn the_setup_token_defaults_to_a_file_beside_the_database() {
        let config = AuthConfig::default();

        assert_eq!(
            config.setup_token_file(Path::new("/var/lib/rustak")),
            PathBuf::from("/var/lib/rustak/setup-token")
        );
    }

    #[test]
    fn the_secret_key_never_appears_in_a_debug_dump() {
        // Configuration gets printed into logs and bug reports; the key that
        // seals every stored secret must not travel with it.
        let config = AuthConfig {
            secret_key: Some("aGVsbG8gd29ybGQgdGhpcyBpcyBhIGtleQ==".to_string()),
            ..AuthConfig::default()
        };

        let rendered = format!("{config:?}");

        assert!(!rendered.contains("aGVsbG8"), "{rendered}");
        assert!(rendered.contains(REDACTED), "{rendered}");
    }

    #[test]
    fn a_debug_dump_still_shows_the_acls_that_decide_access() {
        // `Filter` has no `Debug`, which is why this impl is hand-written; the
        // ACLs are the first thing anybody debugging an access problem asks
        // for, so they have to survive that.
        let config = AuthConfig {
            user_acl: Some(Filter::new("true").unwrap()),
            ..AuthConfig::default()
        };

        let rendered = format!("{config:?}");

        assert!(rendered.contains("user_acl: \"true\""), "{rendered}");
        assert!(rendered.contains("admin_acl: \"false\""), "{rendered}");
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<AuthConfig>("access_token_lifetime = \"1h\"") else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("access_token_lifetime"), "{err}");
    }
}

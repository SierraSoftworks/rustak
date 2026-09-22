//! The identity a sidecar's orchestrator already gave it.
//!
//! Under Nomad or Kubernetes a task is holding a short-lived, signed statement
//! of what it is before rustak has ever heard of it. This module is what finds
//! that token, reads it, and exchanges it — so a deployment holds **no rustak
//! secret at all**: no enrolment token to mint and hand over on the first
//! start, no service token to copy into a file that gets committed.
//!
//! # Where it looks, and in what order
//!
//! `[service] workload_identity` names the source outright. With nothing set,
//! three places are tried, in the order a deployment is likely to have one:
//!
//! | | Where | Who puts it there |
//! |---|---|---|
//! | 1 | the `NOMAD_TOKEN_rustak` environment variable | Nomad, for `identity { name = "rustak", env = true }` |
//! | 2 | `${NOMAD_SECRETS_DIR}/nomad_rustak.jwt` | Nomad, for `file = true` |
//! | 3 | `/var/run/secrets/tokens/rustak` | Kubernetes, by the convention a projected volume's `path` follows |
//!
//! Which one was found is logged at `info`, because "it did not find my token"
//! and "it found the wrong one" are the two ways a first start goes wrong and
//! they look identical from the outside.
//!
//! # It is re-read every single time
//!
//! Both orchestrators rotate the token — Nomad's client renews at about half
//! the TTL, the kubelet rewrites the projected file — so nothing here caches
//! the token itself. A copy held from start-up would work for an hour and then
//! stop, which is the worst shape a failure can have.
//!
//! What *is* cached is the rustak access token a token exchange produces, until
//! a minute before it expires; see [`AccessTokens`].

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rustak_core::prelude::*;

use crate::http;

/// The environment variable Nomad exposes a named `rustak` identity through.
///
/// The suffix is the identity's own name and is case sensitive, exactly as the
/// jobspec spells it: `identity { name = "rustak" }`.
pub const NOMAD_TOKEN_ENV: &str = "NOMAD_TOKEN_rustak";

/// Where Nomad puts a task's secrets, which the file form is relative to.
pub const NOMAD_SECRETS_DIR_ENV: &str = "NOMAD_SECRETS_DIR";

/// The file a named `rustak` identity is written to inside that directory.
pub const NOMAD_TOKEN_FILE: &str = "nomad_rustak.jwt";

/// Where a Kubernetes projected service-account token is mounted by
/// convention — the `path` a `serviceAccountToken` volume names.
pub const KUBERNETES_TOKEN_PATH: &str = "/var/run/secrets/tokens/rustak";

/// The RFC 7523 grant a token exchange asks for.
pub const JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// How long before an access token expires it is exchanged again.
const RENEW_BEFORE: chrono::Duration = chrono::Duration::seconds(60);

/// Advice for a source that names no token.
const ADVICE_MISSING: &[&str] = &[
    "Under Nomad, add an `identity { name = \"rustak\", aud = [\"rustak\"], ttl = \"1h\", env = true, file = true }` block to the task.",
    "Under Kubernetes, project a serviceAccountToken volume with `audience: rustak` and `path: rustak`.",
    "Or remove [service] workload_identity and supply an enrolment token instead.",
];

/// `[service] workload_identity` — where this sidecar's assertion comes from.
///
/// Exactly one of the two, and neither is guessed at: a deployment that names
/// its source has said so deliberately, and one that names none is
/// auto-detected by [`detect`].
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkloadIdentity {
    /// An environment variable holding the token, e.g. `NOMAD_TOKEN_rustak`.
    #[serde(default)]
    pub env: Option<String>,

    /// A file holding the token, e.g.
    /// `/var/run/secrets/tokens/rustak`.
    #[serde(default)]
    pub file: Option<PathBuf>,
}

/// Where this sidecar's assertion is read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// An environment variable, named.
    Env(String),
    /// A file, named.
    File(PathBuf),
}

impl std::fmt::Display for Source {
    /// What the start-up line calls it, and what an operator greps for.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Env(name) => write!(formatter, "{name}"),
            Self::File(path) => write!(formatter, "{}", path.display()),
        }
    }
}

impl Source {
    /// Reads the token, afresh, every time.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the variable is unset, the
    /// file cannot be read, or what is there is empty.
    pub fn read(&self) -> Result<Secret, Error> {
        let raw = match self {
            Self::Env(name) => std::env::var(name).map_err(|_| {
                human_errors::user(
                    format!("The environment variable '{name}' holds no workload identity."),
                    ADVICE_MISSING,
                )
            })?,
            Self::File(path) => std::fs::read_to_string(path).map_err(|err| {
                human_errors::user(
                    format!(
                        "Could not read the workload identity at '{}': {err}.",
                        path.display()
                    ),
                    ADVICE_MISSING,
                )
            })?,
        };

        // Both orchestrators write the token with no trailing newline, and one
        // that arrives with one is a token every JWT parser refuses for a
        // reason nobody can see.
        let trimmed = raw.trim();

        if trimmed.is_empty() {
            return Err(human_errors::user(
                format!("The workload identity at '{self}' is empty."),
                ADVICE_MISSING,
            ));
        }

        Ok(Secret::new(trimmed))
    }
}

/// Where this sidecar's assertion comes from, if it has one.
///
/// A configured source is always answered, even when the variable behind it is
/// not set: naming one is a deliberate act, and a deployment that meant to use
/// a workload identity should be told its variable is missing rather than
/// quietly fall back to something else. Auto-detection is the opposite — it
/// answers only what is really there.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when `[service] workload_identity`
/// names both an `env` and a `file`, or neither.
pub fn detect(configured: Option<&WorkloadIdentity>) -> Result<Option<Source>, Error> {
    let Some(configured) = configured else {
        return Ok(automatic());
    };

    match (&configured.env, &configured.file) {
        (Some(name), None) => Ok(Some(Source::Env(name.clone()))),
        (None, Some(path)) => Ok(Some(Source::File(path.clone()))),
        _ => Err(human_errors::user(
            "[service] workload_identity must name exactly one of `env` and `file`.",
            &[
                "Write `workload_identity = { env = \"NOMAD_TOKEN_rustak\" }` for Nomad's environment form.",
                "Write `workload_identity = { file = \"/var/run/secrets/tokens/rustak\" }` for a projected volume.",
                "Leave it out entirely and both are looked for, in that order.",
            ],
        )),
    }
}

/// The three places a token is looked for when nothing names one.
///
/// Reads no network and opens no file it has not found first, so `--check` can
/// report what a start would use without doing anything.
fn automatic() -> Option<Source> {
    probe(
        std::env::var_os(NOMAD_TOKEN_ENV).is_some(),
        std::env::var_os(NOMAD_SECRETS_DIR_ENV).map(PathBuf::from),
        Path::new(KUBERNETES_TOKEN_PATH),
    )
}

/// [`automatic`] over the environment it reads, so the order can be tested.
///
/// Setting an environment variable is `unsafe` under edition 2024 and this
/// workspace forbids `unsafe_code`, so a test cannot arrange the real thing —
/// and the order *is* the interesting part: a deployment that has both forms of
/// the Nomad identity (`env = true, file = true`, which is what the reference
/// jobspec writes) must land on the same one every time.
fn probe(nomad_env: bool, nomad_secrets_dir: Option<PathBuf>, projected: &Path) -> Option<Source> {
    if nomad_env {
        return Some(Source::Env(NOMAD_TOKEN_ENV.to_string()));
    }

    if let Some(directory) = nomad_secrets_dir {
        let path = directory.join(NOMAD_TOKEN_FILE);

        if path.exists() {
            return Some(Source::File(path));
        }
    }

    projected
        .exists()
        .then(|| Source::File(projected.to_path_buf()))
}

/// The `iss` a token claims, read **without** verifying anything.
///
/// This is the sidecar reading its own token to say where it came from, not a
/// server deciding whether to believe one. Answers [`None`] for anything we
/// cannot take apart, which the start-up line renders as "-".
pub fn unverified_issuer(token: &Secret) -> Option<String> {
    unverified_claim(token, "iss")
}

/// The `sub` a token claims, read **without** verifying anything.
///
/// For the identity line, which has to name the account this sidecar is
/// *actually* acting as rather than the one it asked to be: rustak puts the
/// username it resolved into the access token it issues, so this is the
/// server's own answer read back.
pub fn unverified_subject(token: &Secret) -> Option<String> {
    unverified_claim(token, "sub")
}

/// One claim of a JWT payload, decoded and **not** verified.
///
/// Nothing here is a security decision: a token we could not take apart simply
/// leaves a field unfilled, and a token that lied about its own `sub` would
/// have lied to a log line and nothing else.
fn unverified_claim(token: &Secret, name: &str) -> Option<String> {
    use base64::Engine as _;

    let payload = token.expose().split('.').nth(1)?;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;

    claims.get(name)?.as_str().map(str::to_owned)
}

/// The one line an operator greps for when a first start misbehaves.
///
/// Four credentials can start a sidecar and they fail in ways that look
/// identical from the outside, so exactly one line says which was used, who
/// signed it, and who rustak decided that made this process.
pub fn report_identity(source: &str, issuer: Option<&str>, account: &str) {
    tracing::info!(
        identity_source = source,
        issuer = issuer.unwrap_or("-"),
        account = account,
        "{}",
        identity_line(source, account),
    );
}

/// The identity line for a credential this sidecar *is*.
///
/// Kept apart from the logging so the wording can be asserted on directly: it
/// is the line an operator greps for, and it has been wrong before.
fn identity_line(source: &str, account: &str) -> String {
    format!("Identity: this sidecar is '{account}', from {source}.")
}

/// The identity line for the credential this sidecar authenticates *with*.
///
/// A separate sentence because it says a different thing. The first live
/// deployment rendered this one through [`report_identity`] and it came out as
/// "this sidecar is 'the control API', from NOMAD_TOKEN_rustak" — which names
/// the endpoint where the account belongs, and the endpoint is not an identity.
/// A workload identity is not what this sidecar *is*; it is what it presents at
/// `[server] control` in order to be its own account there.
fn control_identity_line(source: &str, account: &str) -> String {
    format!(
        "Identity: authenticating to the control API as '{account}' with the workload identity from {source}."
    )
}

/// Says, once, which account this sidecar is buying control-API tokens as.
///
/// `account` is the real account — the `sub` of the token the server issued,
/// failing that the configured one — and never a description of the endpoint.
fn report_control_identity(source: &str, issuer: Option<&str>, account: &str) {
    tracing::info!(
        identity_source = source,
        issuer = issuer.unwrap_or("-"),
        account = account,
        "{}",
        control_identity_line(source, account),
    );
}

/// What a token exchange answered.
#[derive(Debug, Deserialize)]
struct Granted {
    access_token: String,

    /// Seconds. Absent is treated as "expired already", which costs one extra
    /// exchange and never leaves a stale token in place.
    #[serde(default)]
    expires_in: Option<i64>,
}

/// The rustak access token a workload identity buys, kept until it is nearly
/// spent.
///
/// This is what replaces `[service] token` for the control API. The assertion
/// itself is re-read from its source on every exchange; only the access token
/// is held, and only until a minute before it expires.
#[derive(Debug)]
pub struct AccessTokens {
    source: Source,
    endpoint: String,
    http: reqwest::Client,
    held: Mutex<Option<Held>>,

    /// The account `[service]` says this sidecar is, for the identity line when
    /// the exchanged token cannot be taken apart to read a `sub` out of.
    account: Option<String>,

    /// Whether the identity line has been written.
    ///
    /// The token is exchanged again every hour and by two callers at start-up
    /// — the harness and the feed task — and the line is a start-up line, so it
    /// is written once and never again.
    reported: std::sync::atomic::AtomicBool,

    /// How many identity lines were written, for the test that asserts "once"
    /// — there is nothing else to read a log line back from.
    #[cfg(test)]
    reports: std::sync::atomic::AtomicUsize,

    /// Held for the duration of an exchange, so two callers asking at once buy
    /// one token rather than two.
    ///
    /// The harness and the feed task both ask the instant a sidecar starts, and
    /// both find nothing cached: without this that is two `/oauth/token`
    /// requests and two pairs of lines in the server's log, for one token.
    exchanging: tokio::sync::Mutex<()>,
}

/// An access token and when it stops being worth presenting.
#[derive(Debug, Clone)]
struct Held {
    token: Secret,
    renew_at: chrono::DateTime<chrono::Utc>,
}

impl AccessTokens {
    /// An exchanger against `endpoint`, reading its assertion from `source`.
    pub fn new(source: Source, endpoint: String, http: reqwest::Client) -> Self {
        Self {
            source,
            endpoint,
            http,
            held: Mutex::new(None),
            account: None,
            reported: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            reports: std::sync::atomic::AtomicUsize::new(0),
            exchanging: tokio::sync::Mutex::new(()),
        }
    }

    /// Names the account `[service]` says this sidecar is.
    ///
    /// Only the identity line uses it, and only when the exchanged token
    /// cannot be taken apart: the `sub` the server put in the token it issued
    /// is the better answer, because it is the account rustak *resolved*
    /// rather than the one the file asked for.
    #[must_use]
    pub fn for_account(mut self, account: impl Into<String>) -> Self {
        self.account = Some(account.into());
        self
    }

    /// Where the assertion comes from, for the start-up line.
    pub fn source(&self) -> &Source {
        &self.source
    }

    /// A live access token, exchanging for one when what is held is nearly
    /// spent.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the token cannot be read, the
    /// server cannot be reached, or the exchange is refused.
    pub async fn current(&self) -> Result<Secret, Error> {
        if let Some(held) = self.live() {
            return Ok(held);
        }

        // One exchange at a time. Nothing is awaited while the `held` lock is
        // taken, so this is the only thing either caller waits on.
        let _exchanging = self.exchanging.lock().await;

        // Whoever was ahead of us may have bought one while we waited, and a
        // sidecar needs a token rather than its own copy of one.
        if let Some(held) = self.live() {
            return Ok(held);
        }

        let assertion = self.source.read()?;
        let granted = self.exchange(&assertion).await?;
        let renew_at = chrono::Utc::now()
            + chrono::Duration::seconds(granted.expires_in.unwrap_or(0))
            - RENEW_BEFORE;
        let token = Secret::new(granted.access_token);

        if let Ok(mut held) = self.held.lock() {
            *held = Some(Held {
                token: token.clone(),
                renew_at,
            });
        }

        self.report_once(&assertion, &token);

        Ok(token)
    }

    /// Writes the identity line the first time a token comes back.
    ///
    /// The account is the `sub` of the access token — the username rustak
    /// resolved the workload identity to — falling back to what `[service]`
    /// configured and then to "-": a description of the endpoint is not an
    /// account, and printing one is what made this line unreadable.
    fn report_once(&self, assertion: &Secret, granted: &Secret) {
        use std::sync::atomic::Ordering;

        if self.reported.swap(true, Ordering::Relaxed) {
            return;
        }

        #[cfg(test)]
        self.reports.fetch_add(1, Ordering::Relaxed);

        let account = unverified_subject(granted)
            .or_else(|| self.account.clone())
            .unwrap_or_else(|| "-".to_string());

        report_control_identity(
            &self.source.to_string(),
            unverified_issuer(assertion).as_deref(),
            &account,
        );
    }

    /// Throws away what is held, so the next call exchanges again.
    ///
    /// Called when the server refuses the access token: a signing key rotated,
    /// or somebody revoked it. Waiting for the cached expiry would leave the
    /// sidecar unable to report for up to an hour.
    pub fn invalidate(&self) {
        if let Ok(mut held) = self.held.lock() {
            *held = None;
        }
    }

    /// The held token, if it is still worth presenting.
    fn live(&self) -> Option<Secret> {
        let held = self.held.lock().ok()?;
        let held = held.as_ref()?;

        (chrono::Utc::now() < held.renew_at).then(|| held.token.clone())
    }

    /// One `POST /oauth/token` with the `jwt-bearer` grant.
    async fn exchange(&self, assertion: &Secret) -> Result<Granted, Error> {
        let base = http::base_url(&self.endpoint, "control")?;
        let url = http::endpoint(&base, "/oauth/token")?;

        let response = self
            .http
            .post(url)
            .form(&[
                ("grant_type", JWT_BEARER_GRANT),
                ("assertion", assertion.expose()),
            ])
            .send()
            .await
            .map_err(|err| http::transport(err, "exchange this workload identity for a token"))?;

        let status = response.status();

        if !status.is_success() {
            return Err(refused(status));
        }

        response.json().await.map_err(|err| {
            human_errors::user(
                format!("The server's token answer was not one we can read ({err})."),
                &["Check that [server] control names a rustak server's public listener."],
            )
        })
    }
}

/// What each way of being refused means for whoever is holding the token.
fn refused(status: reqwest::StatusCode) -> Error {
    let advice: &[&str] = match status.as_u16() {
        400 => &[
            "The server would not accept this workload identity: check `[auth.workload]` on the server, and that the audience matches.",
            "Its own log says which check failed; this end is told only that it was refused.",
        ],
        429 => &["Too many attempts. Wait for the lockout to pass and try again."],
        503 => &["This installation cannot issue tokens right now."],
        _ => &["The status above is the server's own."],
    };

    human_errors::user(
        format!("The server refused this workload identity ({status})."),
        advice,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(text: &str) -> WorkloadIdentity {
        toml::from_str(text).expect("the fragment should parse")
    }

    #[test]
    fn a_configured_source_is_the_one_that_is_used() {
        assert_eq!(
            detect(Some(&configured("env = \"NOMAD_TOKEN_rustak\""))).unwrap(),
            Some(Source::Env("NOMAD_TOKEN_rustak".to_string())),
        );
        assert_eq!(
            detect(Some(&configured(
                "file = \"/var/run/secrets/tokens/rustak\""
            )))
            .unwrap(),
            Some(Source::File(PathBuf::from(
                "/var/run/secrets/tokens/rustak"
            ))),
        );
    }

    #[test]
    fn naming_both_or_neither_is_refused_rather_than_guessed_at() {
        for text in [
            "",
            "env = \"NOMAD_TOKEN_rustak\"\nfile = \"/var/run/secrets/tokens/rustak\"",
        ] {
            let err = detect(Some(&configured(text))).unwrap_err();

            assert!(err.is(human_errors::Kind::User), "{err}");
            assert!(err.to_string().contains("exactly one"), "{err}");
        }
    }

    #[test]
    fn the_environment_is_looked_at_before_the_file_nomad_also_writes() {
        // The reference jobspec sets `env = true, file = true`, so a deployment
        // usually has both and the answer must not depend on which one is
        // noticed first.
        let directory = tempfile::tempdir().unwrap();
        let secrets = directory.path().to_path_buf();
        std::fs::write(secrets.join(NOMAD_TOKEN_FILE), "a.b.c").unwrap();
        let absent = directory.path().join("nothing-is-mounted-here");

        assert_eq!(
            probe(true, Some(secrets.clone()), &absent),
            Some(Source::Env(NOMAD_TOKEN_ENV.to_string())),
        );
        assert_eq!(
            probe(false, Some(secrets.clone()), &absent),
            Some(Source::File(secrets.join(NOMAD_TOKEN_FILE))),
        );
    }

    #[test]
    fn the_projected_volume_is_the_last_place_looked_and_the_answer_when_there_is_none() {
        let directory = tempfile::tempdir().unwrap();
        let projected = directory.path().join("rustak");
        let empty_secrets = directory.path().join("secrets");
        std::fs::create_dir_all(&empty_secrets).unwrap();

        assert_eq!(probe(false, Some(empty_secrets.clone()), &projected), None);

        std::fs::write(&projected, "a.b.c").unwrap();

        assert_eq!(
            probe(false, Some(empty_secrets), &projected),
            Some(Source::File(projected.clone())),
        );
        assert_eq!(
            probe(false, None, &projected),
            Some(Source::File(projected)),
            "a Kubernetes pod has no NOMAD_SECRETS_DIR at all",
        );
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        assert!(toml::from_str::<WorkloadIdentity>("envvar = \"X\"").is_err());
    }

    #[test]
    fn a_token_is_read_from_a_file_and_trimmed() {
        // Nomad writes the file with no trailing newline and an editor adds
        // one; a JWT with a newline on the end is refused for a reason nobody
        // can see from the outside.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nomad_rustak.jwt");
        std::fs::write(&path, "  a.b.c\n").unwrap();

        let source = Source::File(path.clone());

        assert_eq!(source.read().unwrap().expose(), "a.b.c");
        assert_eq!(source.to_string(), path.display().to_string());
    }

    #[test]
    fn a_file_that_is_not_there_or_is_empty_is_refused_by_name() {
        let directory = tempfile::tempdir().unwrap();
        let absent = directory.path().join("nothing.jwt");
        let empty = directory.path().join("empty.jwt");
        std::fs::write(&empty, "\n\n").unwrap();

        for path in [absent, empty] {
            let err = Source::File(path.clone()).read().unwrap_err();

            assert!(err.is(human_errors::Kind::User), "{err}");
            assert!(
                err.to_string().contains(&path.display().to_string()),
                "the refusal has to name the file: {err}",
            );
        }
    }

    #[test]
    fn an_unset_variable_is_refused_by_the_name_of_the_variable() {
        let err = Source::Env("RUSTAK_WORKLOAD_NOT_SET".to_string())
            .read()
            .unwrap_err();

        assert!(err.to_string().contains("RUSTAK_WORKLOAD_NOT_SET"), "{err}");
    }

    #[test]
    fn a_source_renders_as_the_thing_an_operator_would_grep_for() {
        assert_eq!(
            Source::Env(NOMAD_TOKEN_ENV.to_string()).to_string(),
            "NOMAD_TOKEN_rustak",
        );
        assert_eq!(
            Source::File(PathBuf::from("/data/rustak.jwt")).to_string(),
            "/data/rustak.jwt",
        );
    }

    #[test]
    fn the_names_are_the_ones_the_orchestrators_actually_use() {
        // Copied by hand into a jobspec and a pod spec; a typo here is a
        // sidecar that never finds its token and says nothing useful about it.
        assert_eq!(NOMAD_TOKEN_ENV, "NOMAD_TOKEN_rustak");
        assert_eq!(NOMAD_SECRETS_DIR_ENV, "NOMAD_SECRETS_DIR");
        assert_eq!(NOMAD_TOKEN_FILE, "nomad_rustak.jwt");
        assert_eq!(KUBERNETES_TOKEN_PATH, "/var/run/secrets/tokens/rustak");
        assert_eq!(
            JWT_BEARER_GRANT,
            "urn:ietf:params:oauth:grant-type:jwt-bearer"
        );
    }

    #[test]
    fn a_sidecar_can_read_its_own_issuer_without_verifying_anything() {
        use base64::Engine as _;

        let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let token = Secret::new(format!(
            "{}.{}.not-a-signature",
            encoder.encode(br#"{"alg":"RS256","kid":"k1"}"#),
            encoder.encode(br#"{"iss":"https://nomad.example.com","sub":"x"}"#),
        ));

        assert_eq!(
            unverified_issuer(&token).as_deref(),
            Some("https://nomad.example.com"),
        );
        assert_eq!(unverified_issuer(&Secret::new("not-a-jwt")), None);
    }

    #[test]
    fn a_sidecar_can_read_the_account_a_token_was_issued_to() {
        use base64::Engine as _;

        let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let token = Secret::new(format!(
            "{}.{}.not-a-signature",
            encoder.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
            encoder.encode(br#"{"iss":"https://tak.example.com","sub":"svc.adsb"}"#),
        ));

        assert_eq!(unverified_subject(&token).as_deref(), Some("svc.adsb"));
        assert_eq!(unverified_subject(&Secret::new("not-a-jwt")), None);
    }

    #[test]
    fn the_identity_line_for_a_certificate_says_what_this_sidecar_is() {
        // Unchanged, and asserted so that fixing the other one cannot quietly
        // reword this one: it is the line an operator greps for.
        assert_eq!(
            identity_line("the certificate at '/data/adsb.pem'", "adsb"),
            "Identity: this sidecar is 'adsb', from the certificate at '/data/adsb.pem'.",
        );
    }

    #[test]
    fn the_identity_line_for_the_control_api_names_the_account_and_not_the_endpoint() {
        // The production finding, verbatim: "this sidecar is 'the control API',
        // from NOMAD_TOKEN_rustak" — which says that a sidecar is an endpoint.
        // It is not. It is an account, authenticating *to* that endpoint.
        let rendered = control_identity_line(NOMAD_TOKEN_ENV, "svc.adsb");

        assert_eq!(
            rendered,
            "Identity: authenticating to the control API as 'svc.adsb' with the workload identity from NOMAD_TOKEN_rustak.",
        );
        assert!(
            !rendered.contains("this sidecar is 'the control API'"),
            "{rendered}",
        );
    }

    /// A token exchange that answers `sub`, counting what it was asked.
    async fn exchanging() -> (wiremock::MockServer, tempfile::TempDir, PathBuf) {
        use base64::Engine as _;

        let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let granted = format!(
            "{}.{}.not-a-signature",
            encoder.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
            encoder.encode(br#"{"iss":"https://tak.example.com","sub":"svc.adsb"}"#),
        );

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/oauth/token"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": granted,
                    "token_type": "Bearer",
                    "expires_in": 3600,
                })),
            )
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, "a.b.c").unwrap();

        (server, directory, path)
    }

    #[tokio::test]
    async fn reopening_the_feed_does_not_buy_another_token() {
        // The feed asks for a token before every opening, because one held open
        // for hours outlives the token that opened it. What must *not* happen
        // is a `/oauth/token` per reopening: the feed task and the harness both
        // ask, repeatedly, and the cache is what keeps that to one request.
        let (server, _directory, path) = exchanging().await;
        let tokens = AccessTokens::new(Source::File(path), server.uri(), reqwest::Client::new());

        for _ in 0..5 {
            tokens.current().await.expect("a token");
        }

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "five openings, one exchange",
        );
    }

    #[tokio::test]
    async fn two_callers_asking_at_once_buy_one_token_between_them() {
        // The harness and the feed task both ask the instant a sidecar starts,
        // and both find nothing cached. Two exchanges for one token is two
        // `/oauth/token` requests and two pairs of lines in the server's log,
        // on every start, for nothing.
        let (server, _directory, path) = exchanging().await;
        let tokens = std::sync::Arc::new(AccessTokens::new(
            Source::File(path),
            server.uri(),
            reqwest::Client::new(),
        ));

        let asking: Vec<_> = (0..4)
            .map(|_| {
                let tokens = std::sync::Arc::clone(&tokens);

                tokio::spawn(
                    async move { tokens.current().await.map(|token| token.expose().len()) },
                )
            })
            .collect();

        for asked in asking {
            asked.await.expect("the task joins").expect("a token");
        }

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "four callers, one exchange",
        );
    }

    #[tokio::test]
    async fn the_identity_line_is_written_once_however_many_callers_ask() {
        // It was written twice on every start: once by the harness and once by
        // the feed task, which both find no cached token and both exchange. It
        // is a start-up line, so it is written once.
        use std::sync::atomic::Ordering;

        let (server, _directory, path) = exchanging().await;
        let tokens = AccessTokens::new(Source::File(path), server.uri(), reqwest::Client::new())
            .for_account("svc.adsb");

        tokens.current().await.expect("a token");
        tokens.invalidate();
        tokens.current().await.expect("another token");

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "the invalidation did buy a second token",
        );
        assert_eq!(
            tokens.reports.load(Ordering::Relaxed),
            1,
            "and it was still one identity line",
        );
    }

    #[tokio::test]
    async fn an_exchange_asks_for_the_jwt_bearer_grant_and_caches_what_comes_back() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/oauth/token"))
            .and(wiremock::matchers::body_string_contains(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer",
            ))
            .and(wiremock::matchers::body_string_contains("assertion=a.b.c"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": "rustak-access-token",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                })),
            )
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, "a.b.c").unwrap();

        let tokens = AccessTokens::new(Source::File(path), server.uri(), reqwest::Client::new());

        assert_eq!(
            tokens.current().await.unwrap().expose(),
            "rustak-access-token"
        );
        assert_eq!(
            tokens.current().await.unwrap().expose(),
            "rustak-access-token"
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "a token that is still live is not exchanged again",
        );

        tokens.invalidate();
        assert_eq!(
            tokens.current().await.unwrap().expose(),
            "rustak-access-token"
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "and one the server refused is exchanged again straight away",
        );
    }

    #[tokio::test]
    async fn a_token_that_expires_immediately_is_exchanged_on_every_use() {
        // The margin is what keeps a sidecar from presenting a token that
        // expires between the check and the request.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "short-lived", "expires_in": 30 }),
            ))
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, "a.b.c").unwrap();

        let tokens = AccessTokens::new(Source::File(path), server.uri(), reqwest::Client::new());

        tokens.current().await.unwrap();
        tokens.current().await.unwrap();

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "30 seconds is inside the one-minute renewal margin",
        );
    }

    #[tokio::test]
    async fn the_token_is_re_read_from_its_source_on_every_exchange() {
        // Both orchestrators rotate it, so a copy held from start-up would work
        // for an hour and then stop — the worst shape a failure can have.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::body_string_contains("assertion=second"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "from-the-second", "expires_in": 3600 }),
            ))
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, "first").unwrap();

        let tokens = AccessTokens::new(
            Source::File(path.clone()),
            server.uri(),
            reqwest::Client::new(),
        );

        // The first token matches no mock, so the exchange is refused.
        assert!(tokens.current().await.is_err());

        std::fs::write(&path, "second").unwrap();

        assert_eq!(tokens.current().await.unwrap().expose(), "from-the-second");
    }

    #[tokio::test]
    async fn a_refused_exchange_says_what_to_look_at() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "invalid_grant" })),
            )
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, "a.b.c").unwrap();

        let err = AccessTokens::new(Source::File(path), server.uri(), reqwest::Client::new())
            .current()
            .await
            .unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.to_string().contains("[auth.workload]"), "{err}");
    }
}

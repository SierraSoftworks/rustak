//! The identity a sidecar's orchestrator already gave it.
//!
//! Under Nomad or Kubernetes a task is holding a short-lived, signed statement
//! of what it is before rustak has ever heard of it. This module is what finds
//! that token and reads it — so a deployment holds **no rustak secret at all**:
//! no enrolment token to mint and hand over on the first start, no service
//! token to copy into a file that gets committed. [`AccessTokens`] is what
//! spends it.
//!
//! # Where it looks, and in what order
//!
//! `[service] workload_identity` names the source outright. With nothing set,
//! three places are tried, and **every file comes before the environment**:
//!
//! | | Where | Who puts it there |
//! |---|---|---|
//! | 1 | `${NOMAD_SECRETS_DIR}/nomad_rustak.jwt` | Nomad, for `identity { name = "rustak", file = true }` |
//! | 2 | `/var/run/secrets/tokens/rustak` | Kubernetes, by the convention a projected volume's `path` follows |
//! | 3 | the `NOMAD_TOKEN_rustak` environment variable | Nomad, for `env = true` — and only when there is no file |
//!
//! That order is the whole of this module's reason to exist in its current
//! shape. **Both orchestrators renew a workload identity by rewriting the
//! file**: Nomad's client re-signs at about half the TTL, the kubelet rewrites
//! the projected volume. The environment variable is a copy taken at process
//! start and it is never touched again, so a sidecar that prefers it is holding
//! a credential with an hour to live and no way to get another. The first live
//! deployment did exactly that and its control link died, for good, two hours
//! after every start — the second renewal of a one-hour token, presented an
//! hour after it expired, refused every five seconds until somebody noticed.
//!
//! An environment source is therefore still honoured when a deployment names
//! one, and [`warn_about_env`] says plainly what will happen.
//!
//! # It is re-read every single time
//!
//! Nothing here caches the token. A copy held from start-up would work for an
//! hour and then stop, which is the worst shape a failure can have — and
//! [`Assertion`] is how the holder notices that what it has just read is
//! already dead, rather than finding out from a server that will only say
//! "refused".

use std::path::{Path, PathBuf};

use chrono::{DateTime, SecondsFormat, Utc};
use rustak_core::prelude::*;

pub use super::assertion::{Assertion, unverified_issuer, unverified_subject};
pub use super::exchange::AccessTokens;

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

/// Advice for a source that names no token.
const ADVICE_MISSING: &[&str] = &[
    "Under Nomad, add an `identity { name = \"rustak\", aud = [\"rustak\"], ttl = \"1h\", file = true, env = false, change_mode = \"noop\" }` block to the task.",
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
    ///
    /// Honoured, and warned about: an environment variable cannot be renewed.
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
    /// An environment variable, named. Frozen at process start.
    Env(String),
    /// A file, named. Rewritten by the orchestrator on every renewal.
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
    /// Whether the orchestrator can replace what this source holds.
    ///
    /// A file: yes, and it does. An environment variable: never, whatever the
    /// jobspec's `change_mode` says — the process would have to be restarted.
    pub fn is_renewable(&self) -> bool {
        matches!(self, Self::File(_))
    }

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
/// Either way, a source the orchestrator cannot renew is warned about once.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when `[service] workload_identity`
/// names both an `env` and a `file`, or neither.
pub fn detect(configured: Option<&WorkloadIdentity>) -> Result<Option<Source>, Error> {
    let source = match configured {
        None => automatic(),
        Some(configured) => match (&configured.env, &configured.file) {
            (Some(name), None) => Some(Source::Env(name.clone())),
            (None, Some(path)) => Some(Source::File(path.clone())),
            _ => {
                return Err(human_errors::user(
                    "[service] workload_identity must name exactly one of `env` and `file`.",
                    &[
                        "Write `workload_identity = { file = \"/var/run/secrets/tokens/rustak\" }` for a projected volume or a Nomad secrets file.",
                        "Write `workload_identity = { env = \"NOMAD_TOKEN_rustak\" }` only if there is no file; an environment variable cannot be renewed.",
                        "Leave it out entirely and the files are looked for first, then the environment.",
                    ],
                ));
            }
        },
    };

    if let Some(Source::Env(name)) = &source {
        warn_about_env(name);
    }

    Ok(source)
}

/// Whether the environment warning has already been written this process.
///
/// The source is worked out twice on an ordinary start — once by the enrolment
/// and once by the harness — and a start-up warning said twice reads like two
/// problems.
static WARNED_ABOUT_ENV: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Says, once, that a credential from the environment cannot be renewed.
///
/// Not a refusal: a deployment that has only the environment form still works
/// for as long as its first token lasts, and refusing to start would be a worse
/// answer than starting and saying why it will stop.
pub fn warn_about_env(name: &str) {
    if WARNED_ABOUT_ENV.swap(true, std::sync::atomic::Ordering::Relaxed) {
        return;
    }

    tracing::warn!(identity_source = name, "{}", env_warning(name));
}

/// The warning's wording, kept apart from the logging so it can be asserted on.
fn env_warning(name: &str) -> String {
    format!(
        "This sidecar's workload identity comes from '{name}', an environment variable, which is frozen when the process starts. Both Nomad and Kubernetes renew an identity by rewriting its file, so this credential cannot be renewed and the control link will stop working when it expires. Set `env = false, file = true` in the Nomad identity block, or name the file with [service] workload_identity = {{ file = \"<NOMAD_SECRETS_DIR>/{NOMAD_TOKEN_FILE}\" }}.",
    )
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
/// jobspec used to write) must land on the **file**, because that is the one
/// Nomad rewrites when it renews.
fn probe(nomad_env: bool, nomad_secrets_dir: Option<PathBuf>, projected: &Path) -> Option<Source> {
    if let Some(directory) = nomad_secrets_dir {
        let path = directory.join(NOMAD_TOKEN_FILE);

        if path.exists() {
            return Some(Source::File(path));
        }
    }

    if projected.exists() {
        return Some(Source::File(projected.to_path_buf()));
    }

    nomad_env.then(|| Source::Env(NOMAD_TOKEN_ENV.to_string()))
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
///
/// It also names when the credential runs out and whether anything will replace
/// it, because those two facts are the difference between a sidecar that keeps
/// working and one that stops in an hour.
pub(crate) fn control_identity_line(
    source: &Source,
    account: &str,
    expires_at: Option<DateTime<Utc>>,
) -> String {
    format!(
        "Identity: authenticating to the control API as '{account}' with the workload identity from {source} ({}).",
        credential_note(source, expires_at),
    )
}

/// When the credential runs out, and whether anything will replace it.
fn credential_note(source: &Source, expires_at: Option<DateTime<Utc>>) -> String {
    let expiry = match expires_at {
        Some(at) => format!("expires {}", at.to_rfc3339_opts(SecondsFormat::Secs, true)),
        None => "no expiry this end can read".to_string(),
    };

    match source.is_renewable() {
        true => format!("{expiry}, renewed by the orchestrator"),
        false => format!("{expiry}, which an environment variable cannot have renewed"),
    }
}

/// Says, once, which account this sidecar is buying control-API tokens as.
///
/// `account` is the real account — the `sub` of the token the server issued,
/// failing that the configured one — and never a description of the endpoint.
pub(crate) fn report_control_identity(
    source: &Source,
    issuer: Option<&str>,
    account: &str,
    expires_at: Option<DateTime<Utc>>,
) {
    tracing::info!(
        identity_source = %source,
        issuer = issuer.unwrap_or("-"),
        account = account,
        renewable = source.is_renewable(),
        "{}",
        control_identity_line(source, account, expires_at),
    );
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
    fn the_file_nomad_rewrites_is_looked_at_before_the_environment_it_freezes() {
        // The production defect, in one assertion. The reference jobspec used
        // to set `env = true, file = true`, so a deployment has both — and the
        // file is the only one of the two that is ever renewed.
        let directory = tempfile::tempdir().unwrap();
        let secrets = directory.path().to_path_buf();
        std::fs::write(secrets.join(NOMAD_TOKEN_FILE), "a.b.c").unwrap();
        let absent = directory.path().join("nothing-is-mounted-here");

        assert_eq!(
            probe(true, Some(secrets.clone()), &absent),
            Some(Source::File(secrets.join(NOMAD_TOKEN_FILE))),
            "both present: the file wins, because Nomad renews it",
        );
        assert_eq!(
            probe(false, Some(secrets.clone()), &absent),
            Some(Source::File(secrets.join(NOMAD_TOKEN_FILE))),
        );
    }

    #[test]
    fn the_environment_is_the_last_place_looked_and_only_when_there_is_no_file() {
        let directory = tempfile::tempdir().unwrap();
        let projected = directory.path().join("rustak");
        let empty_secrets = directory.path().join("secrets");
        std::fs::create_dir_all(&empty_secrets).unwrap();

        assert_eq!(
            probe(true, Some(empty_secrets.clone()), &projected),
            Some(Source::Env(NOMAD_TOKEN_ENV.to_string())),
            "no file anywhere: the environment is better than nothing",
        );
        assert_eq!(probe(false, Some(empty_secrets.clone()), &projected), None);

        std::fs::write(&projected, "a.b.c").unwrap();

        assert_eq!(
            probe(true, Some(empty_secrets), &projected),
            Some(Source::File(projected.clone())),
            "a projected volume is a file, and a file is renewed",
        );
        assert_eq!(
            probe(false, None, &projected),
            Some(Source::File(projected)),
            "a Kubernetes pod has no NOMAD_SECRETS_DIR at all",
        );
    }

    #[test]
    fn only_a_file_is_a_source_the_orchestrator_can_renew() {
        assert!(Source::File(PathBuf::from("/secrets/nomad_rustak.jwt")).is_renewable());
        assert!(!Source::Env(NOMAD_TOKEN_ENV.to_string()).is_renewable());
    }

    #[test]
    fn a_deployment_that_names_the_environment_is_told_what_will_happen_to_it() {
        // The warning an operator reads at start-up, asserted on the renderer
        // rather than on a log capture.
        let warning = env_warning(NOMAD_TOKEN_ENV);

        assert!(warning.contains(NOMAD_TOKEN_ENV), "{warning}");
        assert!(warning.contains("cannot be renewed"), "{warning}");
        assert!(warning.contains("env = false, file = true"), "{warning}");
        assert!(warning.contains(NOMAD_TOKEN_FILE), "{warning}");
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
    fn the_identity_line_names_the_account_the_source_and_when_it_runs_out() {
        // The production finding, verbatim: "this sidecar is 'the control API',
        // from NOMAD_TOKEN_rustak" — which says that a sidecar is an endpoint.
        // It is not. It is an account, authenticating *to* that endpoint with a
        // credential that has an expiry an operator needs to see.
        let expires_at = DateTime::from_timestamp(1_790_054_367, 0);
        let rendered = control_identity_line(
            &Source::File(PathBuf::from("/secrets/nomad_rustak.jwt")),
            "adsb",
            expires_at,
        );

        assert_eq!(
            rendered,
            "Identity: authenticating to the control API as 'adsb' with the workload identity from /secrets/nomad_rustak.jwt (expires 2026-09-22T05:19:27Z, renewed by the orchestrator).",
        );
        assert!(
            !rendered.contains("this sidecar is 'the control API'"),
            "{rendered}",
        );
    }

    #[test]
    fn an_environment_credential_says_in_the_identity_line_that_it_is_a_dead_end() {
        let expires_at = DateTime::from_timestamp(1_790_054_367, 0);
        let rendered = control_identity_line(
            &Source::Env(NOMAD_TOKEN_ENV.to_string()),
            "adsb",
            expires_at,
        );

        assert!(rendered.contains("from NOMAD_TOKEN_rustak"), "{rendered}");
        assert!(
            rendered.contains("expires 2026-09-22T05:19:27Z"),
            "{rendered}"
        );
        assert!(
            rendered.contains("an environment variable cannot have renewed"),
            "{rendered}",
        );
    }

    #[test]
    fn a_credential_whose_expiry_we_cannot_read_says_so_rather_than_guessing() {
        let rendered =
            control_identity_line(&Source::File(PathBuf::from("/secrets/x.jwt")), "adsb", None);

        assert!(
            rendered.contains("no expiry this end can read"),
            "{rendered}"
        );
    }
}

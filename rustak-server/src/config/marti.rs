//! `[marti]` — the TAK-compatible HTTP surface.
//!
//! Four keys, and every one of them exists because a client outside this
//! project reads the answer and cannot be corrected afterwards.
//!
//! `public_host` is the name that goes into the URLs we hand back to devices —
//! a download link an EUD passes to a *peer*, which therefore has to resolve on
//! the peer's network rather than on ours. `upload_size_limit_mb` is the number
//! CloudTAK's setup wizard reads from `/files/api/config` before it will save a
//! connection at all. The remaining two decide what happens to material a
//! client sends us unprompted: browser preflights and crash reports.

use serde::{Deserialize, Serialize};

/// The upload ceiling, in megabytes, reported to clients.
///
/// 400 MB is what every TAK Server deployment reports by default, and CloudTAK
/// and ATAK both size their own chunking against it.
fn default_upload_size_limit_mb() -> u32 {
    400
}

/// Whether a crash report a client posts is kept.
fn default_store_error_logs() -> bool {
    true
}

/// How many crash reports are kept before the oldest is dropped.
fn default_error_log_retention() -> usize {
    200
}

/// `[marti]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MartiConfig {
    /// The host name written into URLs we hand to clients.
    ///
    /// Defaults to the `Host` header of the request being answered, which is
    /// right for a directly reachable server and wrong for one behind a NAT or
    /// inside a container: the URL an EUD gets back from a package upload is
    /// passed on to its *peers*, so it has to be a name they can resolve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_host: Option<String>,

    /// The upload ceiling reported by `GET /files/api/config`, in megabytes.
    ///
    /// CloudTAK's setup wizard refuses to save a server connection until this
    /// endpoint answers with an integer here, so it is the first thing a
    /// CloudTAK bring-up exercises.
    #[serde(default = "default_upload_size_limit_mb")]
    pub upload_size_limit_mb: u32,

    /// Whether `Access-Control-Allow-*: *` is added to Marti responses.
    ///
    /// Off by default: ATAK and CloudTAK's server are not browsers and never
    /// send an `Origin`, and the admin UI is same-origin. Turn it on only for a
    /// browser-based client served from somewhere else — it makes every Marti
    /// response readable by any page the operator's users happen to visit.
    #[serde(default)]
    pub allow_all_origins: bool,

    /// Whether crash reports posted to `/Marti/ErrorLog` are kept.
    ///
    /// They are a client's own stack traces, which are useful when diagnosing
    /// an EUD and are also unvetted text from a device. They are stored with
    /// the body truncated and the count capped; turning this off discards them
    /// and answers `200` anyway, because a client that cannot file a crash
    /// report retries.
    #[serde(default = "default_store_error_logs")]
    pub store_error_logs: bool,

    /// How many crash reports are kept before the oldest is dropped.
    #[serde(default = "default_error_log_retention")]
    pub error_log_retention: usize,
}

impl Default for MartiConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            public_host: None,
            upload_size_limit_mb: default_upload_size_limit_mb(),
            allow_all_origins: false,
            store_error_logs: default_store_error_logs(),
            error_log_retention: default_error_log_retention(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        let parsed: MartiConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, MartiConfig::default());
        assert_eq!(parsed.upload_size_limit_mb, 400);
        assert!(parsed.public_host.is_none());
        assert!(!parsed.allow_all_origins);
        assert!(parsed.store_error_logs);
        assert_eq!(parsed.error_log_retention, 200);
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        // `deny_unknown_fields` is what turns "the upload limit did not take
        // effect" into a start-up error naming the key.
        let Err(err) = toml::from_str::<MartiConfig>("upload_size_limit = 400") else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("upload_size_limit"), "{err}");
    }

    #[test]
    fn the_keys_an_operator_writes_are_the_keys_we_read() {
        let parsed: MartiConfig = toml::from_str(
            r#"
            public_host = "tak.example.com"
            upload_size_limit_mb = 100
            allow_all_origins = true
            store_error_logs = false
            error_log_retention = 10
            "#,
        )
        .unwrap();

        assert_eq!(parsed.public_host.as_deref(), Some("tak.example.com"));
        assert_eq!(parsed.upload_size_limit_mb, 100);
        assert!(parsed.allow_all_origins);
        assert!(!parsed.store_error_logs);
        assert_eq!(parsed.error_log_retention, 10);
    }
}

//! The installation-wide settings the first-run wizard writes and the admin UI
//! edits.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The settings that describe this installation to its clients.
///
/// Anything set in the configuration file wins over what is stored here: the
/// wizard exists so that a server can be brought up without hand-writing TOML,
/// not so that it can quietly disagree with a file an operator is managing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSettings {
    /// What this server calls itself to clients, in the Marti version and
    /// configuration responses and at the top of the admin UI.
    pub name: String,

    /// The public host names clients reach this server on.
    ///
    /// The first is canonical: it is the host that goes into enrolment QR
    /// codes, preference files and certificate subject alternative names.
    #[serde(default)]
    pub domains: Vec<String>,

    /// The externally reachable base URL, when it cannot be derived from
    /// [`ServerSettings::domains`] and the listener's port — behind a proxy
    /// that terminates on a different port, for instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// This installation's stable identifier, minted once at first start.
    ///
    /// TAK clients and federated peers use it to tell one server from another
    /// when the host name changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,

    /// When the first-run wizard was finished. While this is absent the setup
    /// routes are open; once it is set they answer 410 forever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup_completed_at: Option<DateTime<Utc>>,
}

impl ServerSettings {
    /// The canonical host name clients should be told to use.
    pub fn canonical_domain(&self) -> Option<&str> {
        self.domains.first().map(String::as_str)
    }

    /// Whether the first-run wizard has been completed.
    pub fn is_setup_complete(&self) -> bool {
        self.setup_completed_at.is_some()
    }
}

/// Where the public listener's certificate comes from.
///
/// The spellings are the ones `[web.public.tls] mode` uses, so that the admin
/// UI and the configuration file name the same thing the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsSource {
    /// This installation's own certificate authority issued it.
    Internal,
    /// It was read from disk.
    Files,
    /// It was ordered from an ACME authority.
    Acme,
    /// The listener serves plaintext.
    None,
}

/// How healthy that certificate is.
///
/// A certificate from the internal authority has one answer — it is being
/// served, or the server did not start. ACME and `files` have all four: both
/// bind the listener on a bootstrap certificate and fill it in afterwards, one
/// from an order and one from two files an agent writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TlsCertificateState {
    /// Nothing has been issued or read yet; the listener is serving a
    /// bootstrap certificate until the first order finishes, or until the
    /// files named in the configuration appear.
    Missing,
    /// Issued, and not yet due for renewal.
    Valid,
    /// Issued, and inside its renewal window — or already past its expiry.
    Expiring,
    /// The last order failed. [`TlsStatus::last_error`] says why.
    Failed,
}

/// What the public listener is presenting, and what happens to it next.
///
/// Administrative: the host names, the expiry and the authority's own error
/// messages are not something an installation should publish to everybody who
/// can sign in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TlsStatus {
    /// Where the certificate comes from.
    pub source: TlsSource,

    /// How healthy it is.
    pub state: TlsCertificateState,

    /// The names it covers, or — before the first order — the names it will be
    /// ordered for.
    #[serde(default)]
    pub domains: Vec<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_before: Option<DateTime<Utc>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<DateTime<Utc>>,

    /// When the renewal job will next try to replace it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renews_at: Option<DateTime<Utc>>,

    /// When the pair was last read off disk, for `files`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded_at: Option<DateTime<Utc>>,

    /// The chain `[web.public.tls] cert_file` names, for `files`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cert_file: Option<String>,

    /// The key `[web.public.tls] key_file` names, for `files`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_file: Option<String>,

    /// What the listener is doing about it, where `state` alone does not say
    /// — "waiting for the certificate files to appear", for instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,

    /// The ACME directory it was ordered from, as the configuration spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,

    /// How the last successful order was validated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,

    /// Consecutive failed orders since the last success.
    #[serde(default)]
    pub attempts: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<DateTime<Utc>>,

    /// What the authority said the last time an order failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl TlsStatus {
    /// A status for a source that has nothing to renew.
    pub fn fixed(source: TlsSource) -> Self {
        Self {
            source,
            state: match source {
                TlsSource::None => TlsCertificateState::Missing,
                _ => TlsCertificateState::Valid,
            },
            domains: Vec::new(),
            not_before: None,
            not_after: None,
            renews_at: None,
            loaded_at: None,
            cert_file: None,
            key_file: None,
            note: None,
            directory: None,
            challenge: None,
            attempts: 0,
            last_attempt_at: None,
            last_error: None,
        }
    }

    /// Whether an administrator should be shown a warning about this.
    ///
    /// The two sources that can sit in an unhealthy *steady state* are the two
    /// that fetch their certificate from somewhere else while the server runs.
    /// An ACME order can fail and be retried for days; a `files` listener
    /// binds with an internal certificate and waits for a pair that a sidecar
    /// may never write, or re-reads one it cannot use — both of which leave a
    /// running server presenting something no client will trust, with nothing
    /// but the log to say so.
    ///
    /// `internal` cannot: it is issued here, and a failure to issue it stops
    /// start-up rather than producing a status to read. `none` has no
    /// certificate to have an opinion about, and the console says so in far
    /// stronger terms than a banner.
    pub fn needs_attention(&self) -> bool {
        matches!(
            self.state,
            TlsCertificateState::Failed | TlsCertificateState::Missing
        ) && matches!(self.source, TlsSource::Acme | TlsSource::Files)
    }
}

/// The one setting an operator changes about Enterprise Sync.
///
/// It is the number CloudTAK's setup wizard reads from `/files/api/config`
/// before it will save a server connection, and the ceiling every upload path
/// enforces inside its own reader — so it is a *limit*, not a suggestion, and
/// changing it changes what clients are told as well as what they may send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSettings {
    /// The largest upload this server accepts, in megabytes.
    pub upload_size_limit_mb: u32,

    /// Whether the configuration file is what set it, in which case a `PUT`
    /// here is refused rather than silently overridden at the next start.
    #[serde(default)]
    pub from_config_file: bool,
}

/// What the TAK-compatible surface tells clients about itself.
///
/// Read-only: both values change how URLs handed to *peers* resolve and
/// whether every Marti response is readable by any page an operator's users
/// happen to visit, which are decisions for the file an operator deploys
/// rather than for a web form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MartiSettings {
    /// The host name written into the URLs handed back to clients, when an
    /// operator has pinned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_host: Option<String>,

    /// Whether `Access-Control-Allow-*: *` is added to Marti responses.
    pub allow_all_origins: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_through_serde() {
        let settings = ServerSettings {
            name: "rustak".into(),
            domains: vec!["tak.example.com".into(), "tak2.example.com".into()],
            base_url: Some("https://tak.example.com:8446".into()),
            node_id: Some("0d4f1a6e".into()),
            setup_completed_at: Some("2026-09-18T12:00:00.500Z".parse().unwrap()),
        };

        let json = serde_json::to_string(&settings).unwrap();
        assert_eq!(
            serde_json::from_str::<ServerSettings>(&json).unwrap(),
            settings
        );

        assert_eq!(settings.canonical_domain(), Some("tak.example.com"));
        assert!(settings.is_setup_complete());
    }

    #[test]
    fn a_freshly_installed_server_has_only_a_name() {
        // This is what the UI sees between the first start and the wizard, so
        // every field the wizard fills in has to be optional.
        let settings: ServerSettings = serde_json::from_value(serde_json::json!({
            "name": "rustak",
        }))
        .unwrap();

        assert!(settings.domains.is_empty());
        assert_eq!(settings.canonical_domain(), None);
        assert!(!settings.is_setup_complete());
        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            r#"{"name":"rustak","domains":[]}"#
        );
    }

    #[test]
    fn a_tls_status_round_trips_and_leaves_out_what_it_does_not_know() {
        let status = TlsStatus::fixed(TlsSource::Internal);

        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"source":"internal","state":"valid","domains":[],"attempts":0}"#,
        );
        assert_eq!(
            serde_json::from_str::<TlsStatus>(&serde_json::to_string(&status).unwrap()).unwrap(),
            status,
        );
    }

    #[test]
    fn a_files_status_carries_where_it_read_the_pair_and_what_it_is_waiting_for() {
        // The fields a `files` installation needs and ACME has no use for:
        // without them an operator whose sidecar has not written the pair yet
        // is shown "not issued" and nothing that says which paths are watched.
        let status = TlsStatus {
            cert_file: Some("/etc/rustak/fullchain.pem".to_string()),
            key_file: Some("/etc/rustak/privkey.pem".to_string()),
            note: Some("Waiting for the certificate files to appear.".to_string()),
            state: TlsCertificateState::Missing,
            ..TlsStatus::fixed(TlsSource::Files)
        };

        let json = serde_json::to_string(&status).unwrap();

        assert_eq!(serde_json::from_str::<TlsStatus>(&json).unwrap(), status);
        assert!(json.contains("fullchain.pem"), "{json}");
        assert!(
            !json.contains("loaded_at"),
            "a pair that has never been read says nothing about when it was: {json}",
        );
    }

    #[test]
    fn a_certificate_fetched_while_the_server_runs_is_the_one_worth_warning_about() {
        // An `internal` certificate is issued here, and a failure to issue one
        // stops start-up — so there is no unhealthy steady state to report.
        let mut internal = TlsStatus::fixed(TlsSource::Internal);
        internal.state = TlsCertificateState::Failed;
        assert!(!internal.needs_attention());

        let mut acme = TlsStatus::fixed(TlsSource::Acme);
        acme.state = TlsCertificateState::Failed;
        assert!(acme.needs_attention());

        acme.state = TlsCertificateState::Valid;
        assert!(!acme.needs_attention());

        // `files` binds with an internal certificate and waits when the pair
        // is not there (M2-13), so a listener that is serving something no
        // client will trust is an ordinary running state — and the only thing
        // that says so, before this, was the log.
        let mut waiting = TlsStatus::fixed(TlsSource::Files);
        waiting.state = TlsCertificateState::Missing;
        waiting.note = Some("Waiting for the certificate files to appear.".to_string());
        assert!(
            waiting.needs_attention(),
            "a pair that has not arrived leaves the listener presenting the wrong certificate",
        );

        waiting.state = TlsCertificateState::Failed;
        assert!(
            waiting.needs_attention(),
            "and a pair that arrived unusable is worse, not better",
        );

        waiting.state = TlsCertificateState::Valid;
        assert!(!waiting.needs_attention());

        // `none` has no certificate to have an opinion about; the console says
        // what that means in far stronger terms than a banner.
        assert!(!TlsStatus::fixed(TlsSource::None).needs_attention());
    }

    #[test]
    fn the_sources_are_spelled_the_way_the_configuration_file_spells_them() {
        for (source, written) in [
            (TlsSource::Internal, "internal"),
            (TlsSource::Files, "files"),
            (TlsSource::Acme, "acme"),
            (TlsSource::None, "none"),
        ] {
            assert_eq!(
                serde_json::to_string(&source).unwrap(),
                format!("\"{written}\"")
            );
        }
    }

    #[test]
    fn the_file_settings_round_trip_and_say_where_the_value_came_from() {
        let settings = FileSettings {
            upload_size_limit_mb: 400,
            from_config_file: true,
        };

        let json = serde_json::to_string(&settings).unwrap();

        assert_eq!(
            serde_json::from_str::<FileSettings>(&json).unwrap(),
            settings
        );
        assert!(
            json.contains("from_config_file"),
            "a page that cannot tell a pinned value from an editable one \
             offers an edit that will not stick: {json}",
        );
    }

    #[test]
    fn the_marti_settings_omit_a_host_nobody_pinned() {
        let settings = MartiSettings {
            public_host: None,
            allow_all_origins: false,
        };

        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            r#"{"allow_all_origins":false}"#,
        );
        assert_eq!(
            serde_json::from_str::<MartiSettings>(
                r#"{"public_host":"tak.example.com","allow_all_origins":true}"#
            )
            .unwrap()
            .public_host
            .as_deref(),
            Some("tak.example.com"),
        );
    }
}

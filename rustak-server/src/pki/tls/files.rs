//! `[web.public.tls] mode = "files"`: a chain and a key an operator puts on
//! disk, re-read while the server runs.
//!
//! # Why the files may not be there yet
//!
//! The deployment this exists for renders a certificate into the task's
//! filesystem from a sidecar — Tailscale, an ACME agent, a secrets agent —
//! which starts *alongside* rustak rather than before it. A listener that
//! refused to bind until the pair appeared would make every such deploy a
//! race, so a missing pair is a warning naming both paths and a listener bound
//! with this installation's own certificate, exactly as `mode = "acme"`
//! bootstraps. `require_files_at_start = true` asks for the older behaviour,
//! which is the right one when the files are baked into the image.
//!
//! # Why polling
//!
//! The reload job stats both files every `[web.public.tls] reload_interval`
//! and re-reads them only when the size, modification time or inode of either
//! has moved. inotify and kqueue would save two `stat` calls every thirty
//! seconds and cost a platform-specific watcher that has to be rearmed every
//! time a renewal replaces the directory a symlink points at — which is how
//! most agents write these files.
//!
//! # A new pair is proved before it is served
//!
//! Renewal writes two files, and there is a moment in between when the chain
//! is the new one and the key is still the old one. Every load therefore goes
//! through [`CertifiedKey::from_der`], which parses the chain and checks that
//! the key belongs to the leaf; a pair that does not agree is recorded,
//! reported by `GET /api/v1/settings/tls` and otherwise ignored, and the
//! listener keeps serving what it already had. The next stamp change — the
//! rest of the write landing — is retried, which is why the fingerprint of a
//! rejected pair is remembered too: the same bytes will fail the same way.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

use chrono::{DateTime, TimeZone as _, Utc};
use rustak_api::{TlsCertificateState, TlsSource, TlsStatus};
use rustls::sign::CertifiedKey;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};

use crate::prelude::*;

use super::HotSwapCertResolver;

/// What the status says while the listener is bound on the internal
/// certificate because the pair is not on disk yet.
pub const WAITING: &str = "The public listener is presenting a certificate from this \
                           installation's own authority while it waits for the certificate \
                           files to appear.";

/// Advice for a pair that could not be read.
const ADVICE: &[&str] = &[
    "Check that `[web.public.tls] cert_file` is the full chain, leaf first, in PEM form, and that `key_file` is its private key.",
    "rustak re-reads both files when they change, so fixing them does not need a restart.",
];

/// What a cheap `stat` says about one of the two files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    inode: u64,
}

impl Stamp {
    /// The stamp of a file, or [`None`] when it is not there (or not
    /// readable, which the next load will report properly).
    fn of(path: &Path) -> Option<Self> {
        let metadata = std::fs::metadata(path).ok()?;

        Some(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            inode: inode(&metadata),
        })
    }
}

/// The inode, where the platform has one.
///
/// It is what catches a renewal that writes a new file and renames it over the
/// old one within the same second, which `len` and `modified` alone can miss.
#[cfg(unix)]
fn inode(metadata: &std::fs::Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::ino(metadata)
}

#[cfg(not(unix))]
fn inode(_metadata: &std::fs::Metadata) -> u64 {
    0
}

/// What both files looked like when they were last read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    cert: Stamp,
    key: Stamp,
}

impl Fingerprint {
    /// The stamps of both files, or [`None`] when either is missing.
    fn of(cert_file: &Path, key_file: &Path) -> Option<Self> {
        Some(Self {
            cert: Stamp::of(cert_file)?,
            key: Stamp::of(key_file)?,
        })
    }
}

/// A chain and key read from disk, proved to go together.
pub struct Loaded {
    /// What the listener presents.
    pub certified: Arc<CertifiedKey>,

    /// What the two files looked like *before* they were read, so that a pair
    /// rewritten mid-read is picked up again at the next look rather than
    /// recorded as the one being served.
    fingerprint: Option<Fingerprint>,

    /// How many certificates the chain held.
    pub certificates: usize,

    pub not_before: Option<DateTime<Utc>>,
    pub not_after: Option<DateTime<Utc>>,

    /// When this pair was read off disk.
    pub loaded_at: DateTime<Utc>,
}

impl std::fmt::Debug for Loaded {
    /// Written out rather than derived, so that a private key cannot reach a
    /// log through a `{:?}` on something that happens to hold one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Loaded")
            .field("certificates", &self.certificates)
            .field("not_after", &self.not_after)
            .field("loaded_at", &self.loaded_at)
            .finish_non_exhaustive()
    }
}

/// What one look at the pair concluded.
#[derive(Debug)]
pub enum Outcome {
    /// Neither file has moved since the last look. The overwhelming majority.
    Unchanged,

    /// One or both are not on disk yet; the listener is serving its bootstrap
    /// certificate.
    Waiting,

    /// A new pair was read, proved and installed without a restart.
    Swapped { not_after: Option<DateTime<Utc>> },

    /// The pair on disk changed and is not usable. The previous one — or the
    /// bootstrap certificate — keeps serving.
    Rejected(Error),
}

/// What the last look found, and what is being served because of it.
#[derive(Debug, Default)]
struct Seen {
    /// The stamps the last attempt was made against, whether it succeeded or
    /// not, so that the same bytes are not re-read every interval.
    fingerprint: Option<Fingerprint>,
    loaded: Option<Loaded>,
    last_error: Option<String>,
    attempts: u32,
    last_attempt_at: Option<DateTime<Utc>>,
}

/// The pair on disk, the listener presenting it, and what happened last.
pub struct FilesCertificate {
    cert_file: PathBuf,
    key_file: PathBuf,
    resolver: Arc<HotSwapCertResolver>,
    seen: Mutex<Seen>,
}

impl FilesCertificate {
    /// What [`web::tls`](crate::web::tls) built at start-up: the paths, the
    /// listener's resolver, and the pair it managed to read — or the reason it
    /// could not.
    pub fn new(
        cert_file: PathBuf,
        key_file: PathBuf,
        resolver: Arc<HotSwapCertResolver>,
        loaded: Option<Loaded>,
        failure: Option<String>,
    ) -> Arc<Self> {
        let fingerprint = loaded
            .as_ref()
            .and_then(|pair| pair.fingerprint)
            .or_else(|| Fingerprint::of(&cert_file, &key_file));

        // A pair that is simply not on disk yet is not a failed attempt: the
        // listener is waiting for it, which `note` says and `state` reports as
        // `missing`. A pair that *is* there and cannot be served is a failure.
        let failure = failure.filter(|_| fingerprint.is_some());
        let attempted = loaded.is_some() || failure.is_some();

        Arc::new(Self {
            cert_file,
            key_file,
            resolver,
            seen: Mutex::new(Seen {
                fingerprint,
                attempts: u32::from(failure.is_some()),
                last_attempt_at: attempted.then(Utc::now),
                loaded,
                last_error: failure,
            }),
        })
    }

    /// Re-reads the pair if it has changed, and swaps it in if it is good.
    ///
    /// Cheap enough to call every thirty seconds: two `stat` calls, and
    /// nothing else at all unless one of them moved.
    pub fn reload(&self) -> Outcome {
        let Some(fingerprint) = Fingerprint::of(&self.cert_file, &self.key_file) else {
            return Outcome::Waiting;
        };

        let mut seen = self.seen();

        if seen.fingerprint == Some(fingerprint) {
            return Outcome::Unchanged;
        }

        seen.fingerprint = Some(fingerprint);
        seen.last_attempt_at = Some(Utc::now());

        match load(&self.cert_file, &self.key_file) {
            Ok(loaded) => {
                let not_after = loaded.not_after;

                // Installed before the state is updated, so that a status read
                // between the two says "the old one" rather than announcing a
                // certificate the listener is not presenting yet.
                self.resolver.install(Arc::clone(&loaded.certified));

                seen.loaded = Some(loaded);
                seen.last_error = None;
                seen.attempts = 0;

                Outcome::Swapped { not_after }
            }
            Err(err) => {
                seen.attempts = seen.attempts.saturating_add(1);
                seen.last_error = Some(err.description());

                Outcome::Rejected(err)
            }
        }
    }

    /// What `GET /api/v1/settings/tls` reports for this listener.
    pub fn status(&self, domains: &[String]) -> TlsStatus {
        let seen = self.seen();
        let loaded = seen.loaded.as_ref();
        let expired = loaded.is_some_and(|pair| pair.not_after.is_some_and(|at| at <= Utc::now()));

        TlsStatus {
            source: TlsSource::Files,
            state: match (loaded.is_some(), expired, seen.last_error.is_some()) {
                (true, true, _) => TlsCertificateState::Expiring,
                (true, false, _) => TlsCertificateState::Valid,
                (false, _, true) => TlsCertificateState::Failed,
                (false, _, false) => TlsCertificateState::Missing,
            },
            domains: domains.to_vec(),
            not_before: loaded.and_then(|pair| pair.not_before),
            not_after: loaded.and_then(|pair| pair.not_after),
            loaded_at: loaded.map(|pair| pair.loaded_at),
            cert_file: Some(self.cert_file.display().to_string()),
            key_file: Some(self.key_file.display().to_string()),
            note: (loaded.is_none() && seen.last_error.is_none()).then(|| WAITING.to_string()),
            attempts: seen.attempts,
            last_attempt_at: seen.last_attempt_at,
            last_error: seen.last_error.clone(),
            renews_at: None,
            directory: None,
            challenge: None,
        }
    }

    /// Whether this is the pair the configuration names.
    ///
    /// Always true in a running server, which builds one listener from one
    /// file. It is false in a test process that has built two, which is what
    /// keeps [`report`] from answering with somebody else's paths.
    fn covers(&self, tls: &crate::config::TlsConfig) -> bool {
        tls.cert_file.as_deref() == Some(self.cert_file.as_path())
            && tls.key_file.as_deref() == Some(self.key_file.as_path())
    }

    /// The state, taking a poisoned lock's contents rather than panicking: a
    /// panic in one reload must not stop the listener reporting what it is
    /// serving.
    fn seen(&self) -> MutexGuard<'_, Seen> {
        self.seen.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The public listener's pair, once it has one.
///
/// Process-wide for the same reason [`pki::acme`](crate::pki::acme)'s resolver
/// is: it is built where the listener is built and needed by a queue job
/// holding nothing but [`Services`], and there is exactly one public listener
/// in a process.
static CURRENT: RwLock<Option<Arc<FilesCertificate>>> = RwLock::new(None);

/// Publishes what [`web::tls`](crate::web::tls) built.
pub fn publish(files: Arc<FilesCertificate>) {
    if let Ok(mut held) = CURRENT.write() {
        *held = Some(files);
    }
}

/// The published pair, when it is the one this configuration names.
///
/// [`None`] when the listener is not in files mode — and, in a test process
/// that has built more than one, when the published pair belongs to another
/// listener.
pub fn for_config(config: &Config) -> Option<Arc<FilesCertificate>> {
    CURRENT
        .read()
        .ok()
        .and_then(|held| held.clone())
        .filter(|files| files.covers(&config.web.public.tls))
}

/// What the admin API reports for a `files` installation, whether or not a
/// listener was built in this process.
pub fn report(config: &Config) -> TlsStatus {
    let domains = config.server.domains.clone();

    let Some(files) = for_config(config) else {
        let tls = &config.web.public.tls;

        return TlsStatus {
            domains,
            state: TlsCertificateState::Missing,
            cert_file: tls
                .cert_file
                .as_ref()
                .map(|path| path.display().to_string()),
            key_file: tls.key_file.as_ref().map(|path| path.display().to_string()),
            note: Some(WAITING.to_string()),
            ..TlsStatus::fixed(TlsSource::Files)
        };
    };

    files.status(&domains)
}

/// Reads the pair, parses the chain, and checks that the key is the leaf's.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error naming the file at fault: this is an
/// operator's two paths, and every way it fails is something they can fix.
pub fn load(cert_file: &Path, key_file: &Path) -> Result<Loaded, Error> {
    // Taken first: a pair rewritten while we are reading it should be read
    // again at the next look rather than recorded as the one being served.
    let fingerprint = Fingerprint::of(cert_file, key_file);

    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(cert_file)
        .and_then(|iter| iter.collect())
        .map_err(|err| {
            human_errors::user(
                format!(
                    "We could not read the certificate chain at {}: {err}",
                    cert_file.display()
                ),
                ADVICE,
            )
        })?;

    let leaf = chain.first().cloned().ok_or_else(|| {
        human_errors::user(
            format!("{} contains no certificates.", cert_file.display()),
            ADVICE,
        )
    })?;

    let key = PrivateKeyDer::from_pem_file(key_file).map_err(|err| {
        human_errors::user(
            format!(
                "We could not read the private key at {}: {err}",
                key_file.display()
            ),
            ADVICE,
        )
    })?;

    let certificates = chain.len();
    let certified = CertifiedKey::from_der(
        chain,
        key,
        &rustls::crypto::aws_lc_rs::default_provider(),
    )
    .map_err(|err| {
        human_errors::user(
            format!(
                "The certificate chain at {} and the private key at {} do not go together: {err}",
                cert_file.display(),
                key_file.display()
            ),
            ADVICE,
        )
    })?;

    let (not_before, not_after) = validity(&leaf);

    Ok(Loaded {
        certified: Arc::new(certified),
        fingerprint,
        certificates,
        not_before,
        not_after,
        loaded_at: Utc::now(),
    })
}

/// The leaf's validity window, where it can be read.
///
/// Not an error when it cannot be: rustls has already accepted the chain, and
/// a status missing one date is better than a listener refusing a certificate
/// it could serve.
fn validity(leaf: &CertificateDer<'_>) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(leaf.as_ref()) else {
        return (None, None);
    };

    let at = |time: x509_parser::time::ASN1Time| Utc.timestamp_opt(time.timestamp(), 0).single();

    (
        at(parsed.validity().not_before),
        at(parsed.validity().not_after),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A self-signed pair on disk, valid for `days`, written as an operator's
    /// agent would write it.
    fn write_pair(directory: &Path, name: &str, days: i64) -> (PathBuf, PathBuf, Vec<u8>) {
        use chrono::Datelike as _;

        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let mut params = rcgen::CertificateParams::new(vec![name.to_string()]).unwrap();
        let now = Utc::now();
        let end = now + chrono::Duration::days(days);
        params.not_before = rcgen::date_time_ymd(now.year(), now.month() as u8, now.day() as u8);
        params.not_after = rcgen::date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
        let certificate = params.self_signed(&key).unwrap();

        let cert_file = directory.join("chain.pem");
        let key_file = directory.join("key.pem");
        std::fs::write(&cert_file, certificate.pem()).unwrap();
        std::fs::write(&key_file, key.serialize_pem()).unwrap();

        (cert_file, key_file, certificate.der().to_vec())
    }

    /// A listener holding `loaded`, with nothing published process-wide.
    fn listener(
        cert_file: &Path,
        key_file: &Path,
        loaded: Option<Loaded>,
    ) -> (Arc<FilesCertificate>, Arc<HotSwapCertResolver>) {
        let initial = loaded.as_ref().map(|pair| Arc::clone(&pair.certified));
        let resolver = HotSwapCertResolver::new(initial);
        let files = FilesCertificate::new(
            cert_file.to_path_buf(),
            key_file.to_path_buf(),
            Arc::clone(&resolver),
            loaded,
            None,
        );

        (files, resolver)
    }

    fn presented(resolver: &HotSwapCertResolver) -> Vec<u8> {
        resolver
            .current()
            .unwrap()
            .end_entity_cert()
            .unwrap()
            .as_ref()
            .to_vec()
    }

    #[test]
    fn a_pair_on_disk_is_read_with_its_validity_window() {
        let directory = tempfile::tempdir().unwrap();
        let (cert_file, key_file, der) = write_pair(directory.path(), "tak.example.com", 90);

        let loaded = load(&cert_file, &key_file).unwrap();

        assert_eq!(loaded.certificates, 1);
        assert_eq!(loaded.certified.end_entity_cert().unwrap().as_ref(), der);
        assert!(loaded.not_after.unwrap() > Utc::now() + chrono::Duration::days(80));
        assert!(loaded.not_before.unwrap() <= Utc::now());
    }

    #[test]
    fn a_key_that_is_not_the_leafs_is_refused_rather_than_served() {
        // The half-written renewal: the chain is the new one and the key is
        // still the old one. Serving that is a listener whose every handshake
        // fails.
        let directory = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let (cert_file, _, _) = write_pair(directory.path(), "tak.example.com", 90);
        let (_, key_file, _) = write_pair(other.path(), "tak.example.com", 90);

        let refused = load(&cert_file, &key_file).unwrap_err();

        assert!(refused.is(human_errors::Kind::User));
        assert!(refused.description().contains("do not go together"));
    }

    #[test]
    fn a_missing_file_is_named_in_the_refusal() {
        let directory = tempfile::tempdir().unwrap();

        let refused = load(
            &directory.path().join("absent.crt"),
            &directory.path().join("absent.key"),
        )
        .unwrap_err();

        assert!(refused.description().contains("absent.crt"));
    }

    #[test]
    fn an_empty_chain_says_so_rather_than_failing_inside_rustls() {
        let directory = tempfile::tempdir().unwrap();
        let (cert_file, key_file, _) = write_pair(directory.path(), "tak.example.com", 90);
        std::fs::write(&cert_file, "").unwrap();

        let refused = load(&cert_file, &key_file).unwrap_err();

        assert!(refused.description().contains("no certificates"));
    }

    #[test]
    fn a_renewed_pair_is_swapped_in_without_a_restart() {
        let directory = tempfile::tempdir().unwrap();
        let (cert_file, key_file, first) = write_pair(directory.path(), "tak.example.com", 90);
        let loaded = load(&cert_file, &key_file).unwrap();
        let (files, resolver) = listener(&cert_file, &key_file, Some(loaded));

        assert!(matches!(files.reload(), Outcome::Unchanged));
        assert_eq!(presented(&resolver), first);

        let (_, _, second) = write_pair(directory.path(), "tak.example.com", 90);

        assert!(matches!(files.reload(), Outcome::Swapped { .. }));
        assert_eq!(
            presented(&resolver),
            second,
            "the renewed certificate must be what the next handshake gets",
        );
        assert_ne!(first, second);

        let status = files.status(&["tak.example.com".to_string()]);
        assert_eq!(status.state, TlsCertificateState::Valid);
        assert!(status.note.is_none());
        assert!(status.loaded_at.is_some());
    }

    #[test]
    fn a_broken_new_pair_is_ignored_and_the_previous_one_keeps_serving() {
        let directory = tempfile::tempdir().unwrap();
        let (cert_file, key_file, first) = write_pair(directory.path(), "tak.example.com", 90);
        let loaded = load(&cert_file, &key_file).unwrap();
        let (files, resolver) = listener(&cert_file, &key_file, Some(loaded));

        // Only the chain replaced: the key on disk is still the old one.
        let elsewhere = tempfile::tempdir().unwrap();
        let (replacement, _, _) = write_pair(elsewhere.path(), "tak.example.com", 90);
        std::fs::write(&cert_file, std::fs::read(&replacement).unwrap()).unwrap();

        assert!(matches!(files.reload(), Outcome::Rejected(_)));
        assert_eq!(
            presented(&resolver),
            first,
            "a pair that does not go together must not replace one that does",
        );

        let status = files.status(&[]);
        assert_eq!(status.state, TlsCertificateState::Valid);
        assert_eq!(status.attempts, 1);
        assert!(status.last_error.unwrap().contains("do not go together"));

        // The same bytes are not re-read every interval.
        assert!(matches!(files.reload(), Outcome::Unchanged));
    }

    #[test]
    fn a_pair_that_is_not_there_yet_is_waited_for_and_then_picked_up() {
        let directory = tempfile::tempdir().unwrap();
        let cert_file = directory.path().join("chain.pem");
        let key_file = directory.path().join("key.pem");
        let (files, resolver) = listener(&cert_file, &key_file, None);

        assert!(matches!(files.reload(), Outcome::Waiting));

        let waiting = files.status(&["tak.example.com".to_string()]);
        assert_eq!(waiting.state, TlsCertificateState::Missing);
        assert_eq!(waiting.source, TlsSource::Files);
        assert_eq!(waiting.note.as_deref(), Some(WAITING));
        assert!(waiting.cert_file.unwrap().contains("chain.pem"));
        assert!(waiting.not_after.is_none());

        let (_, _, der) = write_pair(directory.path(), "tak.example.com", 90);

        assert!(matches!(files.reload(), Outcome::Swapped { .. }));
        assert_eq!(presented(&resolver), der);
        assert_eq!(
            files.status(&[]).state,
            TlsCertificateState::Valid,
            "a sidecar that renders the pair after start-up must end up serving it",
        );
    }

    #[test]
    fn an_expired_certificate_is_reported_rather_than_shown_as_healthy() {
        let directory = tempfile::tempdir().unwrap();
        let (cert_file, key_file, _) = write_pair(directory.path(), "tak.example.com", -1);
        let loaded = load(&cert_file, &key_file).unwrap();
        let (files, _) = listener(&cert_file, &key_file, Some(loaded));

        assert_eq!(
            files.status(&[]).state,
            TlsCertificateState::Expiring,
            "an operator whose agent stopped renewing should see it here",
        );
    }

    #[test]
    fn a_pair_that_could_not_be_read_at_start_up_is_reported_as_failed() {
        let directory = tempfile::tempdir().unwrap();
        let cert_file = directory.path().join("chain.pem");
        let key_file = directory.path().join("key.pem");
        std::fs::write(&cert_file, "not a certificate").unwrap();
        std::fs::write(&key_file, "not a key").unwrap();

        let files = FilesCertificate::new(
            cert_file.clone(),
            key_file.clone(),
            HotSwapCertResolver::new(None),
            None,
            Some("We could not read the certificate chain".to_string()),
        );

        let status = files.status(&[]);

        assert_eq!(status.state, TlsCertificateState::Failed);
        assert_eq!(status.attempts, 1);
        assert!(status.last_attempt_at.is_some());
        assert!(
            status.note.is_none(),
            "a pair that is there and unusable is not one we are waiting for",
        );

        // A pair that is simply not there yet reads as waiting, not failed.
        let waiting = FilesCertificate::new(
            directory.path().join("absent.pem"),
            directory.path().join("absent.key"),
            HotSwapCertResolver::new(None),
            None,
            Some("We could not read the certificate chain".to_string()),
        );

        assert_eq!(waiting.status(&[]).state, TlsCertificateState::Missing);
        assert_eq!(waiting.status(&[]).attempts, 0);
        assert!(waiting.status(&[]).last_error.is_none());
    }

    #[test]
    fn what_the_listener_holds_is_what_the_admin_api_finds() {
        let directory = tempfile::tempdir().unwrap();
        let (cert_file, key_file, _) = write_pair(directory.path(), "tak.example.com", 90);
        let loaded = load(&cert_file, &key_file).unwrap();
        let (files, _) = listener(&cert_file, &key_file, Some(loaded));

        publish(Arc::clone(&files));

        let mut config = Config::default();
        config.web.public.tls.mode = crate::config::TlsMode::Files;
        config.web.public.tls.cert_file = Some(cert_file);
        config.web.public.tls.key_file = Some(key_file);
        config.server.domains = vec!["tak.example.com".to_string()];

        assert!(Arc::ptr_eq(&for_config(&config).unwrap(), &files));

        let reported = report(&config);

        assert_eq!(reported.source, TlsSource::Files);
        assert_eq!(reported.state, TlsCertificateState::Valid);
        assert_eq!(reported.domains, vec!["tak.example.com".to_string()]);
        assert!(reported.directory.is_none(), "files mode orders nothing");
    }

    #[test]
    fn a_loaded_pair_never_renders_its_key() {
        let directory = tempfile::tempdir().unwrap();
        let (cert_file, key_file, _) = write_pair(directory.path(), "tak.example.com", 90);

        let rendered = format!("{:?}", load(&cert_file, &key_file).unwrap());

        assert!(rendered.contains("certificates"));
        assert!(!rendered.contains("key"));
    }
}

//! Taking a certificate back, and enforcing that at the handshake.
//!
//! # Why the check is a cache rather than a query
//!
//! rustls decides whether to accept a client certificate inside
//! [`ClientCertVerifier::verify_client_cert`], which is synchronous and runs on
//! the thread driving the handshake. There is no way to await a database read
//! from it. The set of revoked fingerprints is therefore held in memory, loaded
//! at start-up and updated by [`revoke`] as certificates are taken back, and
//! the handshake consults that.
//!
//! The cache is authoritative for *refusals only*: it can refuse a certificate
//! the database would have accepted (never the reverse, since nothing is ever
//! un-revoked), so a cache that has fallen behind fails closed.
//!
//! # `require_known_cert`
//!
//! Chaining to our authority is not quite the same question as "we issued
//! this". A backup restored over a newer database, or a certificate signed
//! before a row was deleted, chains perfectly and has no record. With
//! `require_known_cert` on — the default — such a certificate is refused, so
//! the database is the register of who may connect rather than merely a log of
//! who was once allowed to.
//!
//! [`ClientCertVerifier::verify_client_cert`]: rustls::server::danger::ClientCertVerifier::verify_client_cert

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use rustak_api::{AuditCategory, AuditOutcome};
use rustak_core::prelude::*;

use crate::db::repos::RevocationDetails;
use crate::db::{AuditEntry, AuditStore as _, Database};

/// Why a handshake's certificate was turned away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertRejection {
    /// It was issued by us and has since been taken back.
    Revoked,

    /// It chains to our authority but we have no record of issuing it.
    Unknown,
}

impl CertRejection {
    /// How the rejection is named in the log.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Revoked => "revoked",
            Self::Unknown => "unknown",
        }
    }
}

/// Why a certificate was taken back.
///
/// Recorded on the row and in the audit log, because "revoked" on its own does
/// not tell an administrator six months later whether a device was lost or a
/// certificate simply replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevokeReason {
    /// The person who holds it asked.
    UserRequest,

    /// The device carrying it is gone.
    DeviceLost,

    /// A newer certificate replaced it.
    Superseded,

    /// An administrator decided.
    AdminAction,

    /// The credential it was enrolled with was revoked.
    CredentialRevoked,

    /// The account it belongs to was disabled.
    UserDisabled,
}

impl RevokeReason {
    /// The value stored on the row and shown in the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserRequest => "user_request",
            Self::DeviceLost => "device_lost",
            Self::Superseded => "superseded",
            Self::AdminAction => "admin_action",
            Self::CredentialRevoked => "credential_revoked",
            Self::UserDisabled => "user_disabled",
        }
    }
}

/// Something to run when a certificate is taken back.
///
/// The stream module registers one so that revoking a certificate drops the
/// connections already holding it — a handshake check alone would leave a
/// revoked device connected until it next reconnected, which for a long-lived
/// CoT stream could be days.
pub type RevocationHook = Arc<dyn Fn(&str) + Send + Sync>;

/// Which certificates the TLS listeners will accept.
pub struct RevocationCache {
    revoked: RwLock<HashSet<String>>,
    known: RwLock<HashSet<String>>,
    hooks: RwLock<Vec<RevocationHook>>,
    require_known: bool,
}

impl std::fmt::Debug for RevocationCache {
    /// Written out because the hooks are closures with no useful `Debug`, and
    /// rustls requires the verifier holding this to be `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevocationCache")
            .field("revoked", &self.revoked.read().map(|set| set.len()).ok())
            .field("known", &self.known.read().map(|set| set.len()).ok())
            .field("require_known", &self.require_known)
            .finish_non_exhaustive()
    }
}

impl RevocationCache {
    /// An empty cache.
    ///
    /// Empty means "nothing is known", which with `require_known` on refuses
    /// every certificate until [`RevocationCache::reload`] has run. Start-up
    /// reloads before it binds a listener, so that window is not reachable.
    pub fn new(require_known: bool) -> Arc<Self> {
        Arc::new(Self {
            revoked: RwLock::new(HashSet::new()),
            known: RwLock::new(HashSet::new()),
            hooks: RwLock::new(Vec::new()),
            require_known,
        })
    }

    /// The cache `[pki]` describes.
    pub fn from_config(pki: &crate::config::PkiConfig) -> Arc<Self> {
        Self::new(pki.require_known_cert)
    }

    /// Whether a certificate missing from the database is refused.
    pub fn require_known(&self) -> bool {
        self.require_known
    }

    /// Reads the whole register out of the database, replacing what is held.
    ///
    /// Both sets come from one read, so the cache cannot be assembled from two
    /// queries taken either side of a revocation.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the read fails.
    #[instrument("pki.revocation.reload", skip_all, err(Display))]
    pub async fn reload(&self, db: &Database) -> Result<(), Error> {
        let (known, revoked) = db.certificates().fingerprints().await?;

        debug!(
            known = known.len(),
            revoked = revoked.len(),
            "Reloaded the certificate register."
        );

        self.replace(&self.known, known);
        self.replace(&self.revoked, revoked);

        Ok(())
    }

    /// Whether a handshake presenting this certificate may continue.
    ///
    /// # Errors
    ///
    /// [`CertRejection::Revoked`] when it has been taken back, and
    /// [`CertRejection::Unknown`] when `require_known_cert` is on and we have
    /// no record of it.
    pub fn is_acceptable(&self, fingerprint: &str) -> Result<(), CertRejection> {
        if self.contains(&self.revoked, fingerprint) {
            return Err(CertRejection::Revoked);
        }

        if self.require_known && !self.contains(&self.known, fingerprint) {
            return Err(CertRejection::Unknown);
        }

        Ok(())
    }

    /// Notes a certificate we have just issued, so it works immediately rather
    /// than after the next reload.
    pub fn note_issued(&self, fingerprint: &str) {
        if let Ok(mut known) = self.known.write() {
            known.insert(fingerprint.to_owned());
        }
    }

    /// Notes a certificate we have just taken back, and runs the hooks.
    pub fn note_revoked(&self, fingerprint: &str) {
        if let Ok(mut revoked) = self.revoked.write() {
            revoked.insert(fingerprint.to_owned());
        }

        let hooks = match self.hooks.read() {
            Ok(hooks) => hooks.clone(),
            Err(_) => return,
        };

        // Cloned and released first: a hook that revokes something else would
        // otherwise deadlock on the lock it is being called under.
        for hook in hooks {
            hook(fingerprint);
        }
    }

    /// Registers something to run whenever a certificate is taken back.
    pub fn on_revoked(&self, hook: RevocationHook) {
        if let Ok(mut hooks) = self.hooks.write() {
            hooks.push(hook);
        }
    }

    /// How many certificates are known and how many of those are revoked.
    pub fn counts(&self) -> (usize, usize) {
        (
            self.known.read().map(|set| set.len()).unwrap_or_default(),
            self.revoked.read().map(|set| set.len()).unwrap_or_default(),
        )
    }

    /// A read that treats a poisoned lock as "not in the set".
    ///
    /// A poisoned lock means a thread panicked while holding it, which cannot
    /// leave a *missing* entry — only a half-written one — so the safe reading
    /// for the `known` set is "we have no record", which refuses, and for the
    /// `revoked` set the reload that follows any such panic restores it.
    fn contains(&self, set: &RwLock<HashSet<String>>, fingerprint: &str) -> bool {
        set.read()
            .map(|set| set.contains(fingerprint))
            .unwrap_or(false)
    }

    fn replace(&self, set: &RwLock<HashSet<String>>, values: Vec<String>) {
        if let Ok(mut held) = set.write() {
            *held = values.into_iter().collect();
        }
    }
}

/// Takes a certificate back: the row, the cache, the hooks and the audit log.
///
/// Reports whether it was live — revoking one twice is not an error, because
/// an administrator clicking again and a job catching up should both succeed.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the write fails, and a
/// [`human_errors::Kind::User`] error when there is no such certificate.
#[instrument("pki.revoke", skip_all, fields(fingerprint = %fingerprint), err(Display))]
pub async fn revoke(
    db: &Database,
    cache: &RevocationCache,
    fingerprint: &str,
    reason: RevokeReason,
    actor: Option<&Username>,
) -> Result<bool, Error> {
    let Some(row) = db.certificates().get_by_fingerprint(fingerprint).await? else {
        return Err(human_errors::user(
            "There is no certificate with that fingerprint.",
            &["Check the fingerprint against the certificate list for the device."],
        ));
    };

    let revoked = db
        .certificates()
        .revoke(
            row.id,
            RevocationDetails {
                reason: reason.as_str().to_owned(),
                by: actor.cloned(),
            },
        )
        .await?;

    // Outside the `if`: a certificate already marked in the database but absent
    // from the cache — a second process revoked it, say — must still be refused
    // here, and the connections holding it still dropped.
    cache.note_revoked(fingerprint);

    let outcome = if revoked {
        AuditOutcome::Success
    } else {
        AuditOutcome::Skipped
    };

    let mut entry = AuditEntry::new(AuditCategory::Pki, "certificate.revoked", outcome)
        .subject(&row.subject_cn)
        .message(format!(
            "Certificate {} for {} was revoked ({}).",
            short(fingerprint),
            row.subject_cn,
            reason.as_str()
        ))
        .detail(serde_json::json!({
            "fingerprint": fingerprint,
            "serial": row.serial_hex,
            "reason": reason.as_str(),
            "client_uid": row.client_uid,
            "already_revoked": !revoked,
        }));

    if let Some(actor) = actor {
        entry = entry.actor(actor);
    }

    db.record(entry).await?;

    Ok(revoked)
}

/// Takes back every live certificate bought with one credential.
///
/// The cache is refreshed from the database afterwards rather than being
/// updated entry by entry, because the repository reports how many rows changed
/// and not which ones.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the write or the reload fails.
#[instrument("pki.revoke.credential", skip_all, err(Display))]
pub async fn revoke_for_credential(
    db: &Database,
    cache: &RevocationCache,
    credential_id: CredentialId,
    reason: RevokeReason,
    actor: Option<&Username>,
) -> Result<usize, Error> {
    let before = db.certificates().fingerprints().await?.1;

    let revoked = db
        .certificates()
        .revoke_for_credential(
            credential_id,
            RevocationDetails {
                reason: reason.as_str().to_owned(),
                by: actor.cloned(),
            },
        )
        .await?;

    if revoked == 0 {
        return Ok(0);
    }

    let after = db.certificates().fingerprints().await?.1;
    let previously: HashSet<&String> = before.iter().collect();

    for fingerprint in after.iter().filter(|fp| !previously.contains(fp)) {
        cache.note_revoked(fingerprint);
    }

    let mut entry = AuditEntry::new(
        AuditCategory::Pki,
        "certificate.revoked",
        AuditOutcome::Success,
    )
    .message(format!(
        "{revoked} certificate(s) issued with a revoked credential were taken back ({}).",
        reason.as_str()
    ))
    .detail(serde_json::json!({ "credential_id": credential_id.get(), "count": revoked }));

    if let Some(actor) = actor {
        entry = entry.actor(actor);
    }

    db.record(entry).await?;

    Ok(revoked)
}

/// The leading bytes of a fingerprint, which is what a person reads.
fn short(fingerprint: &str) -> &str {
    &fingerprint[..fingerprint.len().min(16)]
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use chrono::{Duration, Utc};
    use rustak_api::{CertificateKind, CertificateSource};

    use super::*;
    use crate::db::repos::NewCertificate;

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    async fn certificate(db: &Database, fingerprint: &str) -> crate::db::repos::CertificateRow {
        let now = Utc::now();

        db.certificates()
            .create(NewCertificate {
                kind: CertificateKind::Client,
                source: CertificateSource::Enrollment,
                issued_via: Some("enroll_v2_json".to_owned()),
                serial_hex: format!("{fingerprint}-serial"),
                fingerprint: fingerprint.to_owned(),
                subject_cn: "alice".to_owned(),
                san: Vec::new(),
                user_id: None,
                device_id: None,
                client_uid: Some("ANDROID-1".to_owned()),
                credential_id: None,
                issuer_id: None,
                der: vec![1, 2, 3],
                key_sealed: None,
                not_before: now - Duration::minutes(5),
                not_after: now + Duration::days(1),
            })
            .await
            .unwrap()
    }

    #[test]
    fn an_empty_cache_refuses_everything_it_does_not_know() {
        let strict = RevocationCache::new(true);
        let lenient = RevocationCache::new(false);

        assert_eq!(strict.is_acceptable("aa"), Err(CertRejection::Unknown));
        assert_eq!(lenient.is_acceptable("aa"), Ok(()));
    }

    #[test]
    fn a_certificate_we_just_issued_works_before_the_next_reload() {
        let cache = RevocationCache::new(true);

        cache.note_issued("aa");

        assert_eq!(cache.is_acceptable("aa"), Ok(()));
        assert_eq!(cache.is_acceptable("bb"), Err(CertRejection::Unknown));
    }

    #[test]
    fn revocation_beats_being_known() {
        let cache = RevocationCache::new(true);

        cache.note_issued("aa");
        cache.note_revoked("aa");

        assert_eq!(cache.is_acceptable("aa"), Err(CertRejection::Revoked));
    }

    #[test]
    fn a_revocation_without_the_known_check_is_still_refused() {
        let cache = RevocationCache::new(false);

        cache.note_revoked("aa");

        assert_eq!(cache.is_acceptable("aa"), Err(CertRejection::Revoked));
        assert_eq!(cache.is_acceptable("bb"), Ok(()));
    }

    #[test]
    fn every_hook_hears_about_a_revocation() {
        let cache = RevocationCache::new(false);
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(RwLock::new(Vec::new()));

        for counter in [Arc::clone(&first), Arc::clone(&second)] {
            cache.on_revoked(Arc::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            }));
        }

        let recorder = Arc::clone(&seen);
        cache.on_revoked(Arc::new(move |fingerprint: &str| {
            recorder.write().unwrap().push(fingerprint.to_owned());
        }));

        cache.note_revoked("aa");

        assert_eq!(first.load(Ordering::SeqCst), 1);
        assert_eq!(second.load(Ordering::SeqCst), 1);
        assert_eq!(seen.read().unwrap().as_slice(), ["aa"]);
    }

    #[test]
    fn a_hook_may_revoke_something_else_without_deadlocking() {
        let cache = RevocationCache::new(false);
        let inner = Arc::clone(&cache);

        cache.on_revoked(Arc::new(move |fingerprint: &str| {
            if fingerprint == "aa" {
                inner.note_revoked("bb");
            }
        }));

        cache.note_revoked("aa");

        assert_eq!(cache.is_acceptable("bb"), Err(CertRejection::Revoked));
    }

    #[tokio::test]
    async fn reloading_reads_the_whole_register() {
        let db = database().await;
        let cache = RevocationCache::new(true);

        certificate(&db, "aa").await;
        let taken_back = certificate(&db, "bb").await;

        db.certificates()
            .revoke(
                taken_back.id,
                RevocationDetails {
                    reason: "admin_action".to_owned(),
                    by: None,
                },
            )
            .await
            .unwrap();

        cache.reload(&db).await.unwrap();

        assert_eq!(cache.counts(), (2, 1));
        assert_eq!(cache.is_acceptable("aa"), Ok(()));
        assert_eq!(cache.is_acceptable("bb"), Err(CertRejection::Revoked));
        assert_eq!(cache.is_acceptable("cc"), Err(CertRejection::Unknown));
    }

    #[tokio::test]
    async fn reloading_replaces_rather_than_merges() {
        let db = database().await;
        let cache = RevocationCache::new(true);

        cache.note_issued("gone");
        cache.reload(&db).await.unwrap();

        assert_eq!(
            cache.is_acceptable("gone"),
            Err(CertRejection::Unknown),
            "a certificate deleted from the database must stop working"
        );
    }

    #[tokio::test]
    async fn revoking_marks_the_row_updates_the_cache_and_fires_the_hooks() {
        let db = database().await;
        let cache = RevocationCache::new(true);
        let dropped = Arc::new(RwLock::new(Vec::new()));
        let recorder = Arc::clone(&dropped);

        certificate(&db, "aa").await;
        cache.reload(&db).await.unwrap();
        cache.on_revoked(Arc::new(move |fingerprint: &str| {
            recorder.write().unwrap().push(fingerprint.to_owned());
        }));

        let actor = Username::parse("admin").unwrap();

        assert!(
            revoke(&db, &cache, "aa", RevokeReason::DeviceLost, Some(&actor))
                .await
                .unwrap()
        );

        assert_eq!(cache.is_acceptable("aa"), Err(CertRejection::Revoked));
        assert_eq!(dropped.read().unwrap().as_slice(), ["aa"]);

        let row = db
            .certificates()
            .get_by_fingerprint("aa")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(row.revocation_reason.as_deref(), Some("device_lost"));
        assert_eq!(row.revoked_by.as_deref(), Some("admin"));
        assert!(row.revoked_at.is_some());
    }

    #[tokio::test]
    async fn revoking_twice_is_not_an_error() {
        let db = database().await;
        let cache = RevocationCache::new(false);

        certificate(&db, "aa").await;

        assert!(
            revoke(&db, &cache, "aa", RevokeReason::AdminAction, None)
                .await
                .unwrap()
        );
        assert!(
            !revoke(&db, &cache, "aa", RevokeReason::AdminAction, None)
                .await
                .unwrap(),
            "the second call reports that there was nothing to do"
        );
    }

    #[tokio::test]
    async fn revoking_something_we_never_issued_is_a_user_error() {
        let db = database().await;
        let cache = RevocationCache::new(false);

        assert!(
            revoke(&db, &cache, "nothing", RevokeReason::AdminAction, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_revocation_is_audited() {
        let db = database().await;
        let cache = RevocationCache::new(false);

        certificate(&db, "aa").await;
        revoke(&db, &cache, "aa", RevokeReason::UserDisabled, None)
            .await
            .unwrap();

        let entries = db
            .audit(crate::db::AuditQuery::recent(10).in_category(AuditCategory::Pki))
            .await
            .unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].action, "certificate.revoked");
        assert_eq!(entries[0].subject.as_deref(), Some("alice"));
    }

    #[tokio::test]
    async fn revoking_a_credential_takes_back_everything_it_bought() {
        let db = database().await;
        let cache = RevocationCache::new(true);
        let user = db
            .users()
            .create(crate::db::repos::NewUser::person(
                Username::parse("alice").unwrap(),
            ))
            .await
            .unwrap();
        let credential = db
            .credentials()
            .create(crate::db::repos::NewCredential {
                user_id: user.id,
                kind: rustak_api::CredentialKind::EnrollmentToken,
                label: "first device".to_owned(),
                secret_hash: rustak_core::identity::password::hash("a-secret").unwrap(),
                lookup_hint: "hint".to_owned(),
                expires_at: None,
                max_uses: Some(1),
                created_by: None,
            })
            .await
            .unwrap();

        for fingerprint in ["aa", "bb"] {
            let row = certificate(&db, fingerprint).await;

            db.write(move |tx| {
                tx.execute(
                    "UPDATE certificates SET credential_id = ?2 WHERE id = ?1",
                    rusqlite::params![row.id.get(), credential.id.get()],
                )
            })
            .await
            .unwrap();
        }

        certificate(&db, "cc").await;
        cache.reload(&db).await.unwrap();

        let taken = revoke_for_credential(
            &db,
            &cache,
            credential.id,
            RevokeReason::CredentialRevoked,
            None,
        )
        .await
        .unwrap();

        assert_eq!(taken, 2);
        assert_eq!(cache.is_acceptable("aa"), Err(CertRejection::Revoked));
        assert_eq!(cache.is_acceptable("bb"), Err(CertRejection::Revoked));
        assert_eq!(
            cache.is_acceptable("cc"),
            Ok(()),
            "a certificate from a different credential is untouched"
        );
    }

    #[tokio::test]
    async fn revoking_a_credential_that_bought_nothing_is_a_no_op() {
        let db = database().await;
        let cache = RevocationCache::new(false);

        assert_eq!(
            revoke_for_credential(
                &db,
                &cache,
                CredentialId::new(1),
                RevokeReason::CredentialRevoked,
                None,
            )
            .await
            .unwrap(),
            0
        );
    }

    #[test]
    fn the_configuration_decides_whether_unknown_certificates_are_refused() {
        let strict = RevocationCache::from_config(&crate::config::PkiConfig::default());
        let lenient = RevocationCache::from_config(&crate::config::PkiConfig {
            require_known_cert: false,
            ..crate::config::PkiConfig::default()
        });

        assert!(strict.require_known());
        assert!(!lenient.require_known());
    }

    #[test]
    fn every_reason_and_rejection_has_a_stored_name() {
        for reason in [
            RevokeReason::UserRequest,
            RevokeReason::DeviceLost,
            RevokeReason::Superseded,
            RevokeReason::AdminAction,
            RevokeReason::CredentialRevoked,
            RevokeReason::UserDisabled,
        ] {
            assert!(!reason.as_str().is_empty());
        }

        assert_eq!(CertRejection::Revoked.as_str(), "revoked");
        assert_eq!(CertRejection::Unknown.as_str(), "unknown");
    }

    #[test]
    fn a_cache_never_renders_what_it_holds() {
        let cache = RevocationCache::new(true);

        cache.note_revoked("aa");

        let rendered = format!("{cache:?}");

        assert!(!rendered.contains("aa"));
        assert!(rendered.contains("require_known"));
    }
}

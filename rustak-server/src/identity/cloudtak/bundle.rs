//! Where a prepared keystore waits, and the rules that keep it from waiting
//! long.
//!
//! The hand-over is two requests — one that prepares and one that collects —
//! and between them there is a private key this server has to hold. This module
//! is the whole of that interval, kept apart from the flow in [`super`] so that
//! "what happens to the key" can be read on its own.
//!
//! Three properties, and each is a line of code rather than a convention:
//!
//! * **Sealed.** [`crate::crypto::SecretStore`] seals the bundle under the
//!   certificate it belongs to, so the key/value row is a ciphertext bound to
//!   that row and cannot be grafted onto another hand-over by somebody who can
//!   write to the database but does not hold the sealing key.
//! * **One shot.** [`take`] is a single delete-returning write, so of two
//!   simultaneous collections exactly one gets bytes. The one-shot property is
//!   the *delete*, not the value: a caller that then finds the record expired
//!   has still consumed it, which is what makes a replayed link useless inside
//!   a window no sweep has covered.
//! * **Brief.** Ten minutes, enforced on read and swept on the next
//!   preparation, whether or not anybody collected it.

use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use rand::Rng as _;
use rustak_api::cloudtak::BUNDLE_TTL_MINUTES;

use crate::crypto::{Sealed, SecretContext};
use crate::db::Database;
use crate::prelude::*;

/// The key/value partition the sealed bundles wait in.
pub const BUNDLE_PARTITION: &str = "cloudtak_onboarding";

/// Bytes behind a download identifier: 192 bits, unguessable, and 32 URL-safe
/// characters once encoded.
const DOWNLOAD_ID_BYTES: usize = 24;

/// The longest a download identifier may be before it is looked up.
const MAX_DOWNLOAD_ID: usize = 64;

/// The record format this release writes.
const RECORD_VERSION: u8 = 1;

/// Base64 as a URL path segment needs it.
const B64: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A prepared bundle, taken out of the store.
pub struct Bundle {
    /// Whose it is, for the audit entry the download writes.
    pub username: Username,

    /// The certificate it carries, so the entry names the same row the
    /// preparation did.
    pub certificate_id: CertificateId,

    /// The PKCS#12 itself.
    pub p12: Vec<u8>,
}

impl std::fmt::Debug for Bundle {
    /// The bytes are a private key; only their length is printable.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bundle")
            .field("username", &self.username)
            .field("certificate_id", &self.certificate_id)
            .field("p12", &format_args!("{} bytes", self.p12.len()))
            .finish()
    }
}

/// What waits in the key/value store between preparing and downloading.
#[derive(Serialize, Deserialize)]
struct Stashed {
    /// The record format, so a later change can be told from this one.
    v: u8,
    username: Username,
    certificate_id: CertificateId,
    /// The bundle, sealed under [`SecretContext::ServiceCertKey`].
    sealed: Sealed,
    expires_at: DateTime<Utc>,
}

/// Seals the bundle and puts it where the download will find it.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when sealing or the write fails.
pub async fn stash(
    context: &AppContext,
    username: &Username,
    certificate_id: CertificateId,
    p12: Vec<u8>,
) -> Result<(String, DateTime<Utc>), Error> {
    sweep(context.db(), Utc::now()).await?;

    let expires_at = Utc::now() + Duration::minutes(BUNDLE_TTL_MINUTES);
    let sealed = context.secrets().seal(
        &p12,
        // The bundle *is* this certificate's key material; see the module
        // documentation for what binding it to that row buys.
        SecretContext::ServiceCertKey {
            certificate: certificate_id,
        },
    )?;

    let stashed = Stashed {
        v: RECORD_VERSION,
        username: username.clone(),
        certificate_id,
        sealed,
        expires_at,
    };

    let id = download_id();
    if !context
        .db()
        .insert(BUNDLE_PARTITION, id.clone(), stashed)
        .await?
    {
        // 192 bits: this is a bug or a broken random source, not a collision to
        // retry around.
        return Err(human_errors::system(
            "A CloudTAK hand-over was prepared under an identifier already in use.",
            &["This is unexpected; please report it with the surrounding log entries."],
        ));
    }

    Ok((id, expires_at))
}

/// Takes a prepared bundle, once.
///
/// [`None`] covers every reason the caller cannot have it — unknown, already
/// collected, expired — because telling them apart would make the endpoint an
/// oracle for which hand-overs are outstanding, and because the answer is the
/// same `410` either way.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the write or the decryption
/// fails.
#[instrument("identity.cloudtak.take", skip_all, err(Display))]
pub async fn take(context: &AppContext, id: &str) -> Result<Option<Bundle>, Error> {
    if !is_download_id(id) {
        return Ok(None);
    }

    let Some(stashed) = context
        .db()
        .take::<Stashed>(BUNDLE_PARTITION, id.to_string())
        .await?
    else {
        return Ok(None);
    };

    if stashed.v != RECORD_VERSION || stashed.expires_at <= Utc::now() {
        return Ok(None);
    }

    let p12 = context.secrets().open(
        &stashed.sealed,
        SecretContext::ServiceCertKey {
            certificate: stashed.certificate_id,
        },
    )?;

    Ok(Some(Bundle {
        username: stashed.username,
        certificate_id: stashed.certificate_id,
        p12,
    }))
}

/// Deletes every bundle whose window has closed, and reports how many.
///
/// Run when a hand-over is prepared rather than on a timer: the partition is
/// touched only by this feature, it holds at most a handful of rows, and a
/// sealed private key that nothing will ever hand over should not wait for the
/// next scheduled sweep to go.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when a read or a write fails.
pub async fn sweep(db: &Database, now: DateTime<Utc>) -> Result<usize, Error> {
    let stashed: Vec<(String, Stashed)> = db.list(BUNDLE_PARTITION).await?;
    let mut swept = 0;

    for (key, bundle) in stashed {
        if bundle.expires_at > now {
            continue;
        }

        db.remove(BUNDLE_PARTITION, key).await?;
        swept += 1;
    }

    if swept > 0 {
        debug!(swept, "Swept expired CloudTAK hand-over bundles.");
    }

    Ok(swept)
}

/// A fresh download identifier.
fn download_id() -> String {
    let mut bytes = [0u8; DOWNLOAD_ID_BYTES];
    rand::rng().fill_bytes(&mut bytes);

    B64.encode(bytes)
}

/// Whether a path segment could be one of ours.
///
/// Checked before the lookup so that a caller cannot address an unrelated
/// key/value row, and so that the answer to a malformed identifier costs no
/// read at all.
fn is_download_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_DOWNLOAD_ID
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_download_identifier_is_unguessable_and_url_safe() {
        let id = download_id();

        assert_eq!(id.len(), 32, "192 bits of base64url");
        assert!(is_download_id(&id), "{id}");
        assert!(!id.contains('.'), "the path appends '.p12': {id}");
        assert_ne!(id, download_id(), "every hand-over gets its own");
    }

    #[test]
    fn nothing_that_is_not_a_download_identifier_reaches_the_store() {
        // The segment becomes a key/value key, so a traversal or a wildcard
        // must be refused before the read rather than looked up and missed.
        for candidate in ["", "../../pki/ca", "abc def", "abc.p12", &"a".repeat(65)] {
            assert!(!is_download_id(candidate), "{candidate}");
        }
    }

    #[test]
    fn a_stashed_bundle_renders_no_key_material() {
        let bundle = Bundle {
            username: Username::parse("ada").unwrap(),
            certificate_id: CertificateId::new(3),
            p12: vec![1, 2, 3, 4],
        };

        let rendered = format!("{bundle:?}");

        assert!(rendered.contains("4 bytes"), "{rendered}");
        assert!(!rendered.contains("[1, 2, 3, 4]"), "{rendered}");
    }
}

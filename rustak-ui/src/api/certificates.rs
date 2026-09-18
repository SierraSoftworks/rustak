//! What this installation's authority has issued, and taking one back.
//!
//! Reading follows the same rule as credentials and devices: yours, or
//! anybody's if you administer the installation. Revoking is administrative
//! on its own, because it drops the live connections holding the certificate
//! and cannot be undone.
//!
//! # Revoking is a `POST`, not a `DELETE`
//!
//! Forgetting a certificate is the *opposite* of revoking it: with
//! `require_known_cert` on, the register is the list of who may connect, so a
//! deleted row is a certificate nothing can refuse a second time. The row
//! stays and gains a revocation, and the verb says which of the two happened.

use rustak_api::{Certificate, CertificateId, DeviceUid, RevocationReason, Username};

use crate::api::{ApiError, get_json, post_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// Every certificate, or only the ones matching a filter.
///
/// An administrator naming nobody is asking about the installation; anybody
/// else naming nobody gets their own, because that is what the server answers
/// rather than a refusal.
pub async fn list(
    username: Option<&Username>,
    device_uid: Option<&DeviceUid>,
) -> Result<Vec<Certificate>, ApiError> {
    demo!(Ok(fixtures::certificates(username, device_uid)));

    let mut query = Vec::new();
    if let Some(username) = username {
        query.push(format!("username={}", urlencode(username.as_str())));
    }
    if let Some(uid) = device_uid {
        query.push(format!("device_uid={}", urlencode(uid.as_str())));
    }

    match query.is_empty() {
        true => get_json("/certificates").await,
        false => get_json(&format!("/certificates?{}", query.join("&"))).await,
    }
}

/// One certificate.
#[allow(dead_code)]
pub async fn get(id: CertificateId) -> Result<Certificate, ApiError> {
    demo!(fixtures::certificate(id));

    get_json(&format!("/certificates/{}", id.get())).await
}

/// Takes one back, and answers with the row as it now stands.
///
/// The reason is stored, audited and shown afterwards: "revoked" on its own
/// does not tell an administrator six months later whether a device was lost
/// or a certificate simply replaced, and the two lead to different actions.
pub async fn revoke(id: CertificateId, reason: RevocationReason) -> Result<Certificate, ApiError> {
    demo!(fixtures::revoke_certificate(id, reason));

    post_json(
        &format!("/certificates/{}/revoke", id.get()),
        &serde_json::json!({ "reason": reason }),
    )
    .await
}

//! Demo data for the CloudTAK hand-over.
//!
//! The one property worth demonstrating here is the one the page is built
//! around: the keystore can be collected **once**. A fixture that handed the
//! same bytes back every time would let the page's `410` branch — the whole
//! reason it has one — go unexercised until somebody hit it in production, so
//! the prepared bundles are held in a thread-local store and taken out of it by
//! the download, exactly as the server takes them out of the key/value store.
//!
//! Every generated value names itself as fake. A demo build must not hand
//! anybody a password or a passphrase that could be typed into a real client.

use std::cell::RefCell;
use std::collections::HashSet;

use rustak_api::{
    CertificateId, CloudTakOnboarding, CloudTakOnboardingRequest, CloudTakUrls, CredentialId,
    Username,
};

use super::data::{DEMO_HOST, ago};
use crate::api::ApiError;
use crate::api::download::Download;

/// What a demo build says instead of a secret.
const NOT_A_SECRET: &str = "demo-mode-not-a-real-secret";

// The download identifiers that have been prepared and not yet collected.
thread_local! {
    static OUTSTANDING: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Prepares a hand-over, honouring whatever the advanced section overrode.
pub fn cloudtak_onboarding(
    username: &Username,
    request: &CloudTakOnboardingRequest,
) -> Result<CloudTakOnboarding, ApiError> {
    let host = request
        .host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .unwrap_or(DEMO_HOST);

    let ports = request.ports.unwrap_or_default();
    let id = format!("demo{}", OUTSTANDING.with(|held| held.borrow().len() + 1));

    OUTSTANDING.with(|held| held.borrow_mut().insert(id.clone()));

    Ok(CloudTakOnboarding {
        username: username.clone(),
        // Absent when a credential was reused, exactly as the server answers:
        // it keeps a hash and could not re-emit one.
        password: request
            .credential
            .existing()
            .is_none()
            .then(|| format!("{NOT_A_SECRET}-password")),
        p12_download_url: format!("/api/v1/cloudtak-onboarding/{id}.p12"),
        p12_password: format!("{NOT_A_SECRET}-passphrase"),
        urls: CloudTakUrls {
            stream: format!("ssl://{host}:{}", ports.stream.unwrap_or(8089)),
            api: format!("https://{host}:{}", ports.marti.unwrap_or(8443)),
            webtak: format!("https://{host}:{}", ports.public.unwrap_or(8446)),
        },
        certificate_id: CertificateId::new(11),
        credential_id: request
            .credential
            .existing()
            .unwrap_or(CredentialId::new(3)),
        // Ten minutes, as the server allows.
        expires_at: ago(-10),
    })
}

/// Collects a prepared keystore, once.
pub fn cloudtak_p12(url: &str, username: &Username) -> Result<Download, ApiError> {
    let id = url
        .rsplit('/')
        .next()
        .and_then(|segment| segment.strip_suffix(".p12"))
        .unwrap_or_default()
        .to_string();

    if !OUTSTANDING.with(|held| held.borrow_mut().remove(&id)) {
        return Err(ApiError::Gone);
    }

    Ok(Download {
        filename: format!("{username}-cloudtak.p12"),
        mime: "application/x-pkcs12".to_string(),
        // Demo mode has no certificate authority behind it, and a file that
        // looked like a keystore without being one would be worse than an
        // obviously empty one: somebody would try to upload it.
        bytes: NOT_A_SECRET.as_bytes().to_vec(),
    })
}

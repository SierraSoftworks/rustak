//! Demo data for certificates, configuration packages and the three settings
//! the console reads.
//!
//! The certificates line up with the `last_certificate_id` on each fixture
//! device, so the Devices tab shows a fingerprint and an expiry rather than
//! "Certificate #11" — which is what the identity brief could not do before
//! the endpoint existed.
//!
//! The TLS status is an ACME one that has *failed*, because that is the state
//! the card exists for: an operator opens it when something is wrong, and a
//! demo showing only the healthy case would never have exercised the error or
//! the Renew button beside it.

use std::cell::RefCell;

use rustak_api::{
    Certificate, CertificateId, CertificateKind, CertificateSource, ConfigPackageRequest,
    DeviceUid, FileSettings, MartiSettings, RevocationReason, TlsCertificateState, TlsSource,
    TlsStatus, Username,
};

use super::data::ago;
use super::empty_zip;
use crate::api::ApiError;
use crate::api::download::Download;

thread_local! {
    static CERTIFICATES: RefCell<Vec<Certificate>> = RefCell::new(seed());
    static TLS: RefCell<TlsStatus> = RefCell::new(acme_status());
    static FILES: RefCell<FileSettings> = const {
        RefCell::new(FileSettings {
            upload_size_limit_mb: 400,
            from_config_file: false,
        })
    };
}

fn with<R>(action: impl FnOnce(&mut Vec<Certificate>) -> R) -> R {
    CERTIFICATES.with(|held| action(&mut held.borrow_mut()))
}

fn username(value: &str) -> Username {
    Username::parse(value).expect("the fixture usernames are valid")
}

/// A fingerprint that looks like one without being one: every fixture secret
/// in this crate names itself as fake, so a value copied out of a demo can
/// never be mistaken for something an installation issued.
fn fingerprint(seed: &str) -> String {
    let mut value = format!("{seed}{}", "0".repeat(64));
    value.truncate(64);
    value
}

fn seed() -> Vec<Certificate> {
    vec![
        Certificate {
            id: CertificateId::new(11),
            kind: CertificateKind::Client,
            serial: "5b1f9c4e2a7d40318f6c0b9d3e8a1247".to_string(),
            fingerprint: fingerprint("demofa1e"),
            subject_cn: "avery".to_string(),
            username: Some(username("avery")),
            san: Vec::new(),
            not_before: ago(60 * 24 * 21),
            not_after: ago(-60 * 24 * 344),
            device_uid: DeviceUid::parse("ANDROID-2f1c9a7b4e0d").ok(),
            revoked_at: None,
            revocation_reason: None,
            source: CertificateSource::Enrollment,
        },
        Certificate {
            id: CertificateId::new(12),
            kind: CertificateKind::Client,
            serial: "9c22ae70f13b48c2a55d17e8b0c4936f".to_string(),
            fingerprint: fingerprint("demofa2e"),
            subject_cn: "avery".to_string(),
            username: Some(username("avery")),
            san: Vec::new(),
            not_before: ago(60 * 24 * 14),
            // Inside the month a client starts warning about, so the expiry
            // pill has something to be amber for.
            not_after: ago(-60 * 24 * 19),
            device_uid: DeviceUid::parse("WINTAK-7b3e10cc").ok(),
            revoked_at: None,
            revocation_reason: None,
            source: CertificateSource::Enrollment,
        },
        Certificate {
            id: CertificateId::new(13),
            kind: CertificateKind::Client,
            serial: "41e0b7d95a2c4f18930ac6e2b71d5480".to_string(),
            fingerprint: fingerprint("demofa3e"),
            subject_cn: "bhavna".to_string(),
            username: Some(username("bhavna")),
            san: Vec::new(),
            not_before: ago(60 * 24 * 6),
            not_after: ago(-60 * 24 * 359),
            device_uid: DeviceUid::parse("IOS-91ac4d55f207").ok(),
            revoked_at: None,
            revocation_reason: None,
            source: CertificateSource::Enrollment,
        },
        Certificate {
            id: CertificateId::new(14),
            kind: CertificateKind::Service,
            serial: "7d30f2ba18e94c07bb5416cd2af3e069".to_string(),
            fingerprint: fingerprint("demofa4e"),
            subject_cn: "service-weather".to_string(),
            username: Some(username("service-weather")),
            san: Vec::new(),
            not_before: ago(60 * 24 * 4),
            not_after: ago(-60 * 24 * 361),
            device_uid: DeviceUid::parse("SERVICE-weather").ok(),
            revoked_at: Some(ago(60 * 24)),
            revocation_reason: Some("superseded".to_string()),
            source: CertificateSource::AdminPackage,
        },
    ]
}

pub fn certificates(owner: Option<&Username>, device_uid: Option<&DeviceUid>) -> Vec<Certificate> {
    with(|held| {
        held.iter()
            .filter(|certificate| match owner {
                Some(owner) => certificate.username.as_ref() == Some(owner),
                None => true,
            })
            .filter(|certificate| match device_uid {
                Some(uid) => certificate.device_uid.as_ref() == Some(uid),
                None => true,
            })
            .cloned()
            .collect()
    })
}

pub fn certificate(id: CertificateId) -> Result<Certificate, ApiError> {
    with(|held| {
        held.iter()
            .find(|certificate| certificate.id == id)
            .cloned()
            .ok_or_else(|| ApiError::Server("There is no such certificate.".to_string()))
    })
}

pub fn revoke_certificate(
    id: CertificateId,
    reason: RevocationReason,
) -> Result<Certificate, ApiError> {
    with(|held| {
        let certificate = held
            .iter_mut()
            .find(|certificate| certificate.id == id)
            .ok_or_else(|| ApiError::Server("There is no such certificate.".to_string()))?;

        if certificate.revoked_at.is_some() {
            return Err(ApiError::Server(
                "That certificate had already been revoked.".to_string(),
            ));
        }

        certificate.revoked_at = Some(chrono::Utc::now());
        certificate.revocation_reason = Some(reason.as_str().to_string());

        Ok(certificate.clone())
    })
}

/// A configuration package. Demo mode has no package builder, so this is a
/// valid but empty zip named the way the real one would be.
pub fn config_package(request: &ConfigPackageRequest) -> Result<Download, ApiError> {
    if request.include_client_cert {
        return Err(ApiError::Server(
            "This installation holds no private key for that account, so there is no keystore \
             to package. Build the enrolment variant instead."
                .to_string(),
        ));
    }

    Ok(Download {
        filename: format!("{}-{}.zip", request.username, request.variant.as_str()),
        mime: "application/zip".to_string(),
        bytes: empty_zip(),
    })
}

/// An ACME certificate whose last order failed.
fn acme_status() -> TlsStatus {
    TlsStatus {
        source: TlsSource::Acme,
        state: TlsCertificateState::Failed,
        domains: vec!["tak.example.com".to_string()],
        not_before: Some(ago(60 * 24 * 26)),
        not_after: Some(ago(-60 * 24 * 64)),
        renews_at: Some(ago(-60 * 24 * 34)),
        loaded_at: None,
        cert_file: None,
        key_file: None,
        note: None,
        directory: Some("https://acme-v02.api.letsencrypt.org/directory".to_string()),
        challenge: Some("http-01".to_string()),
        attempts: 3,
        last_attempt_at: Some(ago(95)),
        last_error: Some(
            "urn:ietf:params:acme:error:dns — no valid A record found for tak.example.com"
                .to_string(),
        ),
    }
}

/// What the TLS card shows.
pub fn tls_status() -> TlsStatus {
    TLS.with(|held| held.borrow().clone())
}

/// Orders a certificate now.
///
/// Demo mode has no authority behind it, so the order simply succeeds — which
/// is the state an operator is trying to reach and the one the card has to be
/// able to render afterwards.
pub fn renew_tls() -> Result<TlsStatus, ApiError> {
    TLS.with(|held| {
        let mut status = held.borrow_mut();

        status.state = TlsCertificateState::Valid;
        status.attempts = 0;
        status.last_error = None;
        status.last_attempt_at = Some(chrono::Utc::now());
        status.not_before = Some(chrono::Utc::now());
        status.not_after = Some(ago(-60 * 24 * 90));
        status.renews_at = Some(ago(-60 * 24 * 60));

        Ok(status.clone())
    })
}

pub fn file_settings() -> FileSettings {
    FILES.with(|held| *held.borrow())
}

pub fn set_file_settings(upload_size_limit_mb: u32) -> Result<FileSettings, ApiError> {
    if upload_size_limit_mb == 0 {
        return Err(ApiError::Server(
            "An upload limit of zero would refuse every upload.".to_string(),
        ));
    }

    FILES.with(|held| {
        let mut settings = held.borrow_mut();
        settings.upload_size_limit_mb = upload_size_limit_mb;

        Ok(*settings)
    })
}

pub fn marti_settings() -> MartiSettings {
    MartiSettings {
        public_host: Some("tak.example.com".to_string()),
        allow_all_origins: false,
    }
}

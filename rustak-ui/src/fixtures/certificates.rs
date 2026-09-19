//! Demo data for certificates, configuration packages and the three settings
//! the console reads.
//!
//! The certificates line up with the `last_certificate_id` on each fixture
//! device, so the Devices tab shows a fingerprint and an expiry rather than
//! "Certificate #11" — which is what the identity brief could not do before
//! the endpoint existed.
//!
//! The TLS status opens on an ACME order that has *failed*, because that is the
//! state the card exists for: an operator opens it when something is wrong, and
//! a demo showing only the healthy case would never have exercised the error or
//! the Renew button beside it. The other three sources render different cards
//! and no URL could reach them while that one was the only fixture, so
//! `?demo&tls=files`, `&tls=internal` and `&tls=none` each select their own.

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
    static TLS: RefCell<TlsStatus> =
        RefCell::new(status_for(super::demo_flag("tls").as_deref()));
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

/// Which TLS status `?demo&tls=…` asks the card to render.
///
/// One baked-in status can only show one source, and each source renders a
/// different card: `files` has three rows, a note and a differently worded
/// error nothing else reaches; `internal` has neither the ACME rows nor the
/// button; `none` is a warning and nothing else. So the source is a flag, and
/// anything else — including no flag at all — keeps the failed ACME order this
/// has always opened on.
fn status_for(flag: Option<&str>) -> TlsStatus {
    match flag {
        Some("files") => files_status(),
        Some("internal") => internal_status(),
        Some("none") => TlsStatus::fixed(TlsSource::None),
        _ => acme_status(),
    }
}

/// A `files` listener still waiting for the pair a sidecar will write.
///
/// The state M2-13 made ordinary: the listener is up and presenting this
/// installation's own certificate, which no client trusts, and until the note
/// M2-14 added to the card nothing but the log said so.
fn files_status() -> TlsStatus {
    TlsStatus {
        state: TlsCertificateState::Missing,
        domains: vec!["tak.example.com".to_string()],
        cert_file: Some("/var/lib/rustak/tls/fullchain.pem".to_string()),
        key_file: Some("/var/lib/rustak/tls/privkey.pem".to_string()),
        note: Some(
            "Waiting for the certificate files to appear. Until they do this listener is \
             presenting a certificate from this installation's own authority, which no \
             client trusts."
                .to_string(),
        ),
        ..TlsStatus::fixed(TlsSource::Files)
    }
}

/// The certificate this installation issued itself.
///
/// Nothing fetches it while the server runs, so it has no renewal, no error
/// and no button — the card is down to the pill, the names and the dates.
fn internal_status() -> TlsStatus {
    TlsStatus {
        domains: vec!["tak.example.com".to_string(), "10.0.0.12".to_string()],
        not_before: Some(ago(60 * 24 * 12)),
        not_after: Some(ago(-60 * 24 * 353)),
        ..TlsStatus::fixed(TlsSource::Internal)
    }
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
        fetched(&mut status);
        Ok(status.clone())
    })
}

/// What a successful fetch does to a status, whichever button asked for it.
///
/// Separate from the thread-local above so it can be tested: reaching `TLS`
/// reads the browser's location, and a host test has no browser.
fn fetched(status: &mut TlsStatus) {
    status.state = TlsCertificateState::Valid;
    status.attempts = 0;
    status.last_error = None;
    // Whatever the listener was waiting for, it is not waiting for it now.
    status.note = None;
    status.not_before = Some(ago(0));
    status.not_after = Some(ago(-60 * 24 * 90));

    // The two sources answer the same button with different words, and the
    // card reads different fields for each: a re-read is a read, and an order
    // is an attempt that schedules the next one.
    match status.source {
        TlsSource::Files => status.loaded_at = Some(ago(0)),
        _ => {
            status.last_attempt_at = Some(ago(0));
            status.renews_at = Some(ago(-60 * 24 * 60));
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every source the card renders, and the flag that asks for it.
    const FLAGS: &[(&str, TlsSource)] = &[
        ("files", TlsSource::Files),
        ("internal", TlsSource::Internal),
        ("none", TlsSource::None),
    ];

    #[test]
    fn every_source_the_card_renders_can_be_asked_for_by_name() {
        for (flag, source) in FLAGS {
            assert_eq!(
                status_for(Some(flag)).source,
                *source,
                "?demo&tls={flag} should show a {source:?} listener",
            );
        }

        // No flag, and a flag naming nothing, both keep the failed order the
        // demo has always opened on rather than rendering an empty card.
        assert_eq!(status_for(None).source, TlsSource::Acme);
        assert_eq!(status_for(Some("")).source, TlsSource::Acme);
        assert_eq!(status_for(Some("Files")).source, TlsSource::Acme);
    }

    #[test]
    fn the_files_listener_shows_what_only_a_files_listener_has() {
        // The three rows, the note and the warning tone are the whole of what
        // M2-14 added and nothing else in demo mode reaches.
        let status = files_status();

        assert_eq!(status.state, TlsCertificateState::Missing);
        assert!(status.cert_file.is_some());
        assert!(status.key_file.is_some());
        assert!(
            status.loaded_at.is_none(),
            "a pair that has never appeared has never been read",
        );
        assert!(status.note.is_some());
        assert!(
            status.needs_attention(),
            "a listener presenting a certificate no client trusts is not a healthy one",
        );
    }

    #[test]
    fn re_reading_the_files_stops_the_card_saying_it_is_still_waiting() {
        let mut status = files_status();
        fetched(&mut status);

        assert_eq!(status.state, TlsCertificateState::Valid);
        assert!(status.note.is_none(), "it is not waiting for them any more");
        assert!(status.loaded_at.is_some(), "a re-read is a read");
        assert!(!status.needs_attention());
    }

    #[test]
    fn an_order_that_succeeds_schedules_the_next_one() {
        let mut status = acme_status();
        fetched(&mut status);

        assert_eq!(status.attempts, 0);
        assert!(status.last_error.is_none());
        assert!(status.renews_at.is_some());
        assert!(
            status.loaded_at.is_none(),
            "nothing was read off disk, so the files row would be a lie",
        );
    }

    #[test]
    fn the_certificate_this_installation_issued_itself_is_not_a_problem() {
        let status = internal_status();

        assert_eq!(status.state, TlsCertificateState::Valid);
        assert!(!status.needs_attention());
        assert!(
            status.directory.is_none() && status.renews_at.is_none(),
            "nobody orders or renews this one, so the ACME rows would be empty",
        );
    }
}

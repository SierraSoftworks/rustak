//! `/api/v1/certificates`: what this installation's authority has issued, and
//! taking one back.
//!
//! Reading is "yours, or anybody's if you administer the installation", which
//! is [`super::subject`]'s rule and the same one credentials and devices
//! follow. Revoking is administrative: it drops the live connections holding
//! the certificate and cannot be undone, and the self-service way to take back
//! your own access is `DELETE /api/v1/credentials/{id}`, which revokes the
//! certificates that credential bought.
//!
//! # Why revoking is a `POST` to a sub-path
//!
//! `DELETE /certificates/{id}` would read as "forget this certificate", and
//! forgetting one is the opposite of revoking it: with `require_known_cert` on,
//! the register *is* the list of who may connect, so a deleted row is a
//! certificate nothing can refuse a second time. The row therefore stays and
//! gains a revocation, and the verb says which of the two happened.
//!
//! # The reason is not decoration
//!
//! "Revoked" on its own does not tell an administrator six months later
//! whether a device was lost or a certificate simply replaced, and the two lead
//! to different actions. It is stored on the row, written to the audit log and
//! returned on the certificate.

use std::collections::HashMap;

use actix_web::web;
use chrono::Utc;
use rustak_api::{
    Certificate, CertificateId, CertificateKind, CertificateState, RevocationReason,
    RevokeCertificateRequest,
};

use crate::db::Database;
use crate::db::repos::{CertificateFilter, CertificateRow, Page};
use crate::identity::devices;
use crate::pki::RevokeReason;
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::{Administrative, Authenticated};
use super::subject::{self, failed};

/// How many certificates one page carries when the caller does not say.
const PAGE_SIZE: u32 = 100;

/// The most one page may carry however loudly the caller asks.
const MAX_PAGE_SIZE: u32 = 200;

/// What a listing may be narrowed by.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    /// Whose certificates to list. Absent means every certificate for an
    /// administrator and the caller's own for anybody else.
    #[serde(default)]
    pub username: Option<Username>,

    /// Only the certificates issued to one device.
    #[serde(default)]
    pub device_uid: Option<DeviceUid>,

    /// `active`, `revoked` or `expired`.
    #[serde(default)]
    pub state: Option<CertificateState>,

    /// `ca`, `server`, `client` or `service`.
    #[serde(default)]
    pub kind: Option<CertificateKind>,

    /// Which page, counted from zero.
    #[serde(default)]
    pub page: Option<u32>,

    /// How many rows. Capped at the module's own ceiling, so a caller
    /// asking for a million is answered with a page rather than refused.
    #[serde(default)]
    pub limit: Option<u32>,
}

impl ListQuery {
    /// The window this query asks for.
    fn page(&self) -> Page {
        let limit = self.limit.unwrap_or(PAGE_SIZE).clamp(1, MAX_PAGE_SIZE);

        Page::at(self.page.unwrap_or(0).saturating_mul(limit), limit)
    }
}

/// `GET /api/v1/certificates`.
///
/// # Errors
///
/// A `403` when somebody names an account or a device that is not theirs, a
/// `404` when an administrator names one that is not here, and a `500` when a
/// read fails.
pub async fn list(
    context: web::Data<AppContext>,
    query: web::Query<ListQuery>,
    caller: Authenticated,
) -> ApiResult {
    let mut filter = CertificateFilter {
        kind: query.kind,
        state: query.state,
        ..CertificateFilter::default()
    };

    // An administrator who named nobody is asking about the installation; for
    // everybody else an absent name means their own, which is what `resolve`
    // answers — and it refuses a stranger's rather than quietly widening.
    if query.username.is_some() || !caller.principal.is_admin {
        let subject = subject::resolve(&context, &caller, query.username.as_ref()).await?;

        filter.user_id = Some(subject.user.id);
    }

    if let Some(uid) = &query.device_uid {
        filter.device_id = Some(device(&context, uid, &caller).await?);
    }

    let rows = context
        .db()
        .certificates()
        .list(filter, Utc::now(), query.page())
        .await
        .map_err(|err| failed(&context, &err))?;

    let listed = to_dtos(context.db(), &rows)
        .await
        .map_err(|err| failed(&context, &err))?;

    Ok(json_ok(&listed))
}

/// `GET /api/v1/certificates/{id}`.
///
/// # Errors
///
/// A `403` when the certificate is somebody else's, a `404` when it is not
/// here, and a `500` when a read fails.
pub async fn get(
    context: web::Data<AppContext>,
    id: web::Path<i64>,
    caller: Authenticated,
) -> ApiResult {
    let row = load(&context, CertificateId::new(id.into_inner()), &caller).await?;

    let certificate = to_dto(context.db(), &row)
        .await
        .map_err(|err| failed(&context, &err))?;

    Ok(json_ok(&certificate))
}

/// `POST /api/v1/certificates/{id}/revoke`.
///
/// Takes the certificate back, refuses it at the next handshake, and drops the
/// stream connections already holding it — the last through the revocation
/// hook the stream listener registers, so a revoked device does not stay
/// connected until it happens to reconnect.
///
/// # Errors
///
/// A `404` when the certificate is not here, a `409` when it has already been
/// revoked, a `503` when this installation has no authority, and a `500` when
/// a write fails.
pub async fn revoke(
    context: web::Data<AppContext>,
    id: web::Path<i64>,
    body: web::Json<RevokeCertificateRequest>,
    caller: Administrative,
) -> ApiResult {
    let id = CertificateId::new(id.into_inner());
    let row = context
        .db()
        .certificates()
        .get(id)
        .await
        .map_err(|err| failed(&context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no such certificate."))?;

    let pki = context.pki().map_err(|err| {
        context.session().record_human_error(&err);

        ApiError::new(
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "This installation has no certificate authority yet.",
        )
    })?;

    // `Pki::revoke` writes the row, updates the cache the TLS verifier reads,
    // runs the hooks that close live connections, and records the audit entry
    // naming the actor — so there is nothing left for this handler to write.
    let revoked = pki
        .revoke(
            context.db(),
            &row.fingerprint,
            reason(body.reason),
            Some(&caller.user.username),
        )
        .await
        .map_err(|err| failed(&context, &err))?;

    if !revoked {
        return Err(ApiError::conflict(
            "That certificate had already been revoked.",
        ));
    }

    let updated = context
        .db()
        .certificates()
        .get(id)
        .await
        .map_err(|err| failed(&context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no such certificate."))?;

    let certificate = to_dto(context.db(), &updated)
        .await
        .map_err(|err| failed(&context, &err))?;

    Ok(json_ok(&certificate))
}

/// Reads a certificate and confirms the caller may see it.
///
/// A certificate belonging to nobody — the authority's own, a listener's — is
/// administrative, because there is no owner for the self-service rule to let
/// through.
async fn load(
    context: &AppContext,
    id: CertificateId,
    caller: &Authenticated,
) -> Result<CertificateRow, ApiError> {
    let row = context
        .db()
        .certificates()
        .get(id)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no such certificate."))?;

    match row.user_id {
        Some(owner) => subject::owns(caller, owner)?,
        None if caller.principal.is_admin => true,
        None => {
            return Err(ApiError::forbidden(
                "Only an administrator may read this server's own certificates.",
            ));
        }
    };

    Ok(row)
}

/// The device a `device_uid` names, refusing a stranger's.
async fn device(
    context: &AppContext,
    uid: &DeviceUid,
    caller: &Authenticated,
) -> Result<DeviceId, ApiError> {
    let row = devices::get(context.db(), uid)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no such device."))?;

    subject::owns(caller, row.user_id)?;

    Ok(row.id)
}

/// What the API's reason means to the authority.
fn reason(reason: RevocationReason) -> RevokeReason {
    match reason {
        RevocationReason::UserRequest => RevokeReason::UserRequest,
        RevocationReason::DeviceLost => RevokeReason::DeviceLost,
        RevocationReason::Superseded => RevokeReason::Superseded,
        RevocationReason::AdminAction => RevokeReason::AdminAction,
        RevocationReason::CredentialRevoked => RevokeReason::CredentialRevoked,
        RevocationReason::UserDisabled => RevokeReason::UserDisabled,
    }
}

/// One certificate as the API describes it.
async fn to_dto(db: &Database, row: &CertificateRow) -> Result<Certificate, Error> {
    let username = match row.user_id {
        Some(id) => db.users().get(id).await?.map(|user| user.username),
        None => None,
    };

    Ok(render(row, username, device_uid(db, row).await?))
}

/// Several certificates as the API describes them, resolving the owners in one
/// read rather than one per row.
async fn to_dtos(db: &Database, rows: &[CertificateRow]) -> Result<Vec<Certificate>, Error> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    let owners: HashMap<UserId, Username> = devices::usernames(db).await?;
    let mut listed = Vec::with_capacity(rows.len());

    for row in rows {
        let username = row.user_id.and_then(|id| owners.get(&id).cloned());

        listed.push(render(row, username, device_uid(db, row).await?));
    }

    Ok(listed)
}

/// The device a certificate names, from the identifier the client sent where
/// there is one and from the device row otherwise.
///
/// The recorded `client_uid` is the same string the device calls itself by, so
/// the common case costs no read at all.
async fn device_uid(db: &Database, row: &CertificateRow) -> Result<Option<DeviceUid>, Error> {
    if let Some(uid) = row
        .client_uid
        .as_deref()
        .and_then(|uid| DeviceUid::parse(uid).ok())
    {
        return Ok(Some(uid));
    }

    let Some(id) = row.device_id else {
        return Ok(None);
    };

    Ok(db.devices().get(id).await?.map(|device| device.uid))
}

/// The row as a DTO, once its two references have been resolved.
///
/// The DER and the sealed private key are not here and there is nowhere on
/// [`Certificate`] to put them.
fn render(
    row: &CertificateRow,
    username: Option<Username>,
    device_uid: Option<DeviceUid>,
) -> Certificate {
    Certificate {
        id: row.id,
        kind: row.kind,
        serial: row.serial_hex.clone(),
        fingerprint: row.fingerprint.clone(),
        subject_cn: row.subject_cn.clone(),
        username,
        san: row.san.clone(),
        not_before: row.not_before,
        not_after: row.not_after,
        device_uid,
        revoked_at: row.revoked_at,
        revocation_reason: row.revocation_reason.clone(),
        source: row.source,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::CertificateSource;

    use super::*;
    use crate::db::repos::{DeviceSeen, NewCertificate};
    use crate::identity::devices;
    use crate::pki::{CertRejection, Pki};
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    /// A certificate row, as an enrolment would have left one.
    ///
    /// Not signed by anything: every assertion here is about the register and
    /// the rules over it, and `pki::testing` already proves that what the
    /// authority issues is what rustls accepts.
    async fn issue(
        server: &TestServer,
        username: &str,
        uid: Option<&str>,
        not_after: &str,
    ) -> CertificateRow {
        let user = server
            .db()
            .users()
            .get_by_username(&Username::parse(username).unwrap())
            .await
            .unwrap()
            .unwrap();

        let device = match uid {
            Some(uid) => Some(
                devices::upsert_seen(
                    server.db(),
                    &DeviceUid::parse(uid).unwrap(),
                    user.id,
                    DeviceSeen::default(),
                )
                .await
                .unwrap(),
            ),
            None => None,
        };

        let serial = server
            .db()
            .certificates()
            .list(CertificateFilter::default(), Utc::now(), Page::first(500))
            .await
            .unwrap()
            .len();

        server
            .db()
            .certificates()
            .create(NewCertificate {
                kind: CertificateKind::Client,
                source: CertificateSource::Enrollment,
                issued_via: Some("enroll_v2_json".into()),
                serial_hex: format!("{:032x}", serial + 1),
                fingerprint: format!("{:064x}", serial + 1),
                subject_cn: username.into(),
                san: Vec::new(),
                user_id: Some(user.id),
                device_id: device.as_ref().map(|row| row.id),
                client_uid: uid.map(str::to_owned),
                credential_id: None,
                issuer_id: None,
                der: vec![serial as u8 + 1],
                key_sealed: None,
                not_before: "2020-01-01T00:00:00Z".parse().unwrap(),
                not_after: not_after.parse().unwrap(),
            })
            .await
            .unwrap()
    }

    /// Far enough ahead that nothing under test has run out.
    const LATER: &str = "2099-01-01T00:00:00Z";

    /// Long gone.
    const EARLIER: &str = "2021-01-01T00:00:00Z";

    #[actix_web::test]
    async fn a_person_sees_their_own_and_an_administrator_sees_everybodys() {
        let server = TestServer::start().await;
        let (_, grace) = server.signed_in("grace", false).await;
        let (_, ada) = server.signed_in("ada", true).await;
        issue(&server, "grace", Some("ANDROID-1"), LATER).await;
        issue(&server, "ada", None, LATER).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let hers: Vec<Certificate> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/certificates")
                .insert_header(("authorization", bearer(&grace)))
                .to_request(),
        )
        .await;

        assert_eq!(hers.len(), 1);
        assert_eq!(hers[0].username.as_ref().unwrap().as_str(), "grace");
        assert_eq!(hers[0].device_uid.as_ref().unwrap().as_str(), "ANDROID-1");
        assert_eq!(hers[0].fingerprint.len(), 64);
        assert!(hers[0].revoked_at.is_none());

        let all: Vec<Certificate> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/certificates")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(all.len(), 2);

        let narrowed: Vec<Certificate> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/certificates?username=grace")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(narrowed.len(), 1);
    }

    #[actix_web::test]
    async fn the_three_states_partition_the_register() {
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;
        issue(&server, "ada", None, LATER).await;
        let expired = issue(&server, "ada", None, EARLIER).await;
        let revoked = issue(&server, "ada", None, LATER).await;

        server
            .db()
            .certificates()
            .revoke(
                revoked.id,
                crate::db::repos::RevocationDetails {
                    reason: "device_lost".into(),
                    by: None,
                },
            )
            .await
            .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        for (state, expected) in [("active", 1), ("expired", 1), ("revoked", 1)] {
            let listed: Vec<Certificate> = test::call_and_read_body_json(
                &app,
                test::TestRequest::get()
                    .uri(&format!("/api/v1/certificates?state={state}"))
                    .insert_header(("authorization", bearer(&ada)))
                    .to_request(),
            )
            .await;

            assert_eq!(listed.len(), expected, "{state}");
        }

        let taken_back: Vec<Certificate> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/certificates?state=revoked")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(taken_back[0].id, revoked.id);
        assert_eq!(
            taken_back[0].revocation_reason.as_deref(),
            Some("device_lost")
        );
        assert_eq!(taken_back[0].state(Utc::now()), CertificateState::Revoked);

        let ran_out: Vec<Certificate> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/certificates?state=expired")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(ran_out[0].id, expired.id);
    }

    #[actix_web::test]
    async fn a_device_narrows_the_listing_and_a_strangers_device_is_refused() {
        let server = TestServer::start().await;
        let (_, grace) = server.signed_in("grace", false).await;
        let (_, ada) = server.signed_in("ada", true).await;
        issue(&server, "grace", Some("ANDROID-1"), LATER).await;
        issue(&server, "grace", Some("ANDROID-2"), LATER).await;
        issue(&server, "ada", Some("LAPTOP-1"), LATER).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let one: Vec<Certificate> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/certificates?device_uid=ANDROID-1")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(one.len(), 1);
        assert_eq!(one[0].device_uid.as_ref().unwrap().as_str(), "ANDROID-1");

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/certificates?device_uid=LAPTOP-1")
                    .insert_header(("authorization", bearer(&grace)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::FORBIDDEN,
        );

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/certificates?username=ada")
                    .insert_header(("authorization", bearer(&grace)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::FORBIDDEN,
        );
    }

    #[actix_web::test]
    async fn one_certificate_is_yours_or_administrative_and_the_servers_own_is_neither() {
        let server = TestServer::start().await;
        let (_, grace) = server.signed_in("grace", false).await;
        let (_, ada) = server.signed_in("ada", true).await;
        let hers = issue(&server, "grace", Some("ANDROID-1"), LATER).await;
        let his = issue(&server, "ada", None, LATER).await;

        // A certificate belonging to nobody, as the authority's own does.
        let ours = server
            .db()
            .certificates()
            .create(NewCertificate {
                kind: CertificateKind::Server,
                source: CertificateSource::Internal,
                issued_via: None,
                serial_hex: "ff".repeat(16),
                fingerprint: "f".repeat(64),
                subject_cn: "localhost".into(),
                san: vec!["localhost".into()],
                user_id: None,
                device_id: None,
                client_uid: None,
                credential_id: None,
                issuer_id: None,
                der: vec![9],
                key_sealed: None,
                not_before: "2020-01-01T00:00:00Z".parse().unwrap(),
                not_after: LATER.parse().unwrap(),
            })
            .await
            .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        let mine: Certificate = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri(&format!("/api/v1/certificates/{}", hers.id.get()))
                .insert_header(("authorization", bearer(&grace)))
                .to_request(),
        )
        .await;

        assert_eq!(mine.id, hers.id);

        for (session, id, expected) in [
            (&grace, his.id, StatusCode::FORBIDDEN),
            (&grace, ours.id, StatusCode::FORBIDDEN),
            (&ada, ours.id, StatusCode::OK),
            (&ada, hers.id, StatusCode::OK),
        ] {
            assert_eq!(
                test::call_service(
                    &app,
                    test::TestRequest::get()
                        .uri(&format!("/api/v1/certificates/{}", id.get()))
                        .insert_header(("authorization", bearer(session)))
                        .to_request(),
                )
                .await
                .status(),
                expected,
                "certificate {}",
                id.get(),
            );
        }

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/certificates/9999")
                    .insert_header(("authorization", bearer(&ada)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND,
        );
    }

    /// A server with a real authority, so that revocation reaches the cache the
    /// TLS verifier consults.
    async fn with_authority() -> (TestServer, Arc<Pki>) {
        let server = TestServer::start_with(|config| {
            // Elliptic curve rather than the RSA default: loading an authority
            // generates two keys, and RSA would dominate the test's run time
            // without proving anything `pki::` does not already prove.
            config.pki.key_type = crate::pki::KeyType::EcdsaP256;
        })
        .await;

        let config = server.config();
        let pki = Pki::load(
            server.db(),
            server.secrets(),
            &config.pki,
            &config.server.data_dir,
            &["localhost".to_string()],
            &[],
        )
        .await
        .unwrap();

        server.context.install_pki(Arc::clone(&pki)).unwrap();

        (server, pki)
    }

    #[actix_web::test]
    async fn revoking_refuses_the_certificate_at_the_next_handshake_and_closes_what_holds_it() {
        let (server, pki) = with_authority().await;
        let (_, ada) = server.signed_in("ada", true).await;
        let row = issue(&server, "ada", Some("ANDROID-1"), LATER).await;

        // Standing in for the stream listener's own hook, which is what drops
        // the connections already holding the certificate.
        let closed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&closed);
        pki.revocations().on_revoked(Arc::new(move |fingerprint| {
            recorder.lock().unwrap().push(fingerprint.to_owned());
        }));

        // What `Pki::enroll` does the moment it has written the row, so that a
        // certificate works immediately rather than after the next reload.
        pki.revocations().note_issued(&row.fingerprint);

        assert!(pki.revocations().is_acceptable(&row.fingerprint).is_ok());

        let app = test::init_service(App::new().configure(server.app())).await;

        let revoked: Certificate = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri(&format!("/api/v1/certificates/{}/revoke", row.id.get()))
                .insert_header(("authorization", bearer(&ada)))
                .set_json(serde_json::json!({ "reason": "device_lost" }))
                .to_request(),
        )
        .await;

        assert!(revoked.revoked_at.is_some());
        assert_eq!(revoked.revocation_reason.as_deref(), Some("device_lost"));
        assert_eq!(revoked.state(Utc::now()), CertificateState::Revoked);

        assert_eq!(
            pki.revocations().is_acceptable(&row.fingerprint),
            Err(CertRejection::Revoked),
            "the handshake must refuse it from now on",
        );
        assert_eq!(
            closed.lock().unwrap().as_slice(),
            std::slice::from_ref(&row.fingerprint),
        );

        assert!(
            server
                .db()
                .audit(crate::db::AuditQuery::recent(20))
                .await
                .unwrap()
                .iter()
                .any(|record| record.action == "certificate.revoked"
                    && record.actor.as_deref() == Some("ada")),
        );
    }

    #[actix_web::test]
    async fn revoking_twice_is_a_conflict_and_only_an_administrator_may_do_it_at_all() {
        let (server, _) = with_authority().await;
        let (_, ada) = server.signed_in("ada", true).await;
        let (_, grace) = server.signed_in("grace", false).await;
        let row = issue(&server, "grace", Some("ANDROID-1"), LATER).await;

        let app = test::init_service(App::new().configure(server.app())).await;
        let request = |session: &rustak_api::TokenResponse, id: i64| {
            test::TestRequest::post()
                .uri(&format!("/api/v1/certificates/{id}/revoke"))
                .insert_header(("authorization", bearer(session)))
                .set_json(serde_json::json!({}))
                .to_request()
        };

        // Not even over her own certificate: revocation drops live connections
        // and cannot be undone.
        assert_eq!(
            test::call_service(&app, request(&grace, row.id.get()))
                .await
                .status(),
            StatusCode::FORBIDDEN,
        );

        assert_eq!(
            test::call_service(&app, request(&ada, row.id.get()))
                .await
                .status(),
            StatusCode::OK,
        );
        assert_eq!(
            test::call_service(&app, request(&ada, row.id.get()))
                .await
                .status(),
            StatusCode::CONFLICT,
        );
        assert_eq!(
            test::call_service(&app, request(&ada, 9999)).await.status(),
            StatusCode::NOT_FOUND,
        );

        let stored = server
            .db()
            .certificates()
            .get(row.id)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(stored.revocation_reason.as_deref(), Some("admin_action"));
        assert_eq!(stored.revoked_by.as_deref(), Some("ada"));
    }

    #[actix_web::test]
    async fn a_page_is_capped_however_loudly_it_is_asked_for() {
        let query = ListQuery {
            limit: Some(100_000),
            page: Some(2),
            ..ListQuery::default()
        };

        assert_eq!(query.page().limit, MAX_PAGE_SIZE);
        assert_eq!(query.page().offset, MAX_PAGE_SIZE * 2);
        assert_eq!(ListQuery::default().page().limit, PAGE_SIZE);
    }
}

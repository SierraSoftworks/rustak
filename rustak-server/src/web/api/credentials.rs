//! `/api/v1/credentials`: minting, listing and revoking the secrets a client
//! presents.
//!
//! Self-service for anybody signed in, and administrative over anybody else —
//! the one rule lives in [`super::subject`]. A person mints an enrolment token
//! for their own phone without an administrator being involved, which is the
//! whole point of the QR flow.
//!
//! # The secret is in exactly one response
//!
//! [`create`] returns it; nothing else ever can, because only its argon2 hash
//! was stored. That is why `GET /credentials/{id}/enroll-url` hands back the
//! URL with `{token}` still in it and lets the UI fill in whichever mint
//! response it is still holding, rather than composing a working link server
//! side. An endpoint that could re-emit the URL would be an endpoint proving
//! the server had kept the secret.
//!
//! # `all=true` is a different question from an absent name
//!
//! An absent `username` means "mine", because that is what a person's own page
//! asks and defaulting an administrator to the installation's would make the
//! same request mean different things depending on who sent it. So the
//! installation-wide listing — what an operator auditing outstanding client
//! passwords wants — is asked for explicitly, is administrative, and is paged,
//! because it is the one listing here with no natural bound.

use actix_web::{HttpResponse, web};
use rustak_api::{
    AuditCategory, AuditOutcome, CreateCredentialRequest, Credential, CredentialCreated,
    CredentialId, CredentialKind, EnrollTemplate,
};

use crate::db::AuditEntry;
use crate::db::repos::Page;
use crate::identity::{credentials, devices, secret_cache::VerifiedSecretCache, settings};
use crate::prelude::*;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Authenticated;
use super::subject::{self, Subject, failed};

/// How many credentials one page of the installation-wide listing carries.
const PAGE_SIZE: u32 = 100;

/// The most one page may carry however loudly the caller asks.
const MAX_PAGE_SIZE: u32 = 200;

/// What a listing may be narrowed by.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    /// Whose credentials to list. Absent means the caller's own; only an
    /// administrator may name somebody else.
    #[serde(default)]
    pub username: Option<Username>,

    /// Whether to include the ones already revoked or spent.
    #[serde(default)]
    pub include_revoked: bool,

    /// Every credential this installation holds. Administrative, and not
    /// combinable with a name.
    #[serde(default)]
    pub all: bool,

    /// Which page of the installation-wide listing, counted from zero.
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

/// `GET /api/v1/credentials`.
///
/// # Errors
///
/// A `400` when `all=true` is combined with a name, a `403` when somebody
/// names an account that is not theirs or asks for the installation's without
/// administering it, a `404` when an administrator names one that is not here,
/// and a `500` when the read fails.
pub async fn list(
    context: web::Data<AppContext>,
    query: web::Query<ListQuery>,
    caller: Authenticated,
) -> ApiResult {
    if query.all {
        return everybodys(&context, &query, &caller).await;
    }

    let subject = subject::resolve(&context, &caller, query.username.as_ref()).await?;

    let rows = context
        .db()
        .credentials()
        .list_for_user(subject.user.id, query.include_revoked)
        .await
        .map_err(|err| failed(&context, &err))?;

    let held: Vec<Credential> = rows
        .iter()
        .map(|row| credentials::to_dto(row, owner(&subject)))
        .collect();

    Ok(json_ok(&held))
}

/// Every credential this installation holds, for an administrator.
///
/// Metadata only, exactly as the per-account listing is: no secret, no hash,
/// no hint and no length, because the row does not carry one to return.
async fn everybodys(context: &AppContext, query: &ListQuery, caller: &Authenticated) -> ApiResult {
    if !caller.principal.is_admin {
        return Err(ApiError::forbidden(
            "Only an administrator may list this installation's credentials.",
        ));
    }

    if query.username.is_some() {
        return Err(ApiError::bad_request(
            "Ask for one account's credentials or for every one of them, not both.",
        ));
    }

    let rows = context
        .db()
        .credentials()
        .list_all(query.include_revoked, query.page())
        .await
        .map_err(|err| failed(context, &err))?;

    let owners = devices::usernames(context.db())
        .await
        .map_err(|err| failed(context, &err))?;

    // A credential whose account has gone is left out rather than rendered
    // without an owner, for the reason the device listing gives.
    let held: Vec<Credential> = rows
        .iter()
        .filter_map(|row| {
            owners
                .get(&row.user_id)
                .map(|username| credentials::to_dto(row, Some(username.clone())))
        })
        .collect();

    Ok(json_ok(&held))
}

/// `POST /api/v1/credentials`, which is the one response carrying a secret.
///
/// # Errors
///
/// A `400` when the request asks for something the installation does not mint,
/// a `403` when somebody mints for an account that is not theirs, and a `500`
/// when hashing or the write fails.
pub async fn create(
    context: web::Data<AppContext>,
    body: web::Json<CreateCredentialRequest>,
    caller: Authenticated,
) -> ApiResult {
    let body = body.into_inner();
    let subject = subject::resolve(&context, &caller, body.username.as_ref()).await?;

    let minted = credentials::mint(
        context.db(),
        &context.config().auth,
        &subject.user,
        credentials::MintRequest {
            expires_in: body.expires_in_days.map(days),
            max_uses: body.max_uses,
            ..credentials::MintRequest::new(body.kind, &body.label, &caller.user.username)
        },
    )
    .await
    .map_err(|err| failed(&context, &err))?;

    record(
        &context,
        "credential.minted",
        &caller,
        &subject,
        serde_json::json!({
            "credential": minted.credential.id,
            "kind": body.kind.as_str(),
            "label": minted.credential.label,
            "expires_at": minted.credential.expires_at,
        }),
    )
    .await;

    let enroll_url = match body.kind {
        CredentialKind::EnrollmentToken => host(&context)
            .await?
            .map(|host| credentials::enroll_url(&host, subject.username(), minted.secret.expose())),
        _ => None,
    };

    // The only place a secret is serialised. `CredentialCreated` redacts it in
    // `Debug`, so a handler or middleware that logs its response cannot leak it.
    Ok(json_ok(&CredentialCreated {
        credential: credentials::to_dto(&minted.credential, owner(&subject)),
        secret: minted.secret.expose().to_string(),
        enroll_url,
    }))
}

/// `DELETE /api/v1/credentials/{id}`.
///
/// Revoking cascades to the certificates issued with the credential, because a
/// certificate outliving the secret that bought it is exactly the gap an
/// enrolment token is meant to close.
///
/// # Errors
///
/// A `403` when the credential is somebody else's, a `404` when it is not here
/// or was already revoked, and a `500` when a write fails.
pub async fn remove(
    context: web::Data<AppContext>,
    id: web::Path<i64>,
    caller: Authenticated,
) -> ApiResult {
    let (row, subject) = load(&context, CredentialId::new(id.into_inner()), &caller).await?;

    let revoked = credentials::revoke(
        context.db(),
        row.id,
        &caller.user.username,
        VerifiedSecretCache::shared(),
    )
    .await
    .map_err(|err| failed(&context, &err))?;

    if !revoked {
        return Err(ApiError::not_found("That credential has already gone."));
    }

    record(
        &context,
        "credential.revoked",
        &caller,
        &subject,
        serde_json::json!({
            "credential": row.id,
            "kind": row.kind.as_str(),
            "label": row.label,
        }),
    )
    .await;

    Ok(HttpResponse::NoContent().finish())
}

/// `GET /api/v1/credentials/{id}/enroll-url`.
///
/// # Errors
///
/// A `400` when the credential is not one a client enrols with, a `403` when it
/// is somebody else's, a `404` when it is not here, a `409` when the
/// installation does not yet know its own host name, and a `500` when a read
/// fails.
pub async fn enroll_template(
    context: web::Data<AppContext>,
    id: web::Path<i64>,
    caller: Authenticated,
) -> ApiResult {
    let (row, subject) = load(&context, CredentialId::new(id.into_inner()), &caller).await?;

    if row.kind != CredentialKind::EnrollmentToken {
        return Err(ApiError::bad_request(
            "Only an enrolment token goes in a QR code.",
        ));
    }

    let host = host(&context)
        .await?
        .ok_or_else(|| ApiError::conflict("This server does not know its own host name yet."))?;

    Ok(json_ok(&EnrollTemplate {
        credential: row.id,
        url_template: credentials::enroll_url_template(&host, subject.username()),
        host,
        username: subject.username().clone(),
    }))
}

/// Reads a credential and confirms the caller may act on it.
async fn load(
    context: &AppContext,
    id: CredentialId,
    caller: &Authenticated,
) -> Result<(crate::db::repos::CredentialRow, Subject), ApiError> {
    let row = context
        .db()
        .credentials()
        .get(id)
        .await
        .map_err(|err| failed(context, &err))?
        .ok_or_else(|| ApiError::not_found("There is no such credential."))?;

    let is_self = subject::owns(caller, row.user_id)?;

    let user = if is_self {
        caller.user.clone()
    } else {
        context
            .db()
            .users()
            .get(row.user_id)
            .await
            .map_err(|err| failed(context, &err))?
            .ok_or_else(|| ApiError::not_found("There is no such credential."))?
    };

    Ok((row, Subject { user, is_self }))
}

/// The host an enrolling client is pointed at.
///
/// The server's canonical domain rather than the `Host` header the caller sent:
/// a QR code is scanned by a device that has never spoken to us, so it has to
/// carry the name the certificate is issued for.
async fn host(context: &AppContext) -> Result<Option<String>, ApiError> {
    let resolved = settings::resolve(&context.config(), context.db())
        .await
        .map_err(|err| failed(context, &err))?;

    Ok(resolved.canonical_domain().map(str::to_string))
}

/// Whose credential it is, which a person's own listing leaves out.
fn owner(subject: &Subject) -> Option<Username> {
    (!subject.is_self).then(|| subject.user.username.clone())
}

/// Turns the request's whole days into a duration.
fn days(count: u32) -> chrono::Duration {
    chrono::Duration::days(i64::from(count))
}

/// Writes what was done, to whom, and by whom. Never the secret.
async fn record(
    context: &AppContext,
    action: &'static str,
    caller: &Authenticated,
    subject: &Subject,
    detail: serde_json::Value,
) {
    let entry = AuditEntry::new(AuditCategory::Enrollment, action, AuditOutcome::Success)
        .subject(subject.username())
        .actor(&caller.user.username)
        .detail(detail);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a credential change in the audit log.");
        context.session().record_human_error(&err);
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};

    use super::*;
    use crate::testing::TestServer;
    use crate::testing::context::bearer;

    async fn mint_for(server: &TestServer, username: &str) -> crate::db::repos::CredentialRow {
        let user = server
            .db()
            .users()
            .get_by_username(&Username::parse(username).unwrap())
            .await
            .unwrap()
            .unwrap();

        credentials::mint(
            server.db(),
            &server.config().auth,
            &user,
            credentials::MintRequest::new(CredentialKind::EnrollmentToken, "Phone", &user.username),
        )
        .await
        .unwrap()
        .credential
    }

    #[actix_web::test]
    async fn minting_shows_the_secret_once_and_the_listing_never_does() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let created: CredentialCreated = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/credentials")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({
                    "kind": "enrollment_token",
                    "label": "Grace's phone",
                }))
                .to_request(),
        )
        .await;

        assert!(!created.secret.is_empty());
        assert_eq!(created.credential.max_uses, Some(1));
        assert!(
            created
                .enroll_url
                .as_deref()
                .unwrap()
                .contains(&created.secret),
            "the QR code is the only place the secret goes",
        );

        let held: Vec<Credential> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/credentials")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(held.len(), 1);
        assert_eq!(held[0].id, created.credential.id);
        assert_eq!(
            held[0].username, None,
            "a person listing their own does not need to be told whose they are",
        );

        let rendered = serde_json::to_string(&held).unwrap();
        assert!(!rendered.contains(&created.secret), "{rendered}");
    }

    #[actix_web::test]
    async fn an_ordinary_caller_cannot_mint_for_somebody_else() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;
        server.user("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/credentials")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({
                    "kind": "enrollment_token",
                    "label": "Not mine",
                    "username": "ada",
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[actix_web::test]
    async fn an_administrator_can_mint_for_somebody_and_the_log_says_for_whom() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        server.user("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let created: CredentialCreated = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/credentials")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({
                    "kind": "enrollment_token",
                    "label": "Grace's phone",
                    "username": "grace",
                }))
                .to_request(),
        )
        .await;

        assert_eq!(
            created.credential.username.as_ref().unwrap().as_str(),
            "grace"
        );
        assert_eq!(
            created.credential.created_by.as_ref().unwrap().as_str(),
            "ada",
        );

        let records = server
            .db()
            .audit(crate::db::AuditQuery::about("grace", 10))
            .await
            .unwrap();

        assert!(
            records
                .iter()
                .any(|record| record.action == "credential.minted"
                    && record.actor.as_deref() == Some("ada")),
            "{records:?}",
        );

        let rendered = serde_json::to_string(&records).unwrap();
        assert!(!rendered.contains(&created.secret), "{rendered}");
    }

    #[actix_web::test]
    async fn a_stranger_cannot_list_or_revoke_somebody_elses_credential() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;
        server.user("ada", true).await;
        let hers = mint_for(&server, "ada").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        for request in [
            test::TestRequest::get()
                .uri("/api/v1/credentials?username=ada")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
            test::TestRequest::delete()
                .uri(&format!("/api/v1/credentials/{}", hers.id))
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        ] {
            assert_eq!(
                test::call_service(&app, request).await.status(),
                StatusCode::FORBIDDEN,
            );
        }
    }

    #[actix_web::test]
    async fn revoking_a_credential_stops_it_and_is_recorded() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;
        let hers = mint_for(&server, "grace").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri(&format!("/api/v1/credentials/{}", hers.id))
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        let again = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri(&format!("/api/v1/credentials/{}", hers.id))
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(
            again.status(),
            StatusCode::NOT_FOUND,
            "revoking twice should not claim to have done something",
        );

        assert!(
            server
                .db()
                .audit(crate::db::AuditQuery::about("grace", 10))
                .await
                .unwrap()
                .iter()
                .any(|record| record.action == "credential.revoked"),
        );
    }

    #[actix_web::test]
    async fn the_enrolment_template_is_the_url_without_the_token() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;
        let hers = mint_for(&server, "grace").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let template: EnrollTemplate = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri(&format!("/api/v1/credentials/{}/enroll-url", hers.id))
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(template.host, "localhost");
        assert_eq!(template.username.as_str(), "grace");
        assert!(template.url_template.ends_with("&token={token}"));
    }

    #[actix_web::test]
    async fn a_credential_that_is_not_an_enrolment_token_has_no_qr_code() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("grace", false).await;

        let password = credentials::mint(
            server.db(),
            &server.config().auth,
            &user,
            credentials::MintRequest::new(
                CredentialKind::ClientPassword,
                "CloudTAK",
                &user.username,
            ),
        )
        .await
        .unwrap();

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!(
                    "/api/v1/credentials/{}/enroll-url",
                    password.credential.id
                ))
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn an_installation_that_refuses_client_passwords_says_so() {
        let server = TestServer::start_with(|config| {
            config.auth.client_passwords_enabled = false;
        })
        .await;
        let (_, session) = server.signed_in("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/credentials")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({
                    "kind": "client_password",
                    "label": "CloudTAK",
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn a_credential_that_is_not_here_is_a_not_found() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("grace", false).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::delete()
                    .uri("/api/v1/credentials/404")
                    .insert_header(("authorization", bearer(&session)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::NOT_FOUND,
        );
    }

    #[actix_web::test]
    async fn an_administrator_can_list_the_whole_installations_credentials() {
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;
        server.user("grace", false).await;
        server.user("bhavna", false).await;
        mint_for(&server, "ada").await;
        mint_for(&server, "grace").await;
        mint_for(&server, "bhavna").await;

        let app = test::init_service(App::new().configure(server.app())).await;

        let everybodys: Vec<Credential> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/credentials?all=true")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(everybodys.len(), 3);
        assert!(
            everybodys.iter().all(|held| held.username.is_some()),
            "an installation-wide listing has to say whose each one is",
        );

        // An absent name still means the caller's own, so the two questions
        // never collapse into one.
        let own: Vec<Credential> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/credentials")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        assert_eq!(own.len(), 1);

        let serialised = serde_json::to_string(&everybodys).unwrap();
        assert!(
            !serialised.contains("secret") && !serialised.contains("hash"),
            "the listing carries no secret and no hash",
        );
    }

    #[actix_web::test]
    async fn the_whole_installations_listing_is_paged() {
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", true).await;
        for _ in 0..3 {
            mint_for(&server, "ada").await;
        }

        let app = test::init_service(App::new().configure(server.app())).await;
        let page = |page: u32, limit: u32| {
            test::TestRequest::get()
                .uri(&format!(
                    "/api/v1/credentials?all=true&page={page}&limit={limit}"
                ))
                .insert_header(("authorization", bearer(&ada)))
                .to_request()
        };

        let first: Vec<Credential> = test::call_and_read_body_json(&app, page(0, 2)).await;
        let second: Vec<Credential> = test::call_and_read_body_json(&app, page(1, 2)).await;

        assert_eq!(first.len(), 2);
        assert_eq!(second.len(), 1);
        assert!(first.iter().all(|held| held.id != second[0].id));

        let query = ListQuery {
            all: true,
            limit: Some(100_000),
            page: Some(1),
            ..ListQuery::default()
        };

        assert_eq!(query.page().limit, MAX_PAGE_SIZE);
        assert_eq!(query.page().offset, MAX_PAGE_SIZE);
    }

    #[actix_web::test]
    async fn the_whole_installations_listing_is_administrative_and_not_a_narrowed_one() {
        let server = TestServer::start().await;
        let (_, grace) = server.signed_in("grace", false).await;
        let (_, ada) = server.signed_in("ada", true).await;

        let app = test::init_service(App::new().configure(server.app())).await;

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::get()
                    .uri("/api/v1/credentials?all=true")
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
                    .uri("/api/v1/credentials?all=true&username=grace")
                    .insert_header(("authorization", bearer(&ada)))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST,
        );
    }
}

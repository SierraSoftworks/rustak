//! The passkey ceremonies, and the passkeys somebody holds.
//!
//! # Why the registration routes are public
//!
//! The same two endpoints serve two callers: somebody who is signed in and
//! adding a second passkey, and the first administrator, who has no session
//! because the passkey is what is about to give them one. Mounting them behind
//! the authenticating scope would exclude the second; mounting them outside it
//! and checking inside lets both be served by one path, with the check written
//! where it can express "one of these two, and nothing else".
//!
//! # Why a registration may answer with a session
//!
//! The wizard has just created an administrator who cannot sign in. Handing
//! back a session with the passkey saves a second ceremony that would prove
//! exactly what the first one just did. It happens only on that path — an
//! already-signed-in caller gets the passkey and keeps the session they had.

use std::sync::Arc;

use actix_web::{HttpRequest, HttpResponse, web};
use rustak_api::{
    AuditCategory, AuditOutcome, PasskeyId, PasskeyLoginFinish, PasskeyLoginStart,
    PasskeyRegistrationFinish, PasskeyRegistrationStart,
};

use crate::auth::{Passkeys, RateLimiter, passkeys, setup, tokens};
use crate::db::{AuditEntry, repos::UserRow};
use crate::identity::settings;
use crate::prelude::*;
use crate::web::helpers::request::client_address;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Authenticated;
use super::middleware::bearer_token;

/// The rate-limiter subject the ceremonies share.
const SUBJECT: &str = "passkey";

/// `POST /auth/passkey/register/start`.
///
/// # Errors
///
/// `401` when neither a session nor a registration token was presented, `429`
/// when the caller has been failing, and `400` when the ceremony cannot be
/// started.
pub async fn register_start(
    context: web::Data<AppContext>,
    limiter: web::Data<Arc<RateLimiter>>,
    request: HttpRequest,
    body: web::Json<PasskeyRegistrationStart>,
) -> ApiResult {
    let address = address(&context, &request);
    limiter.check(address, SUBJECT).map_err(too_many)?;

    let (user, bootstrap) = authorise(&context, &request, body.registration_token.as_deref())
        .await
        .inspect_err(|_| {
            limiter.record_failure(address, SUBJECT);
        })?;

    limiter.record_success(address, SUBJECT);

    let trimmed = body.label.trim();
    let label = if trimmed.is_empty() {
        "Passkey".to_string()
    } else {
        trimmed.to_string()
    };

    let challenge = relying_party(&context)
        .await?
        .start_registration(context.db(), &user, label, bootstrap)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    Ok(json_ok(&challenge))
}

/// `POST /auth/passkey/register/finish`.
///
/// Answers with a session when the ceremony was the wizard's, and with the
/// registered passkey otherwise.
///
/// # Errors
///
/// `400` when the ceremony has expired or the response does not verify, and
/// `500` when the credential cannot be stored.
pub async fn register_finish(
    context: web::Data<AppContext>,
    limiter: web::Data<Arc<RateLimiter>>,
    request: HttpRequest,
    body: web::Json<PasskeyRegistrationFinish>,
) -> ApiResult {
    let address = address(&context, &request);
    limiter.check(address, SUBJECT).map_err(too_many)?;

    let outcome = relying_party(&context)
        .await?
        .finish_registration(
            context.db(),
            &body.challenge_id,
            &body.credential,
            body.label.as_deref(),
        )
        .await;

    let (user_id, row, bootstrap) = match outcome {
        Ok(registered) => registered,
        Err(err) => {
            limiter.record_failure(address, SUBJECT);

            return Err(ApiError::from_human(&err));
        }
    };

    limiter.record_success(address, SUBJECT);

    let user = load(&context, user_id).await?;

    // Re-read rather than trusted from the ceremony: an account switched off
    // between `register/start` and `register/finish` must not be handed the
    // bootstrap session below, which `login_finish` already refuses (R-01 L3).
    if user.disabled {
        record(
            &context,
            "passkey.registered",
            AuditOutcome::Denied,
            &user.username,
        )
        .await;

        return Err(ApiError::forbidden("That account has been switched off."));
    }

    record(
        &context,
        "passkey.registered",
        AuditOutcome::Success,
        &user.username,
    )
    .await;

    if !bootstrap {
        return Ok(json_ok(&passkeys::to_summary(&row)));
    }

    let session = tokens::issue_session(
        context.get_ref(),
        &user,
        user.is_effective_admin(),
        Some("admin-ui"),
    )
    .await
    .map_err(|err| ApiError::from_human(&err))?;

    Ok(json_ok(&session))
}

/// `POST /auth/passkey/login/start`.
///
/// # Errors
///
/// `429` when the caller has been failing, and `400` when the ceremony cannot
/// be started.
pub async fn login_start(
    context: web::Data<AppContext>,
    limiter: web::Data<Arc<RateLimiter>>,
    request: HttpRequest,
    body: web::Json<PasskeyLoginStart>,
) -> ApiResult {
    let address = address(&context, &request);
    limiter.check(address, SUBJECT).map_err(too_many)?;

    let user = match body.username.as_ref() {
        Some(username) => Some(
            context
                .db()
                .users()
                .get_by_username(username)
                .await
                .map_err(|err| ApiError::from_human(&err))?
                .filter(|user| !user.disabled)
                // Deliberately the same failure as an account with no passkey:
                // the sign-in page must not be a way to ask which accounts
                // exist. The *status* still differs — a `200` carrying options
                // means the account is there — so the outcome is recorded with
                // the limiter, which is what makes the difference cost
                // something to observe (R-01 M9).
                .ok_or_else(|| {
                    limiter.record_failure(address, SUBJECT);

                    no_passkey()
                })?,
        ),
        None => None,
    };

    let challenge = relying_party(&context)
        .await?
        .start_login(context.db(), user.as_ref())
        .await
        .map_err(|err| {
            limiter.record_failure(address, SUBJECT);

            ApiError::from_human(&err)
        })?;

    // No `record_success` here on purpose. A `login/start` that found an
    // account proves nothing about who asked, so clearing the bucket would let
    // a probe reset its own budget every time it guessed a real name; the
    // window expires on its own, and `login/finish` is where somebody who
    // actually signed in is credited.
    Ok(json_ok(&challenge))
}

/// `POST /auth/passkey/login/finish`.
///
/// # Errors
///
/// `401` when the assertion does not verify, `429` when the caller has been
/// failing, and `500` when the session cannot be issued.
pub async fn login_finish(
    context: web::Data<AppContext>,
    limiter: web::Data<Arc<RateLimiter>>,
    request: HttpRequest,
    body: web::Json<PasskeyLoginFinish>,
) -> ApiResult {
    let address = address(&context, &request);
    limiter.check(address, SUBJECT).map_err(too_many)?;

    let row = match relying_party(&context)
        .await?
        .finish_login(context.db(), &body.challenge_id, &body.credential)
        .await
    {
        Ok(row) => row,
        Err(err) => {
            limiter.record_failure(address, SUBJECT);
            debug!(error = %err, "A passkey sign-in did not verify.");

            return Err(ApiError::unauthorized(
                "That passkey could not sign you in.",
            ));
        }
    };

    limiter.record_success(address, SUBJECT);

    let user = load(&context, row.user_id).await?;

    if user.disabled {
        record(&context, "login", AuditOutcome::Denied, &user.username).await;

        return Err(ApiError::forbidden("That account has been switched off."));
    }

    let session = tokens::issue_session(
        context.get_ref(),
        &user,
        user.is_effective_admin(),
        Some("admin-ui"),
    )
    .await
    .map_err(|err| ApiError::from_human(&err))?;

    record(&context, "login", AuditOutcome::Success, &user.username).await;

    Ok(json_ok(&session))
}

/// `GET /auth/passkeys`: the caller's own passkeys.
///
/// # Errors
///
/// A `500` when the read fails.
pub async fn list(context: web::Data<AppContext>, caller: Authenticated) -> ApiResult {
    let rows = context
        .db()
        .passkeys()
        .list_for_user(caller.user.id)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    let summaries: Vec<_> = rows.iter().map(passkeys::to_summary).collect();

    Ok(json_ok(&summaries))
}

/// `DELETE /auth/passkeys/{id}`: removes one of the caller's passkeys.
///
/// # Errors
///
/// `404` when it is not one of theirs — the same answer as one that does not
/// exist, so the endpoint cannot be used to enumerate anybody else's — and
/// `409` when removing it would leave the account with no way in at all.
pub async fn remove(
    context: web::Data<AppContext>,
    id: web::Path<i64>,
    caller: Authenticated,
) -> ApiResult {
    let db = context.db();
    let id = PasskeyId::new(id.into_inner());

    let row = db
        .passkeys()
        .get(id)
        .await
        .map_err(|err| ApiError::from_human(&err))?
        .filter(|row| row.user_id == caller.user.id)
        .ok_or_else(|| ApiError::not_found("There is no such passkey."))?;

    let held = db
        .passkeys()
        .list_for_user(caller.user.id)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    // An account with no identity provider behind it and no passkeys left has
    // no way in at all, and the only cure is reaching into the database by hand.
    if held.len() <= 1 && caller.user.oidc_subject.is_none() {
        return Err(ApiError::conflict(
            "That is the only way you can sign in, so it cannot be removed.",
        ));
    }

    db.passkeys()
        .delete(id)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    record(
        &context,
        "passkey.removed",
        AuditOutcome::Success,
        &caller.user.username,
    )
    .await;

    info!(passkey = %row.id, "A passkey was removed.");

    Ok(HttpResponse::NoContent().finish())
}

/// Who a registration is for, and whether it is the wizard's.
///
/// Exactly one of the two credentials has to be present. A caller who sends
/// both is not doing anything dangerous, but the session is the stronger of the
/// two and is what we act on.
async fn authorise(
    context: &AppContext,
    request: &HttpRequest,
    registration_token: Option<&str>,
) -> Result<(UserRow, bool), ApiError> {
    if let Some(token) = bearer_token(request.headers()) {
        let facts = crate::auth::resolve::RequestFacts {
            method: request.method().as_str(),
            path: request.path(),
            client_ip: crate::web::helpers::request::client_ip(
                context.config().server.trust_proxy,
                request.headers(),
                request.peer_addr(),
            ),
            headers: request.headers(),
        };

        return match crate::auth::resolve::bearer(context, token, &facts).await {
            Ok(resolved) => Ok((resolved.user, false)),
            Err(_) => Err(ApiError::unauthorized(
                "Your session is not valid. Please sign in again.",
            )),
        };
    }

    let Some(token) = registration_token else {
        return Err(ApiError::unauthorized(
            "Registering a passkey needs either a session or the wizard's registration token.",
        ));
    };

    let user_id = setup::claim_registration(context.db(), token)
        .await
        .map_err(|err| ApiError::from_human(&err))?;

    Ok((load(context, user_id).await?, true))
}

/// The relying party this installation is configured as.
///
/// From `[server] base_url`/`domains` or the wizard's stored answer, and from
/// **nowhere else**. It used to fall back to the request's own `Host` header —
/// or `X-Forwarded-Host` under `trust_proxy` — which made the caller the source
/// of the value the origin check compares against, and let a server reachable
/// under a second name mint credentials under that name and accept them
/// (R-01 M10). Refusing is the right answer: an installation that does not know
/// what it is called cannot pin a passkey to itself, and a ceremony run under a
/// guess would have to be re-run under the real name anyway.
async fn relying_party(context: &AppContext) -> Result<Passkeys, ApiError> {
    let config = context.config();

    let base_url = settings::base_url(&config, context.db())
        .await
        .map_err(|err| ApiError::from_human(&err))?
        .ok_or_else(|| {
            ApiError::bad_request(
                "This server does not know what host it is reached on, so it cannot run a passkey ceremony.                  Set `[server] domains` or `[server] base_url` and restart it.",
            )
        })?;

    Passkeys::for_base_url(&base_url, &config.server.name).map_err(|err| ApiError::from_human(&err))
}

/// Reads an account the ceremony named.
async fn load(context: &AppContext, user_id: UserId) -> Result<UserRow, ApiError> {
    context
        .db()
        .users()
        .get(user_id)
        .await
        .map_err(|err| ApiError::from_human(&err))?
        .ok_or_else(|| ApiError::not_found("That account no longer exists."))
}

/// Where the request came from, for the rate limiter.
fn address(context: &AppContext, request: &HttpRequest) -> Option<std::net::IpAddr> {
    client_address(
        context.config().server.trust_proxy,
        request.headers(),
        request.peer_addr(),
    )
}

/// Writes a ceremony to the audit log.
async fn record(context: &AppContext, action: &'static str, outcome: AuditOutcome, who: &Username) {
    let entry = AuditEntry::new(AuditCategory::Authentication, action, outcome).subject(who);

    if let Err(err) = context.db().record(entry).await {
        warn!(error = %err, "Could not record a passkey ceremony in the audit log.");
    }
}

/// The one thing a sign-in that cannot start is ever told.
fn no_passkey() -> ApiError {
    ApiError::bad_request("That account has no passkey registered.")
}

/// The failure for somebody who has been failing ceremonies.
fn too_many(retry_after: chrono::Duration) -> ApiError {
    ApiError::new(
        actix_web::http::StatusCode::TOO_MANY_REQUESTS,
        format!(
            "Too many attempts. Try again in {} minutes.",
            retry_after.num_minutes().max(1)
        ),
    )
    .with_code("rate_limited")
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test};
    use rustak_api::{PasskeyChallenge, PasskeySummary, TokenResponse};

    use super::*;
    use crate::testing::authenticator::Algorithm;
    use crate::testing::context::{TEST_HOST, TEST_ORIGIN, bearer};
    use crate::testing::{SoftAuthenticator, TestServer};

    /// The app, the authenticator and a signed-in account.
    macro_rules! ceremony {
        ($server:expr) => {{
            (
                test::init_service(App::new().configure($server.app())).await,
                SoftAuthenticator::new(TEST_ORIGIN),
            )
        }};
    }

    /// Starts a ceremony and reads back its challenge.
    ///
    /// A macro rather than a function because the service `init_service`
    /// returns cannot be named without depending on `actix-http` directly.
    macro_rules! challenge {
        ($app:expr, $uri:expr, $body:expr) => {{
            let started: PasskeyChallenge = test::call_and_read_body_json(
                &$app,
                test::TestRequest::post()
                    .uri($uri)
                    .set_json($body)
                    .to_request(),
            )
            .await;

            started
        }};
        ($app:expr, $uri:expr, $body:expr, $session:expr) => {{
            let started: PasskeyChallenge = test::call_and_read_body_json(
                &$app,
                test::TestRequest::post()
                    .uri($uri)
                    .insert_header(("authorization", bearer($session)))
                    .set_json($body)
                    .to_request(),
            )
            .await;

            started
        }};
    }

    /// Registers a passkey for a signed-in account, end to end.
    macro_rules! register {
        ($app:expr, $authenticator:expr, $session:expr, $label:expr) => {{
            let started = challenge!(
                $app,
                "/api/v1/auth/passkey/register/start",
                serde_json::json!({ "label": $label }),
                $session
            );
            let credential = $authenticator.create(&started.options);

            test::call_service(
                &$app,
                test::TestRequest::post()
                    .uri("/api/v1/auth/passkey/register/finish")
                    .insert_header(("authorization", bearer($session)))
                    .set_json(serde_json::json!({
                        "challenge_id": started.challenge_id,
                        "credential": credential,
                    }))
                    .to_request(),
            )
            .await
        }};
    }

    #[actix_web::test]
    async fn a_passkey_registered_here_signs_somebody_in_here() {
        // The whole point, and the positive control every refusal below rests
        // on: a real ceremony against a real authenticator, twice.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        let registered = register!(app, authenticator, &session, "Phone");
        assert_eq!(registered.status(), StatusCode::OK);

        let summary: PasskeySummary = test::read_body_json(registered).await;
        assert_eq!(summary.label, "Phone");
        assert_eq!(summary.last_used_at, None);

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({ "username": "ada" })
        );

        let assertion = authenticator.get(&started.options);

        let signed_in: TokenResponse = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": assertion,
                }))
                .to_request(),
        )
        .await;

        let claims = server.jwt().unwrap().verify(&signed_in.token).unwrap();
        assert_eq!(claims.sub, "ada");
    }

    #[actix_web::test]
    async fn a_registration_asks_for_a_credential_the_authenticator_will_keep() {
        // The sign-in prompt has no username field, so it can only run a
        // discoverable ceremony — and a credential the authenticator did not
        // store cannot answer one. This is the assertion that the passkey
        // profile survives to the wire, whatever builds the options.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, _) = ceremony!(server);

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/register/start",
            serde_json::json!({ "label": "Phone" }),
            &session
        );

        let selection = &started.options["authenticatorSelection"];
        assert_eq!(selection["residentKey"], "required");
        assert_eq!(selection["requireResidentKey"], true);
        assert_eq!(selection["userVerification"], "required");
    }

    #[actix_web::test]
    async fn the_options_a_browser_is_handed_are_the_ones_the_ui_knows_how_to_read() {
        // The UI decodes `challenge`, `user.id` and every credential
        // descriptor's `id` from base64url and hands the rest to
        // `navigator.credentials` untouched, so these field names and encodings
        // are the contract between the two halves.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/register/start",
            serde_json::json!({ "label": "Second" }),
            &session
        );
        let options = &started.options;

        assert_eq!(options["rp"]["id"], TEST_HOST);
        assert_eq!(
            options["rp"]["name"], "rustak",
            "the installation's own name is what the prompt has room for",
        );
        assert_eq!(options["attestation"], "none");
        assert_eq!(options["user"]["name"], "ada");

        for encoded in [&options["challenge"], &options["user"]["id"]] {
            assert!(
                base64::Engine::decode(
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                    encoded.as_str().expect("a base64url string"),
                )
                .is_ok(),
            );
        }

        let excluded = options["excludeCredentials"]
            .as_array()
            .expect("the passkey already registered is excluded");
        assert_eq!(excluded.len(), 1);
        assert!(
            base64::Engine::decode(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                excluded[0]["id"].as_str().expect("a base64url string"),
            )
            .is_ok(),
        );

        // And the sign-in half, which lists nothing at all.
        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({})
        );

        assert_eq!(started.options["rpId"], TEST_HOST);
        assert_eq!(started.options["userVerification"], "required");
        assert!(
            started.options["allowCredentials"]
                .as_array()
                .is_none_or(|list| list.is_empty()),
            "a discoverable ceremony names nobody",
        );
    }

    #[actix_web::test]
    async fn a_sign_in_that_names_nobody_lets_the_authenticator_say_who_it_holds() {
        // The better flow: naming an account before proving anything is what
        // would let somebody ask this endpoint which accounts exist.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({})
        );

        let assertion = authenticator.get(&started.options);

        let signed_in: TokenResponse = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": assertion,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(
            server.jwt().unwrap().verify(&signed_in.token).unwrap().sub,
            "ada"
        );
    }

    #[actix_web::test]
    async fn a_challenge_is_good_for_exactly_one_attempt() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({ "username": "ada" })
        );

        let assertion = authenticator.get(&started.options);
        let body = serde_json::json!({
            "challenge_id": started.challenge_id,
            "credential": assertion,
        });

        let first = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(&body)
                .to_request(),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);

        let replayed = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(&body)
                .to_request(),
        )
        .await;

        assert_eq!(
            replayed.status(),
            StatusCode::UNAUTHORIZED,
            "an assertion that could be presented twice is a replay",
        );
    }

    #[actix_web::test]
    async fn an_assertion_from_another_origin_is_refused() {
        // This is what makes a passkey unphishable: a credential is bound to
        // the host it was registered against, and an authenticator that signs
        // for a different one is signing about a different site.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({ "username": "ada" })
        );

        // The same credential, the same key, the same challenge — produced by
        // a page somewhere else.
        let phishing = authenticator.at_origin("https://phishing.example.com");
        let assertion = phishing.get(&started.options);

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": assertion,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn a_cloned_authenticator_is_caught_by_its_counter() {
        // The counter is not bookkeeping: an authenticator that has been copied
        // reports one that has not moved forward, and that is the only signal
        // there is.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        for _ in 0..2 {
            let started = challenge!(
                app,
                "/api/v1/auth/passkey/login/start",
                serde_json::json!({ "username": "ada" })
            );

            let assertion = authenticator.get(&started.options);

            assert_eq!(
                test::call_service(
                    &app,
                    test::TestRequest::post()
                        .uri("/api/v1/auth/passkey/login/finish")
                        .set_json(serde_json::json!({
                            "challenge_id": started.challenge_id,
                            "credential": assertion,
                        }))
                        .to_request(),
                )
                .await
                .status(),
                StatusCode::OK
            );
        }

        authenticator.clone_credential();

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({ "username": "ada" })
        );

        let assertion = authenticator.get(&started.options);

        assert_eq!(
            test::call_service(
                &app,
                test::TestRequest::post()
                    .uri("/api/v1/auth/passkey/login/finish")
                    .set_json(serde_json::json!({
                        "challenge_id": started.challenge_id,
                        "credential": assertion,
                    }))
                    .to_request(),
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED,
        );
    }

    #[actix_web::test]
    async fn the_wizards_registration_token_signs_the_new_administrator_in() {
        // The first administrator has no session, because the passkey is what
        // is about to give them one. Handing back a session with it saves a
        // second ceremony that would prove exactly what the first one just did.
        let server = TestServer::start_with(|config| {
            config.server.domains = vec![crate::testing::context::TEST_HOST.to_string()];
        })
        .await;

        let token = crate::auth::setup::ensure(server.db(), &server.config().setup_token_file())
            .await
            .unwrap()
            .unwrap()
            .token;

        let (app, authenticator) = ceremony!(server);

        let created: rustak_api::AdminCreated = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/setup/admin")
                .set_json(serde_json::json!({
                    "setup_token": token,
                    "username": "ada",
                }))
                .to_request(),
        )
        .await;

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/register/start",
            serde_json::json!({
                "label": "Laptop",
                "registration_token": created.registration_token,
            })
        );

        let credential = authenticator.create(&started.options);

        let session: TokenResponse = test::call_and_read_body_json(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/register/finish")
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": credential,
                }))
                .to_request(),
        )
        .await;

        let me: rustak_api::Me = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(me.username.as_str(), "ada");
        assert!(me.is_admin);
    }

    #[actix_web::test]
    async fn registering_needs_a_session_or_the_wizards_token_and_nothing_else() {
        let server = TestServer::start().await;
        let (app, _) = ceremony!(server);

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/register/start")
                .set_json(serde_json::json!({ "label": "Phone" }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn a_registration_token_is_spent_on_the_first_use() {
        let server = TestServer::start().await;
        let user = server.user("ada", true).await;
        let token = crate::auth::setup::issue_registration(server.db(), user.id)
            .await
            .unwrap();

        let (app, _) = ceremony!(server);
        let body = serde_json::json!({ "label": "Phone", "registration_token": token });

        let first = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/register/start")
                .set_json(&body)
                .to_request(),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);

        let second = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/register/start")
                .set_json(&body)
                .to_request(),
        )
        .await;

        assert_eq!(second.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn an_account_with_no_passkey_and_one_that_does_not_exist_answer_alike() {
        // Otherwise the sign-in page is a way to ask which accounts exist.
        let server = TestServer::start().await;
        server.user("ada", false).await;
        let (app, _) = ceremony!(server);

        let mut bodies = Vec::new();

        for username in ["ada", "nobody"] {
            let response = test::call_service(
                &app,
                test::TestRequest::post()
                    .uri("/api/v1/auth/passkey/login/start")
                    .set_json(serde_json::json!({ "username": username }))
                    .to_request(),
            )
            .await;

            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            bodies.push(test::read_body(response).await);
        }

        assert_eq!(bodies[0], bodies[1]);
    }

    #[actix_web::test]
    async fn somebody_can_see_and_remove_their_own_passkeys() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        let second = SoftAuthenticator::new(TEST_ORIGIN);
        register!(app, second, &session, "Security key");

        let listed: Vec<PasskeySummary> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/passkeys")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(listed.len(), 2);

        let removed = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri(&format!("/api/v1/auth/passkeys/{}", listed[0].id))
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    }

    #[actix_web::test]
    async fn nobody_can_remove_the_only_way_they_have_of_signing_in() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        let listed: Vec<PasskeySummary> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/passkeys")
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        let response = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri(&format!("/api/v1/auth/passkeys/{}", listed[0].id))
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "an account with no provider behind it and no passkeys left has no way in at all",
        );
    }

    #[actix_web::test]
    async fn nobody_can_remove_somebody_elses() {
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", false).await;
        let (_, grace) = server.signed_in("grace", true).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &ada, "Phone");

        let listed: Vec<PasskeySummary> = test::call_and_read_body_json(
            &app,
            test::TestRequest::get()
                .uri("/api/v1/auth/passkeys")
                .insert_header(("authorization", bearer(&ada)))
                .to_request(),
        )
        .await;

        let response = test::call_service(
            &app,
            test::TestRequest::delete()
                .uri(&format!("/api/v1/auth/passkeys/{}", listed[0].id))
                .insert_header(("authorization", bearer(&grace)))
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "the same answer as one that does not exist, so this cannot enumerate anybody else's",
        );
    }

    #[actix_web::test]
    async fn a_disabled_account_cannot_sign_in_with_a_passkey_it_still_holds() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({ "username": "ada" })
        );
        let assertion = authenticator.get(&started.options);

        server
            .db()
            .users()
            .set_disabled(user.id, true)
            .await
            .unwrap();

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": assertion,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// Registers a passkey with a particular authenticator and signs in with
    /// it, returning what the sign-in answered.
    macro_rules! sign_in {
        ($app:expr, $registrant:expr, $asserter:expr, $session:expr) => {{
            let registered = register!($app, $registrant, $session, "Phone");
            assert_eq!(registered.status(), StatusCode::OK);

            let started = challenge!(
                $app,
                "/api/v1/auth/passkey/login/start",
                serde_json::json!({ "username": "ada" })
            );

            let assertion = $asserter.get(&started.options);

            test::call_service(
                &$app,
                test::TestRequest::post()
                    .uri("/api/v1/auth/passkey/login/finish")
                    .set_json(serde_json::json!({
                        "challenge_id": started.challenge_id,
                        "credential": assertion,
                    }))
                    .to_request(),
            )
            .await
        }};
    }

    #[actix_web::test]
    async fn every_algorithm_the_registration_offers_can_actually_sign_somebody_in() {
        // An allow-list nothing exercises is an allow-list nobody knows works.
        // The registration options name ES256, RS256 and EdDSA, so all three
        // have to survive a whole ceremony, not just appear in a list.
        for algorithm in [Algorithm::Es256, Algorithm::Rs256, Algorithm::Ed25519] {
            let server = TestServer::start().await;
            let (_, session) = server.signed_in("ada", false).await;
            let app = test::init_service(App::new().configure(server.app())).await;
            let authenticator = SoftAuthenticator::new(TEST_ORIGIN).signing_with(algorithm);

            assert_eq!(
                sign_in!(app, authenticator, authenticator, &session).status(),
                StatusCode::OK,
                "{algorithm:?} is offered at registration, so it has to work",
            );
        }
    }

    #[actix_web::test]
    async fn a_credential_using_an_algorithm_we_never_offered_is_refused() {
        // The key material is a real P-256 key; the only thing wrong with it is
        // the COSE identifier. Accepting it would mean the list of algorithms
        // the options advertise is decoration rather than a decision.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let authenticator =
            SoftAuthenticator::new(TEST_ORIGIN).signing_with(Algorithm::Unsupported);

        let registered = register!(app, authenticator, &session, "Phone");

        assert_eq!(registered.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn a_registration_run_from_somewhere_else_is_refused() {
        // Registration is bound to the origin and the relying party exactly as
        // sign-in is. A credential accepted from a page somewhere else would be
        // a credential somebody else's site chose.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let honest = SoftAuthenticator::new(TEST_ORIGIN);

        for liar in [
            honest.at_origin("https://phishing.example.com"),
            honest.at_rp_id("phishing.example.com"),
            honest.without_user_verification(),
            honest.without_user_presence(),
        ] {
            assert_eq!(
                register!(app, liar, &session, "Phone").status(),
                StatusCode::BAD_REQUEST,
            );
        }

        // The positive control, so that the four refusals above are the lie
        // rather than anything else about this server.
        assert_eq!(
            register!(app, honest, &session, "Phone").status(),
            StatusCode::OK,
        );
    }

    #[actix_web::test]
    async fn an_assertion_whose_two_identifiers_disagree_is_refused() {
        // The row to verify against is selected by `rawId`, while the verifier
        // is handed the assertion's own `id`. If those could disagree, the
        // caller would choose which credential the signature is checked with.
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", false).await;
        let (_, grace) = server.signed_in("grace", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let hers = SoftAuthenticator::new(TEST_ORIGIN);
        let his = SoftAuthenticator::new(TEST_ORIGIN);
        register!(app, hers, &ada, "Hers");
        register!(app, his, &grace, "His");

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({ "username": "ada" })
        );

        // `grace`'s signature, presented under `ada`'s credential identifier.
        let mut assertion = his.get(&serde_json::json!({
            "challenge": started.options["challenge"].clone(),
        }));
        let hers_id = hers.get(&serde_json::json!({
            "challenge": started.options["challenge"].clone(),
        }))["rawId"]
            .clone();
        assertion["rawId"] = hers_id;

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": assertion,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn a_ceremony_labelled_as_the_other_one_is_refused() {
        // `type` in the client data is what stops an assertion being replayed
        // as a registration and the other way round; it is signed over, so it
        // cannot be corrected on the way past either.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let honest = SoftAuthenticator::new(TEST_ORIGIN);

        assert_eq!(
            register!(app, honest.mislabelling_the_ceremony(), &session, "Phone").status(),
            StatusCode::BAD_REQUEST,
        );

        assert_eq!(
            sign_in!(app, honest, honest.mislabelling_the_ceremony(), &session).status(),
            StatusCode::UNAUTHORIZED,
        );
    }

    #[actix_web::test]
    async fn a_ceremony_run_inside_somebody_elses_frame_is_refused() {
        // Nothing here is ever meant to be completed from a nested browsing
        // context, so `crossOrigin: true` is a ceremony the person may not have
        // understood they were completing.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let honest = SoftAuthenticator::new(TEST_ORIGIN);

        assert_eq!(
            register!(app, honest.in_a_frame(), &session, "Phone").status(),
            StatusCode::BAD_REQUEST,
        );

        assert_eq!(
            sign_in!(app, honest, honest.in_a_frame(), &session).status(),
            StatusCode::UNAUTHORIZED,
        );
    }

    #[actix_web::test]
    async fn a_registration_for_a_challenge_nobody_issued_is_refused() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/register/start",
            serde_json::json!({ "label": "Phone" }),
            &session
        );

        // A well-formed ceremony over a challenge of the caller's own choosing.
        let credential = authenticator.create(&serde_json::json!({
            "challenge": "AAAAAAAAAAAAAAAAAAAAAA",
            "user": started.options["user"].clone(),
        }));

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/register/finish")
                .insert_header(("authorization", bearer(&session)))
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": credential,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn an_assertion_signed_over_another_relying_party_is_refused() {
        // The origin in the client data is ours; only the relying-party
        // identifier hashed into the authenticator data is somebody else's.
        // That is the half of the binding a browser does not police, so it is
        // the half this server has to.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let authenticator = SoftAuthenticator::new(TEST_ORIGIN);

        assert_eq!(
            sign_in!(
                app,
                authenticator,
                authenticator.at_rp_id("phishing.example.com"),
                &session
            )
            .status(),
            StatusCode::UNAUTHORIZED,
        );
    }

    #[actix_web::test]
    async fn an_assertion_nobody_verified_is_refused() {
        // The credential was registered with `userVerification: "required"`, so
        // an assertion the authenticator did not verify a user for is a weaker
        // proof than the one the credential promises.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let authenticator = SoftAuthenticator::new(TEST_ORIGIN);

        assert_eq!(
            sign_in!(
                app,
                authenticator,
                authenticator.without_user_verification(),
                &session
            )
            .status(),
            StatusCode::UNAUTHORIZED,
        );
    }

    #[actix_web::test]
    async fn an_assertion_nobody_was_present_for_is_refused() {
        // User presence is the one flag every ceremony requires, whatever else
        // is asked for: it is what makes the assertion a deliberate act.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let authenticator = SoftAuthenticator::new(TEST_ORIGIN);

        assert_eq!(
            sign_in!(
                app,
                authenticator,
                authenticator.without_user_presence(),
                &session
            )
            .status(),
            StatusCode::UNAUTHORIZED,
        );
    }

    #[actix_web::test]
    async fn an_assertion_whose_signature_covers_something_else_is_refused() {
        // Everything about this assertion is well formed — the origin, the
        // relying party, the flags, the counter and the challenge. Only the
        // signature is over different bytes, which is the whole of the attack.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let authenticator = SoftAuthenticator::new(TEST_ORIGIN);

        assert_eq!(
            sign_in!(
                app,
                authenticator,
                authenticator.with_tampered_signature(),
                &session
            )
            .status(),
            StatusCode::UNAUTHORIZED,
        );
    }

    #[actix_web::test]
    async fn an_assertion_for_a_challenge_nobody_issued_is_refused() {
        // The challenge is the whole of what makes an assertion fresh, so an
        // assertion over one this server never sent has to be refused whatever
        // else about it verifies.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({ "username": "ada" })
        );

        // A well-formed ceremony, run against a challenge of the caller's own
        // choosing rather than the one the handle was issued for.
        let assertion = authenticator.get(&serde_json::json!({
            "challenge": "AAAAAAAAAAAAAAAAAAAAAA",
        }));

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": assertion,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn a_registration_handle_cannot_be_spent_on_a_sign_in() {
        // A handle carries which ceremony it was started for, and the two
        // verifiers check different things. Letting a caller choose which one
        // runs would be letting them choose which checks apply.
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", false).await;
        let (app, authenticator) = ceremony!(server);

        register!(app, authenticator, &session, "Phone");

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/register/start",
            serde_json::json!({ "label": "Second" }),
            &session
        );

        let assertion = authenticator.get(&started.options);

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": assertion,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn a_sign_in_handle_issued_for_one_account_cannot_sign_in_another() {
        // The named ceremony lists exactly one account's credentials. An
        // assertion from somebody else's authenticator is refused before any
        // signature is looked at, because the handle already said who this was
        // meant to be.
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", false).await;
        let (_, grace) = server.signed_in("grace", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let hers = SoftAuthenticator::new(TEST_ORIGIN);
        let his = SoftAuthenticator::new(TEST_ORIGIN);
        register!(app, hers, &ada, "Hers");
        register!(app, his, &grace, "His");

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({ "username": "ada" })
        );

        // `grace`'s authenticator answers a challenge issued for `ada`, with a
        // perfectly valid assertion of its own credential.
        let assertion = his.get(&serde_json::json!({
            "challenge": started.options["challenge"].clone(),
        }));

        let response = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/finish")
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": assertion,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[actix_web::test]
    async fn the_same_passkey_cannot_be_registered_twice() {
        // A credential registered to two accounts would let whoever holds it
        // sign in as either.
        let server = TestServer::start().await;
        let (_, ada) = server.signed_in("ada", false).await;
        let (_, grace) = server.signed_in("grace", false).await;
        let app = test::init_service(App::new().configure(server.app())).await;
        let authenticator = SoftAuthenticator::new(TEST_ORIGIN);

        assert_eq!(
            register!(app, authenticator, &ada, "Phone").status(),
            StatusCode::OK,
        );

        // The same authenticator registers a *new* credential each time, so the
        // collision has to be manufactured: replay the first credential into a
        // second account's ceremony.
        let started = challenge!(
            app,
            "/api/v1/auth/passkey/register/start",
            serde_json::json!({ "label": "Phone" }),
            &grace
        );
        let credential = authenticator.create(&started.options);

        let first = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/register/finish")
                .insert_header(("authorization", bearer(&grace)))
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": credential.clone(),
                }))
                .to_request(),
        )
        .await;
        assert_eq!(first.status(), StatusCode::OK);

        let started = challenge!(
            app,
            "/api/v1/auth/passkey/register/start",
            serde_json::json!({ "label": "Phone" }),
            &ada
        );
        let mut replayed = credential;
        replayed["response"]["clientDataJSON"] = serde_json::Value::String(base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            serde_json::to_vec(&serde_json::json!({
                "type": "webauthn.create",
                "challenge": started.options["challenge"],
                "origin": TEST_ORIGIN,
                "crossOrigin": false,
            }))
            .unwrap(),
        ));

        let second = test::call_service(
            &app,
            test::TestRequest::post()
                .uri("/api/v1/auth/passkey/register/finish")
                .insert_header(("authorization", bearer(&ada)))
                .set_json(serde_json::json!({
                    "challenge_id": started.challenge_id,
                    "credential": replayed,
                }))
                .to_request(),
        )
        .await;

        assert_eq!(
            second.status(),
            StatusCode::BAD_REQUEST,
            "a credential registered to two accounts would sign in as either",
        );
        assert!(
            String::from_utf8_lossy(&test::read_body(second).await).contains("already registered"),
            "and it has to be refused for that reason rather than incidentally",
        );
    }

    #[actix_web::test]
    async fn probing_for_accounts_by_name_runs_into_the_limiter() {
        // R-01 M9. `login/start` checked the limiter and never recorded an
        // outcome, and a bucket only locks once a failure is counted — so the
        // endpoint was effectively unlimited, and a `200` carrying challenge
        // options is a clean "this account exists, is enabled and has a
        // passkey".
        let server = TestServer::start_with(|config| {
            config.auth.rate_limit.attempts = 3;
        })
        .await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let mut statuses = Vec::new();

        for index in 0..6 {
            let response = test::TestRequest::post()
                .uri("/api/v1/auth/passkey/login/start")
                .set_json(serde_json::json!({ "username": format!("guess{index}") }))
                .send_request(&app)
                .await;

            statuses.push(response.status());
        }

        assert!(
            statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
            "a wordlist has to run out of attempts: {statuses:?}",
        );
    }

    #[actix_web::test]
    async fn a_ceremony_needs_a_configured_base_url_rather_than_the_host_header() {
        // R-01 M10. The relying-party identifier used to fall back to the
        // request's own `Host` — so the caller supplied the value the origin
        // check compares against.
        let server = TestServer::start_with(|config| {
            config.server.domains = Vec::new();
            config.server.base_url = None;
        })
        .await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let refused = test::TestRequest::post()
            .uri("/api/v1/auth/passkey/login/start")
            .insert_header(("host", "evil.example.net"))
            .set_json(serde_json::json!({}))
            .send_request(&app)
            .await;

        assert_eq!(refused.status(), StatusCode::BAD_REQUEST);

        let body = String::from_utf8(test::read_body(refused).await.to_vec()).unwrap();

        assert!(!body.contains("evil.example.net"), "{body}");
    }

    #[actix_web::test]
    async fn a_ceremony_records_the_relying_party_it_was_started_under() {
        let server = TestServer::start().await;
        let (app, _) = ceremony!(server);

        let started = challenge!(
            &app,
            "/api/v1/auth/passkey/login/start",
            serde_json::json!({})
        );

        let ceremony = crate::auth::passkey_store::claim(server.db(), &started.challenge_id)
            .await
            .expect("the ceremony is claimable once");

        assert_eq!(ceremony.rp_id, TEST_HOST);
        assert!(
            crate::auth::passkey_store::require_rp_id(&ceremony, "evil.example.net").is_err(),
            "a ceremony started here cannot be finished somewhere else",
        );
    }
}

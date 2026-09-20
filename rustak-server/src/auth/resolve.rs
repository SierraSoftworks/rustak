//! Turning a credential into a [`Principal`].
//!
//! There are three credentials to turn, and [`ListenerAuthPolicy`] says which
//! of them a given listener accepts: a client certificate (the whole point of
//! `:8443`), one of our own bearer tokens, and HTTP Basic — the last confined
//! by `conventions.md`'s security defaults to the enrolment endpoints and
//! `/oauth/token`, because those are the only places a TAK client has no
//! alternative.
//!
//! [`resolve_principal`] tries them in that order, which is deliberate: a
//! certificate is the strongest thing a caller can present and the only one
//! that cannot be replayed from a log, so a request carrying both a certificate
//! and a header is answered as the certificate.
//!
//! A fourth credential arrives by the same door: the `access_token_N` cookies
//! the `/login/*` flow sets. They are read through [`bearer`] like any other
//! token of ours, but only on the paths
//! [`cookies_allowed`](crate::auth::oauth_server::cookies_allowed) permits and
//! only when the `Authorization` header carried nothing — an explicit
//! credential always beats an ambient one.
//!
//! [`bearer`] itself is written against [`Services`] and plain request facts
//! rather than against an actix request, so it can be exercised without a
//! server and reused by the listeners that are not actix at all.

use std::sync::Arc;

use actix_web::http::header::HeaderMap;
use actix_web::{HttpRequest, web};
use rustak_core::identity::{AuthMethod, Principal};
use rustak_core::prelude::*;

use rustak_api::UserSource;

use crate::db::repos::UserRow;
use crate::identity::users;
use crate::identity::verify::{Grace, Purpose};
use crate::pki::PeerCertificate;
use crate::prelude::Services;
use crate::web::helpers::request::client_address;

use super::AccessClaims;
use super::acl::{AuthRequestFilter, evaluate};
use super::basic::{basic_credential, verify_basic_with_grace};
use super::ratelimit::RateLimiter;

/// What a request says about itself, for the access-control expressions.
pub struct RequestFacts<'a> {
    /// The HTTP method, upper case.
    pub method: &'a str,
    /// The path, without the query string.
    pub path: &'a str,
    /// Where the request came from, as
    /// [`client_ip`](crate::web::helpers::request::client_ip) resolved it.
    pub client_ip: Option<String>,
    /// The request headers.
    pub headers: &'a HeaderMap,
}

/// Who a request is from, and what they may do.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The rights this request carries.
    pub principal: Principal,
    /// The account as it is stored.
    pub user: UserRow,
    /// The claims of the token that was presented, when one was.
    ///
    /// [`None`] for a client certificate and for Basic: neither carries claims,
    /// and synthesising some would put a `jti` in the audit log that revocation
    /// could never match.
    pub claims: Option<AccessClaims>,
}

impl Resolved {
    /// The `jti` and expiry of the token behind this request, when there is
    /// one — which is what signing out revokes.
    pub fn token(&self) -> Option<&AccessClaims> {
        self.claims.as_ref()
    }
}

/// Why a request could not be answered.
#[derive(Debug)]
pub enum AuthFailure {
    /// No credential was presented, or the one presented is not one we accept.
    ///
    /// One variant for both on purpose: telling a caller that their token was
    /// merely expired, or that the account behind it does not exist, is an
    /// oracle they did not need.
    Rejected,
    /// The credential was good and the answer will not change by presenting
    /// another one.
    Forbidden(&'static str),
    /// Too many failures from this caller, and how long until the next try.
    RateLimited(chrono::Duration),
    /// Something of ours failed.
    Unavailable(Error),
}

impl From<Error> for AuthFailure {
    fn from(err: Error) -> Self {
        Self::Unavailable(err)
    }
}

/// Resolves one of our own access tokens.
///
/// # Errors
///
/// [`AuthFailure::Rejected`] for a token we would not accept, an account that
/// does not exist and an account that has been switched off;
/// [`AuthFailure::Forbidden`] when a configured access-control expression
/// refuses the request; [`AuthFailure::Unavailable`] when a read fails.
#[instrument("auth.resolve.bearer", skip_all, err(Debug))]
pub async fn bearer<S: Services>(
    services: &S,
    token: &str,
    facts: &RequestFacts<'_>,
) -> Result<Resolved, AuthFailure> {
    let config = services.config();
    let jwt = services.jwt()?;

    let claims = jwt.verify(token).map_err(|err| {
        debug!(error = %err, "Refused a request carrying a token we would not accept.");

        AuthFailure::Rejected
    })?;

    let db = services.db();

    // Revocation is a database read, which is why `JwtIssuer::verify` does not
    // do it: a pure function that reached for the database would put one on
    // every path that only needs to check a signature.
    if db.revoked_jtis().is_revoked(&claims.jti).await? {
        debug!("Refused a request carrying a token that has been revoked.");

        return Err(AuthFailure::Rejected);
    }

    // Looked up per request rather than carried in the token, so that switching
    // an account off takes effect now rather than when its token expires.
    let username = Username::from_storage(&claims.sub);
    let user = match db.users().get_by_username(&username).await? {
        Some(user) if !user.disabled => user,
        _ => return Err(AuthFailure::Rejected),
    };

    // Our own token carries no provider claims, so the expressions see the
    // ones the account's last sign-in recorded — which is what `claims.groups
    // contains "…"` has to be judged against, or it would be null here and
    // refuse everybody between sign-ins.
    let recorded = user.oidc_claims.as_deref().and_then(stored_claims);
    let filter = AuthRequestFilter {
        method: facts.method,
        path: facts.path,
        client_ip: facts.client_ip.clone(),
        headers: facts.headers,
        claims: recorded.as_ref(),
        username: user.username.as_str(),
        source: user.source.as_str(),
    };

    let outcome = evaluate(&config.auth, &filter);

    // `user_acl` says who the *provider* may sign in. A local account was
    // admitted when the wizard or an administrator made it here, and a service
    // account when it was registered; neither has claims for the expression
    // to look at, and an expression written for the directory must not lock
    // out the passkey that exists precisely for when the directory is wrong.
    if user.source == UserSource::Oidc && config.auth.user_acl.is_some() && !outcome.allowed {
        info!(
            username = %user.username,
            path = facts.path,
            "An access-control expression refused an authenticated request."
        );

        return Err(AuthFailure::Forbidden(
            "Your account is not permitted to use this.",
        ));
    }

    let acl_admin = config.auth.admin_acl.is_some() && outcome.is_admin;
    let method = AuthMethod::Bearer {
        jti: claims.jti.clone(),
        scope: claims.scope.clone(),
    };
    let principal = users::principal(db, &user, method, acl_admin).await?;

    Ok(Resolved {
        principal,
        user,
        claims: Some(claims),
    })
}

/// The claims an account's last sign-in recorded, if they still parse.
///
/// A row whose stored claims cannot be read is treated as one with none rather
/// than refused outright: the expression then decides, exactly as it would for
/// a sign-in that carried no such claim.
fn stored_claims(raw: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(serde_json::Value::Object(map)) => Some(map),
        Ok(_) | Err(_) => {
            warn!("An account's stored identity-provider claims could not be read.");
            None
        }
    }
}

/// Where HTTP Basic is accepted on a listener.
///
/// Three values rather than a `bool`, because "only the endpoints that have no
/// alternative" is the position `conventions.md` takes and there is no way to
/// express it with two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasicPolicy {
    /// Never. A Basic header is ignored.
    Off,

    /// Only on the paths a TAK client cannot reach any other way:
    /// [`ENROLLMENT_PREFIX`] and [`OAUTH_TOKEN_PATH`].
    EnrollmentOnly,

    /// Anywhere on the listener. Nothing selects this today; it exists so that
    /// an installation that deliberately opts in is a configuration change
    /// rather than a code change.
    All,
}

/// The enrolment paths, which are the only `/Marti` paths Basic reaches.
pub const ENROLLMENT_PREFIX: &str = "/Marti/api/tls/";

/// The two device-profile routes under the enrolment prefix.
///
/// The only paths a **spent** enrolment token still reaches, and only inside
/// `[auth] enrollment_grace`: `/tls/profile/enrollment` and
/// `/tls/profile/tool/{tool}/file` are both registered under this, and nothing
/// else is (`marti::profiles::routes`).
pub const ENROLLMENT_PROFILE_PREFIX: &str = "/Marti/api/tls/profile/";

/// The token endpoint, whose whole job is to take a password.
pub const OAUTH_TOKEN_PATH: &str = "/oauth/token";

/// Which credentials one listener accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListenerAuthPolicy {
    /// A client certificate this installation's CA issued.
    pub cert: bool,

    /// One of our own RS256 access tokens.
    pub bearer: bool,

    /// Where a username and secret are accepted.
    pub basic: BasicPolicy,
}

impl ListenerAuthPolicy {
    /// `[web.public]`: no certificate is asked for at the handshake, so a
    /// bearer token or — on the two paths that need it — Basic.
    pub const fn public() -> Self {
        Self {
            cert: false,
            bearer: true,
            basic: BasicPolicy::EnrollmentOnly,
        }
    }

    /// `[web.marti]`: the certificate is the point, and the other two are still
    /// accepted because CloudTAK points its `webtak` and `api` URLs at whatever
    /// host answers and we would rather not care which port it picked.
    pub const fn marti() -> Self {
        Self {
            cert: true,
            bearer: true,
            basic: BasicPolicy::EnrollmentOnly,
        }
    }

    /// Whether Basic may be used for `path`, and for what.
    ///
    /// [`None`] means "not here": the header is left unread rather than being
    /// checked and refused, so a stray Basic header on an ordinary route cannot
    /// be turned into a password-guessing oracle.
    pub fn basic_purpose(&self, path: &str) -> Option<Purpose> {
        match self.basic {
            BasicPolicy::Off => None,
            BasicPolicy::All | BasicPolicy::EnrollmentOnly
                if path.starts_with(ENROLLMENT_PREFIX) =>
            {
                Some(enrollment_purpose(path))
            }
            BasicPolicy::All | BasicPolicy::EnrollmentOnly if path == OAUTH_TOKEN_PATH => {
                Some(Purpose::OAuthPassword)
            }
            BasicPolicy::All => Some(Purpose::Marti),
            BasicPolicy::EnrollmentOnly => None,
        }
    }
}

/// Which of the two enrolment purposes a `/Marti/api/tls/` path is.
///
/// The split is what keeps the grace window off the signing endpoints: a spent
/// token presented to `signClient/v2` is [`Purpose::Enrollment`], which no
/// [`Grace`] applies to, whatever the request says about itself.
fn enrollment_purpose(path: &str) -> Purpose {
    match path.starts_with(ENROLLMENT_PROFILE_PREFIX) {
        true => Purpose::EnrollmentProfile,
        false => Purpose::Enrollment,
    }
}

/// The device a request claiming the grace window is for.
///
/// Three conditions, all of them read off the request rather than supplied by
/// it: the purpose is the profile one (so the path is one of the two profile
/// routes), the method is `GET` — they are read-only, and nothing else is
/// routed to them — and the installation has left a window configured.
/// Anything else is [`None`], which is the behaviour every other credential
/// gets. The uid itself is returned owned, because the [`Grace`] borrows it and
/// the query string it came from is percent-encoded.
fn grace_uid(request: &HttpRequest, purpose: Purpose, window: chrono::Duration) -> Option<String> {
    if purpose != Purpose::EnrollmentProfile
        || request.method() != actix_web::http::Method::GET
        || window <= chrono::Duration::zero()
    {
        return None;
    }

    client_uid(request.query_string())
}

/// The `clientUid` of a query string, named case-insensitively as the Marti
/// surface reads every other parameter (`marti::extract::CiQuery`).
///
/// Percent-decoded, because ATAK sends `ANDROID-…` and CloudTAK sends
/// `alice (ETL)` — a space and parentheses — and the value recorded with the
/// spend is the decoded one.
fn client_uid(query: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, value)| name.eq_ignore_ascii_case("clientUid") && !value.is_empty())
        .map(|(_, value)| value.into_owned())
}

/// Who a request is from, given what its listener accepts.
///
/// Certificate, then bearer, then Basic — strongest first, so that a request
/// carrying two credentials is answered as the one that cannot be replayed.
/// Each arm is skipped entirely when the policy does not allow it, rather than
/// being tried and refused.
///
/// The Basic arm is rate limited per client address and username, through the
/// [`RateLimiter`] the listener installed as application data. A listener that
/// installed none refuses Basic outright and says so: an unlimited password
/// endpoint is worse than one that is temporarily unavailable.
///
/// # Errors
///
/// [`AuthFailure::Rejected`] when nothing usable was presented;
/// [`AuthFailure::Forbidden`] when a credential was good and the answer will
/// not change; [`AuthFailure::RateLimited`] when the caller has guessed too
/// often; [`AuthFailure::Unavailable`] when a read fails.
pub async fn resolve_principal<S: Services>(
    services: &S,
    request: &HttpRequest,
    policy: ListenerAuthPolicy,
) -> Result<Resolved, AuthFailure> {
    if policy.cert
        && let Some(peer) = request.conn_data::<PeerCertificate>()
    {
        return super::cert::client_cert(services, peer).await;
    }

    let config = services.config();
    let address = client_address(
        config.server.trust_proxy,
        request.headers(),
        request.peer_addr(),
    );

    if policy.bearer
        && let Some(token) = crate::web::api::middleware::bearer_token(request.headers())
    {
        let facts = RequestFacts {
            method: request.method().as_str(),
            path: request.path(),
            client_ip: address.map(|ip| ip.to_string()),
            headers: request.headers(),
        };

        // A bearer token that is not one of ours is *not* an identity rather
        // than a refusal (design 04 D2): the same header carries mission tokens,
        // and refusing here would break a call that never claimed to be one.
        match bearer(services, token, &facts).await {
            Ok(resolved) => return Ok(resolved),
            Err(AuthFailure::Unavailable(err)) => return Err(AuthFailure::Unavailable(err)),
            Err(failure) => debug!(reason = ?failure, "A bearer token established no identity."),
        }
    }

    // Only where `cookies_allowed` says so — the sign-in endpoints and the TAK
    // surface, never `/api/v1` — and only when the header carried nothing, so
    // that an explicit credential always wins over an ambient one.
    if policy.bearer
        && crate::auth::oauth_server::cookies_allowed(request.path())
        && let Some(token) = crate::auth::oauth_server::access_token_from_cookies(request.headers())
    {
        let facts = RequestFacts {
            method: request.method().as_str(),
            path: request.path(),
            client_ip: address.map(|ip| ip.to_string()),
            headers: request.headers(),
        };

        match bearer(services, &token, &facts).await {
            Ok(resolved) => return Ok(resolved),
            Err(AuthFailure::Unavailable(err)) => return Err(AuthFailure::Unavailable(err)),
            Err(failure) => debug!(reason = ?failure, "A session cookie established no identity."),
        }
    }

    // An orchestrator's workload identity, which arrives in either header and
    // reaches the enrolment routes alone. It is tried *before* the Basic
    // credential below because a JWT presented as a password would otherwise be
    // argon2-verified against every credential the named account holds, at a
    // cost the caller chose; a token this does not claim answers [`None`] and
    // leaves the request exactly as it found it.
    if let Some(outcome) = super::workload::from_request(services, request, address).await {
        return outcome;
    }

    let Some(purpose) = policy.basic_purpose(request.path()) else {
        return Err(AuthFailure::Rejected);
    };

    let Some(credential) = basic_credential(request.headers()) else {
        return Err(AuthFailure::Rejected);
    };

    let Some(limiter) = request.app_data::<web::Data<Arc<RateLimiter>>>() else {
        error!(
            path = request.path(),
            "A listener serving a Basic-authenticated path installed no rate limiter."
        );

        return Err(AuthFailure::Rejected);
    };

    let subject = credential.username.as_str();

    limiter
        .check(address, subject)
        .map_err(AuthFailure::RateLimited)?;

    // The grace names a device this request is about, not a credential the
    // caller gets to choose: both halves are read off the request here.
    let window = config.auth.enrollment_grace;
    let uid = grace_uid(request, purpose, window);
    let grace = uid
        .as_deref()
        .map(|client_uid| Grace { client_uid, window });

    match verify_basic_with_grace(services, &credential, purpose, grace).await {
        Ok(resolved) => {
            limiter.record_success(address, subject);

            Ok(resolved)
        }
        Err(failure) => {
            limiter.record_failure(address, subject);

            Err(failure)
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::test::TestRequest;

    use super::*;
    use crate::db::repos::OidcProfile;
    use crate::testing::TestServer;
    use crate::testing::context::session_for;
    use rustak_api::TokenResponse;

    fn facts(headers: &HeaderMap) -> RequestFacts<'_> {
        RequestFacts {
            method: "GET",
            path: "/api/v1/me",
            client_ip: Some("10.0.0.1".to_string()),
            headers,
        }
    }

    #[tokio::test]
    async fn a_token_we_issued_resolves_to_the_account_it_names() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", true).await;
        let headers = HeaderMap::new();

        let resolved = bearer(&server.context, &session.token, &facts(&headers))
            .await
            .unwrap();

        assert_eq!(resolved.user.id, user.id);
        assert!(resolved.principal.is_admin);
    }

    #[test]
    fn only_the_two_profile_routes_are_the_purpose_a_spent_token_can_reach() {
        // The grace window applies to `Purpose::EnrollmentProfile` alone, so
        // which paths map to it *is* the blast radius.
        let policy = ListenerAuthPolicy::public();

        for path in [
            "/Marti/api/tls/profile/enrollment",
            "/Marti/api/tls/profile/tool/atak/file",
        ] {
            assert_eq!(
                policy.basic_purpose(path),
                Some(Purpose::EnrollmentProfile),
                "{path}",
            );
        }

        for path in [
            "/Marti/api/tls/config",
            "/Marti/api/tls/signClient/v2",
            "/Marti/api/tls/signClient",
            // Not under the enrolment prefix at all: Basic is not read there.
            "/Marti/api/device/profile/connection",
        ] {
            assert_ne!(
                policy.basic_purpose(path),
                Some(Purpose::EnrollmentProfile),
                "{path} must not reach the relaxation",
            );
        }

        assert_eq!(
            policy.basic_purpose("/Marti/api/tls/config"),
            Some(Purpose::Enrollment),
        );
        assert_eq!(
            policy.basic_purpose(OAUTH_TOKEN_PATH),
            Some(Purpose::OAuthPassword),
        );
        assert_eq!(policy.basic_purpose("/Marti/api/device/profile"), None);
    }

    #[test]
    fn the_device_a_grace_is_for_is_read_off_the_query_string() {
        // ATAK sends `clientUid`; CloudTAK sends `alice (ETL)`, percent-encoded,
        // and the uid recorded with the spend is the decoded one.
        assert_eq!(
            client_uid("clientUid=ANDROID-7e0bf5df978a87d8").as_deref(),
            Some("ANDROID-7e0bf5df978a87d8"),
        );
        assert_eq!(
            client_uid("syncSecago=0&CLIENTUID=ada%20(ETL)").as_deref(),
            Some("ada (ETL)"),
        );
        assert_eq!(client_uid("clientUid="), None);
        assert_eq!(client_uid(""), None);
    }

    #[actix_web::test]
    async fn a_grace_is_offered_only_to_a_get_on_a_profile_route() {
        let window = chrono::Duration::minutes(10);
        let get = TestRequest::get()
            .uri("/Marti/api/tls/profile/enrollment?clientUid=ANDROID-1")
            .to_http_request();

        assert_eq!(
            grace_uid(&get, Purpose::EnrollmentProfile, window).as_deref(),
            Some("ANDROID-1"),
        );

        // Wrong purpose, wrong method, no device, no window: each on its own is
        // enough to leave a spent token refused.
        assert_eq!(grace_uid(&get, Purpose::Enrollment, window), None);
        assert_eq!(
            grace_uid(&get, Purpose::EnrollmentProfile, chrono::Duration::zero()),
            None,
        );

        let post = TestRequest::post()
            .uri("/Marti/api/tls/profile/enrollment?clientUid=ANDROID-1")
            .to_http_request();
        assert_eq!(grace_uid(&post, Purpose::EnrollmentProfile, window), None);

        let anonymous = TestRequest::get()
            .uri("/Marti/api/tls/profile/enrollment")
            .to_http_request();
        assert_eq!(
            grace_uid(&anonymous, Purpose::EnrollmentProfile, window),
            None,
        );
    }

    #[tokio::test]
    async fn a_token_we_did_not_sign_is_refused() {
        let server = TestServer::start().await;
        let headers = HeaderMap::new();

        assert!(matches!(
            bearer(&server.context, "not.a.token", &facts(&headers)).await,
            Err(AuthFailure::Rejected)
        ));
    }

    #[tokio::test]
    async fn a_revoked_token_stops_working_before_it_expires() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", false).await;
        let claims = server.jwt().unwrap().verify(&session.token).unwrap();
        let headers = HeaderMap::new();

        crate::auth::tokens::revoke(
            &server.context,
            &claims.jti,
            chrono::DateTime::from_timestamp(claims.exp, 0).unwrap(),
            user.id,
        )
        .await
        .unwrap();

        assert!(matches!(
            bearer(&server.context, &session.token, &facts(&headers)).await,
            Err(AuthFailure::Rejected)
        ));
    }

    #[tokio::test]
    async fn a_token_naming_an_account_that_is_gone_is_refused() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", false).await;
        let headers = HeaderMap::new();

        server.db().users().delete(user.id).await.unwrap();

        assert!(matches!(
            bearer(&server.context, &session.token, &facts(&headers)).await,
            Err(AuthFailure::Rejected)
        ));
    }

    /// A provider-backed account, signed in, with `groups` as its last sign-in
    /// recorded them.
    async fn federated_session(server: &TestServer, groups: &[&str]) -> TokenResponse {
        let claims = serde_json::json!({ "groups": groups }).to_string();
        let user = server
            .db()
            .users()
            .upsert_oidc(
                "https://id.example.com",
                "subject-1",
                &Username::parse("ada").unwrap(),
                OidcProfile {
                    claims: Some(claims),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // Issued with administrative scope, so the scope ceiling is not what
        // decides and the expressions are.
        session_for(&server.context, &user, true).await
    }

    #[tokio::test]
    async fn a_configured_expression_can_refuse_a_request_it_does_not_like() {
        let server = TestServer::start_with(|config| {
            config.auth.user_acl = Some(filt_rs::Filter::new(r#"path != "/api/v1/me""#).unwrap());
        })
        .await;
        let session = federated_session(&server, &[]).await;
        let headers = HeaderMap::new();

        assert!(matches!(
            bearer(&server.context, &session.token, &facts(&headers)).await,
            Err(AuthFailure::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn a_provider_account_is_judged_by_the_claims_its_sign_in_recorded() {
        let server = TestServer::start_with(|config| {
            config.auth.user_acl =
                Some(filt_rs::Filter::new(r#"claims.groups contains "tak-users""#).unwrap());
            config.auth.admin_acl =
                Some(filt_rs::Filter::new(r#"claims.groups contains "tak-admins""#).unwrap());
        })
        .await;
        let headers = HeaderMap::new();

        let refused = federated_session(&server, &["something-else"]).await;
        assert!(matches!(
            bearer(&server.context, &refused.token, &facts(&headers)).await,
            Err(AuthFailure::Forbidden(_))
        ));

        let admitted = federated_session(&server, &["tak-users", "tak-admins"]).await;
        let resolved = bearer(&server.context, &admitted.token, &facts(&headers))
            .await
            .expect("the recorded claims satisfy the expression");
        assert!(
            resolved.principal.is_admin,
            "and `admin_acl` is answered from the same claims"
        );
    }

    #[tokio::test]
    async fn an_expression_written_for_the_directory_does_not_lock_out_a_passkey() {
        // The administrator's passkey exists for when the directory is wrong or
        // away, so `claims.*` — which a local account never has — must not be
        // the thing that refuses them.
        let server = TestServer::start_with(|config| {
            config.auth.user_acl =
                Some(filt_rs::Filter::new(r#"claims.groups contains "tak-users""#).unwrap());
        })
        .await;
        let (_, session) = server.signed_in("ada", true).await;
        let headers = HeaderMap::new();

        let resolved = bearer(&server.context, &session.token, &facts(&headers))
            .await
            .expect("a local account is not gated by user_acl");
        assert!(resolved.principal.is_admin);
    }

    #[tokio::test]
    async fn an_installation_with_no_expression_relies_on_the_account_instead() {
        // Applying a default-deny expression to a surface that cannot satisfy
        // it would refuse every request on an installation that never wrote one.
        let server = TestServer::start_with(|config| {
            config.auth.user_acl = None;
        })
        .await;
        let (_, session) = server.signed_in("ada", false).await;
        let headers = HeaderMap::new();

        assert!(
            bearer(&server.context, &session.token, &facts(&headers))
                .await
                .is_ok()
        );
    }

    #[test]
    fn basic_reaches_the_two_paths_that_have_no_alternative_and_no_others() {
        let policy = ListenerAuthPolicy::public();

        assert_eq!(
            policy.basic_purpose("/Marti/api/tls/config"),
            Some(Purpose::Enrollment),
        );
        assert_eq!(
            policy.basic_purpose("/Marti/api/tls/signClient/v2"),
            Some(Purpose::Enrollment),
        );
        assert_eq!(
            policy.basic_purpose(OAUTH_TOKEN_PATH),
            Some(Purpose::OAuthPassword),
        );

        for path in [
            "/Marti/api/version",
            "/Marti/api/groups/all",
            "/api/v1/users",
            "/oauth/jwks",
            // Close enough to look right and not one of the two.
            "/Marti/api/tls",
        ] {
            assert_eq!(
                policy.basic_purpose(path),
                None,
                "{path} must not be a password-guessing oracle",
            );
        }
    }

    #[test]
    fn a_listener_that_refuses_basic_reads_no_password_anywhere() {
        let policy = ListenerAuthPolicy {
            basic: BasicPolicy::Off,
            ..ListenerAuthPolicy::marti()
        };

        assert_eq!(policy.basic_purpose("/Marti/api/tls/config"), None);
        assert_eq!(policy.basic_purpose(OAUTH_TOKEN_PATH), None);
    }

    #[test]
    fn an_installation_that_opens_basic_up_still_scopes_it_by_path() {
        // Nothing selects `All` today; when something does, an enrolment path
        // must still mean `Enrollment` rather than the general Marti purpose,
        // or a client password would start enrolling devices by accident.
        let policy = ListenerAuthPolicy {
            basic: BasicPolicy::All,
            ..ListenerAuthPolicy::marti()
        };

        assert_eq!(
            policy.basic_purpose("/Marti/api/tls/config"),
            Some(Purpose::Enrollment),
        );
        assert_eq!(
            policy.basic_purpose(OAUTH_TOKEN_PATH),
            Some(Purpose::OAuthPassword),
        );
        assert_eq!(
            policy.basic_purpose("/Marti/api/groups/all"),
            Some(Purpose::Marti),
        );
    }

    #[test]
    fn only_the_mutually_authenticated_listener_reads_a_certificate() {
        assert!(!ListenerAuthPolicy::public().cert);
        assert!(ListenerAuthPolicy::marti().cert);
        assert!(ListenerAuthPolicy::public().bearer);
        assert!(ListenerAuthPolicy::marti().bearer);
    }

    #[actix_web::test]
    async fn a_basic_path_with_no_rate_limiter_installed_refuses_rather_than_guesses_freely() {
        // An unlimited password endpoint is worse than one that is briefly
        // unavailable, so a listener that forgot the limiter fails closed.
        let server = TestServer::start().await;
        let request = actix_web::test::TestRequest::get()
            .uri("/Marti/api/tls/config")
            .insert_header(("authorization", "Basic YWRhOnNlY3JldA=="))
            .app_data(web::Data::new(server.context.clone()))
            .to_http_request();

        assert!(matches!(
            resolve_principal(&server.context, &request, ListenerAuthPolicy::public()).await,
            Err(AuthFailure::Rejected),
        ));
    }

    #[actix_web::test]
    async fn a_bearer_token_is_preferred_where_the_policy_allows_both() {
        let server = TestServer::start().await;
        let (user, session) = server.signed_in("ada", false).await;
        let request = actix_web::test::TestRequest::get()
            .uri("/Marti/api/tls/config")
            .insert_header(("authorization", crate::testing::context::bearer(&session)))
            .app_data(web::Data::new(server.context.clone()))
            .app_data(web::Data::new(Arc::clone(&server.limiter)))
            .to_http_request();

        let resolved = resolve_principal(&server.context, &request, ListenerAuthPolicy::public())
            .await
            .unwrap();

        assert_eq!(resolved.user.id, user.id);
        assert!(resolved.claims.is_some());
    }

    #[actix_web::test]
    async fn a_request_with_nothing_to_offer_is_rejected() {
        let server = TestServer::start().await;
        let request = actix_web::test::TestRequest::get()
            .uri("/Marti/api/version")
            .app_data(web::Data::new(server.context.clone()))
            .app_data(web::Data::new(Arc::clone(&server.limiter)))
            .to_http_request();

        assert!(matches!(
            resolve_principal(&server.context, &request, ListenerAuthPolicy::public()).await,
            Err(AuthFailure::Rejected),
        ));
    }

    #[tokio::test]
    async fn an_expression_can_grant_administrative_access_per_request() {
        // The account is not stored as an administrator; the expression is what
        // makes this request an administrative one. The token carries the admin
        // scope, because after R-01 H1 the granted scope is a ceiling over
        // every source of administrative access, the expression included.
        let server = TestServer::start_with(|config| {
            config.auth.admin_acl = Some(filt_rs::Filter::new(r#"username == "ada""#).unwrap());
        })
        .await;
        let user = server.user("ada", false).await;
        let (token, _) = server
            .jwt()
            .unwrap()
            .issue(&user.username, "api admin", None, None)
            .unwrap();
        let headers = HeaderMap::new();

        let resolved = bearer(&server.context, &token, &facts(&headers))
            .await
            .unwrap();

        assert!(resolved.principal.is_admin);
    }

    #[tokio::test]
    async fn an_expression_cannot_widen_a_token_that_was_granted_no_admin_scope() {
        // R-01 H1. Both the installation's policy and the credential's own
        // grant have to say "administrator"; the narrower of the two wins, so a
        // password-grant token held by somebody an expression would promote is
        // still only an ordinary session.
        let server = TestServer::start_with(|config| {
            config.auth.admin_acl = Some(filt_rs::Filter::new(r#"username == "ada""#).unwrap());
        })
        .await;
        let user = server.user("ada", false).await;
        let (token, _) = server
            .jwt()
            .unwrap()
            .issue(&user.username, "api", None, None)
            .unwrap();
        let headers = HeaderMap::new();

        let resolved = bearer(&server.context, &token, &facts(&headers))
            .await
            .unwrap();

        assert!(!resolved.principal.is_admin);
    }

    #[tokio::test]
    async fn an_expression_still_takes_administrative_access_away_per_request() {
        // The narrowing direction is the one that must stay live: a token
        // granted the admin scope is still refused when the expression that
        // guards this request says no.
        let server = TestServer::start_with(|config| {
            config.auth.admin_acl = Some(filt_rs::Filter::new(r#"username == "nobody""#).unwrap());
        })
        .await;
        let user = server.user("ada", false).await;
        let (token, _) = server
            .jwt()
            .unwrap()
            .issue(&user.username, "api admin", None, None)
            .unwrap();
        let headers = HeaderMap::new();

        let resolved = bearer(&server.context, &token, &facts(&headers))
            .await
            .unwrap();

        assert!(!resolved.principal.is_admin);
    }
}

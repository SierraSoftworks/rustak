//! Who a Marti request is from, and the seam where that answer grows.
//!
//! # Anonymous is a real answer here
//!
//! Most of `/api/v1` needs a session before the handler runs. The Marti surface
//! does not: `/Marti/api/version` is probed before enrolment, `/files/api/config`
//! is what CloudTAK's setup wizard calls with nothing but a certificate, and
//! `/Marti/api/util/isAdmin` answers `false` rather than `401`. So
//! [`MartiPrincipal`] carries an **optional** identity and a handler says what it
//! needs — [`MartiPrincipal::require`] or
//! [`MartiPrincipal::require_admin`] — rather than the extractor deciding for
//! every route at once.
//!
//! That is also why a bearer token we cannot verify resolves to *anonymous*
//! rather than to a refusal: design 04 D2 makes the `Authorization` header
//! usable for two different things at once (our identity token and a mission
//! token), so "this is not one of ours" has to mean "not an identity" and not
//! "go away".
//!
//! # The seam
//!
//! [`auth_policy`] is the one function that says which credentials a listener
//! accepts, and the resolution below hands the answer straight to
//! [`crate::auth::resolve_principal`], which tries each allowed credential in
//! turn. Widening what a listener accepts is therefore a change to
//! [`auth_policy`] alone; no route file changes.

use std::future::Future;
use std::pin::Pin;

use actix_web::{FromRequest, HttpRequest, dev::Payload, web};
use rustak_core::identity::Principal;

use crate::auth::resolve::{BasicPolicy, ListenerAuthPolicy, Resolved, resolve_principal};
use crate::prelude::*;

use super::error::MartiError;
use super::extract::{ApiVersion, ListenerRole};

/// Which credentials a listener accepts.
///
/// A struct rather than three arguments so that adding a fourth kind is a field
/// with a doc comment rather than another positional `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthPolicy {
    /// Our own RS256 access token in `Authorization: Bearer`.
    pub bearer: bool,

    /// A client certificate this installation's CA issued.
    ///
    /// The whole reason `:8443` exists. Resolution arrives with M2-03; until
    /// then this is `false` on both listeners and a certificate is ignored
    /// rather than half-trusted.
    pub client_cert: bool,

    /// `Authorization: Basic`, username and client password.
    ///
    /// Scoped to enrolment in `conventions.md`'s security defaults, so it is
    /// never blanket-enabled for the Marti surface. M2-03 owns it.
    pub basic: bool,
}

impl AuthPolicy {
    /// The same policy in the form [`crate::auth::resolve_principal`] takes.
    ///
    /// The Basic arm widens to
    /// [`crate::auth::BasicPolicy::EnrollmentOnly`]
    /// rather than to `All`: a listener saying "Basic is accepted" means "on
    /// the paths that have no alternative", which is where
    /// `conventions.md` leaves it.
    pub fn listener(self) -> ListenerAuthPolicy {
        ListenerAuthPolicy {
            cert: self.client_cert,
            bearer: self.bearer,
            basic: if self.basic {
                BasicPolicy::EnrollmentOnly
            } else {
                BasicPolicy::Off
            },
        }
    }
}

/// Which credentials `role` accepts.
///
/// The seam M2-03 widens; see the module documentation.
pub fn auth_policy(role: ListenerRole) -> AuthPolicy {
    match role {
        // No certificate is asked for at the public handshake, so there is
        // never one to read here however the request arrived.
        ListenerRole::Public => AuthPolicy {
            bearer: true,
            client_cert: false,
            basic: true,
        },
        // Bearer and Basic stay on: CloudTAK is configured with three
        // independent base URLs that may all name this port, and refusing a
        // credential because of which socket it arrived on would make the
        // deployment layout part of the contract.
        ListenerRole::Marti => AuthPolicy {
            bearer: true,
            client_cert: true,
            basic: true,
        },
    }
}

/// Who a Marti request is from.
#[derive(Debug, Clone)]
pub struct MartiPrincipal {
    /// The caller, when one was established. `None` is anonymous, which most of
    /// this surface serves.
    pub identity: Option<Resolved>,

    /// Which listener the request arrived on.
    pub listener: ListenerRole,

    /// The payload shape the client said it can read.
    pub api_version: ApiVersion,
}

impl MartiPrincipal {
    /// The rights this request carries, when it carries any.
    pub fn principal(&self) -> Option<&Principal> {
        self.identity.as_ref().map(|resolved| &resolved.principal)
    }

    /// The caller's name, when there is one.
    pub fn username(&self) -> Option<&str> {
        self.identity
            .as_ref()
            .map(|resolved| resolved.user.username.as_str())
    }

    /// Whether the caller administers this installation.
    ///
    /// `false` for an anonymous request rather than an error: `/util/isAdmin`
    /// is a question, not a gate.
    pub fn is_admin(&self) -> bool {
        self.principal().is_some_and(|principal| principal.is_admin)
    }

    /// Whether nobody was identified.
    pub fn is_anonymous(&self) -> bool {
        self.identity.is_none()
    }

    /// The caller, or a refusal for a route that needs one.
    ///
    /// # Errors
    ///
    /// [`MartiError::Unauthorized`].
    pub fn require(&self) -> Result<&Resolved, MartiError> {
        self.identity.as_ref().ok_or_else(|| {
            MartiError::Unauthorized("this endpoint requires a credential".to_string())
        })
    }

    /// The caller, when they administer this installation.
    ///
    /// # Errors
    ///
    /// [`MartiError::Unauthorized`] with no credential, and
    /// [`MartiError::Forbidden`] with one that is not an administrator's —
    /// the distinction matters because the second will not change by signing in
    /// again.
    pub fn require_admin(&self) -> Result<&Resolved, MartiError> {
        let resolved = self.require()?;

        if resolved.principal.is_admin {
            return Ok(resolved);
        }

        Err(MartiError::Forbidden(
            "this endpoint is for administrators".to_string(),
        ))
    }
}

impl FromRequest for MartiPrincipal {
    type Error = MartiError;
    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        let request = request.clone();

        Box::pin(async move {
            let listener = request
                .app_data::<web::Data<ListenerRole>>()
                .map_or(ListenerRole::Public, |role| *role.get_ref());

            Ok(MartiPrincipal {
                identity: resolve(&request, auth_policy(listener)).await,
                listener,
                api_version: ApiVersion::of(&request),
            })
        })
    }
}

/// Resolves whatever credential the policy accepts, or nobody.
///
/// Never an error. A credential we could not verify is *absent*, because
/// `Authorization: Bearer` also carries mission tokens (design 04 D2) and
/// refusing the request would break a mission call that never claimed to be an
/// identity in the first place.
async fn resolve(request: &HttpRequest, policy: AuthPolicy) -> Option<Resolved> {
    let context = request.app_data::<web::Data<AppContext>>()?;

    match resolve_principal(context.get_ref(), request, policy.listener()).await {
        Ok(resolved) => Some(resolved),
        Err(failure) => {
            debug!(
                reason = ?failure,
                "A Marti request carried no identity we could establish.",
            );

            None
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::test::TestRequest;

    use super::*;
    use crate::testing::{TestServer, context::bearer as bearer_header};

    /// A request carrying whatever the test wants to say about itself.
    async fn extract(server: &TestServer, authorization: Option<&str>) -> MartiPrincipal {
        let mut request = TestRequest::get()
            .uri("/Marti/api/util/isAdmin")
            .app_data(web::Data::new(server.context.clone()))
            .app_data(web::Data::new(ListenerRole::Public));

        if let Some(value) = authorization {
            request = request.insert_header(("authorization", value.to_string()));
        }

        MartiPrincipal::extract(&request.to_http_request())
            .await
            .expect("the extractor never refuses a request")
    }

    #[actix_web::test]
    async fn a_request_with_no_credential_is_anonymous_rather_than_refused() {
        // `/Marti/api/version` is probed before enrolment; a 401 there would
        // stop a device before it could ask for a certificate.
        let server = TestServer::start().await;

        let who = extract(&server, None).await;

        assert!(who.is_anonymous());
        assert!(!who.is_admin());
        assert_eq!(who.username(), None);
        assert_eq!(who.listener, ListenerRole::Public);
        assert_eq!(who.api_version, ApiVersion(2));
    }

    #[actix_web::test]
    async fn one_of_our_own_tokens_names_the_account_it_was_issued_to() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;

        let who = extract(&server, Some(&bearer_header(&session))).await;

        assert_eq!(who.username(), Some("ada"));
        assert!(who.is_admin());
        assert!(who.require().is_ok());
        assert!(who.require_admin().is_ok());
    }

    #[actix_web::test]
    async fn a_bearer_token_that_is_not_ours_is_no_identity_rather_than_a_refusal() {
        // The same header carries mission tokens (design 04 D2). Refusing here
        // would break a mission call that never claimed to be an identity.
        let server = TestServer::start().await;

        let who = extract(&server, Some("Bearer not.a.token")).await;

        assert!(who.is_anonymous());
    }

    #[actix_web::test]
    async fn a_route_that_needs_somebody_says_which_kind_of_no_it_is() {
        // 401 means "try again with a credential"; 403 means "not with that
        // one". A client that cannot tell them apart loops on the sign-in.
        let server = TestServer::start().await;

        let nobody = extract(&server, None).await;
        assert_eq!(nobody.require().unwrap_err().status().as_u16(), 401);
        assert_eq!(nobody.require_admin().unwrap_err().status().as_u16(), 401);

        let (_, session) = server.signed_in("ada", false).await;
        let ordinary = extract(&server, Some(&bearer_header(&session))).await;
        assert!(ordinary.require().is_ok());
        assert_eq!(ordinary.require_admin().unwrap_err().status().as_u16(), 403,);
    }

    #[actix_web::test]
    async fn the_declared_payload_version_reaches_the_handler() {
        let server = TestServer::start().await;
        let request = TestRequest::get()
            .uri("/Marti/api/missions")
            .insert_header(("API_VERSION", "3"))
            .app_data(web::Data::new(server.context.clone()))
            .app_data(web::Data::new(ListenerRole::Marti))
            .to_http_request();

        let who = MartiPrincipal::extract(&request).await.unwrap();

        assert_eq!(who.api_version, ApiVersion(3));
        assert_eq!(who.listener, ListenerRole::Marti);
    }

    #[test]
    fn only_the_mutually_authenticated_listener_reads_a_client_certificate() {
        // Asserted so that widening what a listener accepts is a deliberate
        // change to this test rather than a silent one.
        for role in [ListenerRole::Public, ListenerRole::Marti] {
            let policy = auth_policy(role);

            assert!(policy.bearer, "{role:?}");
            assert!(policy.basic, "{role:?}");
        }

        assert!(
            !auth_policy(ListenerRole::Public).client_cert,
            "the public listener never asks for a certificate, so there is never one to read",
        );
        assert!(auth_policy(ListenerRole::Marti).client_cert);
    }

    #[actix_web::test]
    async fn a_policy_that_accepts_nothing_resolves_nobody() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        let request = TestRequest::get()
            .uri("/Marti/api/version")
            .insert_header(("authorization", bearer_header(&session)))
            .app_data(web::Data::new(server.context.clone()))
            .to_http_request();

        let nothing = AuthPolicy {
            bearer: false,
            client_cert: false,
            basic: false,
        };

        assert!(resolve(&request, nothing).await.is_none());
    }
}

//! `/Marti/api/missions/**` — Data Sync, as the wire spells it.
//!
//! # Registration order is the contract
//!
//! `/missions/{name}` would happily match `/missions/all`, `/missions/logs`,
//! `/missions/guid` and `/missions/invitations`, and actix matches services in
//! the order they were registered. Every literal is therefore registered before
//! the parameterised shape that would also match it, the GUID family before the
//! name family, and [`model::RESERVED_NAMES`] refuses those names at creation
//! so that the two cannot disagree. The ordering test at the bottom of this
//! file asks for each literal path and asserts it was not read as a name.
//!
//! [`model::RESERVED_NAMES`]: crate::missions::model::RESERVED_NAMES
//!
//! # Invitations, logs and layers
//!
//! Mounted by `reserved_routes` — which no longer reserves anything, and is
//! kept under that name because it is the one place the four literal paths
//! below `/missions/` are registered ahead of `{name}`.

pub mod changes;
pub mod contents;
pub mod crud;
pub mod external;
pub mod invitations;
pub mod layers;
pub mod logs;
pub mod misc;
pub mod subscription;

use std::future::Future;
use std::pin::Pin;

use actix_web::{FromRequest, HttpRequest, dev::Payload, web};

use crate::auth::{MissionClaims, MissionTokens, mission_bearer, mission_token};
use crate::missions::MissionService;
use crate::prelude::*;

use super::error::{MartiError, MartiResult};
use super::principal::MartiPrincipal;

/// Everything a mission handler needs before it has read a parameter.
///
/// One extractor rather than three, because resolving the mission token is not
/// free — it reads a sealed secret — and doing it once per request is the
/// difference between a cheap read and three of them.
pub struct MissionCtx {
    /// The service every handler delegates to.
    pub service: MissionService,
    /// Who is asking.
    pub who: MartiPrincipal,
    /// The mission token the request carried, when it carried one that
    /// verified. A token that did not verify is *absent*, never a refusal.
    pub claims: Option<MissionClaims>,
}

impl MissionCtx {
    /// The claims, for passing to the service.
    pub fn claims(&self) -> Option<&MissionClaims> {
        self.claims.as_ref()
    }
}

impl FromRequest for MissionCtx {
    type Error = MartiError;
    type Future = Pin<Box<dyn Future<Output = Result<Self, Self::Error>>>>;

    fn from_request(request: &HttpRequest, payload: &mut Payload) -> Self::Future {
        let who = MartiPrincipal::from_request(request, payload);
        let request = request.clone();

        Box::pin(async move {
            let who = who.await?;
            let context = request
                .app_data::<web::Data<AppContext>>()
                .ok_or_else(|| {
                    MartiError::Internal("the mission API has no application context".to_string())
                })?
                .get_ref()
                .clone();
            let claims = claims_for(&context, &request, &who).await;

            Ok(MissionCtx {
                service: MissionService::new(context),
                who,
                claims,
            })
        })
    }
}

/// The mission token a request carried, when it carried one that verifies.
///
/// Never an error: `Authorization: Bearer` also carries our identity tokens, so
/// "this is not a mission token" has to mean "no mission token" rather than
/// "go away".
async fn claims_for(
    context: &AppContext,
    request: &HttpRequest,
    who: &MartiPrincipal,
) -> Option<MissionClaims> {
    let raw = mission_bearer(request, mission_token::identity_used_authorization(who))?;

    match MissionTokens::load(context).await {
        Ok(tokens) => tokens.verify(&raw).ok(),
        Err(err) => {
            warn!(error = %err, "Could not read the mission-token secret.");

            None
        }
    }
}

/// Registers the whole mission surface inside the `/Marti/api` scope.
pub fn routes(config: &mut web::ServiceConfig) {
    reserved_routes(config);

    config
        // The two administrative listings, before `/missions/{name}/…` would
        // read `all` as a mission called "all".
        .route(
            "/missions/all/subscriptions/guid",
            web::get().to(subscription::all_by_guid),
        )
        .route(
            "/missions/all/subscriptions",
            web::get().to(subscription::all),
        )
        // The collection itself.
        .route("/pagedmissions", web::get().to(crud::paged))
        .route("/missioncount", web::get().to(crud::count))
        .route("/missions", web::get().to(crud::list))
        .route("/missions", web::delete().to(crud::delete_by_guid));

    // The GUID family first: `/missions/guid/{guid}/…` would otherwise be read
    // as a mission named `guid` with a child route.
    family(config, "/missions/guid/{guid}", false);
    family(config, "/missions/{name}", true);

    config
        .route("/missions/guid/{guid}", web::get().to(crud::get))
        .route("/missions/{name}", web::get().to(crud::get))
        .route("/missions/{name}", web::put().to(crud::create))
        .route("/missions/{name}", web::post().to(crud::create))
        .route("/missions/{name}", web::delete().to(crud::delete));
}

/// The four literal paths below `/missions/`, registered ahead of `{name}`.
///
/// Registered first so that `/missions/logs/entries` is never read as a
/// mission called `logs`, and `/missions/invitations` never as one called
/// `invitations` — both of which [`model::RESERVED_NAMES`] also refuses at
/// creation, so that the two cannot disagree.
///
/// [`model::RESERVED_NAMES`]: crate::missions::model::RESERVED_NAMES
fn reserved_routes(config: &mut web::ServiceConfig) {
    config
        .route("/missions/all/invitations", web::get().to(invitations::all))
        .route("/missions/all/logs", web::get().to(logs::all))
        .route("/missions/logs/entries", web::post().to(logs::create))
        .route("/missions/logs/entries", web::put().to(logs::update))
        .route("/missions/logs/entries/{id}", web::get().to(logs::get))
        .route(
            "/missions/logs/entries/{id}",
            web::delete().to(logs::remove),
        )
        .route(
            "/missions/invitations",
            web::get().to(invitations::for_client),
        );
}

/// The routes that hang off one mission, in either spelling.
///
/// `by_name` mounts the four routes TAK Server has no GUID form for — keywords,
/// the archive, the package import, the copy and the access token — which
/// CloudTAK falls back to the name family for anyway.
fn family(config: &mut web::ServiceConfig, prefix: &str, by_name: bool) {
    let at = |tail: &str| format!("{prefix}{tail}");

    config
        // `/subscriptions/roles` before `/subscriptions`, and both before the
        // singular `/subscription` they would not match anyway.
        .route(
            &at("/subscriptions/roles"),
            web::get().to(subscription::roles),
        )
        .route(&at("/subscriptions"), web::get().to(subscription::list))
        .route(&at("/subscription"), web::put().to(subscription::subscribe))
        .route(&at("/subscription"), web::get().to(subscription::get))
        .route(
            &at("/subscription"),
            web::delete().to(subscription::unsubscribe),
        )
        .route(
            &at("/subscription"),
            web::post().to(subscription::set_roles),
        )
        .route(&at("/role"), web::get().to(subscription::role))
        .route(&at("/role"), web::put().to(subscription::set_role))
        .route(&at("/password"), web::put().to(subscription::set_password))
        .route(
            &at("/password"),
            web::delete().to(subscription::clear_password),
        )
        .route(
            &at("/expiration"),
            web::put().to(subscription::set_expiration),
        )
        .route(&at("/contents"), web::put().to(contents::add))
        .route(&at("/contents"), web::delete().to(contents::remove))
        .route(&at("/changes"), web::get().to(changes::listing))
        .route(&at("/cot"), web::get().to(changes::cot))
        .route(&at("/children"), web::get().to(misc::children))
        .route(&at("/parent"), web::get().to(misc::parent))
        .route(&at("/parent"), web::delete().to(misc::clear_parent))
        .route(&at("/send"), web::post().to(misc::send))
        .route(&at("/contacts"), web::get().to(misc::contacts))
        .route(&at("/invitations"), web::get().to(invitations::listing))
        .route(&at("/invite"), web::post().to(invitations::invite_bulk))
        .route(
            &at("/invite/{kind}/{invitee}"),
            web::put().to(invitations::invite),
        )
        .route(
            &at("/invite/{kind}/{invitee}"),
            web::delete().to(invitations::uninvite),
        )
        .route(&at("/log"), web::get().to(logs::listing))
        // The three edit routes before the bare `/layers` they sit under.
        .route(&at("/layers/parent"), web::put().to(layers::reparent))
        .route(&at("/layers/{uid}/name"), web::put().to(layers::rename))
        .route(
            &at("/layers/{uid}/position"),
            web::put().to(layers::position),
        )
        .route(&at("/layers"), web::get().to(layers::listing))
        .route(&at("/layers"), web::put().to(layers::create))
        .route(&at("/layers"), web::delete().to(layers::remove))
        .route(
            &at("/maplayers/{uid}"),
            web::delete().to(external::delete_map_layer),
        )
        .route(&at("/maplayers"), web::post().to(external::put_map_layer))
        .route(&at("/maplayers"), web::put().to(external::put_map_layer))
        .route(
            &at("/externaldata/{id}"),
            web::delete().to(external::delete_external),
        )
        .route(&at("/externaldata"), web::post().to(external::put_external))
        .route(&at("/feed/{uid}"), web::delete().to(external::delete_feed))
        .route(&at("/feed"), web::post().to(external::put_feed))
        .route(&at("/feed"), web::delete().to(external::put_feed));

    if by_name {
        config
            .route(
                "/missions/{name}/contents/missionpackage",
                web::put().to(contents::import),
            )
            .route(
                "/missions/{name}/uid/{uid}/keywords",
                web::put().to(contents::uid_keywords),
            )
            .route(
                "/missions/{name}/uid/{uid}/keywords",
                web::delete().to(contents::uid_keywords),
            )
            .route(
                "/missions/{name}/content/{hash}/keywords",
                web::put().to(contents::content_keywords),
            )
            .route(
                "/missions/{name}/content/{hash}/keywords",
                web::delete().to(contents::content_keywords),
            )
            .route(
                "/missions/{name}/keywords/{keyword}",
                web::delete().to(contents::delete_keyword),
            )
            .route(
                "/missions/{name}/keywords",
                web::put().to(contents::set_keywords),
            )
            .route(
                "/missions/{name}/keywords",
                web::delete().to(contents::clear_keywords),
            )
            .route("/missions/{name}/archive", web::get().to(contents::archive))
            .route("/missions/{name}/copy", web::put().to(misc::copy))
            .route(
                "/missions/{name}/token",
                web::get().to(subscription::access_token),
            )
            .route(
                "/missions/{name}/parent/{parent}",
                web::put().to(misc::set_parent),
            );

        return;
    }

    config.route(
        "/missions/guid/{guid}/parent/guid/{parent}",
        web::put().to(misc::set_parent),
    );
}

/// A mission route M4-02 fills in.
///
/// # Errors
///
/// Always [`MartiError::NotImplemented`].
pub async fn reserved() -> MartiResult {
    Err(MartiError::NotImplemented("this mission endpoint"))
}

#[cfg(test)]
mod tests {
    use actix_web::{App, test};

    use crate::testing::TestServer;

    /// Paths whose first segment after `/missions/` is a literal this scope
    /// owns, and which must therefore never be read as a mission name.
    ///
    /// Since M4-02 filled these in, several answer a perfectly ordinary `404`
    /// for the *thing* they name — a log entry that is not there. What must
    /// never happen is a `404` naming a **mission** called `all`, `logs` or
    /// `invitations`, which is what falling through to `{name}` looks like.
    const LITERALS: &[(&str, &str)] = &[
        ("GET", "/Marti/api/missions/all/subscriptions"),
        ("GET", "/Marti/api/missions/all/subscriptions/guid"),
        ("GET", "/Marti/api/missions/all/invitations"),
        ("GET", "/Marti/api/missions/all/logs"),
        ("POST", "/Marti/api/missions/logs/entries"),
        ("GET", "/Marti/api/missions/logs/entries/abc"),
        ("GET", "/Marti/api/missions/invitations"),
    ];

    #[actix_web::test]
    async fn a_literal_segment_is_never_read_as_a_mission_name() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        for (method, uri) in LITERALS {
            let response = test::call_service(
                &app,
                test::TestRequest::default()
                    .method(method.parse().unwrap())
                    .uri(uri)
                    .to_request(),
            )
            .await;

            let status = response.status().as_u16();
            let body = test::read_body(response).await;
            let text = String::from_utf8_lossy(&body).to_string();

            assert!(
                !(status == 404 && text.contains("Mission ")),
                "{method} {uri} fell through to the mission-name route: {text}",
            );
        }
    }

    #[actix_web::test]
    async fn the_guid_family_is_matched_before_a_mission_named_guid() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/Marti/api/missions/guid/7e57d004-2b97-0e7a-b45f-5387367791cd/changes")
                .to_request(),
        )
        .await;

        // 404 for the mission itself is right; what must not happen is the
        // request being read as a mission called `guid`.
        assert_eq!(response.status().as_u16(), 404);

        let body = test::read_body(response).await;
        let text = String::from_utf8_lossy(&body);

        assert!(
            text.contains("7e57d004-2b97-0e7a-b45f-5387367791cd"),
            "the guid was read as the mission identifier: {text}",
        );
    }

    #[actix_web::test]
    async fn the_mission_kml_stub_still_wins_over_the_name_family() {
        let server = TestServer::start().await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/Marti/api/missions/Alpha/kml")
                .to_request(),
        )
        .await;

        assert_eq!(response.status().as_u16(), 501);
    }
}

//! `/api/v1/me` and `PATCH /api/v1/me/preferences`: what an account has chosen
//! about how the console looks to it, as the page that changes it sees it.

#![cfg(feature = "testing")]

use actix_web::http::StatusCode;
use actix_web::{App, test};
use rustak_api::{Me, Symbology, TokenResponse, UserPreferences};
use rustak_server::testing::TestServer;
use rustak_server::testing::context::bearer;

macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

macro_rules! me {
    ($app:expr, $session:expr) => {{
        let me: Me = test::call_and_read_body_json(
            $app,
            test::TestRequest::get()
                .uri("/api/v1/me")
                .insert_header(("authorization", bearer($session)))
                .to_request(),
        )
        .await;
        me
    }};
}

fn change(session: &TokenResponse, body: serde_json::Value) -> test::TestRequest {
    test::TestRequest::patch()
        .uri("/api/v1/me/preferences")
        .insert_header(("authorization", bearer(session)))
        .set_json(body)
}

#[actix_web::test]
async fn an_account_that_has_chosen_nothing_is_told_the_defaults() {
    let server = TestServer::start().await;
    let (_, ada) = server.signed_in("ada", false).await;
    let app = app!(server);

    assert_eq!(me!(&app, &ada).preferences, UserPreferences::default());
}

#[actix_web::test]
async fn anybody_may_change_their_own_and_nobody_elses_moves() {
    // Not an administrator: these are the account's own, so no more than being
    // signed in is asked for.
    let server = TestServer::start().await;
    let (_, ada) = server.signed_in("ada", false).await;
    let (_, grace) = server.signed_in("grace", true).await;
    let app = app!(server);

    let answered: UserPreferences = test::call_and_read_body_json(
        &app,
        change(&ada, serde_json::json!({ "symbology": "2525d" })).to_request(),
    )
    .await;

    assert_eq!(answered.symbology, Symbology::Milstd2525D);
    assert_eq!(
        me!(&app, &ada).preferences.symbology,
        Symbology::Milstd2525D
    );
    assert_eq!(
        me!(&app, &grace).preferences.symbology,
        Symbology::Milstd2525C
    );
}

#[actix_web::test]
async fn a_change_that_says_nothing_or_names_no_edition_we_know_is_refused() {
    let server = TestServer::start().await;
    let (_, ada) = server.signed_in("ada", false).await;
    let app = app!(server);

    for body in [
        serde_json::json!({}),
        serde_json::json!({ "symbology": "app6d" }),
    ] {
        let response = test::call_service(&app, change(&ada, body.clone()).to_request()).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
    }

    assert_eq!(me!(&app, &ada).preferences, UserPreferences::default());
}

#[actix_web::test]
async fn nobody_in_particular_has_no_preferences_to_change() {
    let server = TestServer::start().await;
    let app = app!(server);

    let response = test::call_service(
        &app,
        test::TestRequest::patch()
            .uri("/api/v1/me/preferences")
            .set_json(serde_json::json!({ "symbology": "2525d" }))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

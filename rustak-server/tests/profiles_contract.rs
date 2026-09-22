//! Device profiles end to end: what ATAK actually receives, and with which
//! status.
//!
//! The unit tests inside `src/profiles/**` and `src/marti/profiles.rs` check
//! the pieces — the renderer's bytes, the zip layout, the path matcher. This
//! suite checks the thing they add up to, against the real application from
//! `web::server::services`, because everything worth catching here is a wiring
//! failure a handler tested in isolation cannot see.
//!
//! # What breaks if these fail
//!
//! * **A status.** ATAK treats anything other than `200`, `204` and `304` as a
//!   `ConnectionException`, which surfaces to the user as a failed connection
//!   rather than as a missing profile. A `204` where a `200` was meant is worse:
//!   it is silent.
//! * **`deviceProfileEnableOnConnect`.** It defaults to `false` on ATAK, so an
//!   enrolment profile that does not carry it leaves every connection profile
//!   an operator configures silently doing nothing.
//! * **The `class` attribute.** ATAK dereferences it without a null check: a
//!   missing one does not skip an entry, it aborts the import of the whole
//!   document.
//! * **`relativePath` traversal.** These paths are joined against stored names,
//!   and ATAK percent-encodes only a literal space — whatever a caller puts in
//!   arrives almost verbatim.
//!
//! Run with `cargo test -p rustak-server --features testing`.

#![cfg(feature = "testing")]

use std::io::{Cursor, Read as _};

use actix_web::http::StatusCode;
use actix_web::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE, LAST_MODIFIED};
use actix_web::{App, test};
use base64::Engine as _;
use rustak_api::{GroupName, PrefClass, PrefEntry, ProfileId};
use rustak_server::pki::{KeyType, Pki};
use rustak_server::prelude::*;
use rustak_server::profiles::model::NewProfile;
use rustak_server::profiles::repo::ProfilesRepo;
use rustak_server::testing::TestServer;
use rustak_server::testing::context::bearer;

/// A profile that is delivered to everybody at enrolment and on connect.
fn everywhere(name: &str) -> NewProfile {
    NewProfile {
        name: name.to_string(),
        active: true,
        apply_on_enrollment: true,
        apply_on_connect: true,
        ..NewProfile::default()
    }
}

/// Seeds a profile with one preference and returns its identifier.
async fn seeded(server: &TestServer, new: NewProfile, key: &str) -> ProfileId {
    let repo = ProfilesRepo::new(server.db());
    let created = repo.create(new).await.expect("a profile");

    repo.set_prefs(created.id, &[PrefEntry::string(key, "true")])
        .await
        .expect("preferences");

    created.id
}

/// The entry names of a zip body.
fn names(body: &[u8]) -> Vec<String> {
    zip::ZipArchive::new(Cursor::new(body.to_vec()))
        .expect("a readable zip")
        .file_names()
        .map(str::to_string)
        .collect()
}

/// One entry's bytes.
fn entry(body: &[u8], name: &str) -> Vec<u8> {
    let mut archive = zip::ZipArchive::new(Cursor::new(body.to_vec())).expect("a readable zip");
    let mut found = Vec::new();
    archive
        .by_name(name)
        .unwrap_or_else(|_| panic!("{name} is not in {:?}", names(body)))
        .read_to_end(&mut found)
        .expect("readable bytes");

    found
}

#[actix_web::test]
async fn an_enrolment_always_carries_the_preference_connect_profiles_depend_on() {
    // ATAK's `deviceProfileEnableOnConnect` defaults to false, so an
    // installation that has configured nothing still has to send this or every
    // connection profile it later configures will silently never fire.
    let server = TestServer::start_with(|config| {
        config.marti.public_host = Some("tak.example.com".to_string());
    })
    .await;
    let (_, session) = server.signed_in("ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/tls/profile/enrollment?clientUid=ANDROID-1")
            .insert_header(("authorization", bearer(&session)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/zip"
    );
    assert_eq!(
        response.headers().get(CONTENT_DISPOSITION).unwrap(),
        "attachment; filename=profile.zip",
        "the value is unquoted, as TAK Server's is",
    );
    assert!(response.headers().contains_key(LAST_MODIFIED));

    let body = test::read_body(response).await.to_vec();

    assert!(names(&body).contains(&"MANIFEST/manifest.xml".to_string()));

    let document = String::from_utf8(entry(&body, "file0/rustak-enrollment.pref")).unwrap();

    assert!(
        document.starts_with("<?xml version='1.0' standalone='yes'?><preferences>"),
        "the declaration is single-quoted with no encoding: {document}",
    );
    assert!(
        document.contains(r#"<preference version="1" name="com.atakmap.app.civ_preferences">"#)
    );
    assert!(document.contains(
        r#"<entry key="deviceProfileEnableOnConnect" class="class java.lang.String">true</entry>"#
    ));
    assert!(document.contains("prefs_enable_channels_host-tak.example.com"));
    assert!(!document.contains("> "), "no whitespace between elements");
}

#[actix_web::test]
async fn the_enrolment_profile_is_reachable_with_the_basic_credential_atak_enrolled_with() {
    // ATAK fetches this immediately after enrolling, on the public listener,
    // with the same username and client password it just enrolled with — it has
    // no bearer token and, on `:8446`, no client certificate. Basic reaches
    // `/Marti/api/tls/**` and nothing else, so this is the one profile route it
    // works on.
    let server = TestServer::start().await;
    let user = server.user("ada", false).await;
    let actor = rustak_api::Username::parse("ada").unwrap();

    let secret = rustak_server::identity::credentials::mint(
        server.db(),
        &server.config().auth,
        &user,
        rustak_server::identity::MintRequest::new(
            rustak_api::CredentialKind::ClientPassword,
            "Test device",
            &actor,
        ),
    )
    .await
    .expect("a client password")
    .secret
    .expose()
    .to_string();

    let app = test::init_service(App::new().configure(server.app())).await;
    let header = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("ada:{secret}"))
    );

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/tls/profile/enrollment?clientUid=ANDROID-1")
            .insert_header(("authorization", header))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/zip",
    );
}

#[actix_web::test]
async fn a_connection_fetch_with_nothing_to_send_is_a_204_with_an_empty_body() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/device/profile/connection?clientUid=ANDROID-1&syncSecago=-1")
            .insert_header(("authorization", bearer(&session)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(test::read_body(response).await.is_empty());
}

#[actix_web::test]
async fn what_an_account_chose_for_itself_reaches_its_own_devices_and_nobody_elses() {
    let server = TestServer::start().await;
    let (_, ada) = server.signed_in("ada", false).await;
    let (_, grace) = server.signed_in("grace", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let chosen = test::call_service(
        &app,
        test::TestRequest::patch()
            .uri("/api/v1/me/preferences")
            .insert_header(("authorization", bearer(&ada)))
            .set_json(serde_json::json!({ "symbology": "2525d" }))
            .to_request(),
    )
    .await;
    assert_eq!(chosen.status(), StatusCode::OK);

    let connection = |session: &_| {
        test::TestRequest::get()
            .uri("/Marti/api/device/profile/connection?clientUid=ANDROID-1&syncSecago=-1")
            .insert_header(("authorization", bearer(session)))
            .to_request()
    };

    let hers = test::call_service(&app, connection(&ada)).await;
    assert_eq!(hers.status(), StatusCode::OK);

    let body = test::read_body(hers).await.to_vec();
    let prefs = String::from_utf8(entry(&body, "file0/rustak-account.pref")).unwrap();
    assert!(prefs.contains(r#"key="symbologyProvider""#), "{prefs}");
    assert!(prefs.contains(">2525D<"), "{prefs}");

    // Somebody who has chosen nothing is sent nothing, not this console's default.
    let theirs = test::call_service(&app, connection(&grace)).await;
    assert_eq!(theirs.status(), StatusCode::NO_CONTENT);
}

#[actix_web::test]
async fn a_connection_fetch_with_a_profile_returns_it_as_a_package() {
    let server = TestServer::start().await;
    seeded(&server, everywhere("Channels"), "prefs_enable_channels").await;

    let (_, session) = server.signed_in("ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/device/profile/connection?clientUid=ANDROID-1&syncSecago=-1")
            .insert_header(("authorization", bearer(&session)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = test::read_body(response).await.to_vec();
    let manifest = String::from_utf8(entry(&body, "MANIFEST/manifest.xml")).unwrap();

    assert!(manifest.contains(r#"<Parameter name="name" value="Connection"/>"#));
    assert!(manifest.contains(r#"<Parameter name="onReceiveImport" value="true"/>"#));
    assert!(manifest.contains(r#"<Parameter name="onReceiveDelete" value="true"/>"#));
    assert!(names(&body).contains(&"file0/Channels.pref".to_string()));
}

#[actix_web::test]
async fn every_one_of_these_endpoints_needs_a_client_uid() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    for uri in [
        "/Marti/api/tls/profile/enrollment",
        "/Marti/api/device/profile/connection?syncSecago=-1",
        "/Marti/api/device/profile/tool/Corps",
        "/Marti/api/device/profile/tool/Corps/file?relativePath=/a",
        "/Marti/api/tls/profile/tool/Corps/file?relativePath=/a",
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(uri)
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "application/json",
            "{uri}",
        );
    }
}

#[actix_web::test]
async fn a_profile_scoped_to_a_channel_is_not_delivered_to_somebody_outside_it() {
    let server = TestServer::start().await;
    ProfilesRepo::new(server.db())
        .create(NewProfile {
            groups: vec![GroupName::parse("Blue").unwrap()],
            ..everywhere("Blue only")
        })
        .await
        .expect("a profile");

    let (_, session) = server.signed_in("ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/device/profile/connection?clientUid=ANDROID-1&syncSecago=-1")
            .insert_header(("authorization", bearer(&session)))
            .to_request(),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "a channel nobody is in delivers to nobody",
    );
}

#[actix_web::test]
async fn a_relative_path_that_climbs_out_of_the_profile_is_refused() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    for path in ["/../../etc/passwd", "a/../../b", ".."] {
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&format!(
                    "/Marti/api/device/profile/tool/Corps/file?clientUid=A&relativePath={path}"
                ))
                .insert_header(("authorization", bearer(&session)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
}

#[actix_web::test]
async fn a_tool_nothing_serves_is_a_404_rather_than_a_204() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/device/profile/tool/Nothing/file?clientUid=A&relativePath=/a")
            .insert_header(("authorization", bearer(&session)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[actix_web::test]
async fn one_matching_file_comes_back_raw_and_a_repeat_request_is_a_304() {
    let server = TestServer::start().await;
    let id = seeded(
        &server,
        NewProfile {
            tool: Some("Corps".to_string()),
            ..everywhere("Corps")
        },
        "prefs_enable_channels",
    )
    .await;

    let stored = server
        .content()
        .expect("a content store")
        .put_bytes(b"<x/>")
        .await
        .expect("stored bytes");

    ProfilesRepo::new(server.db())
        .put_file(
            id,
            "maps/source.xml".to_string(),
            stored.hash,
            stored.size,
            None,
        )
        .await
        .expect("an attached file");

    let (_, session) = server.signed_in("ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;
    let uri = "/Marti/api/device/profile/tool/Corps/file?clientUid=A&relativePath=/maps";

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(uri)
            .insert_header(("authorization", bearer(&session)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/xml",
        "the type is guessed from the extension",
    );
    assert_eq!(
        response.headers().get(CONTENT_DISPOSITION).unwrap(),
        "attachment; filename=source.xml",
    );

    let last_modified = response
        .headers()
        .get(LAST_MODIFIED)
        .expect("a Last-Modified the client will echo back")
        .clone();

    assert_eq!(test::read_body(response).await.as_ref(), b"<x/>");

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(uri)
            .insert_header(("authorization", bearer(&session)))
            .insert_header(("if-modified-since", last_modified))
            .to_request(),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::NOT_MODIFIED,
        "nothing changed since the value we ourselves handed out",
    );
    assert!(test::read_body(response).await.is_empty());
}

#[actix_web::test]
async fn several_matching_files_come_back_as_a_multi_file_package() {
    let server = TestServer::start().await;
    let id = ProfilesRepo::new(server.db())
        .create(NewProfile {
            tool: Some("Corps".to_string()),
            ..everywhere("Corps")
        })
        .await
        .expect("a profile")
        .id;

    let store = server.content().expect("a content store");
    for (path, body) in [("maps/a.xml", &b"<a/>"[..]), ("maps/b.xml", &b"<b/>"[..])] {
        let stored = store.put_bytes(body).await.expect("stored bytes");
        ProfilesRepo::new(server.db())
            .put_file(id, path.to_string(), stored.hash, stored.size, None)
            .await
            .expect("an attached file");
    }

    let (_, session) = server.signed_in("ada", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/tls/profile/tool/Corps/file?clientUid=A&relativePath=/maps")
            .insert_header(("authorization", bearer(&session)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/zip"
    );

    let body = test::read_body(response).await.to_vec();
    let listed = names(&body);

    assert!(
        listed.contains(&"maps/a.xml".to_string()) && listed.contains(&"maps/b.xml".to_string()),
        "the multiFile layout keeps the stored directories: {listed:?}",
    );

    let manifest = String::from_utf8(entry(&body, "MANIFEST/manifest.xml")).unwrap();
    assert!(manifest.contains(r#"<Parameter name="name" value="multiFile"/>"#));
}

#[actix_web::test]
async fn the_admin_api_edits_a_profile_end_to_end_and_previews_what_a_device_would_get() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("ada", true).await;
    let app = test::init_service(App::new().configure(server.app())).await;
    let token = bearer(&session);

    let created: rustak_api::Profile = test::call_and_read_body_json(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/profiles")
            .insert_header(("authorization", token.clone()))
            .set_json(serde_json::json!({
                "name": "Channels",
                "apply_on_enrollment": true,
            }))
            .to_request(),
    )
    .await;

    assert_eq!(created.name, "Channels");
    assert!(created.active, "a profile nobody disabled is on");
    assert_eq!(created.pref_count, 0);

    let saved: Vec<PrefEntry> = test::call_and_read_body_json(
        &app,
        test::TestRequest::put()
            .uri(&format!("/api/v1/profiles/{}/prefs", created.id))
            .insert_header(("authorization", token.clone()))
            .set_json(serde_json::json!([
                { "key": "prefs_enable_channels", "class": "String", "value": "true" },
                { "key": "constantReportingRateUnreliable", "class": "Integer", "value": "20" },
            ]))
            .to_request(),
    )
    .await;

    assert_eq!(saved.len(), 2);
    assert_eq!(saved[1].class, PrefClass::Integer);

    let refused = test::call_service(
        &app,
        test::TestRequest::put()
            .uri(&format!("/api/v1/profiles/{}/prefs", created.id))
            .insert_header(("authorization", token.clone()))
            .set_json(serde_json::json!([
                { "key": "a", "class": "Integer", "value": "3.5" },
            ]))
            .to_request(),
    )
    .await;

    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "a value ATAK could not read is refused here rather than delivered",
    );

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/api/v1/profiles/{}/preview", created.id))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);

    let body = test::read_body(response).await.to_vec();
    let document = String::from_utf8(entry(&body, "file0/Channels.pref")).unwrap();

    assert!(document.contains(
        r#"<entry key="constantReportingRateUnreliable" class="class java.lang.Integer">20</entry>"#
    ));

    let catalogue: Vec<rustak_api::PrefCatalogEntry> = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/profiles/pref-catalog")
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert!(
        catalogue
            .iter()
            .any(|entry| entry.key == "deviceProfileEnableOnConnect"),
        "the catalogue is what the editor autocompletes from",
    );

    let deleted = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri(&format!("/api/v1/profiles/{}", created.id))
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
}

#[actix_web::test]
async fn nothing_under_profiles_or_config_packages_answers_without_a_session() {
    let server = TestServer::start().await;
    let app = test::init_service(App::new().configure(server.app())).await;

    for (method, uri) in [
        ("GET", "/api/v1/profiles"),
        ("POST", "/api/v1/profiles"),
        ("GET", "/api/v1/profiles/pref-catalog"),
        ("GET", "/api/v1/profiles/1"),
        ("PATCH", "/api/v1/profiles/1"),
        ("DELETE", "/api/v1/profiles/1"),
        ("GET", "/api/v1/profiles/1/files"),
        ("POST", "/api/v1/profiles/1/files"),
        ("GET", "/api/v1/profiles/1/files/2"),
        ("DELETE", "/api/v1/profiles/1/files/2"),
        ("GET", "/api/v1/profiles/1/prefs"),
        ("PUT", "/api/v1/profiles/1/prefs"),
        ("GET", "/api/v1/profiles/1/preview"),
        ("POST", "/api/v1/config-packages"),
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::default()
                .method(method.parse().unwrap())
                .uri(uri)
                .set_json(serde_json::json!({}))
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri}"
        );
    }
}

#[actix_web::test]
async fn an_ordinary_caller_administers_no_profile() {
    let server = TestServer::start().await;
    let (_, session) = server.signed_in("grace", false).await;
    let app = test::init_service(App::new().configure(server.app())).await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/api/v1/profiles")
            .insert_header(("authorization", bearer(&session)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[actix_web::test]
async fn a_configuration_package_carries_the_truststore_and_never_mints_a_keystore() {
    let server = TestServer::start_with(|config| {
        config.pki.key_type = KeyType::EcdsaP256;
    })
    .await;

    let config = server.config();
    let pki = Pki::load(
        server.db(),
        server.secrets(),
        &config.pki,
        &config.server.data_dir,
        &["localhost".to_string()],
        &config.pki.server_ips,
    )
    .await
    .expect("an authority for the test server");

    server
        .context
        .install_pki(pki)
        .expect("the authority is installed once");

    let (_, session) = server.signed_in("ada", true).await;
    let app = test::init_service(App::new().configure(server.app())).await;
    let token = bearer(&session);

    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/config-packages")
            .insert_header(("authorization", token.clone()))
            .set_json(serde_json::json!({
                "username": "ada",
                "variant": "itak",
                "include_client_cert": false,
            }))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_DISPOSITION).unwrap(),
        "attachment; filename=\"ada_CONFIG_iTAK.zip\"",
    );

    let body = test::read_body(response).await.to_vec();
    let listed = names(&body);

    assert_eq!(listed, vec!["config.pref", "truststore.p12"], "{listed:?}");

    let document = String::from_utf8(entry(&body, "config.pref")).unwrap();

    assert!(document.contains("cot_streams"));
    assert!(document.contains(
        r#"<entry key="enrollForCertificateWithTrust0" class="class java.lang.Boolean">true</entry>"#
    ));
    assert!(
        !document.contains("key=\"password") && !document.contains("key=\"username"),
        "ATAK never reads those keys, so a secret there would only leak",
    );

    let refused = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/config-packages")
            .insert_header(("authorization", token))
            .set_json(serde_json::json!({
                "username": "ada",
                "variant": "wintak_atak",
                "include_client_cert": true,
            }))
            .to_request(),
    )
    .await;

    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "this server holds no device private key, so it cannot build a keystore",
    );
}

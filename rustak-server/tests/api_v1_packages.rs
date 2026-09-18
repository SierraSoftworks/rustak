//! `/api/v1/packages` end to end, against the real application.
//!
//! The unit tests inside `src/web/api/packages*.rs` and `src/files/patch.rs`
//! check the pieces — what a summary carries, what a search matches, what a
//! patch writes. This suite checks the thing they add up to: the authorisation
//! matrix, the paging bounds, and the one property a handler tested in
//! isolation cannot show, which is that a multi-megabyte upload never becomes a
//! multi-megabyte allocation.
//!
//! # What breaks if these fail
//!
//! * **The visibility rule.** A package listing that ignores channels hands one
//!   team's material to another, and a `404` that becomes a `403` turns the
//!   store into an oracle for which hashes exist.
//! * **`install_on_enrollment`.** It puts a file on every device that enrols,
//!   so a `PATCH` that writes it when it should not is a distribution.
//! * **Streaming.** The ceiling is four hundred megabytes; a handler that
//!   collected a body would make the server's resident set the number of
//!   operators uploading times the size of the largest package.
//!
//! Run with `cargo test -p rustak-server --features testing --test api_v1_packages`.

#![cfg(feature = "testing")]

use actix_web::http::StatusCode;
use actix_web::http::header::CONTENT_TYPE;
use actix_web::web::Bytes;
use actix_web::{App, test};
use rustak_api::PackageSummary;
use rustak_server::db::repos::NewGroup;
use rustak_server::prelude::*;
use rustak_server::testing::TestServer;
use rustak_server::testing::context::bearer;

/// The multipart boundary every request in this suite uses.
const BOUNDARY: &str = "rustakadminboundary";

macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

/// A `multipart/form-data` body: one file part, then any number of fields.
fn multipart(filename: &str, body: &[u8], fields: &[(&str, &str)]) -> Bytes {
    let mut out = Vec::new();

    out.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; \
             filename=\"{filename}\"\r\nContent-Type: application/x-zip-compressed\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(body);

    for (name, value) in fields {
        out.extend_from_slice(
            format!(
                "\r\n--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}"
            )
            .as_bytes(),
        );
    }

    out.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());

    Bytes::from(out)
}

/// The header a multipart body is announced with.
fn multipart_type() -> (&'static str, String) {
    (
        "content-type",
        format!("multipart/form-data; boundary={BOUNDARY}"),
    )
}

/// Uploads one package and answers with what the server stored.
///
/// A macro rather than a function: the application `test::init_service` builds
/// has an opaque type, and naming it in a signature costs more than it saves.
macro_rules! upload {
    ($app:expr, $token:expr, $filename:expr, $body:expr, $fields:expr) => {{
        let response = test::call_service(
            &$app,
            test::TestRequest::post()
                .uri("/api/v1/packages")
                .insert_header(("authorization", $token))
                .insert_header(multipart_type())
                .set_payload(multipart($filename, $body, $fields))
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "a stored package is a 201",
        );

        let stored: PackageSummary = test::read_body_json(response).await;
        stored
    }};
}

/// One page of the listing, as the caller asked for it.
macro_rules! page {
    ($app:expr, $token:expr, $uri:expr) => {{
        let listed: Vec<PackageSummary> = test::call_and_read_body_json(
            &$app,
            test::TestRequest::get()
                .uri($uri)
                .insert_header(("authorization", $token))
                .to_request(),
        )
        .await;

        listed
    }};
}

#[actix_web::test]
async fn an_upload_is_stored_with_what_the_form_said_about_it() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let stored = upload!(
        app,
        bearer(&admin),
        "patrol.zip",
        b"PK-not-really-a-zip",
        &[
            ("name", "Patrol Brief"),
            ("tool", "public"),
            ("keywords", "missionpackage,patrol"),
        ]
    );

    assert_eq!(stored.name, "Patrol Brief");
    assert_eq!(stored.filename.as_deref(), Some("patrol.zip"));
    assert_eq!(stored.tool, "public");
    assert_eq!(stored.size, 19);
    assert_eq!(stored.submitter.as_deref(), Some("grace"));
    assert!(
        stored.mission_package,
        "the missionpackage keyword is what puts it in a client's browser",
    );
    assert_eq!(
        stored.keywords,
        vec!["missionpackage".to_string(), "patrol".to_string()],
        "a comma-separated field is read as a list",
    );
    assert!(
        !stored.install_on_enrollment,
        "nothing ships with an enrolment until somebody says so",
    );
}

#[actix_web::test]
async fn an_upload_with_no_file_part_is_refused_rather_than_stored_empty() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/packages")
            .insert_header(("authorization", bearer(&admin)))
            .insert_header(multipart_type())
            .set_payload(Bytes::from(format!(
                "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\n\
                 Nothing\r\n--{BOUNDARY}--\r\n"
            )))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
async fn only_an_administrator_may_upload_change_or_remove_a_package() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let (_, ordinary) = server.signed_in("ada", false).await;
    let app = app!(server);

    let stored = upload!(app, bearer(&admin), "patrol.zip", b"bytes", &[]);

    for request in [
        test::TestRequest::post()
            .uri("/api/v1/packages")
            .insert_header(multipart_type())
            .set_payload(multipart("other.zip", b"bytes", &[])),
        test::TestRequest::patch()
            .uri(&format!("/api/v1/packages/{}", stored.hash))
            .set_json(serde_json::json!({ "install_on_enrollment": true })),
        test::TestRequest::delete().uri(&format!("/api/v1/packages/{}", stored.hash)),
    ] {
        let response = test::call_service(
            &app,
            request
                .insert_header(("authorization", bearer(&ordinary)))
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "writing to the package store is an operator's job",
        );
    }
}

#[actix_web::test]
async fn a_package_in_a_channel_somebody_is_not_in_is_not_there_for_them() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let (_, ordinary) = server.signed_in("ada", false).await;
    let app = app!(server);

    server
        .db()
        .groups()
        .create(NewGroup::manual(GroupName::parse("Blue").unwrap()))
        .await
        .expect("a channel");

    let stored = upload!(
        app,
        bearer(&admin),
        "secret.zip",
        b"bytes",
        &[("groups", "Blue")]
    );

    for uri in [
        format!("/api/v1/packages/{}", stored.hash),
        format!("/api/v1/packages/{}/content", stored.hash),
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(&uri)
                .insert_header(("authorization", bearer(&ordinary)))
                .to_request(),
        )
        .await;

        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "a package that is not theirs answers exactly as one that is not here",
        );
    }

    let listed = page!(app, bearer(&ordinary), "/api/v1/packages");

    assert!(listed.is_empty(), "and it is not in their listing either");
}

#[actix_web::test]
async fn a_listing_pages_narrows_and_caps_what_it_is_asked_for() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    for index in 0..5 {
        let name = format!("Package {index}");
        let keywords = match index % 2 == 0 {
            true => "missionpackage",
            false => "overlay",
        };

        upload!(
            app,
            bearer(&admin),
            &format!("file-{index}.zip"),
            format!("bytes-{index}").as_bytes(),
            &[("name", name.as_str()), ("keywords", keywords)]
        );
    }

    assert_eq!(
        page!(app, bearer(&admin), "/api/v1/packages?limit=2").len(),
        2
    );
    assert_eq!(
        page!(app, bearer(&admin), "/api/v1/packages?limit=2&page=2").len(),
        1,
    );
    assert_eq!(
        page!(app, bearer(&admin), "/api/v1/packages?limit=1000000").len(),
        5,
        "an absurd limit is answered with a page rather than refused",
    );
    assert_eq!(
        page!(app, bearer(&admin), "/api/v1/packages?missionPackage=true").len(),
        3,
        "the keyword filter is the one a client's data-package browser uses",
    );
    assert_eq!(
        page!(app, bearer(&admin), "/api/v1/packages?q=package%203").len(),
        1,
        "the free-text search matches a name without regard to case",
    );
}

#[actix_web::test]
async fn a_patch_writes_every_field_and_a_change_that_does_nothing_is_refused() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let stored = upload!(app, bearer(&admin), "patrol.zip", b"bytes", &[]);

    let patched: PackageSummary = test::call_and_read_body_json(
        &app,
        test::TestRequest::patch()
            .uri(&format!("/api/v1/packages/{}", stored.hash))
            .insert_header(("authorization", bearer(&admin)))
            .set_json(serde_json::json!({
                "name": "Renamed",
                "tool": "atak",
                "keywords": ["missionpackage"],
                "install_on_enrollment": true,
                "expiration": "2026-09-25T12:00:00Z",
            }))
            .to_request(),
    )
    .await;

    assert_eq!(patched.name, "Renamed");
    assert_eq!(patched.tool, "atak");
    assert!(patched.install_on_enrollment);
    assert!(patched.mission_package);
    assert_eq!(
        patched.expiration,
        Some("2026-09-25T12:00:00Z".parse().unwrap()),
        "an expiry is an instant on this surface, whatever the column holds",
    );

    // And an explicit `null` clears it, which is the thing TAK's `-1` said with
    // a sign and nothing in a JSON body can.
    let cleared: PackageSummary = test::call_and_read_body_json(
        &app,
        test::TestRequest::patch()
            .uri(&format!("/api/v1/packages/{}", stored.hash))
            .insert_header(("authorization", bearer(&admin)))
            .set_json(serde_json::json!({ "expiration": serde_json::Value::Null }))
            .to_request(),
    )
    .await;

    assert_eq!(cleared.expiration, None);

    // A change that names nothing but the name leaves it alone, rather than
    // reading as "clear it" — the difference the second `Option` exists for.
    let renamed: PackageSummary = test::call_and_read_body_json(
        &app,
        test::TestRequest::patch()
            .uri(&format!("/api/v1/packages/{}", stored.hash))
            .insert_header(("authorization", bearer(&admin)))
            .set_json(serde_json::json!({ "expiration": "2026-11-01T00:00:00Z" }))
            .to_request(),
    )
    .await;
    assert!(renamed.expiration.is_some());

    let untouched: PackageSummary = test::call_and_read_body_json(
        &app,
        test::TestRequest::patch()
            .uri(&format!("/api/v1/packages/{}", stored.hash))
            .insert_header(("authorization", bearer(&admin)))
            .set_json(serde_json::json!({ "name": "Renamed again" }))
            .to_request(),
    )
    .await;

    assert_eq!(untouched.expiration, renamed.expiration);

    let empty = test::call_service(
        &app,
        test::TestRequest::patch()
            .uri(&format!("/api/v1/packages/{}", stored.hash))
            .insert_header(("authorization", bearer(&admin)))
            .set_json(serde_json::json!({}))
            .to_request(),
    )
    .await;

    assert_eq!(empty.status(), StatusCode::BAD_REQUEST);

    let blank_name = test::call_service(
        &app,
        test::TestRequest::patch()
            .uri(&format!("/api/v1/packages/{}", stored.hash))
            .insert_header(("authorization", bearer(&admin)))
            .set_json(serde_json::json!({ "name": "   " }))
            .to_request(),
    )
    .await;

    assert_eq!(blank_name.status(), StatusCode::BAD_REQUEST);
}

#[actix_web::test]
async fn the_content_is_handed_back_byte_for_byte_and_then_deleted() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let stored = upload!(app, bearer(&admin), "patrol.zip", b"PK-bytes", &[]);

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/api/v1/packages/{}/content", stored.hash))
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/x-zip-compressed",
    );
    assert_eq!(
        test::read_body(response).await,
        Bytes::from_static(b"PK-bytes"),
    );

    let removed = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri(&format!("/api/v1/packages/{}", stored.hash))
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(removed.status(), StatusCode::NO_CONTENT);

    let gone = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/api/v1/packages/{}", stored.hash))
            .insert_header(("authorization", bearer(&admin)))
            .to_request(),
    )
    .await;

    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        test::call_service(
            &app,
            test::TestRequest::delete()
                .uri(&format!("/api/v1/packages/{}", stored.hash))
                .insert_header(("authorization", bearer(&admin)))
                .to_request(),
        )
        .await
        .status(),
        StatusCode::NOT_FOUND,
        "deleting twice says there was nothing there",
    );
}

#[actix_web::test]
async fn a_multi_megabyte_upload_reaches_the_store_without_being_collected() {
    // Streaming is not observable from a response, so this asserts the two
    // things that would not hold if the body were buffered and written in one
    // go: the blob is complete and correctly hashed, and the store's temporary
    // directory — which every ingest writes through and renames out of — is
    // empty afterwards.
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let content_dir = server.context.config().content_dir();
    let app = app!(server);

    let payload: Vec<u8> = (0..3_000_000u32).map(|n| n as u8).collect();
    let stored = upload!(app, bearer(&admin), "big.zip", &payload, &[]);

    assert_eq!(stored.size, 3_000_000);

    let blob = content_dir.join(&stored.hash[..2]).join(&stored.hash);

    assert!(blob.is_file(), "the blob landed at {}", blob.display());
    assert_eq!(
        std::fs::read(&blob).unwrap(),
        payload,
        "every byte of the body reached the store",
    );

    let mut temporaries = std::fs::read_dir(content_dir.join("tmp")).unwrap();

    assert!(
        temporaries.next().is_none(),
        "every ingest renames its temporary file into place",
    );
}

#[actix_web::test]
async fn an_upload_past_the_configured_ceiling_is_refused_before_it_is_read() {
    let server = TestServer::start_with(|config| config.marti.upload_size_limit_mb = 1).await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/api/v1/packages")
            .insert_header(("authorization", bearer(&admin)))
            .insert_header(multipart_type())
            .set_payload(multipart("big.zip", &vec![0u8; 2_000_000], &[]))
            .to_request(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let body = test::read_body(response).await;
    let text = String::from_utf8_lossy(&body);

    assert!(text.contains("1 MB"), "the limit is named: {text}");
}

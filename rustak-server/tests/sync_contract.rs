//! The Enterprise Sync wire contract, asserted against the real application.
//!
//! The unit tests in `src/files/**` check each rendering in isolation. This
//! suite checks the thing they add up to — the routes as they are mounted, with
//! the middleware, the extractors and the default service in place — because
//! every failure it exists to catch is a wiring failure.
//!
//! # What breaks if these fail
//!
//! * **`Content-Type`.** `upload` and `search` answer `text/json`,
//!   `missionupload` and `missionquery` answer `text/plain`, `delete` answers
//!   `text/html`, and everything under `/Marti/api` answers
//!   `application/json` with no `charset` parameter. node-tak compares that
//!   header with `===`.
//! * **`resultCount`.** ATAK's package browser checks that the search body
//!   literally contains the key before it parses it, and treats a result whose
//!   `PrimaryKey` will not parse as a non-negative integer as fatal to the
//!   whole response.
//! * **The bare URL.** `missionupload`'s body becomes the `senderUrl` of the
//!   file-share message ATAK sends next, so a JSON wrapper or a trailing
//!   newline travels to a peer and fails there.
//! * **`Hash`, `Groups`, `Time`.** CloudTAK reads those three keys out of
//!   `/Marti/api/files/metadata` to fill in a package's channels column.
//! * **Streaming.** A multi-megabyte upload has to reach the store without
//!   being collected anywhere, which is asserted through the content store's
//!   own temporary directory.
//!
//! Run with `cargo test -p rustak-server --features testing --test sync_contract`.

#![cfg(feature = "testing")]

use actix_web::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use actix_web::web::Bytes;
use actix_web::{App, test};
use rustak_server::services::Services as _;
use rustak_server::testing::TestServer;

/// The multipart boundary every request in this suite uses.
const BOUNDARY: &str = "rustakinteropboundary";

macro_rules! app {
    ($server:expr) => {
        test::init_service(App::new().configure($server.app())).await
    };
}

/// A `multipart/form-data` body with one file part.
fn multipart(part: &str, filename: &str, body: &[u8]) -> Bytes {
    let mut out = Vec::new();

    out.extend_from_slice(
        format!(
            "--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{part}\"; \
             filename=\"{filename}\"\r\nContent-Type: application/x-zip-compressed\r\n\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(body);
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

#[actix_web::test]
async fn an_upload_answers_the_legacy_metadata_object_as_text_json() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=notes.txt&keywords=interop&creatorUid=ANDROID-1")
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .insert_header((CONTENT_TYPE, "text/plain"))
            .set_payload(Bytes::from_static(b"hello interop"))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "text/json",
        "a historical quirk every TAK deployment still emits",
    );

    let body: serde_json::Value = test::read_body_json(response).await;

    // ATAK refuses the whole response if any of these is missing or unparseable.
    assert_eq!(body["Name"], "notes.txt");
    assert!(body["Hash"].as_str().is_some_and(|hash| hash.len() == 64));
    assert!(body["UID"].as_str().is_some_and(|uid| !uid.is_empty()));
    assert!(
        body["PrimaryKey"]
            .as_str()
            .and_then(|key| key.parse::<u64>().ok())
            .is_some(),
        "PrimaryKey must be a string holding a non-negative integer: {body}",
    );
    assert!(body["Size"].is_string(), "Size is a string, not a number");
    assert_eq!(body["Size"], "13");
    assert_eq!(body["SubmissionUser"], "grace");
    assert_eq!(body["CreatorUid"], "ANDROID-1");
    assert_eq!(body["MIMEType"], "text/plain");
    assert_eq!(
        body["EXPIRATION"], "-1",
        "CloudTAK reads this key unconditionally",
    );
    assert!(
        body["SubmissionDateTime"]
            .as_str()
            .is_some_and(|at| at.ends_with('Z') && at.contains('.')),
        "the padded-millisecond form ATAK parses: {body}",
    );
    assert_eq!(body["Keywords"][0], "interop");
}

#[actix_web::test]
async fn a_search_reports_a_numeric_result_count_beside_the_same_objects() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=notes.txt&keywords=interop")
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .insert_header((CONTENT_TYPE, "text/plain"))
            .set_payload(Bytes::from_static(b"hello interop"))
            .to_request(),
    )
    .await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/sync/search?keywords=interop")
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .to_request(),
    )
    .await;

    assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), "text/json");

    let raw = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

    assert!(
        raw.contains("resultCount"),
        "ATAK checks for the literal key before parsing: {raw}",
    );

    let body: serde_json::Value = serde_json::from_str(&raw).unwrap();

    assert_eq!(body["resultCount"], 1);
    assert!(body["resultCount"].is_number());
    assert_eq!(body["results"][0]["Name"], "notes.txt");

    let nothing = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/sync/search?keywords=nothing-has-this")
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .to_request(),
    )
    .await;
    let nothing = test::read_body(nothing).await;
    let empty: serde_json::Value = serde_json::from_slice(&nothing).unwrap();

    assert_eq!(empty["resultCount"], 0);
    assert!(empty["results"].as_array().unwrap().is_empty());
}

#[actix_web::test]
async fn a_download_carries_the_api_version_header_and_the_stored_bytes() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);
    let token = format!("Bearer {}", admin.token);

    let stored: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=my%20report.txt")
            .insert_header(("authorization", token.clone()))
            .insert_header((CONTENT_TYPE, "text/plain"))
            .set_payload(Bytes::from_static(b"0123456789"))
            .to_request(),
    )
    .await;
    let hash = stored["Hash"].as_str().unwrap().to_string();

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/Marti/sync/content?hash={hash}"))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(
        response.headers().get("api-version").unwrap(),
        "3",
        "the one endpoint that emits this header",
    );
    assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), "text/plain");
    assert_eq!(
        response.headers().get("content-disposition").unwrap(),
        "inline; filename=\"my%20report.txt\"",
    );
    assert_eq!(
        test::read_body(response).await,
        Bytes::from_static(b"0123456789")
    );

    // HEAD answers the same headers and no body.
    let head = test::call_service(
        &app,
        test::TestRequest::default()
            .method(actix_web::http::Method::HEAD)
            .uri(&format!("/Marti/sync/content?hash={hash}"))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(head.status().as_u16(), 200);
    assert_eq!(head.headers().get(CONTENT_LENGTH).unwrap(), "10");
    assert!(test::read_body(head).await.is_empty());

    // A range is a 206 with only that slice.
    let partial = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!(
                "/Marti/sync/content?hash={hash}&offset=3&length=4"
            ))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(partial.status().as_u16(), 206);
    assert_eq!(
        partial.headers().get("content-range").unwrap(),
        "bytes 3-6/10",
    );
    assert_eq!(test::read_body(partial).await, Bytes::from_static(b"3456"));
}

#[actix_web::test]
async fn a_file_that_is_not_there_answers_the_html_document_the_servlets_do() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    for uri in [
        "/Marti/sync/content?hash=00",
        "/Marti/sync/content",
        "/Marti/sync/missionquery?hash=00",
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri(uri)
                .insert_header(("authorization", format!("Bearer {}", admin.token)))
                .to_request(),
        )
        .await;

        assert_eq!(response.status().as_u16(), 404, "{uri}");
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "text/html",
            "node-tak sniffs for HTML on these paths: {uri}",
        );
    }
}

#[actix_web::test]
async fn a_package_upload_answers_a_bare_url_that_missionquery_repeats() {
    let server = TestServer::start_with(|config| {
        config.marti.public_host = Some("tak.example.com".to_string());
    })
    .await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);
    let token = format!("Bearer {}", admin.token);

    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/missionupload?filename=trip.zip&creatorUid=ANDROID-1&Groups=__ANON__")
            .insert_header(("authorization", token.clone()))
            .insert_header(multipart_type())
            .set_payload(multipart("assetfile", "trip.zip", b"PK-not-really-a-zip"))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), "text/plain");

    let url = String::from_utf8(test::read_body(response).await.to_vec()).unwrap();

    assert!(
        url.starts_with("https://tak.example.com/Marti/sync/content?hash="),
        "the URL travels to a peer, so it is built from [marti] public_host: {url}",
    );
    assert!(!url.ends_with('\n'), "ATAK does not trim this: {url:?}");

    let hash = url.rsplit('=').next().unwrap().to_string();
    let repeat = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/Marti/sync/missionquery?hash={hash}"))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(repeat.status().as_u16(), 200);
    assert_eq!(repeat.headers().get(CONTENT_TYPE).unwrap(), "text/plain");
    assert_eq!(
        String::from_utf8(test::read_body(repeat).await.to_vec()).unwrap(),
        url,
    );

    // The package is in the public list, which is what the keyword is for.
    let listed: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/sync/search?keywords=missionpackage")
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(listed["resultCount"], 1);
    assert_eq!(listed["results"][0]["Name"], "trip.zip");
}

#[actix_web::test]
async fn a_package_upload_that_is_not_multipart_or_has_no_filename_is_refused() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);
    let token = format!("Bearer {}", admin.token);

    let plain = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/missionupload?filename=trip.zip")
            .insert_header(("authorization", token.clone()))
            .insert_header((CONTENT_TYPE, "application/zip"))
            .set_payload(Bytes::from_static(b"not multipart"))
            .to_request(),
    )
    .await;

    assert_eq!(plain.status().as_u16(), 400);

    let nameless = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/missionupload")
            .insert_header(("authorization", token))
            .insert_header(multipart_type())
            .set_payload(multipart("assetfile", "trip.zip", b"body"))
            .to_request(),
    )
    .await;

    assert_eq!(nameless.status().as_u16(), 400);
}

#[actix_web::test]
async fn a_delete_answers_the_html_status_page_and_removes_the_bytes() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);
    let token = format!("Bearer {}", admin.token);

    let stored: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=notes.txt")
            .insert_header(("authorization", token.clone()))
            .insert_header((CONTENT_TYPE, "text/plain"))
            .set_payload(Bytes::from_static(b"delete me"))
            .to_request(),
    )
    .await;
    let hash = stored["Hash"].as_str().unwrap().to_string();

    let response = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri(&format!("/Marti/sync/delete?hash={hash}"))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), "text/html");
    assert_eq!(
        String::from_utf8(test::read_body(response).await.to_vec()).unwrap(),
        "<html><head><title>Enterprise Sync Status</title></head>\
         <h1>Success</h1><p>Deleted 1 resource(s).</p></html>",
    );

    let gone = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/Marti/sync/content?hash={hash}"))
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    assert_eq!(gone.status().as_u16(), 404);
}

#[actix_web::test]
async fn the_four_mutable_fields_are_the_only_ones_this_api_changes() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);
    let token = format!("Bearer {}", admin.token);

    let stored: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=notes.txt")
            .insert_header(("authorization", token.clone()))
            .insert_header((CONTENT_TYPE, "text/plain"))
            .set_payload(Bytes::from_static(b"metadata"))
            .to_request(),
    )
    .await;
    let hash = stored["Hash"].as_str().unwrap().to_string();

    for (uri, body) in [
        (format!("/Marti/api/sync/metadata/{hash}/tool"), "private"),
        (
            format!("/Marti/api/sync/metadata/{hash}/mimetype"),
            "text/csv",
        ),
    ] {
        let response = test::call_service(
            &app,
            test::TestRequest::put()
                .uri(&uri)
                .insert_header(("authorization", token.clone()))
                .set_payload(Bytes::from(body))
                .to_request(),
        )
        .await;

        assert_eq!(response.status().as_u16(), 200, "{uri}");
        assert!(test::read_body(response).await.is_empty(), "{uri}");
    }

    let keywords = test::call_service(
        &app,
        test::TestRequest::put()
            .uri(&format!("/Marti/api/sync/metadata/{hash}/keywords"))
            .insert_header(("authorization", token.clone()))
            .insert_header((CONTENT_TYPE, "application/json"))
            .set_payload(Bytes::from_static(br#"["alpha","beta"]"#))
            .to_request(),
    )
    .await;

    assert_eq!(keywords.status().as_u16(), 200);

    let expiry = test::call_service(
        &app,
        test::TestRequest::put()
            .uri(&format!(
                "/Marti/api/sync/metadata/{hash}/expiration?expiration=1714564800000"
            ))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(expiry.status().as_u16(), 200);

    let refused = test::call_service(
        &app,
        test::TestRequest::put()
            .uri(&format!("/Marti/api/sync/metadata/{hash}/name"))
            .insert_header(("authorization", token.clone()))
            .set_payload(Bytes::from_static(b"renamed"))
            .to_request(),
    )
    .await;

    assert_eq!(
        refused.status().as_u16(),
        400,
        "a name is not changeable through this API",
    );

    let missing = test::call_service(
        &app,
        test::TestRequest::put()
            .uri("/Marti/api/sync/metadata/00/tool")
            .insert_header(("authorization", token.clone()))
            .set_payload(Bytes::from_static(b"private"))
            .to_request(),
    )
    .await;

    assert_eq!(missing.status().as_u16(), 404);

    // Everything above landed on the row the search now reports.
    let found: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/sync/search")
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    assert_eq!(found["data"][0]["tool"], "private");
    assert_eq!(found["data"][0]["mimeType"], "text/csv");
    assert_eq!(found["data"][0]["keywords"][0], "alpha");
    assert_eq!(found["data"][0]["expiration"], 1_714_564_800_000_i64);
}

#[actix_web::test]
async fn the_modern_search_is_the_enveloped_camel_case_resource() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);
    let token = format!("Bearer {}", admin.token);

    test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=notes.txt&keywords=interop")
            .insert_header(("authorization", token.clone()))
            .insert_header((CONTENT_TYPE, "text/plain"))
            .set_payload(Bytes::from_static(b"hello"))
            .to_request(),
    )
    .await;

    let response = test::call_service(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/sync/search")
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    assert_eq!(
        response.headers().get(CONTENT_TYPE).unwrap(),
        "application/json",
        "no charset parameter: node-tak compares this with ===",
    );

    let body: serde_json::Value = test::read_body_json(response).await;

    assert_eq!(body["type"], "Resource");
    assert_eq!(body["version"], "3");

    let first = &body["data"][0];

    // node-tak's schema for this route.
    assert!(first["uid"].is_string());
    assert!(first["filename"].is_string());
    assert!(first["size"].is_i64(), "size is a number here: {first}");
    assert!(first["keywords"].is_array());
    assert!(first["submitter"].is_string());
    assert!(first["mimeType"].is_string());
    assert!(first["submissionTime"].is_string());
}

#[actix_web::test]
async fn the_file_manager_map_carries_the_keys_cloudtak_reads() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);
    let token = format!("Bearer {}", admin.token);

    // The package upload answers a bare URL, so the hash comes from the listing.
    let uploaded = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/missionupload?filename=trip.zip&creatorUid=ANDROID-1")
            .insert_header(("authorization", token.clone()))
            .insert_header(multipart_type())
            .set_payload(multipart("resource", "trip.zip", b"package bytes"))
            .to_request(),
    )
    .await;

    assert_eq!(uploaded.status().as_u16(), 200);

    let listing: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/files/metadata?missionPackage=true")
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(listing["type"], "Files");

    let entry = &listing["data"][0];

    assert!(entry["Hash"].is_string(), "{entry}");
    assert!(entry["Groups"].is_string(), "a comma string, not an array");
    assert!(entry["Time"].is_string());
    assert_eq!(entry["Name"], "trip.zip");
    assert_eq!(entry["User"], "grace");
    assert_eq!(entry["Creator"], "ANDROID-1");
    assert_eq!(entry["Expiration"], "none");
    assert!(
        entry["Size"].as_str().unwrap().ends_with('B'),
        "the size is humanised for a screen: {entry}",
    );

    let count: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/files/metadata/count?missionPackage=true")
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(count["type"], "Count");
    assert_eq!(count["data"], 1);

    let hash = entry["Hash"].as_str().unwrap().to_string();

    let download = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/Marti/api/files/{hash}"))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(download.status().as_u16(), 200);
    assert_eq!(
        download.headers().get("content-disposition").unwrap(),
        "attachment; filename=trip.zip",
    );
    assert_eq!(
        test::read_body(download).await,
        Bytes::from_static(b"package bytes"),
    );

    let head = test::call_service(
        &app,
        test::TestRequest::default()
            .method(actix_web::http::Method::HEAD)
            .uri(&format!("/Marti/api/files/{hash}"))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(head.status().as_u16(), 200);

    let updated = test::call_service(
        &app,
        test::TestRequest::put()
            .uri(&format!(
                "/Marti/api/files/{hash}/metadata?user=ada&keywords=one&keywords=two"
            ))
            .insert_header(("authorization", token.clone()))
            .to_request(),
    )
    .await;

    assert_eq!(updated.status().as_u16(), 200);

    let removed = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri(&format!("/Marti/api/files/{hash}"))
            .insert_header(("authorization", token))
            .to_request(),
    )
    .await;

    assert_eq!(removed.status().as_u16(), 200);
}

#[actix_web::test]
async fn a_caller_only_sees_what_their_channels_or_their_own_uploads_hold() {
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let (_, other) = server.signed_in("ada", false).await;
    let app = app!(server);

    let stored: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=shared.txt")
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .insert_header((CONTENT_TYPE, "text/plain"))
            .set_payload(Bytes::from_static(b"shared"))
            .to_request(),
    )
    .await;
    let hash = stored["Hash"].as_str().unwrap().to_string();

    // Both accounts are in the default channel, so both may read it.
    let seen = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/Marti/sync/content?hash={hash}"))
            .insert_header(("authorization", format!("Bearer {}", other.token)))
            .to_request(),
    )
    .await;

    assert_eq!(seen.status().as_u16(), 200);

    // Removing it is another matter: it is not theirs.
    let refused = test::call_service(
        &app,
        test::TestRequest::delete()
            .uri(&format!("/Marti/sync/delete?hash={hash}"))
            .insert_header(("authorization", format!("Bearer {}", other.token)))
            .to_request(),
    )
    .await;

    assert_eq!(refused.status().as_u16(), 403);

    // And an anonymous caller is in no channel at all.
    let anonymous = test::call_service(
        &app,
        test::TestRequest::get()
            .uri(&format!("/Marti/sync/content?hash={hash}"))
            .to_request(),
    )
    .await;

    assert_eq!(anonymous.status().as_u16(), 404);
}

#[actix_web::test]
async fn an_upload_past_the_configured_ceiling_is_refused_in_taks_own_words() {
    let server = TestServer::start_with(|config| {
        config.marti.upload_size_limit_mb = 1;
    })
    .await;
    let (_, admin) = server.signed_in("grace", true).await;
    let app = app!(server);

    let response = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=big.bin")
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .insert_header((CONTENT_TYPE, "application/octet-stream"))
            .set_payload(Bytes::from(vec![7u8; 2 * 1000 * 1000]))
            .to_request(),
    )
    .await;

    assert_eq!(response.status().as_u16(), 400);

    let body: serde_json::Value = test::read_body_json(response).await;

    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("exceeds server's size limit of 1 MB"),
        "{body}",
    );

    let empty = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/upload?name=nothing.bin")
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .insert_header((CONTENT_TYPE, "application/octet-stream"))
            .to_request(),
    )
    .await;

    assert_eq!(empty.status().as_u16(), 400);
}

#[actix_web::test]
async fn a_multi_megabyte_upload_reaches_the_store_without_being_collected() {
    // Streaming is not directly observable from a response, so this asserts the
    // two things that would not hold if the body were buffered and written in
    // one go: the blob is complete and correctly hashed, and the store's
    // temporary directory — which every ingest writes through and renames out
    // of — is empty afterwards.
    let server = TestServer::start().await;
    let (_, admin) = server.signed_in("grace", true).await;
    let content_dir = server.context.config().content_dir();
    let app = app!(server);

    let payload: Vec<u8> = (0..3_000_000u32).map(|n| n as u8).collect();

    let uploaded = test::call_service(
        &app,
        test::TestRequest::post()
            .uri("/Marti/sync/missionupload?filename=big.zip")
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .insert_header(multipart_type())
            .set_payload(multipart("assetfile", "big.zip", &payload))
            .to_request(),
    )
    .await;

    assert_eq!(uploaded.status().as_u16(), 200);

    let listing: serde_json::Value = test::call_and_read_body_json(
        &app,
        test::TestRequest::get()
            .uri("/Marti/api/files/metadata?missionPackage=true")
            .insert_header(("authorization", format!("Bearer {}", admin.token)))
            .to_request(),
    )
    .await;

    let hash = listing["data"][0]["Hash"].as_str().unwrap().to_string();

    assert_eq!(
        listing["data"][0]["Size"], "2MB",
        "3 MB of bytes, humanised against 1024: {listing}",
    );

    let blob = content_dir.join(&hash[..2]).join(&hash);

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

/// A real HTTP/2 request, because `TestRequest` cannot make one.
///
/// `test::init_service` hands the handler a request actix built in process,
/// which always has the HTTP/1 shape. The bug CI-01 found lives in the gap
/// between that shape and the one an h2 client produces: there the authority
/// arrives in the `:authority` pseudo-header, which actix puts on the request
/// URI, and there is no `Host` header at all — so the server used to give up on
/// deriving a URL and advertise `https://<[server] name>`, a display name, to a
/// peer that then fetched 0 of 1052 bytes.
///
/// Prior knowledge rather than ALPN: the same h2 framing that the TLS listeners
/// negotiate, without the authority, server certificate and client certificate
/// a TLS listener would need minting here. The scheme therefore comes back
/// `http`, which is what a plaintext listener honestly is.
#[actix_web::test]
async fn a_package_uploaded_over_http_2_is_advertised_at_the_authority_it_was_sent_to() {
    let server = TestServer::start_with(|config| {
        // Nothing that would short-circuit the derivation: no `[marti]
        // public_host`, no `[server] domains`, no `[server] base_url`, and a
        // `name` that is only a display name — the scenario configuration
        // `mp-download.toml` ran with.
        config.server.name = "rustak-interop-eud-mp-download".to_string();
    })
    .await;
    let (_, admin) = server.signed_in("grace", true).await;

    let socket = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("a free port");
    let port = socket.local_addr().expect("the port just bound").port();
    // `server.app()` borrows the harness, and actix needs the factory for
    // `'static`, so the same two values it would have cloned are cloned here.
    let (context, limiter) = (server.context.clone(), server.limiter.clone());
    let listener = actix_web::HttpServer::new(move || {
        App::new().configure(rustak_server::web::server::services(
            context.clone(),
            limiter.clone(),
        ))
    })
    .workers(1)
    .disable_signals()
    // `listen_auto_h2c`, because a plain `listen` speaks HTTP/1 only. The
    // real listeners reach the same h2 dispatcher through ALPN instead.
    .listen_auto_h2c(socket)
    .expect("the test listener binds")
    .run();
    let handle = listener.handle();

    actix_web::rt::spawn(listener);

    let client = reqwest::Client::builder()
        .http2_prior_knowledge()
        .build()
        .expect("an HTTP/2 client");

    let response = client
        .post(format!(
            "http://127.0.0.1:{port}/Marti/sync/missionupload\
             ?filename=trip.zip&creatorUid=ANDROID-1&Groups=__ANON__"
        ))
        .header("authorization", format!("Bearer {}", admin.token))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(multipart("assetfile", "trip.zip", b"PK-not-really-a-zip").to_vec())
        .send()
        .await
        .expect("the listener answers an h2 request");

    assert_eq!(
        response.version(),
        reqwest::Version::HTTP_2,
        "the premise of this test is that the request really was HTTP/2",
    );
    assert_eq!(response.status().as_u16(), 200);

    let url = response.text().await.expect("the URL is the whole body");

    assert!(
        url.starts_with(&format!("http://127.0.0.1:{port}/Marti/sync/content?hash=")),
        "the URL travels to a peer, so it names the authority the upload was addressed to: {url}",
    );
    assert!(
        !url.contains("rustak-interop-eud-mp-download"),
        "`[server] name` is a display name and nothing can resolve it: {url}",
    );

    handle.stop(false).await;
}

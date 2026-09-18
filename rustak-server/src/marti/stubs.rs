//! The routes that exist so that a client does not fall over, and do nothing.
//!
//! Three different kinds of nothing, and the difference matters to the client:
//!
//! * **An empty list.** Video, the CoT tag injectors and the repeater all have
//!   clients that fetch a list on a page load and break on a `404`. They get a
//!   well-formed empty one.
//! * **`501`.** A write to one of those features, and every KML export. The
//!   client learns the server will not do it, in the JSON shape every other
//!   refusal uses, and does not retry.
//! * **`404`.** Asking for one member of an empty collection. There is nothing
//!   there, which is not the same as the feature being absent.
//!
//! # Why video is stubbed rather than implemented
//!
//! CloudTAK's video integration is broken against every TAK server it is run
//! against today: it writes malformed entries (a `port` of `-1`, among others)
//! that then break iTAK's feed download, and OpenTAKServer documents its own
//! implementation as non-functional. Returning an empty list and refusing
//! writes sidesteps that whole family of bugs and is what every other server
//! operators run CloudTAK against effectively does. `compat/cloudtak.md` §8.
//!
//! # `/Marti/vcm` is deliberately not the same thing
//!
//! It is a much older alias some ATAK builds still probe on start-up. It
//! answers an empty `application/xml` document, which is harmless, rather than
//! being routed into the JSON video endpoint that shares its subject.

use actix_web::web;

use crate::prelude::*;

use super::error::{MartiError, MartiResult};
use super::principal::MartiPrincipal;
use super::response::{self, VERSION_1_0_0, kind};

/// The empty video-connection document `/Marti/vcm` answers with.
const EMPTY_VIDEO_XML: &str = "<videoConnections/>";

/// How often TAK Server tells a client the repeater runs, in milliseconds.
///
/// Reported so that a client which reads it before deciding whether to poll
/// gets a number rather than a parse failure. Nothing here repeats anything.
const REPEATER_PERIOD_MS: i64 = 3000;

/// The body of `GET /Marti/api/video`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoConnections {
    /// Always empty; see the module documentation.
    #[serde(rename = "videoConnections")]
    pub video_connections: Vec<serde_json::Value>,
}

/// `GET /Marti/api/video` — an empty, unenveloped list.
///
/// # Errors
///
/// Never.
pub async fn video() -> MartiResult {
    Ok(response::bare_json(&VideoConnections {
        video_connections: Vec::new(),
    }))
}

/// `GET /Marti/api/video/{uid}` — nothing is in the empty list.
///
/// # Errors
///
/// Always [`MartiError::NotFound`].
pub async fn video_feed(path: web::Path<String>) -> MartiResult {
    Err(MartiError::NotFound(format!("video feed {path}")))
}

/// Every write to the video feeds.
///
/// # Errors
///
/// Always [`MartiError::NotImplemented`].
pub async fn video_write() -> MartiResult {
    Err(MartiError::NotImplemented("Video feed management"))
}

/// `GET /Marti/vcm` — the legacy XML alias.
///
/// # Errors
///
/// Never.
pub async fn vcm() -> MartiResult {
    Ok(response::xml(EMPTY_VIDEO_XML))
}

/// `/Marti/vcu` and `/Marti/vcs` — the legacy write aliases.
///
/// # Errors
///
/// Always [`MartiError::NotImplemented`].
pub async fn vcm_write() -> MartiResult {
    Err(MartiError::NotImplemented("Video feed management"))
}

/// `GET /Marti/api/injectors/cot/uid` — an empty enveloped list.
///
/// # Errors
///
/// [`MartiError::Unauthorized`] or [`MartiError::Forbidden`]: this is an
/// administrative diagnostic, and an empty list is still a statement about the
/// installation.
pub async fn injectors(who: MartiPrincipal) -> MartiResult {
    who.require_admin()?;

    Ok(response::ok(
        kind::UID_COT_TAG_INJECTOR,
        Vec::<serde_json::Value>::new(),
    ))
}

/// `GET /Marti/api/injectors/cot/uid/{uid}`.
///
/// # Errors
///
/// [`MartiError::NotFound`], or the administrative refusals.
pub async fn injector(who: MartiPrincipal, path: web::Path<String>) -> MartiResult {
    who.require_admin()?;

    Err(MartiError::NotFound(format!("CoT tag injector {path}")))
}

/// Every write to the CoT tag injectors.
///
/// # Errors
///
/// [`MartiError::NotImplemented`], or the administrative refusals.
pub async fn injector_write(who: MartiPrincipal) -> MartiResult {
    who.require_admin()?;

    Err(MartiError::NotImplemented("CoT tag injectors"))
}

/// `GET /Marti/api/repeater/list` — an empty enveloped list.
///
/// Note the envelope version: the repeater endpoints report `1.0.0` rather than
/// `3`, which is an upstream inconsistency reproduced rather than tidied.
///
/// # Errors
///
/// The administrative refusals.
pub async fn repeater_list(who: MartiPrincipal) -> MartiResult {
    who.require_admin()?;

    Ok(response::versioned(
        VERSION_1_0_0,
        kind::REPEATABLE,
        Vec::<serde_json::Value>::new(),
    ))
}

/// `GET|POST /Marti/api/repeater/period` — the period, unchanged.
///
/// # Errors
///
/// The administrative refusals.
pub async fn repeater_period(who: MartiPrincipal) -> MartiResult {
    who.require_admin()?;

    Ok(response::versioned(
        VERSION_1_0_0,
        kind::INTEGER,
        REPEATER_PERIOD_MS,
    ))
}

/// `GET /Marti/api/repeater/remove/{uid}` — nothing was removed.
///
/// A `GET` that mutates is upstream's choice, not ours; it is registered as one
/// because that is the verb the client sends.
///
/// # Errors
///
/// The administrative refusals.
pub async fn repeater_remove(who: MartiPrincipal, _path: web::Path<String>) -> MartiResult {
    who.require_admin()?;

    Ok(response::versioned(VERSION_1_0_0, kind::BOOLEAN, false))
}

/// Every KML export servlet.
///
/// # Errors
///
/// Always [`MartiError::NotImplemented`].
pub async fn kml() -> MartiResult {
    Err(MartiError::NotImplemented("KML export"))
}

/// `POST /Marti/sync/missioncreate`.
///
/// The legacy servlet spelling of mission creation, which no verified client
/// uses — the REST route under `/Marti/api/missions` is what ATAK and CloudTAK
/// both call.
///
/// # Errors
///
/// Always [`MartiError::NotImplemented`].
pub async fn mission_create() -> MartiResult {
    Err(MartiError::NotImplemented(
        "The legacy /Marti/sync/missioncreate servlet",
    ))
}

#[cfg(test)]
mod tests {
    use actix_web::body::MessageBody as _;
    use actix_web::http::header::CONTENT_TYPE;
    use actix_web::{App, http::Method, test};

    use crate::testing::{TestServer, context::bearer};

    async fn call(
        server: &TestServer,
        method: Method,
        uri: &str,
        token: Option<&str>,
    ) -> (u16, String, String) {
        let app = test::init_service(App::new().configure(server.app())).await;
        let mut request = test::TestRequest::default().method(method).uri(uri);

        if let Some(token) = token {
            request = request.insert_header(("authorization", token.to_string()));
        }

        let response = test::call_service(&app, request.to_request()).await;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .map_or_else(String::new, |value| {
                value.to_str().unwrap_or_default().to_string()
            });
        let body = String::from_utf8(
            response
                .into_body()
                .try_into_bytes()
                .expect("a body")
                .to_vec(),
        )
        .expect("utf-8");

        (status, content_type, body)
    }

    #[actix_web::test]
    async fn video_is_an_empty_list_rather_than_a_missing_endpoint() {
        // CloudTAK's page fetches this on load and breaks on a 404.
        let server = TestServer::start().await;

        let (status, content_type, body) =
            call(&server, Method::GET, "/Marti/api/video", None).await;

        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert_eq!(body, r#"{"videoConnections":[]}"#);
    }

    #[actix_web::test]
    async fn writing_a_video_feed_is_refused_rather_than_half_accepted() {
        // Accepting one is how CloudTAK ends up writing `port: -1` entries that
        // then break iTAK's feed download.
        let server = TestServer::start().await;

        for (method, uri) in [
            (Method::POST, "/Marti/api/video"),
            (Method::PUT, "/Marti/api/video/abc"),
            (Method::DELETE, "/Marti/api/video/abc"),
        ] {
            let (status, content_type, body) = call(&server, method.clone(), uri, None).await;

            assert_eq!(status, 501, "{method} {uri}");
            assert_eq!(content_type, "application/json", "{method} {uri}");

            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
            assert_eq!(parsed["code"], 3, "{method} {uri}");
            assert_eq!(parsed["status"], "NOT_IMPLEMENTED", "{method} {uri}");
        }
    }

    #[actix_web::test]
    async fn one_feed_out_of_an_empty_list_is_not_found_rather_than_not_implemented() {
        // "There is nothing there" and "this server does not do that" are
        // different answers, and a client that retries on one should not on the
        // other.
        let server = TestServer::start().await;

        let (status, _, body) = call(&server, Method::GET, "/Marti/api/video/abc", None).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(status, 404);
        assert_eq!(parsed["code"], 1);
    }

    #[actix_web::test]
    async fn the_legacy_video_alias_is_empty_xml() {
        let server = TestServer::start().await;

        let (status, content_type, body) = call(&server, Method::GET, "/Marti/vcm", None).await;

        assert_eq!(status, 200);
        assert_eq!(content_type, "application/xml");
        assert_eq!(body, "<videoConnections/>");
    }

    #[actix_web::test]
    async fn the_administrative_stubs_answer_only_an_administrator() {
        let server = TestServer::start().await;
        let (_, ordinary) = server.signed_in("ada", false).await;
        let (_, admin) = server.signed_in("grace", true).await;

        for uri in [
            "/Marti/api/injectors/cot/uid",
            "/Marti/api/repeater/list",
            "/Marti/api/repeater/period",
        ] {
            assert_eq!(
                call(&server, Method::GET, uri, None).await.0,
                401,
                "{uri} with no credential",
            );
            assert_eq!(
                call(&server, Method::GET, uri, Some(&bearer(&ordinary)))
                    .await
                    .0,
                403,
                "{uri} as an ordinary caller",
            );
            assert_eq!(
                call(&server, Method::GET, uri, Some(&bearer(&admin)))
                    .await
                    .0,
                200,
                "{uri} as an administrator",
            );
        }
    }

    #[actix_web::test]
    async fn the_repeater_reports_its_own_envelope_version() {
        // `1.0.0` rather than `3`: an upstream inconsistency, reproduced.
        let server = TestServer::start().await;
        let (_, admin) = server.signed_in("grace", true).await;

        let (_, _, list) = call(
            &server,
            Method::GET,
            "/Marti/api/repeater/list",
            Some(&bearer(&admin)),
        )
        .await;
        let (_, _, period) = call(
            &server,
            Method::GET,
            "/Marti/api/repeater/period",
            Some(&bearer(&admin)),
        )
        .await;
        let (_, _, removed) = call(
            &server,
            Method::GET,
            "/Marti/api/repeater/remove/abc",
            Some(&bearer(&admin)),
        )
        .await;

        for (body, kind) in [
            (&list, "Repeatable"),
            (&period, "Integer"),
            (&removed, "java.lang.Boolean"),
        ] {
            let parsed: serde_json::Value = serde_json::from_str(body).unwrap();

            assert_eq!(parsed["version"], "1.0.0", "{body}");
            assert_eq!(parsed["type"], kind, "{body}");
        }

        let parsed: serde_json::Value = serde_json::from_str(&period).unwrap();
        assert_eq!(parsed["data"], 3000);

        let parsed: serde_json::Value = serde_json::from_str(&removed).unwrap();
        assert_eq!(parsed["data"], false);
    }

    #[actix_web::test]
    async fn every_kml_servlet_refuses_in_the_same_shape() {
        let server = TestServer::start().await;

        for uri in [
            "/Marti/ExportMissionKML",
            "/Marti/KmlMasterSA",
            "/Marti/LatestKML",
            "/Marti/TracksKML",
            "/Marti/api/missions/Alpha/kml",
        ] {
            let (status, content_type, body) = call(&server, Method::GET, uri, None).await;
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

            assert_eq!(status, 501, "{uri}");
            assert_eq!(content_type, "application/json", "{uri}");
            assert_eq!(parsed["message"], "KML export is not implemented", "{uri}");
        }
    }

    #[actix_web::test]
    async fn the_legacy_mission_create_servlet_is_refused_as_json() {
        let server = TestServer::start().await;

        let (status, content_type, _) =
            call(&server, Method::POST, "/Marti/sync/missioncreate", None).await;

        assert_eq!(status, 501);
        assert_eq!(content_type, "application/json");
    }
}

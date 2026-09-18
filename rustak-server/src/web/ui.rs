//! Serving the single-page admin UI.
//!
//! The output of `trunk build` in `rustak-ui` is compiled into the binary, so a
//! release is one file with no assets to deploy beside it. Anything that did
//! not match a route is answered with `index.html` rather than a 404, because
//! the UI routes on the client and a deep link reloaded in the browser has to
//! reach the shell before it can decide what to draw.

use actix_web::{HttpRequest, HttpResponse, http::header::ContentType};
use include_dir::{Dir, include_dir};

/// The compiled UI.
static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../rustak-ui/dist");

/// The shell served for anything that is not a file we hold.
const INDEX: &str = "index.html";

/// Serves an embedded asset, or the shell.
pub async fn serve(request: HttpRequest) -> HttpResponse {
    let path = request.path().trim_start_matches('/');
    let path = if path.is_empty() { INDEX } else { path };

    match ASSETS.get_file(path) {
        Some(file) => asset(path, file.contents()),
        None => shell(),
    }
}

/// Tells crawlers to stay away.
///
/// Mounted ahead of the catch-all so that it answers as itself rather than as
/// the SPA shell — a crawler handed HTML where it asked for `robots.txt` reads
/// it as "no rules", which is the opposite of what this says.
pub async fn robots() -> HttpResponse {
    HttpResponse::Ok()
        .content_type(ContentType::plaintext())
        .body("User-agent: *\nDisallow: /\n")
}

/// One asset, with its type inferred from its extension.
fn asset(path: &str, contents: &'static [u8]) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header((actix_web::http::header::CONTENT_TYPE, content_type(path)))
        .body(contents)
}

/// The shell, or an honest placeholder when the UI has not been built.
fn shell() -> HttpResponse {
    match ASSETS.get_file(INDEX) {
        Some(file) => HttpResponse::Ok()
            .content_type(ContentType::html())
            .body(file.contents()),
        None => HttpResponse::InternalServerError()
            .content_type(ContentType::html())
            .body(
                "<!DOCTYPE html><title>rustak</title><p>The user interface has not been built.</p>",
            ),
    }
}

/// What to serve a file as.
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("css") => "text/css; charset=utf-8",
        // No charset parameter: the admin API's responses are compared byte for
        // byte by CloudTAK, and a JSON asset served differently from a JSON
        // response is a difference waiting to be depended on.
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::{App, test as actix_test, web};

    use super::*;

    #[test]
    fn a_file_is_served_as_what_it_is() {
        assert_eq!(content_type("app.wasm"), "application/wasm");
        assert_eq!(content_type("index.html"), "text/html; charset=utf-8");
        assert_eq!(
            content_type("data.json"),
            "application/json",
            "no charset parameter, anywhere",
        );
        assert_eq!(content_type("LICENSE"), "application/octet-stream");
    }

    #[actix_web::test]
    async fn crawlers_are_told_to_stay_away_in_plain_text() {
        let app =
            actix_test::init_service(App::new().route("/robots.txt", web::get().to(robots))).await;

        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/robots.txt")
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("text/plain; charset=utf-8")
        );

        let body = actix_test::read_body(response).await;
        assert!(String::from_utf8_lossy(&body).contains("Disallow: /"));
    }

    #[actix_web::test]
    async fn a_deep_link_reaches_the_shell_rather_than_a_not_found() {
        let app = actix_test::init_service(App::new().default_service(web::get().to(serve))).await;

        let response = actix_test::call_service(
            &app,
            actix_test::TestRequest::get()
                .uri("/admin/users")
                .to_request(),
        )
        .await;

        assert_ne!(
            response.status(),
            StatusCode::NOT_FOUND,
            "the UI routes on the client, so the shell has to answer a reload",
        );
    }
}

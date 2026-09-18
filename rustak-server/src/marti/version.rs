//! What this server says it is: `/Marti/api/version*`, `/Marti/api/node/id`
//! and `/files/api/config`.
//!
//! Four endpoints that between them decide whether a client will talk to us at
//! all, and every one of them is anonymous — a device probes them *before* it
//! has a certificate, and CloudTAK's setup wizard calls one of them to validate
//! a connection it has not saved yet.
//!
//! # The two that are load-bearing
//!
//! `GET /Marti/api/version` must contain the literal words `TAK Server`: ATAK
//! matches on them to decide it is talking to a TAK server rather than to a
//! captive portal. It must also answer `2xx` on the mutually authenticated
//! listener, because CloudTAK probes it on every session check — a certificate
//! we will not accept has to be refused during the handshake, not with a `403`
//! here.
//!
//! `GET /files/api/config` is the single highest-priority endpoint in the whole
//! CloudTAK surface: `PATCH /api/server`, its own setup call, validates a newly
//! entered server by calling exactly this one and checking that
//! `uploadSizeLimit` is not `undefined`. Until it answers, the wizard cannot
//! save a working connection at all.
//!
//! # Why `version` is a string inside an integer
//!
//! `/version/config`'s envelope carries `"version": "3"` at the top level,
//! which ATAK parses as an **integer** to decide whether the server is new
//! enough for `tool` on a sync search — while `data.version` is this server's
//! own semantic version, as a string. Two fields spelled the same, meaning
//! different things, and swapping them silently disables a feature.

use actix_web::web;

use crate::prelude::*;

use super::error::MartiResult;
use super::principal::MartiPrincipal;
use super::response::{self, kind};

/// This build's version, as the manifest states it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The product string ATAK matches on, with our own version after it.
///
/// `TAK Server` is the part that matters; `rustak-<version>` is what tells an
/// operator reading a packet capture whose server answered.
pub fn product_string() -> String {
    format!("TAK Server rustak-{VERSION}")
}

/// The branch name reported by `/version/info`.
const BRANCH: &str = "rustak";

/// The deployment variant reported by `/version/info`.
///
/// TAK Server distinguishes a directly served installation from a federated
/// one here; we only ever are the former.
const VARIANT: &str = "DIRECT";

/// The envelope `version` clients parse as an integer.
const API_LEVEL: &str = "3";

/// `GET /Marti/api/version` — the product string, as plain text.
///
/// # Errors
///
/// Never.
pub async fn version() -> MartiResult {
    Ok(response::text(
        actix_web::http::StatusCode::OK,
        product_string(),
    ))
}

/// The `data` of `/Marti/api/version/config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfigData {
    /// This server's own semantic version, as a **string**.
    pub version: String,

    /// The API level, as a string. Distinct from the envelope's `version`.
    pub api: String,

    /// The host name this installation answers to, with no port.
    pub hostname: String,
}

/// `GET /Marti/api/version/config`.
///
/// # Errors
///
/// Never.
pub async fn version_config(
    request: actix_web::HttpRequest,
    context: web::Data<AppContext>,
) -> MartiResult {
    let data = ServerConfigData {
        version: VERSION.to_string(),
        api: API_LEVEL.to_string(),
        hostname: hostname(&request, &context.config()),
    };

    Ok(response::ok(kind::SERVER_CONFIG, data))
}

/// The body of `/Marti/api/version/info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct VersionInfo {
    /// The major component of this build's version.
    pub major: u32,
    /// The minor component.
    pub minor: u32,
    /// The patch component.
    pub patch: u32,
    /// Always `rustak`.
    pub branch: &'static str,
    /// Always `DIRECT`.
    pub variant: &'static str,
}

impl Default for VersionInfo {
    fn default() -> Self {
        let mut parts = VERSION
            .split(['.', '-', '+'])
            .map(|part| part.parse::<u32>().unwrap_or_default());

        Self {
            major: parts.next().unwrap_or_default(),
            minor: parts.next().unwrap_or_default(),
            patch: parts.next().unwrap_or_default(),
            branch: BRANCH,
            variant: VARIANT,
        }
    }
}

/// `GET /Marti/api/version/info` — the version, taken apart, unenveloped.
///
/// # Errors
///
/// Never.
pub async fn version_info() -> MartiResult {
    Ok(response::bare_json(&VersionInfo::default()))
}

/// `GET /Marti/api/node/id` — this installation's node id, as plain text.
///
/// # Errors
///
/// Never: an id we could not read back answers with the placeholder rather than
/// failing a probe a client makes before it can do anything else.
pub async fn node_id() -> MartiResult {
    Ok(response::text(
        actix_web::http::StatusCode::OK,
        response::node_id(),
    ))
}

/// The body of `/files/api/config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesConfig {
    /// The upload ceiling, in **megabytes**, as an integer.
    #[serde(rename = "uploadSizeLimit")]
    pub upload_size_limit: u32,
}

/// `GET /files/api/config` — the CloudTAK setup gate.
///
/// Note the path: no `/Marti` prefix, which is why it is mounted as its own
/// scope rather than inside the Marti one.
///
/// # Errors
///
/// Never.
pub async fn files_config(context: web::Data<AppContext>) -> MartiResult {
    let config = context.config();

    Ok(response::bare_json(&FilesConfig {
        upload_size_limit: config.marti.upload_size_limit_mb,
    }))
}

/// `GET /Marti/api/util/isAdmin` — a bare boolean.
///
/// `false` for an anonymous caller rather than a `401`: this is a question the
/// admin UI asks to decide what to render, not a gate.
///
/// # Errors
///
/// Never.
pub async fn is_admin(who: MartiPrincipal) -> MartiResult {
    Ok(response::bare_json(&who.is_admin()))
}

/// The host name we tell a client we are.
///
/// `[marti] public_host` first, because it is the only value that is right for
/// a server behind NAT or inside a container — the name we report is passed on
/// to a client's *peers*. The `Host` header next, with its port stripped, which
/// is right for a directly reachable server and is what TAK Server does. Then
/// the configured canonical domain, and finally `localhost`, which is what a
/// first start before the wizard has anything to say is.
fn hostname(request: &actix_web::HttpRequest, config: &Config) -> String {
    if let Some(configured) = config.marti.public_host.as_deref() {
        return configured.to_string();
    }

    let from_header = crate::web::helpers::request::header_str(request.headers(), "host")
        .map(strip_port)
        .filter(|host| !host.is_empty());

    if let Some(host) = from_header {
        return host.to_string();
    }

    config
        .server
        .canonical_domain()
        .map_or_else(|| "localhost".to_string(), str::to_string)
}

/// The host part of an authority, without its port.
///
/// An IPv6 literal keeps its brackets, because `[::1]` without them is not an
/// authority a client can put back into a URL.
fn strip_port(authority: &str) -> &str {
    if let Some(end) = authority.find(']') {
        return &authority[..=end];
    }

    authority.split(':').next().unwrap_or(authority)
}

#[cfg(test)]
mod tests {
    use actix_web::body::MessageBody as _;
    use actix_web::http::header::CONTENT_TYPE;
    use actix_web::{App, test};

    use super::*;
    use crate::testing::TestServer;

    async fn get(server: &TestServer, uri: &str, host: Option<&str>) -> (u16, String, String) {
        let app = test::init_service(App::new().configure(server.app())).await;
        let mut request = test::TestRequest::get().uri(uri);

        if let Some(host) = host {
            request = request.insert_header(("host", host.to_string()));
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
    async fn the_version_probe_says_the_two_words_atak_looks_for() {
        // ATAK's matcher is on the literal `TAK Server`; without it a device
        // decides it reached something that is not a TAK server at all.
        let server = TestServer::start().await;

        let (status, content_type, body) = get(&server, "/Marti/api/version", None).await;

        assert_eq!(status, 200);
        assert_eq!(content_type, "text/plain");
        assert!(body.starts_with("TAK Server "), "{body}");
        assert_eq!(body, format!("TAK Server rustak-{VERSION}"));
        assert!(!body.ends_with('\n'), "no trailing newline: {body:?}");
    }

    #[actix_web::test]
    async fn the_version_probe_answers_without_any_credential() {
        // CloudTAK probes it on every session check, and a device probes it
        // before it has a certificate at all.
        let server = TestServer::start().await;

        assert_eq!(get(&server, "/Marti/api/version", None).await.0, 200);
    }

    #[actix_web::test]
    async fn the_server_config_is_the_shape_ataks_parser_expects() {
        let server = TestServer::start().await;

        let (status, content_type, body) = get(
            &server,
            "/Marti/api/version/config",
            Some("tak.example.com:8443"),
        )
        .await;
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert_eq!(parsed["type"], "ServerConfig");
        assert!(
            parsed["version"].as_str().unwrap().parse::<i32>().is_ok(),
            "ATAK parses the envelope version as an integer: {body}",
        );
        assert!(
            parsed["data"]["version"].is_string(),
            "the data version is this server's own, as a string: {body}",
        );
        assert_eq!(parsed["data"]["api"], "3");
        assert_eq!(
            parsed["data"]["hostname"], "tak.example.com",
            "the port is not part of the host name",
        );
        assert!(parsed["nodeId"].is_string());
    }

    #[actix_web::test]
    async fn the_reported_host_name_prefers_what_an_operator_configured() {
        // The name goes into URLs a device hands to its peers, so a container's
        // own `Host` header would be unreachable for every one of them.
        let server = TestServer::start_with(|config| {
            config.marti.public_host = Some("tak.example.com".to_string());
        })
        .await;

        let (_, _, body) = get(
            &server,
            "/Marti/api/version/config",
            Some("172.17.0.2:8443"),
        )
        .await;
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(parsed["data"]["hostname"], "tak.example.com");
    }

    #[actix_web::test]
    async fn an_ipv6_authority_keeps_its_brackets() {
        // `::1` without them is not something a client can put back in a URL.
        assert_eq!(strip_port("[::1]:8443"), "[::1]");
        assert_eq!(strip_port("[::1]"), "[::1]");
        assert_eq!(strip_port("tak.example.com:8443"), "tak.example.com");
        assert_eq!(strip_port("tak.example.com"), "tak.example.com");
    }

    #[actix_web::test]
    async fn the_version_info_is_the_semantic_version_taken_apart() {
        let server = TestServer::start().await;

        let (status, content_type, body) = get(&server, "/Marti/api/version/info", None).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert_eq!(parsed["branch"], "rustak");
        assert_eq!(parsed["variant"], "DIRECT");
        assert_eq!(
            format!(
                "{}.{}.{}",
                parsed["major"], parsed["minor"], parsed["patch"]
            ),
            VERSION.split(['-', '+']).next().unwrap(),
        );
        assert!(
            !body.contains("\"data\""),
            "this one is not enveloped: {body}",
        );
    }

    #[actix_web::test]
    async fn the_node_id_is_served_as_text_and_matches_the_envelopes() {
        let server = TestServer::start().await;

        // The envelope carries it too, so the two must agree.
        let (_, _, enveloped) = get(&server, "/Marti/api/version/config", None).await;
        let parsed: serde_json::Value = serde_json::from_str(&enveloped).unwrap();
        let (status, content_type, body) = get(&server, "/Marti/api/node/id", None).await;

        assert_eq!(status, 200);
        assert_eq!(content_type, "text/plain");
        assert_eq!(parsed["nodeId"], body);
    }

    #[actix_web::test]
    async fn the_cloudtak_setup_gate_answers_an_integer_with_no_envelope() {
        // CloudTAK's `PATCH /api/server` validates a connection by calling
        // exactly this and checking `uploadSizeLimit !== undefined`.
        let server = TestServer::start().await;

        let (status, content_type, body) = get(&server, "/files/api/config", None).await;
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();

        assert_eq!(status, 200);
        assert_eq!(content_type, "application/json");
        assert_eq!(parsed, serde_json::json!({ "uploadSizeLimit": 400 }));
        assert!(
            parsed["uploadSizeLimit"].is_u64(),
            "an integer, not a string"
        );
    }

    #[actix_web::test]
    async fn the_upload_ceiling_is_the_one_an_operator_configured() {
        let server = TestServer::start_with(|config| {
            config.marti.upload_size_limit_mb = 25;
        })
        .await;

        let (_, _, body) = get(&server, "/files/api/config", None).await;

        assert_eq!(body, r#"{"uploadSizeLimit":25}"#);
    }

    #[actix_web::test]
    async fn whether_the_caller_administers_this_installation_is_a_bare_boolean() {
        let server = TestServer::start().await;
        let (_, session) = server.signed_in("ada", true).await;
        let app = test::init_service(App::new().configure(server.app())).await;

        let (status, _, anonymous) = get(&server, "/Marti/api/util/isAdmin", None).await;
        assert_eq!(status, 200, "an anonymous caller is answered, not refused");
        assert_eq!(anonymous, "false");

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/Marti/api/util/isAdmin")
                .insert_header(("authorization", crate::testing::context::bearer(&session)))
                .to_request(),
        )
        .await;

        let body =
            String::from_utf8(response.into_body().try_into_bytes().unwrap().to_vec()).unwrap();
        assert_eq!(body, "true");
    }
}

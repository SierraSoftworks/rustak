//! The envelope, the `type` strings, and the one place a content type is set.
//!
//! # Why every response is built here
//!
//! `node-tak` — the client library CloudTAK is written against — decides
//! whether a response is JSON with a **string equality** test on the header:
//! `application/json; charset=UTF-8` is not `application/json`, falls into the
//! fallback branch, and hands the caller a raw string where it expected a
//! parsed object. Most of its call sites then index straight into that value.
//! So the header is written out as a [`HeaderValue`] constant rather than left
//! to `HttpResponse::json`, which appends a charset on some paths, and every
//! Marti route goes through a function in this module. A contract test walks
//! the whole route table asserting the bytes.
//!
//! # Why the `type` strings look inconsistent
//!
//! Because they are. TAK Server emits a fully-qualified Java class name for
//! some endpoints, a bare class name for others, and a boxed primitive's name
//! for a third set — sometimes for two spellings of the same model, as
//! `MISSION_SUBSCRIPTION_FQCN` and [`kind::MISSION_SUBSCRIPTION`] do. ATAK and
//! CloudTAK both match on these, so they are reproduced exactly rather than
//! tidied. They live in one module so that the envelope test can be a table.

use std::sync::OnceLock;

use actix_web::http::StatusCode;
use actix_web::http::header::{CONTENT_TYPE, HeaderValue};
use actix_web::{HttpResponse, HttpResponseBuilder};

use crate::prelude::*;

/// The exact content type every Marti JSON response carries.
pub const JSON: HeaderValue = HeaderValue::from_static("application/json");

/// What the legacy Enterprise Sync servlets answer with.
///
/// A historical quirk of TAK Server that every real deployment still emits; no
/// verified client parses these strictly, so it is safe to keep distinct.
pub const TEXT_JSON: HeaderValue = HeaderValue::from_static("text/json");

/// Plain text, with no charset parameter, for the version and time probes.
pub const TEXT_PLAIN: HeaderValue = HeaderValue::from_static("text/plain");

/// The video-connection and KML servlets' content type.
pub const XML: HeaderValue = HeaderValue::from_static("application/xml");

/// The container-level error documents' content type.
pub const HTML: HeaderValue = HeaderValue::from_static("text/html");

/// The envelope version almost every endpoint reports.
pub const VERSION_3: &str = "3";

/// The envelope version the repeater endpoints report instead.
pub const VERSION_1_0_0: &str = "1.0.0";

/// The node id used before one has been read back from storage.
///
/// Only reachable in a unit test that builds a response without going through
/// the listener; [`ensure_node_id`] runs on every Marti request, so a served
/// response always carries the installation's own.
const UNKNOWN_NODE_ID: &str = "rustak-00000000";

/// Where the generated node id is kept.
const NODE_ID_PARTITION: &str = "marti";

/// The key it is kept under.
const NODE_ID_KEY: &str = "node-id";

/// The process-wide copy, so that building a response is not a database read.
static NODE_ID: OnceLock<String> = OnceLock::new();

/// The `type` strings, one constant per endpoint family.
///
/// Faithfully inconsistent: see the module documentation.
pub mod kind {
    /// `/groups/all`, `/groups/user`, `/groups/{name}/{direction}`.
    pub const GROUP: &str = "com.bbn.marti.remote.groups.Group";
    /// `/users/all`.
    pub const USER: &str = "com.bbn.marti.remote.groups.User";
    /// `/groups/groupCacheEnabled`, `/repeater/remove/{uid}`.
    pub const BOOLEAN: &str = "java.lang.Boolean";
    /// `/missions/{name}/token`.
    pub const STRING: &str = "java.lang.String";
    /// `/clientEndPoints`.
    pub const CLIENT_ENDPOINT: &str = "com.bbn.marti.remote.ClientEndpoint";
    /// Every mission payload, and `/missioncount`.
    pub const MISSION: &str = "Mission";
    /// `/missions/{name}/changes`, `/contents/missionpackage`.
    pub const MISSION_CHANGE: &str = "MissionChange";
    /// `/missions/{name}/layers*`.
    pub const MISSION_LAYER: &str = "MissionLayer";
    /// The **singular** `/missions/{name}/subscription`.
    pub const MISSION_SUBSCRIPTION_FQCN: &str = "com.bbn.marti.sync.model.MissionSubscription";
    /// The **plural** subscription listings, which use the bare name.
    pub const MISSION_SUBSCRIPTION: &str = "MissionSubscription";
    /// Every invitation listing.
    pub const MISSION_INVITATION: &str = "MissionInvitation";
    /// `/missions/{name}/role`.
    pub const MISSION_ROLE: &str = "com.bbn.marti.sync.model.MissionRole";
    /// The mission log endpoints.
    pub const LOG_ENTRY: &str = "com.bbn.marti.sync.model.LogEntry";
    /// `/Marti/api/sync/search`, `/resources/{hash}`.
    pub const RESOURCE: &str = "Resource";
    /// `/missions/{name}/maplayers`.
    pub const MAP_LAYER: &str = "MapLayer";
    /// `/missions/{name}/externaldata`.
    pub const EXTERNAL_DATA: &str = "ExternalMissionData";
    /// `/subscriptions/all`, `/subscription/{uid}`.
    pub const SUBSCRIPTION_INFO: &str = "SubscriptionInfo";
    /// `/Marti/api/version/config`. ATAK string-matches this one.
    pub const SERVER_CONFIG: &str = "ServerConfig";
    /// `/Marti/api/files/metadata`.
    pub const FILES: &str = "Files";
    /// `/Marti/api/files/metadata/count`.
    pub const COUNT: &str = "Count";
    /// `HEAD /Marti/api/files/{hash}`.
    pub const DATA: &str = "data";
    /// The device-profile admin endpoints.
    pub const PROFILE: &str = "Profile";
    /// The profile file listings.
    pub const PROFILE_FILE: &str = "ProfileFile";
    /// `/Marti/api/injectors/cot/uid`.
    pub const UID_COT_TAG_INJECTOR: &str = "UidCotTagInjector";
    /// `/Marti/api/repeater/list`.
    pub const REPEATABLE: &str = "Repeatable";
    /// `/Marti/api/repeater/period`.
    pub const INTEGER: &str = "Integer";
}

/// The `{version, type, data, messages, nodeId}` envelope.
///
/// `data` and `messages` are omitted when absent rather than serialised as
/// `null`, which is what TAK Server does and what the transcribed node-tak
/// schemas expect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiResponse<T: Serialize> {
    /// Almost always [`VERSION_3`]; the repeater endpoints say
    /// [`VERSION_1_0_0`].
    pub version: &'static str,

    /// The per-endpoint `type` string; see [`kind`].
    #[serde(rename = "type")]
    pub kind: &'static str,

    /// The payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,

    /// Human-readable notes some endpoints attach beside the payload.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<String>>,

    /// This installation's node id.
    #[serde(rename = "nodeId")]
    pub node_id: String,
}

impl<T: Serialize> ApiResponse<T> {
    /// An envelope carrying a payload.
    pub fn new(kind: &'static str, data: T) -> Self {
        Self {
            version: VERSION_3,
            kind,
            data: Some(data),
            messages: None,
            node_id: node_id(),
        }
    }

    /// An envelope carrying nothing but its own metadata.
    pub fn empty(kind: &'static str) -> Self {
        Self {
            version: VERSION_3,
            kind,
            data: None,
            messages: None,
            node_id: node_id(),
        }
    }

    /// Reports a version other than [`VERSION_3`].
    #[must_use]
    pub fn with_version(mut self, version: &'static str) -> Self {
        self.version = version;
        self
    }

    /// Attaches the notes some endpoints carry beside the payload.
    #[must_use]
    pub fn with_messages(mut self, messages: Vec<String>) -> Self {
        self.messages = Some(messages);
        self
    }
}

/// `200`, enveloped.
pub fn ok<T: Serialize>(kind: &'static str, data: T) -> HttpResponse {
    render(StatusCode::OK, &ApiResponse::new(kind, data))
}

/// `201`, enveloped — what a create answers with.
pub fn created<T: Serialize>(kind: &'static str, data: T) -> HttpResponse {
    render(StatusCode::CREATED, &ApiResponse::new(kind, data))
}

/// An enveloped response with a status and an optional payload.
pub fn status<T: Serialize>(
    status: StatusCode,
    kind: &'static str,
    data: Option<T>,
) -> HttpResponse {
    let envelope = match data {
        Some(data) => ApiResponse::new(kind, data),
        None => ApiResponse::empty(kind),
    };

    render(status, &envelope)
}

/// An enveloped `200` reporting a version other than [`VERSION_3`].
pub fn versioned<T: Serialize>(version: &'static str, kind: &'static str, data: T) -> HttpResponse {
    render(
        StatusCode::OK,
        &ApiResponse::new(kind, data).with_version(version),
    )
}

/// A JSON body with **no** envelope.
///
/// `/Marti/api/contacts/all`, `/files/api/config` and the `/util/*` endpoints
/// deliberately break the envelope convention. Do not "fix" them into one — the
/// clients parse them as bare values.
pub fn bare_json<T: Serialize>(data: &T) -> HttpResponse {
    render(StatusCode::OK, data)
}

/// A JSON body with no envelope and a status of the caller's choosing.
pub fn bare_json_with<T: Serialize>(status: StatusCode, data: &T) -> HttpResponse {
    render(status, data)
}

/// `text/plain`, with no charset parameter.
pub fn text(status: StatusCode, body: impl Into<String>) -> HttpResponse {
    typed(status, TEXT_PLAIN, body.into())
}

/// The legacy Enterprise Sync `text/json` body.
pub fn text_json<T: Serialize>(data: &T) -> HttpResponse {
    match serde_json::to_vec(data) {
        Ok(body) => typed(StatusCode::OK, TEXT_JSON, body),
        Err(err) => unserialisable(&err),
    }
}

/// `application/xml`.
pub fn xml(body: impl Into<String>) -> HttpResponse {
    typed(StatusCode::OK, XML, body.into())
}

/// One of the two container-level error documents.
pub fn html(status: StatusCode, body: &'static str) -> HttpResponse {
    typed(status, HTML, body)
}

/// Adds the headers that stop a response being cached anywhere.
///
/// `/Marti/api/clientEndPoints` is the one endpoint TAK Server marks
/// explicitly non-cacheable, because a stale contact list is indistinguishable
/// from a contact who left.
pub fn no_store(builder: &mut HttpResponseBuilder) -> &mut HttpResponseBuilder {
    builder
        .insert_header((
            actix_web::http::header::CACHE_CONTROL,
            "must-revalidate, max-age=0, no-cache, no-store",
        ))
        .insert_header((actix_web::http::header::EXPIRES, "0"))
}

/// Serialises a value as JSON with the exact content type.
fn render<T: Serialize>(status: StatusCode, value: &T) -> HttpResponse {
    match serde_json::to_vec(value) {
        Ok(body) => typed(status, JSON, body),
        Err(err) => unserialisable(&err),
    }
}

/// A body with a content type written out rather than inferred.
fn typed(
    status: StatusCode,
    content_type: HeaderValue,
    body: impl actix_web::body::MessageBody + 'static,
) -> HttpResponse {
    HttpResponse::build(status)
        .insert_header((CONTENT_TYPE, content_type))
        .body(body)
}

/// What a value we could not serialise turns into.
///
/// Reported as our own failure in the Marti error shape, so that a client which
/// branches on `code` still sees something it understands.
fn unserialisable(err: &serde_json::Error) -> HttpResponse {
    error!(error = %err, "Could not serialise a Marti response.");

    typed(
        StatusCode::INTERNAL_SERVER_ERROR,
        JSON,
        br#"{"status":"INTERNAL_SERVER_ERROR","code":6,"message":""}"#.as_slice(),
    )
}

/// This installation's node id, as [`ensure_node_id`] last read it.
///
/// The id becomes the `TAK-Server-<id>` flow-tag attribute on every CoT event
/// we relay, so it has to be a valid XML NCName — which is why it is generated
/// as `rustak-<8 hex>` rather than as a raw UUID.
pub fn node_id() -> String {
    NODE_ID
        .get()
        .cloned()
        .unwrap_or_else(|| UNKNOWN_NODE_ID.to_string())
}

/// Reads the node id back, generating and storing one the first time.
///
/// # Errors
///
/// Whatever the key/value store returned. The caller answers the request
/// anyway: a node id we could not read is a degraded response rather than a
/// failed one.
pub async fn load_node_id(services: &impl Services) -> Result<String, Error> {
    if let Some(stored) = services
        .kv()
        .get::<String>(NODE_ID_PARTITION, NODE_ID_KEY)
        .await?
    {
        return Ok(stored);
    }

    let generated = format!("rustak-{:08x}", rand::random::<u32>());

    // `insert` rather than `set`: two workers racing on a first request must
    // agree on one id rather than each overwriting the other's.
    if services
        .kv()
        .insert(NODE_ID_PARTITION, NODE_ID_KEY, generated.clone())
        .await?
    {
        info!(node.id = %generated, "Generated this installation's Marti node id.");

        return Ok(generated);
    }

    services
        .kv()
        .get::<String>(NODE_ID_PARTITION, NODE_ID_KEY)
        .await?
        .ok_or_else(|| {
            human_errors::system(
                "The Marti node id could not be read back after it was written.",
                crate::db::ADVICE_REPORT_DEV,
            )
        })
}

/// Fills the process-wide copy, once.
///
/// Called by the Marti middleware rather than at start-up, so that mounting the
/// scope does not add a database read to a server that never serves a Marti
/// request. After the first one this is an atomic load.
pub async fn ensure_node_id(services: &impl Services) {
    if NODE_ID.get().is_some() {
        return;
    }

    match load_node_id(services).await {
        Ok(id) => {
            let _ = NODE_ID.set(id);
        }
        Err(err) => {
            warn!(error = %err, "Could not read this installation's Marti node id.");
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::body::MessageBody as _;

    use super::*;

    fn body_of(response: HttpResponse) -> serde_json::Value {
        let bytes = response.into_body().try_into_bytes().unwrap();

        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn every_json_response_says_application_json_and_nothing_else() {
        // node-tak compares this header with `===`; a charset parameter would
        // be correct HTTP and a broken client.
        for response in [
            ok(kind::MISSION, serde_json::json!([])),
            created(kind::MISSION, serde_json::json!({})),
            bare_json(&serde_json::json!({ "uploadSizeLimit": 400 })),
            status(StatusCode::OK, kind::BOOLEAN, Some(false)),
            versioned(VERSION_1_0_0, kind::REPEATABLE, serde_json::json!([])),
        ] {
            assert_eq!(
                response.headers().get(CONTENT_TYPE).unwrap(),
                "application/json",
            );
        }
    }

    #[test]
    fn the_legacy_content_types_are_the_ones_tak_server_emits() {
        assert_eq!(
            text_json(&serde_json::json!({}))
                .headers()
                .get(CONTENT_TYPE)
                .unwrap(),
            "text/json",
        );
        assert_eq!(
            text(StatusCode::OK, "TAK Server rustak-0.1.0")
                .headers()
                .get(CONTENT_TYPE)
                .unwrap(),
            "text/plain",
        );
        assert_eq!(
            xml("<videoConnections/>")
                .headers()
                .get(CONTENT_TYPE)
                .unwrap(),
            "application/xml",
        );
        assert_eq!(
            html(StatusCode::NOT_FOUND, "<html></html>")
                .headers()
                .get(CONTENT_TYPE)
                .unwrap(),
            "text/html",
        );
    }

    #[test]
    fn an_absent_payload_is_omitted_rather_than_serialised_as_null() {
        let body = body_of(status::<()>(StatusCode::OK, kind::MISSION, None));

        assert_eq!(
            body,
            serde_json::json!({
                "version": "3",
                "type": "Mission",
                "nodeId": node_id(),
            }),
        );
    }

    #[test]
    fn the_envelope_is_the_shape_every_client_parses() {
        let body = body_of(ok(kind::GROUP, serde_json::json!([{ "name": "Blue" }])));

        assert_eq!(body["version"], "3");
        assert_eq!(body["type"], "com.bbn.marti.remote.groups.Group");
        assert_eq!(body["data"][0]["name"], "Blue");
        assert!(body["nodeId"].is_string());
        assert!(body.get("messages").is_none());
    }

    #[test]
    fn the_repeater_endpoints_report_their_own_envelope_version() {
        let body = body_of(versioned(
            VERSION_1_0_0,
            kind::REPEATABLE,
            serde_json::json!([]),
        ));

        assert_eq!(body["version"], "1.0.0");
        assert_eq!(body["type"], "Repeatable");
    }

    #[test]
    fn messages_are_attached_only_when_there_are_some() {
        let with = ApiResponse::new(kind::MISSION, 1).with_messages(vec!["note".to_string()]);
        let without = ApiResponse::new(kind::MISSION, 1);

        assert_eq!(
            with.messages.as_deref(),
            Some(["note".to_string()].as_slice())
        );
        assert!(without.messages.is_none());
    }

    #[test]
    fn a_node_id_is_a_valid_xml_ncname() {
        // It becomes the `TAK-Server-<id>` flow-tag attribute, so a character
        // an XML name cannot hold would produce unparseable CoT.
        let id = node_id();

        assert!(id.starts_with("rustak-"), "{id}");
        assert!(
            id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
            "{id}",
        );
        assert!(id.starts_with(|c: char| c.is_ascii_alphabetic()), "{id}");
    }

    #[tokio::test]
    async fn the_node_id_is_generated_once_and_read_back_afterwards() {
        // It is written into flow tags that peers and federated servers see, so
        // a restart must not change it.
        let server = crate::testing::TestServer::start().await;

        let first = load_node_id(&server.context).await.unwrap();
        let second = load_node_id(&server.context).await.unwrap();

        assert_eq!(first, second);
        assert!(first.starts_with("rustak-"));
    }

    #[test]
    fn a_cache_busting_response_says_so_in_both_headers() {
        let response = no_store(&mut HttpResponse::Ok()).finish();

        assert_eq!(
            response
                .headers()
                .get(actix_web::http::header::CACHE_CONTROL)
                .unwrap(),
            "must-revalidate, max-age=0, no-cache, no-store",
        );
        assert_eq!(
            response
                .headers()
                .get(actix_web::http::header::EXPIRES)
                .unwrap(),
            "0",
        );
    }
}

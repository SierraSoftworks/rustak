//! Enterprise sync: putting a file on the server, finding one, and fetching it
//! back.
//!
//! Two vocabularies live here, and both are TAK's. The modern search
//! (`/Marti/api/sync/search`) answers camel-case [`Resource`] objects; the
//! legacy upload servlet (`/Marti/sync/upload`) answers a Title-case
//! [`Uploaded`] object with a `Size` that is a string. They are not unified,
//! because the two are genuinely different shapes on the wire and a client that
//! hid that would be lying about what it sent.
//!
//! # Uploading
//!
//! The servlet accepts a raw body as well as a multipart part, and a raw body is
//! what a plugin almost always has — bytes it just produced. [`Files::upload`]
//! sends the bytes and puts everything else in the query string, which is the
//! shape ATAK itself uses for a data package.

use std::time::Duration;

use bytes::Bytes;
use rustak_core::prelude::*;

use super::client::MartiClient;

/// How long an upload or a download may take.
///
/// Longer than the default: a mission package is tens of megabytes and a field
/// link is not fast, and a sidecar that timed out half way would upload it again
/// from the beginning.
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(300);

/// A file the server holds, as the modern API renders it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    /// How a client addresses it. Stable across re-uploads of the same file.
    #[serde(default)]
    pub uid: String,

    /// The SHA-256 of the bytes, which is the other way to address it.
    #[serde(default)]
    pub hash: String,

    #[serde(default)]
    pub name: String,

    #[serde(default)]
    pub filename: Option<String>,

    #[serde(default)]
    pub mime_type: String,

    /// A number here, and a string in [`Uploaded`]. TAK's, not ours.
    #[serde(default)]
    pub size: i64,

    #[serde(default)]
    pub tool: String,

    #[serde(default)]
    pub keywords: Vec<String>,

    #[serde(default)]
    pub submitter: Option<String>,

    #[serde(default)]
    pub creator_uid: Option<String>,

    #[serde(default)]
    pub submission_time: Option<String>,

    #[serde(default)]
    pub groups: Vec<String>,

    #[serde(default)]
    pub latitude: Option<f64>,

    #[serde(default)]
    pub longitude: Option<f64>,
}

/// What the legacy upload servlet answers.
///
/// Title-case keys, and `Size` and `PrimaryKey` are strings. Only the fields a
/// plugin acts on are named; the rest of the object is ignored.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Uploaded {
    #[serde(rename = "UID", default)]
    pub uid: String,

    #[serde(rename = "Hash", default)]
    pub hash: String,

    #[serde(rename = "Name", default)]
    pub name: String,

    #[serde(rename = "PrimaryKey", default)]
    pub primary_key: String,

    #[serde(rename = "MIMEType", default)]
    pub mime_type: String,
}

/// What to upload, and what to say about it.
#[derive(Debug, Clone, Default)]
pub struct Upload {
    /// The filename the server files it under, and what a download is named.
    pub filename: String,

    /// What to call it. Defaults to the filename.
    pub name: Option<String>,

    pub mime_type: Option<String>,

    /// The device this file is attributed to.
    pub creator_uid: Option<String>,

    pub keywords: Vec<String>,

    /// The channels that may see it. Empty means the caller's own.
    pub groups: Vec<String>,
}

impl Upload {
    /// An upload of a named file, with nothing else said.
    pub fn named(filename: impl Into<String>) -> Self {
        Self {
            filename: filename.into(),
            ..Self::default()
        }
    }

    /// The query parameters this becomes.
    fn query(&self) -> Vec<(&'static str, String)> {
        let mut query = vec![("filename", self.filename.clone())];

        for (key, value) in [
            ("name", &self.name),
            ("mimetype", &self.mime_type),
            ("creatorUid", &self.creator_uid),
        ] {
            if let Some(value) = value {
                query.push((key, value.clone()));
            }
        }

        query.extend(self.keywords.iter().map(|k| ("keywords", k.clone())));
        query.extend(self.groups.iter().map(|g| ("groups", g.clone())));

        query
    }
}

/// What to search for.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    pub name: Option<String>,
    pub keywords: Vec<String>,
    pub tool: Option<String>,
    pub mission: Option<String>,
}

impl SearchQuery {
    /// Everything filed by one tool.
    pub fn tool(tool: impl Into<String>) -> Self {
        Self {
            tool: Some(tool.into()),
            ..Self::default()
        }
    }

    /// The query parameters this becomes.
    fn query(&self) -> Vec<(&'static str, String)> {
        let mut query = Vec::new();

        for (key, value) in [
            ("name", &self.name),
            ("tool", &self.tool),
            ("mission", &self.mission),
        ] {
            if let Some(value) = value {
                query.push((key, value.clone()));
            }
        }

        query.extend(self.keywords.iter().map(|k| ("keyword", k.clone())));

        query
    }
}

/// File calls, borrowed from a [`MartiClient`].
#[derive(Debug, Clone)]
pub struct Files<'a> {
    client: &'a MartiClient,
}

impl<'a> Files<'a> {
    pub(super) fn new(client: &'a MartiClient) -> Self {
        Self { client }
    }

    /// Puts bytes on the server and answers what it filed.
    ///
    /// Re-uploading identical bytes under the same filename is not an error: the
    /// server answers the row it already holds, which is what makes a sidecar
    /// that republishes on every restart harmless.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the upload exceeds the server's
    /// size limit, names a channel this identity is not in, or the server cannot
    /// be reached.
    pub async fn upload(&self, upload: &Upload, body: impl Into<Bytes>) -> Result<Uploaded, Error> {
        let request = self
            .client
            .request(reqwest::Method::POST, "/Marti/sync/upload")
            .query(&upload.query())
            .body(body.into());

        self.client
            .bare(
                MartiClient::slowly(request, TRANSFER_TIMEOUT),
                &format!("upload '{}'", upload.filename),
            )
            .await
    }

    /// Finds the files matching a query.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the server refuses the query or
    /// cannot be reached.
    pub async fn search(&self, query: &SearchQuery) -> Result<Vec<Resource>, Error> {
        let request = self
            .client
            .get("/Marti/api/sync/search")
            .query(&query.query());

        Ok(self
            .client
            .enveloped::<Vec<Resource>>(request, "search for files")
            .await?
            .unwrap_or_default())
    }

    /// Fetches a file's bytes by content hash.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the server does not hold it, or
    /// holds it in a channel this identity cannot see — the two are deliberately
    /// the same answer on the wire.
    pub async fn download(&self, hash: &str) -> Result<Bytes, Error> {
        let request = self
            .client
            .get("/Marti/sync/content")
            .query(&[("hash", hash)]);

        self.client
            .bytes(
                MartiClient::slowly(request, TRANSFER_TIMEOUT),
                &format!("download the file '{hash}'"),
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{body_bytes, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    async fn client(server: &MockServer) -> MartiClient {
        MartiClient::with_http(&server.uri(), reqwest::Client::new()).unwrap()
    }

    #[tokio::test]
    async fn an_upload_sends_the_bytes_and_says_everything_else_in_the_query() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/Marti/sync/upload"))
            .and(query_param("filename", "weather.xml"))
            .and(query_param("creatorUid", "SERVICE-weather"))
            .and(body_bytes(b"<events/>".as_slice()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "UID": "res-1",
                "Hash": "abc",
                "Name": "weather.xml",
                "PrimaryKey": "7",
                "Size": "9",
            })))
            .mount(&server)
            .await;

        let uploaded = client(&server)
            .await
            .files()
            .upload(
                &Upload {
                    creator_uid: Some("SERVICE-weather".into()),
                    ..Upload::named("weather.xml")
                },
                Bytes::from_static(b"<events/>"),
            )
            .await
            .unwrap();

        assert_eq!(uploaded.uid, "res-1");
        assert_eq!(uploaded.hash, "abc");
        assert_eq!(
            uploaded.primary_key, "7",
            "a string on the wire, and left one",
        );
    }

    #[tokio::test]
    async fn a_search_unwraps_the_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Marti/api/sync/search"))
            .and(query_param("tool", "weather"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "version": "3",
                "type": "Resource",
                "data": [{ "uid": "res-1", "hash": "abc", "name": "weather.xml", "size": 9 }],
                "nodeId": "rustak-test",
            })))
            .mount(&server)
            .await;

        let found = client(&server)
            .await
            .files()
            .search(&SearchQuery::tool("weather"))
            .await
            .unwrap();

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].size, 9, "a number here, unlike the upload answer");
    }

    #[tokio::test]
    async fn a_download_answers_the_bytes_and_a_miss_answers_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Marti/sync/content"))
            .and(query_param("hash", "abc"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"<events/>".as_slice()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/Marti/sync/content"))
            .and(query_param("hash", "missing"))
            .respond_with(ResponseTemplate::new(404).set_body_string("<html>Not Found</html>"))
            .mount(&server)
            .await;
        let client = client(&server).await;

        assert_eq!(
            client.files().download("abc").await.unwrap(),
            Bytes::from_static(b"<events/>")
        );
        assert!(client.files().download("missing").await.is_err());
    }
}

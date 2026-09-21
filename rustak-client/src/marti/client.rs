//! [`MartiClient`]: the connection, and the request plumbing every area shares.
//!
//! One client per server, cheap to clone, holding the `reqwest` client (and
//! therefore the connection pool and the TLS session cache) and the base URL.
//! The four area handles — [`missions`](MartiClient::missions),
//! [`files`](MartiClient::files), [`groups`](MartiClient::groups),
//! [`contacts`](MartiClient::contacts) — borrow it rather than copying it, so a
//! plugin keeps one client and reaches whatever it needs from it.

use std::time::Duration;

use reqwest::{RequestBuilder, Response};
use rustak_core::prelude::*;
use rustak_core::service::ServiceIdentity;
use url::Url;

use crate::http;

use super::contacts::Contacts;
use super::files::Files;
use super::groups::Groups;
use super::missions::Missions;
use super::{API_VERSION, API_VERSION_VALUE, Envelope, refused};

/// A typed client for one server's Marti API.
#[derive(Clone, Debug)]
pub struct MartiClient {
    http: reqwest::Client,
    base: Url,
}

impl MartiClient {
    /// Builds a client for `base`, authenticating with `identity`.
    ///
    /// `base` is `[server] marti`, e.g. `https://tak.example.com:8443`.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the URL is not one we can call,
    /// or when the certificate, key or truststore the identity names cannot be
    /// read — see [`http::client`].
    pub fn new(base: &str, identity: &ServiceIdentity) -> Result<Self, Error> {
        // [`http::Trust::Internal`]: the Marti mTLS listener always presents a
        // certificate from the deployment's own CA, so `[service] truststore`
        // replaces the platform's roots rather than joining them.
        Self::with_http(
            base,
            http::client(identity, http::Trust::Internal, http::DEFAULT_TIMEOUT)?,
        )
    }

    /// Builds a client over an HTTP client somebody else made.
    ///
    /// This is how a sidecar shares one connection pool between the Marti API
    /// and the control API, and how a test points the client at `wiremock`.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when `base` is not an http or https
    /// URL.
    pub fn with_http(base: &str, http: reqwest::Client) -> Result<Self, Error> {
        Ok(Self {
            http,
            base: http::base_url(base, "marti")?,
        })
    }

    /// The server this client calls.
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Missions: create, read, subscribe, file content, read changes and logs.
    pub fn missions(&self) -> Missions<'_> {
        Missions::new(self)
    }

    /// Enterprise sync: upload, search and download.
    pub fn files(&self) -> Files<'_> {
        Files::new(self)
    }

    /// Channels, as this identity holds them.
    pub fn groups(&self) -> Groups<'_> {
        Groups::new(self)
    }

    /// Who is connected, and where.
    pub fn contacts(&self) -> Contacts<'_> {
        Contacts::new(self)
    }

    /// What the server calls itself, which is the cheapest reachability check
    /// there is.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the server cannot be reached or
    /// refuses the request.
    pub async fn version(&self) -> Result<String, Error> {
        let request = self.get("/Marti/api/version");

        Ok(self
            .text(request, "read the server's version")
            .await?
            .trim()
            .to_string())
    }

    /// A `GET` against a path on this server, with the version header set.
    pub(crate) fn get(&self, path: &str) -> RequestBuilder {
        self.request(reqwest::Method::GET, path)
    }

    /// A request against a path on this server, with the version header set.
    ///
    /// The path is appended to the base URL rather than joined onto it, so a
    /// server behind a reverse proxy with a prefix keeps it — see
    /// [`http::endpoint`]. A path that will not parse is left to fail at
    /// [`send`](Self::send) rather than making every call site handle a
    /// `Result` for something it wrote as a literal.
    pub(crate) fn request(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        let url = http::endpoint(&self.base, path)
            .map(String::from)
            .unwrap_or_else(|_| path.to_string());

        self.http
            .request(method, url)
            .header(API_VERSION, API_VERSION_VALUE)
    }

    /// Sends a request and refuses anything but a success status.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error naming what the server said — see
    /// [`refused`].
    pub(crate) async fn send(
        &self,
        request: RequestBuilder,
        what: &str,
    ) -> Result<Response, Error> {
        let response = request
            .send()
            .await
            .map_err(|err| http::transport(err, what))?;

        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }

        let body = response.text().await.unwrap_or_default();

        Err(refused(status, &body, what))
    }

    /// Sends a request and reads the `data` out of the envelope it answers.
    ///
    /// [`None`] is the server saying there is nothing, which several endpoints
    /// mean rather than an empty payload.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the request is refused or the
    /// body is not the envelope it should be.
    pub(crate) async fn enveloped<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        what: &str,
    ) -> Result<Option<T>, Error> {
        let body = self.text(request, what).await?;
        let envelope: Envelope<T> =
            serde_json::from_str(&body).map_err(|err| unreadable(what, err))?;

        Ok(envelope.data)
    }

    /// Sends a request and parses a body that carries no envelope.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the request is refused or the
    /// body will not parse.
    pub(crate) async fn bare<T: DeserializeOwned>(
        &self,
        request: RequestBuilder,
        what: &str,
    ) -> Result<T, Error> {
        let body = self.text(request, what).await?;

        serde_json::from_str(&body).map_err(|err| unreadable(what, err))
    }

    /// Sends a request and reads its body as text.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the request is refused or the
    /// body cannot be read.
    pub(crate) async fn text(&self, request: RequestBuilder, what: &str) -> Result<String, Error> {
        self.send(request, what)
            .await?
            .text()
            .await
            .map_err(|err| http::transport(err, what))
    }

    /// Sends a request and reads its body as bytes, for a download.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the request is refused or the
    /// body cannot be read.
    pub(crate) async fn bytes(
        &self,
        request: RequestBuilder,
        what: &str,
    ) -> Result<bytes::Bytes, Error> {
        self.send(request, what)
            .await?
            .bytes()
            .await
            .map_err(|err| http::transport(err, what))
    }

    /// A request timeout for one call, for the ones that are not like the
    /// others — a mission archive, a large package.
    pub(crate) fn slowly(request: RequestBuilder, timeout: Duration) -> RequestBuilder {
        request.timeout(timeout)
    }
}

/// A body that was not the shape this endpoint promises.
///
/// A `Kind::User` error rather than a system one: the likeliest cause by far is
/// a sidecar pointed at something that is not a rustak server — a reverse proxy
/// error page, a captive portal, the wrong port.
fn unreadable(what: &str, err: serde_json::Error) -> Error {
    human_errors::user(
        format!("Could not {what}: the server's answer was not the shape we expected ({err})."),
        &[
            "Check that [server] marti names the TAK API and not the admin UI or a proxy.",
            "The Marti API is usually on port 8443.",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_core::identity::ServiceName;

    fn client(base: &str) -> Result<MartiClient, Error> {
        MartiClient::with_http(base, reqwest::Client::new())
    }

    #[test]
    fn a_client_is_built_from_an_identity_that_has_no_certificate_yet() {
        // Not useful for calling Marti — which needs one — but it must not be a
        // failure at construction, because a sidecar builds its clients before
        // it enrols.
        let identity = ServiceIdentity::new(ServiceName::parse("weather").unwrap());

        assert!(MartiClient::new("https://tak.example.com:8443", &identity).is_ok());
    }

    #[test]
    fn a_base_url_with_a_trailing_slash_is_the_same_client() {
        let plain = client("https://tak.example.com:8443").unwrap();
        let slashed = client("https://tak.example.com:8443/").unwrap();

        assert_eq!(plain.base().as_str(), slashed.base().as_str());
    }

    #[test]
    fn a_base_that_is_not_a_url_is_the_operators_to_fix() {
        let err = client("tak.example.com").unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
    }

    #[tokio::test]
    async fn a_refusal_names_what_was_being_done() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(403).set_body_string(
                    r#"{"status":"FORBIDDEN","code":10,"message":": not a member"}"#,
                ),
            )
            .mount(&server)
            .await;
        let client = client(&server.uri()).unwrap();

        let err = client.version().await.unwrap_err();

        assert!(err.description().contains("not a member"), "{err}");
        assert!(err.description().contains("version"), "{err}");
    }

    #[tokio::test]
    async fn an_answer_that_is_not_ours_says_so_rather_than_panicking() {
        // A sidecar pointed at the admin UI instead of the Marti API is the
        // common version of this, and "expected value at line 1" is not a
        // sentence anybody can act on.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_string("<html>hello</html>"),
            )
            .mount(&server)
            .await;
        let client = client(&server.uri()).unwrap();

        let err = client.missions().list(None).await.unwrap_err();

        assert!(err.description().contains("not the shape"), "{err}");
    }
}

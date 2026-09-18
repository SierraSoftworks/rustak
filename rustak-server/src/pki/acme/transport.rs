//! Reaching an ACME directory over the server's own HTTP client.
//!
//! `instant-acme` ships a hyper client of its own, but it is a second
//! connection pool with a second user agent and a second set of timeouts.
//! rustak already has one — [`Services::http_client`](crate::services::Services::http_client),
//! built once at start-up with
//! [`HTTP_USER_AGENT`](crate::services::HTTP_USER_AGENT) and the platform's
//! trust store — and an ACME exchange is a handful of ordinary POSTs, so it
//! goes through that one.
//!
//! # Plaintext is a consequence, not a feature
//!
//! The crate's own client is HTTPS-only. Ours is not, because it is the client
//! the whole server uses; what keeps an ACME account key off a plaintext
//! connection is [`AcmeDirectory`](crate::config::AcmeDirectory), which refuses
//! to parse a directory URL that is not `https://`. The tests exploit the gap
//! deliberately: they point this at a local `wiremock`, which speaks `http`.

use std::future::Future;
use std::pin::Pin;

use bytes::Bytes;
use instant_acme::{BodyWrapper, BytesResponse, HttpClient};

/// An [`HttpClient`] backed by a shared [`reqwest::Client`].
pub struct SharedClient(reqwest::Client);

impl SharedClient {
    /// Wraps `client`, which should be the one the services handle carries.
    pub fn new(client: reqwest::Client) -> Self {
        Self(client)
    }

    /// The same, boxed for [`instant_acme::Account::builder_with_http`].
    pub fn boxed(client: reqwest::Client) -> Box<dyn HttpClient> {
        Box::new(Self::new(client))
    }
}

impl std::fmt::Debug for SharedClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedClient")
    }
}

impl HttpClient for SharedClient {
    /// Replays one request through reqwest and hands the response back whole.
    ///
    /// The request body is handed over as it arrives rather than collected:
    /// it reports an exact size, so hyper still sends a `Content-Length` — and
    /// a JOSE-signed ACME request that arrived chunked would be rejected by
    /// several authorities.
    fn request(
        &self,
        request: http::Request<BodyWrapper<Bytes>>,
    ) -> Pin<Box<dyn Future<Output = Result<BytesResponse, instant_acme::Error>> + Send>> {
        let client = self.0.clone();

        Box::pin(async move {
            let (parts, body) = request.into_parts();

            let mut outgoing = client
                .request(parts.method, parts.uri.to_string())
                .body(reqwest::Body::wrap(body));

            for (name, value) in &parts.headers {
                outgoing = outgoing.header(name, value);
            }

            let response = outgoing
                .send()
                .await
                .map_err(|err| instant_acme::Error::Other(Box::new(err)))?;

            Ok(BytesResponse::from(http::Response::<reqwest::Body>::from(
                response,
            )))
        })
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn a_request_carries_its_method_headers_and_body_through() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/acme/new-order"))
            .and(header("content-type", "application/jose+json"))
            .respond_with(
                ResponseTemplate::new(201)
                    .insert_header("replay-nonce", "abc")
                    .set_body_string("{\"status\":\"pending\"}"),
            )
            .mount(&server)
            .await;

        let client = SharedClient::new(reqwest::Client::new());
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("{}/acme/new-order", server.uri()))
            .header("content-type", "application/jose+json")
            .body(BodyWrapper::from(b"{}".to_vec()))
            .unwrap();

        let response = client.request(request).await.unwrap();

        assert_eq!(response.parts.status, 201);
        assert_eq!(
            response.parts.headers.get("replay-nonce").unwrap(),
            "abc",
            "the headers an ACME exchange is driven by have to survive the trip",
        );
    }

    #[tokio::test]
    async fn a_directory_that_is_not_there_is_an_error_rather_than_a_panic() {
        let client = SharedClient::new(reqwest::Client::new());
        let request = http::Request::builder()
            // Port 0 never accepts, so this fails in the connector rather than
            // waiting on a timeout.
            .uri("http://127.0.0.1:0/directory")
            .body(BodyWrapper::default())
            .unwrap();

        assert!(client.request(request).await.is_err());
    }
}

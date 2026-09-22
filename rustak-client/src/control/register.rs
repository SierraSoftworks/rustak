//! Registration, heartbeats, and the configuration an administrator set.
//!
//! Three calls, and a loop the harness runs for a plugin that does not want to
//! write one:
//!
//! | Call | When a sidecar makes it |
//! |---|---|
//! | [`register`](ControlClient::register) | Once at start-up, and again whenever a heartbeat reports the registration has gone |
//! | [`heartbeat`](ControlClient::heartbeat) | On every tick |
//! | [`config`](ControlClient::config) | At start-up, and on any tick that wants to notice a change |
//!
//! # Re-registering is normal
//!
//! Registration is an upsert keyed on the service's name, so a sidecar that
//! restarts registers again rather than failing; its configuration and its last
//! known health survive. An administrator who removes a registration while the
//! sidecar is running makes its next heartbeat a `404`, which
//! [`heartbeat`](ControlClient::heartbeat) reports as [`None`] — the harness
//! registers again rather than exiting.

use rustak_api::{Heartbeat, ServiceDescriptor, ServiceStatus, ServiceSummary};
use rustak_core::prelude::*;

use super::ControlClient;

impl ControlClient {
    /// Registers this sidecar, or refreshes a registration it already holds.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the credential is refused
    /// (`401`), the caller is not a service account (`403`), the name belongs to
    /// another account (`409`), or the server cannot be reached.
    pub async fn register(&self, descriptor: &ServiceDescriptor) -> Result<ServiceSummary, Error> {
        let request = self
            .request(reqwest::Method::POST, "/services/register")
            .json(descriptor);
        let response = self
            .send(request, &format!("register the service '{}'", self.name()))
            .await?;

        parse(response, "register").await
    }

    /// Reports how this sidecar is doing.
    ///
    /// Answers [`None`] when the registration has gone, which is the server
    /// saying "register again" rather than a failure.
    ///
    /// Calling this marks the tick as *reported*, so that the harness does not
    /// send one of its own over the top of it — see
    /// [`Sidecar::health`](crate::sidecar::Sidecar::health), which is the
    /// preferred way for a plugin to say more than "healthy".
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the credential is refused, the
    /// service is not this caller's, or the server cannot be reached.
    pub async fn heartbeat(&self, beat: &Heartbeat) -> Result<Option<ServiceStatus>, Error> {
        // Before the request rather than after it: a plugin that has spoken is
        // one the harness must not talk over, whether or not the server took
        // what it said.
        self.mark_reported();

        self.post_heartbeat(beat).await
    }

    /// [`heartbeat`](Self::heartbeat) without claiming the tick, which is how
    /// the harness sends its own.
    ///
    /// # Errors
    ///
    /// As [`heartbeat`](Self::heartbeat).
    pub(crate) async fn post_heartbeat(
        &self,
        beat: &Heartbeat,
    ) -> Result<Option<ServiceStatus>, Error> {
        let request = self
            .request(
                reqwest::Method::POST,
                &format!("/services/{}/heartbeat", self.name()),
            )
            .json(beat);

        let what = format!("report the health of '{}'", self.name());
        let response = self.raw(request, &what).await?;

        self.note(response.status());

        // The one status this call reads as an answer rather than a failure: an
        // administrator removed the registration while the sidecar was running.
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        parse(super::succeeded(response, &what).await?, "heartbeat")
            .await
            .map(Some)
    }

    /// The configuration an administrator set for this service.
    ///
    /// Always a JSON object; `{}` for a service nobody has configured. A plugin
    /// deserialises it into whatever type it expects and treats a shape it does
    /// not recognise as a configuration error rather than a crash.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the credential is refused, the
    /// service is not registered, or the server cannot be reached.
    pub async fn config(&self) -> Result<serde_json::Value, Error> {
        let request = self.request(
            reqwest::Method::GET,
            &format!("/services/{}/config", self.name()),
        );
        let response = self
            .send(
                request,
                &format!("read the configuration of '{}'", self.name()),
            )
            .await?;

        parse(response, "configuration").await
    }

    /// The configuration, parsed into the plugin's own type.
    ///
    /// # Errors
    ///
    /// Everything [`config`](Self::config) returns, plus a
    /// [`human_errors::Kind::User`] error when the document is not the shape the
    /// plugin expects — which is an administrator's typo, not a bug.
    pub async fn config_as<T: DeserializeOwned>(&self) -> Result<T, Error> {
        let document = self.config().await?;

        serde_json::from_value(document).map_err(|err| {
            human_errors::user(
                format!(
                    "The configuration set for '{}' is not the shape this plugin expects ({err}).",
                    self.name()
                ),
                &[
                    "Check the service's configuration in the admin UI.",
                    "Every key is the plugin's own; nothing here is set by the server.",
                ],
            )
        })
    }

    /// Removes this sidecar's registration.
    ///
    /// Not called by the harness: a sidecar that stops is expected to come back,
    /// and a registration that disappeared on every restart would take its
    /// configuration with it. This is for a plugin that is being retired.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the credential is refused, the
    /// service is not this caller's, or it is not registered.
    pub async fn deregister(&self) -> Result<(), Error> {
        let request = self.request(
            reqwest::Method::DELETE,
            &format!("/services/{}", self.name()),
        );

        self.send(request, &format!("remove the service '{}'", self.name()))
            .await
            .map(drop)
    }
}

/// Reads a JSON body, naming what it was meant to be.
pub(super) async fn parse<T: DeserializeOwned>(
    response: reqwest::Response,
    what: &str,
) -> Result<T, Error> {
    let body = response
        .text()
        .await
        .map_err(|err| crate::http::transport(err, &format!("read the {what} response")))?;

    serde_json::from_str(&body).map_err(|err| {
        human_errors::user(
            format!("The server's {what} answer was not the shape we expected ({err})."),
            &[
                "Check that [server] control names a rustak server's admin API.",
                "It is usually the same host as the Marti API, on port 8446.",
            ],
        )
    })
}

#[cfg(test)]
mod tests {
    use rustak_api::{ServiceName, ServiceState};
    use rustak_core::service::ServiceIdentity;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn identity() -> ServiceIdentity {
        ServiceIdentity::new(ServiceName::parse("weather").unwrap())
            .with_credential(Secret::new("rsk_secret"))
    }

    fn client(server: &MockServer) -> ControlClient {
        ControlClient::with_http(&server.uri(), reqwest::Client::new(), &identity()).unwrap()
    }

    fn descriptor() -> ServiceDescriptor {
        ServiceDescriptor {
            config_schema: None,
            version: Some("1.2.3".into()),
            ..ServiceDescriptor::new(ServiceName::parse("weather").unwrap())
        }
    }

    #[tokio::test]
    async fn registering_sends_the_descriptor_under_the_service_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/services/register"))
            .and(header("authorization", "Bearer rsk_secret"))
            .and(body_json(descriptor()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": 1,
                "name": "weather",
                "version": "1.2.3",
                "endpoints": {},
                "status": { "state": "unknown" },
                "registered_at": "2026-09-18T12:00:00.000Z",
            })))
            .mount(&server)
            .await;

        let summary = client(&server).register(&descriptor()).await.unwrap();

        assert_eq!(summary.descriptor.name.as_str(), "weather");
        assert_eq!(summary.status.state, ServiceState::Unknown);
    }

    #[tokio::test]
    async fn a_heartbeat_for_a_registration_that_has_gone_answers_none() {
        // The one refusal this client reads as an answer: an administrator
        // removed the registration while the sidecar was running, and the
        // harness registers again rather than exiting.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/services/weather/heartbeat"))
            .respond_with(ResponseTemplate::new(404).set_body_json(serde_json::json!({
                "error": "That service is no longer registered. Register it again.",
            })))
            .mount(&server)
            .await;

        let reported = client(&server)
            .heartbeat(&Heartbeat::healthy())
            .await
            .unwrap();

        assert!(reported.is_none());
    }

    #[tokio::test]
    async fn a_heartbeat_answers_what_the_server_now_believes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/services/weather/heartbeat"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "state": "degraded",
                "message": "Upstream is slow.",
                "last_heartbeat_at": "2026-09-18T12:00:00.000Z",
            })))
            .mount(&server)
            .await;

        let status = client(&server)
            .heartbeat(&Heartbeat {
                state: ServiceState::Degraded,
                message: Some("Upstream is slow.".into()),
                metrics: serde_json::json!({ "queue": 4 }),
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(status.state, ServiceState::Degraded);
    }

    #[tokio::test]
    async fn a_configuration_can_be_read_as_the_plugins_own_type() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct Settings {
            interval: u32,
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/services/weather/config"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "interval": 60 })),
            )
            .mount(&server)
            .await;

        let settings: Settings = client(&server).config_as().await.unwrap();

        assert_eq!(settings, Settings { interval: 60 });
    }

    #[tokio::test]
    async fn a_configuration_the_plugin_cannot_read_is_the_administrators_typo() {
        #[derive(Debug, Deserialize)]
        struct Settings {
            #[allow(dead_code)]
            interval: u32,
        }

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "intrval": 60 })),
            )
            .mount(&server)
            .await;

        let err = client(&server).config_as::<Settings>().await.unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(err.description().contains("not the shape"), "{err}");
    }
}

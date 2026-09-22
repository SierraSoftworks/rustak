//! The service's half of a configuration validation.
//!
//! The server cannot call a sidecar, so it asks over the feed the sidecar holds
//! open: a `service.config.validate` event carries an id, and these two calls
//! are what the sidecar does with it — read the candidate back, and answer. The
//! harness makes both for a plugin that implements
//! [`Sidecar::validate_config`](crate::sidecar::Sidecar::validate_config); they
//! are public for a sidecar that runs its own loop. See
//! `rustak_api::service_config` for the whole exchange.
//!
//! # A `404` is "too late", not a failure
//!
//! The administrator's request waits about ten seconds. An id nobody is waiting
//! on any more — or one replayed from the feed's ring after a reconnection — is
//! the ordinary course of things, and is answered as [`None`] and `false`.

use rustak_api::{ConfigValidation, ConfigValidationRequest};
use rustak_core::prelude::*;

use super::{ControlClient, register::parse, succeeded};

impl ControlClient {
    /// The candidate this service was asked about under `id`.
    ///
    /// It may hold a secret an administrator has just typed, and is not yet —
    /// and may never be — this service's configuration. Do not log it, and do
    /// not apply it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the credential is refused or
    /// the server cannot be reached.
    pub async fn validation_request(
        &self,
        id: uuid::Uuid,
    ) -> Result<Option<ConfigValidationRequest>, Error> {
        let what = "read a configuration to validate";
        let response = self
            .raw(self.request(reqwest::Method::GET, &self.path(id)), what)
            .await?;

        self.note(response.status());

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        parse(succeeded(response, what).await?, "validation request")
            .await
            .map(Some)
    }

    /// Answers the validation `id`, saying whether anybody was still waiting.
    ///
    /// # Errors
    ///
    /// As [`validation_request`](Self::validation_request).
    pub async fn answer_validation(
        &self,
        id: uuid::Uuid,
        validation: &ConfigValidation,
    ) -> Result<bool, Error> {
        let what = "answer a configuration validation";
        let request = self
            .request(reqwest::Method::POST, &self.path(id))
            .json(validation);
        let response = self.raw(request, what).await?;

        self.note(response.status());

        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }

        succeeded(response, what).await.map(|_| true)
    }

    fn path(&self, id: uuid::Uuid) -> String {
        format!("/services/{}/config/validations/{id}", self.name())
    }
}

#[cfg(test)]
mod tests {
    use rustak_api::ConfigIssue;
    use rustak_core::service::ServiceIdentity;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const ID: &str = "8a6e0804-2bd0-4672-b79d-d97027f9071a";
    const PATH: &str =
        "/api/v1/services/weather/config/validations/8a6e0804-2bd0-4672-b79d-d97027f9071a";

    fn client(server: &MockServer) -> ControlClient {
        let identity = ServiceIdentity::new(ServiceName::parse("weather").unwrap());

        ControlClient::with_http(&server.uri(), reqwest::Client::new(), &identity).unwrap()
    }

    #[tokio::test]
    async fn a_candidate_is_read_back_and_answered_under_the_same_id() {
        let server = MockServer::start().await;
        let refusal = ConfigValidation::from(ConfigIssue::at("/api_key", "Refused upstream."));
        Mock::given(method("GET"))
            .and(path(PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "id": ID, "config": { "api_key": "abc" } })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(PATH))
            .and(body_json(&refusal))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let control = client(&server);

        let asked = control
            .validation_request(ID.parse().unwrap())
            .await
            .unwrap()
            .expect("somebody is waiting");

        assert_eq!(asked.config, serde_json::json!({ "api_key": "abc" }));
        assert!(control.answer_validation(asked.id, &refusal).await.unwrap());
    }

    #[tokio::test]
    async fn a_question_nobody_is_waiting_on_any_more_is_not_a_failure() {
        let server = MockServer::start().await;
        Mock::given(path(PATH))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let control = client(&server);
        let id = ID.parse().unwrap();

        assert_eq!(control.validation_request(id).await.unwrap(), None);
        assert!(
            !control
                .answer_validation(id, &ConfigValidation::accepted())
                .await
                .unwrap()
        );
    }
}

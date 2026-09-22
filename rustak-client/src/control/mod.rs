//! The control API: registering a sidecar, reporting its health, reading the
//! configuration an administrator set for it, and consuming the server-event
//! feed.
//!
//! ```no_run
//! # async fn example(control: &rustak_client::control::ControlClient) -> Result<(), human_errors::Error> {
//! use rustak_api::Heartbeat;
//!
//! control.heartbeat(&Heartbeat::healthy()).await?;
//!
//! let settings = control.config().await?;
//! println!("{settings}");
//! # Ok(())
//! # }
//! ```
//!
//! # Two credentials, and which one is used
//!
//! A sidecar's *primary* identity is its client certificate, which is what the
//! CoT stream and the Marti API authenticate. The control API also accepts a
//! **service token**, because a plugin may need to reach it before it has a
//! certificate — to find out what it is meant to be doing, or to report that its
//! enrolment failed. [`ControlClient`] presents the token when one is
//! configured and lets the certificate speak for itself when one is not; the
//! server prefers the certificate when both arrive.
//!
//! # The bearer credential can be replaced while the client is running
//!
//! Under an orchestrator the token is not a fixed secret from a file: it is an
//! access token the harness bought with the workload identity the task already
//! holds, and it expires. [`ControlClient::set_credential`] is how the harness
//! puts a fresh one in place, and the harness finds out that the one in place
//! has stopped working from the `401` this client records — one shared slot, so
//! the clone the plugin calls through and the one the event feed holds are both
//! carrying the new token the moment it lands.
//!
//! # Nothing here stops a sidecar
//!
//! Every call answers a `Result`, and the harness treats a failed registration or
//! a heartbeat that did not land as something to log and carry on from. A control
//! API that is briefly unavailable is not a reason to stop publishing CoT — the
//! feed the plugin exists to serve is the part that matters, and the server
//! notices the missing heartbeats on its own.

mod events;
mod register;
mod validate;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use rustak_core::prelude::*;
use rustak_core::service::ServiceIdentity;
use url::Url;

use crate::http;

pub use events::{
    ChannelEvent, ClientEvent, EventStream, MissionEvent, PackageEvent, ServerEvent,
    ServerEventPayload, ServiceEvent,
};

/// The path prefix every control-API route sits under.
pub const CONTROL_ROOT: &str = "/api/v1";

/// A client for one server's control API.
#[derive(Clone, Debug)]
pub struct ControlClient {
    http: reqwest::Client,

    /// The client the server-event feed is read through, when one was supplied.
    ///
    /// A separate client because the feed is a body held open for hours and
    /// `http` carries [`http::DEFAULT_TIMEOUT`] — a *total* deadline, which cut
    /// every feed at thirty seconds. [`None`] falls back to `http`, which is
    /// what a test pointing this at a mock server wants.
    feed: Option<reqwest::Client>,

    base: Url,
    name: ServiceName,

    /// The bearer credential, replaceable while the client is in use.
    ///
    /// Behind an [`Arc`] because every clone of this client — the plugin's, the
    /// harness's, the event feed's — has to see the token the harness last
    /// exchanged for, not the one it was built with.
    token: Arc<RwLock<Option<Secret>>>,

    /// Whether the server has answered `401` since the harness last looked.
    ///
    /// Read once per tick. Waiting for the cached expiry instead would leave a
    /// sidecar unable to report for up to an hour after a signing key rotated.
    unauthorized: Arc<AtomicBool>,

    /// Whether [`heartbeat`](Self::heartbeat) has been called since the harness
    /// last looked.
    ///
    /// The server keeps the *last* heartbeat it was given, so a harness that
    /// always sent its own would overwrite a plugin's a moment after it landed.
    /// Behind an [`Arc`] because the clone the harness holds and the clone the
    /// plugin calls through are the same client; shared with
    /// `sidecar::ControlLink`, which reads and clears it once per tick.
    reported: Arc<AtomicBool>,
}

impl ControlClient {
    /// Builds a client for `base`, authenticating as `identity`.
    ///
    /// `base` is `[server] control`, e.g. `https://tak.example.com:8446`.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the URL is not one we can call,
    /// or when the certificate, key or truststore the identity names cannot be
    /// read.
    pub fn new(base: &str, identity: &ServiceIdentity) -> Result<Self, Error> {
        // [`http::Trust::Public`]: the control API is served by the public
        // listener, which may hold an ACME or operator-supplied certificate as
        // easily as one from the deployment's own CA.
        Ok(Self::with_http(
            base,
            http::client(identity, http::Trust::Public, http::DEFAULT_TIMEOUT)?,
            identity,
        )?
        .with_feed_http(http::feed_client(
            identity,
            http::Trust::Public,
            http::FEED_IDLE_TIMEOUT,
        )?))
    }

    /// Builds a client over an HTTP client somebody else made.
    ///
    /// This is how a sidecar shares one connection pool with the Marti client,
    /// and how a test points this one at `wiremock`.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when `base` is not an http or https
    /// URL.
    pub fn with_http(
        base: &str,
        http: reqwest::Client,
        identity: &ServiceIdentity,
    ) -> Result<Self, Error> {
        Ok(Self {
            http,
            feed: None,
            base: http::base_url(base, "control")?,
            name: identity.name().clone(),
            token: Arc::new(RwLock::new(identity.credential().cloned())),
            reported: Arc::new(AtomicBool::new(false)),
            unauthorized: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Reads the server-event feed through `http` rather than through the
    /// client the ordinary calls use.
    ///
    /// The feed's client carries no total timeout — see
    /// [`http::feed_client`] — and a sidecar that shared one client for both
    /// would have to choose between a heartbeat that can hang forever and a
    /// feed that is cut every thirty seconds.
    #[must_use]
    pub fn with_feed_http(mut self, http: reqwest::Client) -> Self {
        self.feed = Some(http);
        self
    }

    /// The client the feed is opened with: the long-lived one when there is
    /// one, and otherwise the ordinary one.
    fn feed_http(&self) -> &reqwest::Client {
        self.feed.as_ref().unwrap_or(&self.http)
    }

    /// The service this client speaks for.
    pub fn name(&self) -> &ServiceName {
        &self.name
    }

    /// Whether the plugin has reported a heartbeat of its own since this was
    /// last asked, clearing the flag as it answers.
    ///
    /// The harness asks once per tick, after
    /// [`Sidecar::health`](crate::sidecar::Sidecar::health), and stays quiet
    /// when the answer is `true`.
    pub(crate) fn take_reported(&self) -> bool {
        self.reported.swap(false, Ordering::Relaxed)
    }

    /// Records that the plugin has said something about its own health.
    fn mark_reported(&self) {
        self.reported.store(true, Ordering::Relaxed);
    }

    /// The server this client calls.
    pub fn base(&self) -> &Url {
        &self.base
    }

    /// Replaces the bearer credential every later request carries.
    ///
    /// Shared with every clone of this client, so a token the harness has just
    /// exchanged for reaches the event feed as well as the next heartbeat.
    pub fn set_credential(&self, token: Option<Secret>) {
        if let Ok(mut held) = self.token.write() {
            *held = token;
        }
    }

    /// Whether the server refused this client's credential since the harness
    /// last asked, clearing the flag as it answers.
    pub(crate) fn take_unauthorized(&self) -> bool {
        self.unauthorized.swap(false, Ordering::Relaxed)
    }

    /// Records a `401`, so the harness knows to exchange again.
    fn note(&self, status: reqwest::StatusCode) {
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.unauthorized.store(true, Ordering::Relaxed);
        }
    }

    /// The credential to present, if there is one.
    fn credential(&self) -> Option<Secret> {
        self.token.read().ok()?.clone()
    }

    /// A request against a control-API path, carrying the service token when one
    /// is configured.
    pub(crate) fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.request_on(&self.http, method, path)
    }

    /// [`request`](Self::request), over a client the caller names.
    ///
    /// The feed is the one caller that does not go through `self.http`.
    pub(crate) fn request_on(
        &self,
        http: &reqwest::Client,
        method: reqwest::Method,
        path: &str,
    ) -> reqwest::RequestBuilder {
        let url = http::endpoint(&self.base, &format!("{CONTROL_ROOT}{path}"))
            .map(String::from)
            .unwrap_or_else(|_| path.to_string());
        let request = http.request(method, url);

        match self.credential() {
            Some(token) => request.bearer_auth(token.expose()),
            None => request,
        }
    }

    /// Sends a request and refuses anything but a success status.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error carrying whatever the server said —
    /// the admin API's `{"error": …}` body, when it sent one.
    pub(crate) async fn send(
        &self,
        request: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<reqwest::Response, Error> {
        let response = self.raw(request, what).await?;

        self.note(response.status());

        succeeded(response, what).await
    }

    /// Sends a request and answers whatever came back, status and all.
    ///
    /// For the one caller that reads a status as an answer rather than a
    /// failure: a heartbeat's `404` means "register again".
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the server could not be
    /// reached at all.
    pub(crate) async fn raw(
        &self,
        request: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<reqwest::Response, Error> {
        request
            .send()
            .await
            .map_err(|err| http::transport(err, what))
    }
}

/// Refuses anything but a success status, carrying the server's own words.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error — see [`refused`].
pub(crate) async fn succeeded(
    response: reqwest::Response,
    what: &str,
) -> Result<reqwest::Response, Error> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response.text().await.unwrap_or_default();

    Err(refused(status, &body, what))
}

/// What the admin API answers a failure with.
#[derive(Debug, Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    error: String,
}

/// Turns a control-API refusal into something an operator can act on.
fn refused(status: reqwest::StatusCode, body: &str, what: &str) -> Error {
    let detail = serde_json::from_str::<ApiErrorBody>(body)
        .ok()
        .filter(|parsed| !parsed.error.trim().is_empty())
        .map(|parsed| parsed.error.trim().to_string())
        .unwrap_or_else(|| status.to_string());

    let advice: &[&str] = match status.as_u16() {
        401 => &[
            "Set [service] token to a service token minted for this service's account.",
            "Write it as \"${{ env.RUSTAK_SERVICE_TOKEN }}\" and supply it from the environment.",
        ],
        403 => &[
            "A service may only act on its own registration.",
            "Check that [service] name matches the account the token belongs to.",
        ],
        404 => &["Register the service before reporting on it."],
        409 => &["Another account already holds that service name. Choose another."],
        _ => &["The message above is the server's own."],
    };

    human_errors::user(
        format!(
            "Could not {what}: {detail}{}",
            crate::http::full_stop(&detail)
        ),
        advice,
    )
}

#[cfg(test)]
mod tests {
    use rustak_core::identity::ServiceName;

    use super::*;

    #[test]
    fn a_refusal_carries_the_servers_own_sentence_without_doubling_its_full_stop() {
        // The production finding, verbatim: "Could not read this service's
        // configuration: That credential is not one this server accepts for
        // the control API.." — the server's sentence already ended.
        let rendered = refused(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":"That credential is not one this server accepts for the control API."}"#,
            "read this service's configuration",
        );

        assert_eq!(
            rendered.description(),
            "Could not read this service's configuration: That credential is not one this server accepts for the control API.",
        );
    }

    #[test]
    fn a_refusal_whose_detail_is_not_a_sentence_still_ends_in_one() {
        let rendered = refused(
            reqwest::StatusCode::NOT_FOUND,
            r#"{"error":"No service named 'weather' is registered"}"#,
            "report a heartbeat",
        );

        assert!(
            rendered.description().ends_with("is registered."),
            "{}",
            rendered.description(),
        );

        // And a body that is not an error object at all falls back to the
        // status, which never ends in a full stop.
        let bare = refused(reqwest::StatusCode::BAD_GATEWAY, "<html>", "register");

        assert!(
            bare.description().ends_with("Bad Gateway."),
            "{}",
            bare.description()
        );
    }

    #[test]
    fn a_client_presents_the_token_when_one_is_configured() {
        let bare = ServiceIdentity::new(ServiceName::parse("weather").unwrap());
        let held = bare.clone().with_credential(Secret::new("rsk_secret"));

        let without = ControlClient::with_http(
            "https://tak.example.com:8446",
            reqwest::Client::new(),
            &bare,
        )
        .unwrap();
        let with = ControlClient::with_http(
            "https://tak.example.com:8446",
            reqwest::Client::new(),
            &held,
        )
        .unwrap();

        assert!(without.credential().is_none());
        assert!(with.credential().is_some());

        // And it can be replaced without rebuilding the client, which is what
        // an exchanged access token needs.
        without.set_credential(Some(Secret::new("rsk_exchanged")));
        assert_eq!(
            without.credential().map(|token| token.expose().to_string()),
            Some("rsk_exchanged".to_string()),
        );
        assert!(!without.take_unauthorized());
        assert!(
            !format!("{with:?}").contains("rsk_secret"),
            "a client is logged at start-up and must not carry its token into the log",
        );
    }

    #[test]
    fn a_refusal_says_what_to_do_about_it() {
        let unauthorised = refused(
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":"That credential is not one this server accepts."}"#,
            "register",
        );
        let conflict = refused(
            reqwest::StatusCode::CONFLICT,
            r#"{"error":"The service name 'weather' is registered to a different account."}"#,
            "register",
        );

        assert!(unauthorised.is(human_errors::Kind::User));
        assert!(
            unauthorised.description().contains("not one this server"),
            "{unauthorised}"
        );
        assert!(
            conflict.description().contains("different account"),
            "{conflict}"
        );
    }

    #[test]
    fn a_refusal_with_no_body_still_names_the_status() {
        let err = refused(reqwest::StatusCode::BAD_GATEWAY, "", "send a heartbeat");

        assert!(err.description().contains("502"), "{err}");
    }
}

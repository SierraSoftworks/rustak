//! The configuration an administrator set on the Services page, read again
//! whenever the control link is in a position to answer.
//!
//! # Why a plugin cannot just read it once
//!
//! Every feed plugin reads `GET /api/v1/services/<name>/config` at start-up to
//! find the area an administrator moved in the admin UI. A start-up is exactly
//! where that read is least likely to work: the harness has not bought its
//! access token yet, the server may still be coming up, and a sidecar that was
//! restarted after 45 minutes of refused exchanges starts inside a rate-limit
//! lockout it earned before it was born. The first live deployment hit all
//! three — one `Could not read this service's configuration.` at 07:02:59, and
//! an administrator's `area` override silently not in effect for the whole life
//! of the process.
//!
//! So the read is a thing that is *retried*, and this is where the retrying
//! lives so that every plugin does it the same way.
//!
//! # What it will not do
//!
//! It will not provoke a token exchange. A sidecar whose credential comes from
//! its orchestrator has [`AccessTokens`](super::AccessTokens) for that, with a
//! backoff and a log cadence of its own; a plugin asking for a token as well
//! would be a second exchange and a second refusal for one cause. This reads
//! the configuration when the harness is already holding a credential, and
//! waits for the next tick when it is not.

use chrono::{DateTime, Duration, Utc};
use rustak_core::prelude::*;

use super::SidecarContext;

/// How long after a successful read the server's copy is looked at again.
///
/// An administrator moving an area in the admin UI is rare and never urgent, so
/// this is a cadence rather than a subscription — and five minutes is the same
/// window everything else in the harness reminds on.
const RE_READ_EVERY: Duration = Duration::minutes(5);

/// How long after a failed read another attempt is worth making.
///
/// Short, because the failure this exists for is a start-up where the control
/// link is not up *yet*, and the sooner that is picked up the sooner an
/// administrator's setting is actually in effect.
const RETRY_AFTER: Duration = Duration::seconds(30);

/// This service's server-side configuration, kept up to date.
///
/// Held by the plugin across ticks; [`refresh`](Self::refresh) answers a
/// document only when there is a **new** one worth applying, so a plugin can
/// call it every tick and log only when something changed.
#[derive(Debug, Default)]
pub struct ServiceSettings {
    /// A fingerprint of the document last answered.
    applied: Option<u64>,

    /// Whether a read has ever succeeded, which is what makes a failure worth
    /// mentioning: one before the link is up is the ordinary course of a
    /// start-up.
    read: bool,

    /// Whether the current run of failures has been mentioned.
    complained: bool,

    /// The earliest instant another read is worth making.
    next_at: Option<DateTime<Utc>>,
}

impl ServiceSettings {
    /// Settings nothing has been read into yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the server's copy has ever been read.
    ///
    /// A plugin uses this to say where the configuration it is applying came
    /// from: the file it was started with, or the server.
    #[must_use]
    pub fn from_server(&self) -> bool {
        self.read
    }

    /// The server's copy, when there is a new one worth applying.
    ///
    /// [`None`] means "nothing to do": no control API, no credential yet, a
    /// read that failed, or the same document as last time.
    pub async fn refresh<S>(&mut self, context: &SidecarContext<S>) -> Option<serde_json::Value> {
        self.refresh_at(context, Utc::now()).await
    }

    /// One setting of that document, when a new document carries a usable one.
    ///
    /// The shape every feed plugin wants: "has the administrator changed the
    /// area since I last looked". A value that is there but unusable is one
    /// `warn` and a [`None`] — a sidecar keeps running on what it has.
    pub async fn setting<S, T: DeserializeOwned>(
        &mut self,
        context: &SidecarContext<S>,
        key: &str,
    ) -> Option<T> {
        let value = self.refresh(context).await?.get(key)?.clone();

        match serde_json::from_value(value) {
            Ok(setting) => Some(setting),
            Err(err) => {
                tracing::warn!(
                    "The `{key}` this service is configured with is not one we can read ({err}); \
                     the one already in effect stays in effect.",
                );

                None
            }
        }
    }

    /// [`refresh`](Self::refresh), at an instant of the caller's choosing.
    ///
    /// The clock is an argument so that the cadence can be tested without
    /// waiting for it.
    pub async fn refresh_at<S>(
        &mut self,
        context: &SidecarContext<S>,
        now: DateTime<Utc>,
    ) -> Option<serde_json::Value> {
        if self.next_at.is_some_and(|at| now < at) {
            return None;
        }

        let control = context.control()?;

        // A credential the harness has already bought, never one of our own:
        // see the module documentation.
        if let Some(tokens) = context.workload_tokens() {
            let held = tokens.held()?;
            control.set_credential(Some(held));
        }

        let document = match control.config().await {
            Ok(document) => document,
            Err(err) => {
                self.complain(&err);
                self.next_at = Some(now + RETRY_AFTER);

                return None;
            }
        };

        self.read = true;
        self.complained = false;
        self.next_at = Some(now + RE_READ_EVERY);

        let mark = fingerprint(&document);

        if self.applied == Some(mark) {
            return None;
        }

        self.applied = Some(mark);

        Some(document)
    }

    /// Says a read failed, at the level its history calls for.
    ///
    /// Before the first success this is the ordinary course of a start-up and
    /// costs a `debug`; afterwards it is a change, and a change is worth one
    /// `warn`.
    fn complain(&mut self, err: &Error) {
        match (self.read, self.complained) {
            (true, false) => tracing::warn!(
                error = %err,
                "Could not read this service's configuration; the one already in effect stays in effect.",
            ),
            _ => tracing::debug!(
                error = %err.description(),
                "Could not read this service's configuration yet; it will be read again.",
            ),
        }

        self.complained = true;
    }
}

/// A fingerprint of a configuration document, for noticing that it changed.
fn fingerprint(document: &serde_json::Value) -> u64 {
    use std::hash::{Hash as _, Hasher as _};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // `serde_json` holds an object as a sorted map, so the rendering of one
    // document is the same every time.
    document.to_string().hash(&mut hasher);

    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sidecar::{NoSettings, SidecarConfig};

    /// The clock these tests move by hand, so that nothing here waits.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    /// A context against `uri`, with a `[service] token`: the credential the
    /// harness holds is not what these are about.
    fn context(uri: &str) -> SidecarContext<NoSettings> {
        let config: SidecarConfig<NoSettings> = rustak_core::config::load_str(&format!(
            r#"
            [service]
            name = "weather"
            token = "rsk_a_service_token"

            [server]
            control = "{uri}"
            "#
        ))
        .expect("the configuration loads");

        SidecarContext::from_config(config, "1.2.3", Shutdown::new()).expect("a context")
    }

    /// A control API answering `documents`, one per request, in order.
    async fn serving(documents: Vec<serde_json::Value>) -> wiremock::MockServer {
        let server = wiremock::MockServer::start().await;

        for (nth, document) in documents.into_iter().enumerate() {
            wiremock::Mock::given(wiremock::matchers::method("GET"))
                .and(wiremock::matchers::path("/api/v1/services/weather/config"))
                .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(document))
                .up_to_n_times(1)
                .with_priority(u8::try_from(nth + 1).expect("a handful of documents"))
                .mount(&server)
                .await;
        }

        server
    }

    #[tokio::test]
    async fn a_document_is_answered_once_and_not_again_until_it_changes() {
        let server = serving(vec![
            serde_json::json!({ "area": { "kind": "circle", "lat": 1.0 } }),
            serde_json::json!({ "area": { "kind": "circle", "lat": 1.0 } }),
            serde_json::json!({ "area": { "kind": "circle", "lat": 2.0 } }),
        ])
        .await;
        let context = context(&server.uri());
        let mut settings = ServiceSettings::new();

        assert!(settings.refresh_at(&context, at(0)).await.is_some());
        assert!(settings.from_server());
        assert_eq!(
            settings.refresh_at(&context, at(299)).await,
            None,
            "a read that succeeded is not repeated every tick",
        );
        assert_eq!(
            settings.refresh_at(&context, at(300)).await,
            None,
            "the same document is not applied — or logged — twice",
        );
        assert_eq!(
            settings.refresh_at(&context, at(600)).await,
            Some(serde_json::json!({ "area": { "kind": "circle", "lat": 2.0 } })),
            "and a changed one is",
        );
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            3,
            "one request per window, and none in between",
        );
    }

    #[tokio::test]
    async fn a_read_that_fails_is_tried_again_rather_than_giving_up_for_the_life_of_the_process() {
        // The production finding: the configuration read at start-up failed
        // because the token exchange had just been refused, and an
        // administrator's area override was silently not in effect until
        // somebody restarted the task.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(401))
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(
                wiremock::ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "area": { "kind": "circle" } })),
            )
            .with_priority(2)
            .mount(&server)
            .await;

        let context = context(&server.uri());
        let mut settings = ServiceSettings::new();

        assert_eq!(settings.refresh_at(&context, at(0)).await, None, "refused");
        assert!(!settings.from_server());
        assert_eq!(
            settings.refresh_at(&context, at(29)).await,
            None,
            "and not hammered in between",
        );
        assert_eq!(
            settings.refresh_at(&context, at(30)).await,
            Some(serde_json::json!({ "area": { "kind": "circle" } })),
            "the next attempt is where the link is up and the read works",
        );
        assert!(settings.from_server());
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_setting_that_is_not_the_shape_the_plugin_wants_leaves_it_alone() {
        let server = serving(vec![serde_json::json!({ "area": "the whole world" })]).await;
        let context = context(&server.uri());
        let mut settings = ServiceSettings::new();

        assert_eq!(
            settings
                .setting::<_, crate::feed::Area>(&context, "area")
                .await,
            None,
        );
    }

    #[tokio::test]
    async fn a_setting_the_administrator_did_set_is_answered_as_the_plugin_wants_it() {
        let server = serving(vec![serde_json::json!({
            "area": { "kind": "circle", "lat": 51.4775, "lon": -0.4614, "radius_km": 120.0 },
        })])
        .await;
        let context = context(&server.uri());
        let mut settings = ServiceSettings::new();

        assert_eq!(
            settings
                .setting::<_, crate::feed::Area>(&context, "area")
                .await,
            Some(crate::feed::Area::Circle {
                lat: 51.4775,
                lon: -0.4614,
                radius_km: 120.0,
            }),
        );
    }

    #[tokio::test]
    async fn a_sidecar_with_no_control_api_asks_for_nothing() {
        let config: SidecarConfig<NoSettings> = rustak_core::config::load_str(
            r#"
            [service]
            name = "weather"
            "#,
        )
        .expect("the configuration loads");
        let context =
            SidecarContext::from_config(config, "1.2.3", Shutdown::new()).expect("a context");

        assert_eq!(ServiceSettings::new().refresh(&context).await, None);
    }
}

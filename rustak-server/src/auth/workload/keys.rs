//! Where an orchestrator's signing keys come from, and how long we keep them.
//!
//! Three sources, because the three deployments that matter differ: a Nomad
//! cluster publishes a bare key set on its HTTP API, a Kubernetes API server
//! publishes a discovery document beside one, and an air-gapped installation
//! hands rustak a file. All three end as a [`jsonwebtoken::jwk::JwkSet`] and
//! nothing downstream knows which it was.
//!
//! # Cached, with a refetch on a key we do not know
//!
//! Fetching per request would put the orchestrator in the path of every
//! enrolment; never refetching would mean a key rotation locked every workload
//! out until `jwks_refresh` expired. So the set is cached for `jwks_refresh`
//! and a token naming a `kid` we do not hold refetches **on the spot** — at
//! most once a minute per issuer, because that refetch is something any caller
//! can provoke by sending us a token with a `kid` that will never exist.
//!
//! The maintainer's Nomad serves six keys and rotates them, which is exactly
//! the shape that argument is about: the rotation is picked up by the first
//! token signed with the new key rather than up to an hour later.

use std::time::Duration as StdDuration;

use chrono::{DateTime, Utc};
use jsonwebtoken::jwk::JwkSet;

use crate::config::WorkloadIssuer;
use crate::prelude::*;
use crate::services::http::{MAX_JSON_BYTES, body_within};

/// Where a cached key set lives.
const JWKS_PARTITION: &str = "workload:jwks";

/// Where a cached discovery document lives.
const DISCOVERY_PARTITION: &str = "workload:discovery";

/// Where the "when did we last refetch this" marker lives.
const REFRESH_PARTITION: &str = "workload:jwks-refresh";

/// How often an unknown `kid` may send us back to the orchestrator.
const REFRESH_EVERY: chrono::Duration = chrono::Duration::minutes(1);

/// How long a discovery document is kept. It changes about never, and the key
/// set it points at has its own, shorter life.
const DISCOVERY_TTL_HOURS: i64 = 24;

/// What to tell an operator whose orchestrator will not answer.
const ADVICE_UNREACHABLE: &[&str] = &[
    "Check that the issuer's `jwks_url` (or `discovery_url`) is reachable from this server.",
    "Nomad publishes its key set at /.well-known/jwks.json on the HTTP API, and needs no ACL token for it.",
    "Kubernetes publishes its at /openid/v1/jwks, readable anonymously only with a binding to the system:service-account-issuer-discovery ClusterRole.",
];

/// The part of a discovery document we use.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Discovery {
    /// Where the keys are published. Read from the document rather than
    /// guessed, because an API server is entitled to publish its own path.
    jwks_uri: String,
}

/// The issuer's key set, from the cache or from the issuer.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the orchestrator cannot be
/// reached, answers with something we cannot read, or names a file we cannot
/// open.
#[instrument("auth.workload.keys", skip_all, fields(issuer = %issuer.name), err(Display))]
pub async fn key_set<S: Services>(services: &S, issuer: &WorkloadIssuer) -> Result<JwkSet, Error> {
    if let Some(path) = &issuer.jwks_file {
        return from_file(path);
    }

    let url = jwks_url(services, issuer).await?;

    services
        .cache()
        .cached(
            JWKS_PARTITION,
            url.clone(),
            {
                let http = services.http_client();
                move || Box::pin(async move { fetch(&http, &url, "signing keys").await })
            },
            issuer.jwks_refresh,
        )
        .await
}

/// The issuer's key set, having dropped whatever was cached first.
///
/// Answers [`None`] when the refetch was throttled, which the caller treats as
/// "the keys we have are the keys there are" and refuses the token.
///
/// # Errors
///
/// As [`key_set`].
#[instrument("auth.workload.keys.refresh", skip_all, fields(issuer = %issuer.name), err(Display))]
pub async fn refreshed<S: Services>(
    services: &S,
    issuer: &WorkloadIssuer,
) -> Result<Option<JwkSet>, Error> {
    // A file is re-read every time anyway, so there is nothing to throttle and
    // nothing new to learn from reading it twice in one request.
    if issuer.jwks_file.is_some() {
        return Ok(None);
    }

    if !may_refresh(services, &issuer.name).await? {
        debug!("A token named an unknown key and the refetch was throttled.");

        return Ok(None);
    }

    let url = jwks_url(services, issuer).await?;

    // The cache and the key/value store are the same table, so removing the
    // entry is what makes the call below rebuild it.
    services.kv().remove(JWKS_PARTITION, url.clone()).await?;

    info!("Refetching an orchestrator's key set: a token named a key we do not hold.");

    key_set(services, issuer).await.map(Some)
}

/// Where this issuer's keys are published, following a discovery document when
/// that is what was configured.
async fn jwks_url<S: Services>(services: &S, issuer: &WorkloadIssuer) -> Result<String, Error> {
    if let Some(url) = &issuer.jwks_url {
        return Ok(url.clone());
    }

    let Some(url) = issuer.discovery_url.clone() else {
        return Err(human_errors::system(
            format!(
                "The workload issuer `{}` has no key set to fetch, which `--check` should have refused.",
                issuer.name
            ),
            &["Please report this issue via GitHub."],
        ));
    };

    let document: Discovery = services
        .cache()
        .cached(
            DISCOVERY_PARTITION,
            url.clone(),
            {
                let http = services.http_client();
                move || Box::pin(async move { fetch(&http, &url, "discovery document").await })
            },
            chrono::Duration::hours(DISCOVERY_TTL_HOURS),
        )
        .await?;

    Ok(document.jwks_uri)
}

/// Whether this issuer's key set may be refetched again yet.
///
/// Recorded rather than counted, because the thing being limited is a request
/// to somebody else's control plane that any caller can provoke.
async fn may_refresh<S: Services>(services: &S, issuer: &str) -> Result<bool, Error> {
    let now = Utc::now();
    let last: Option<DateTime<Utc>> = services
        .kv()
        .get(REFRESH_PARTITION, issuer.to_owned())
        .await?;

    if last.is_some_and(|at| now < at + REFRESH_EVERY) {
        return Ok(false);
    }

    services
        .kv()
        .set(REFRESH_PARTITION, issuer.to_owned(), now)
        .await?;

    Ok(true)
}

/// A key set an installation keeps on disk.
fn from_file(path: &std::path::Path) -> Result<JwkSet, Error> {
    let body = std::fs::read(path).map_err(|err| {
        human_errors::user(
            format!("Could not read the workload key set at '{}': {err}.", path.display()),
            &[
                "Check that the file exists and that this process may read it.",
                "It holds the public half of an orchestrator's signing keys, as a JSON Web Key Set.",
            ],
        )
    })?;

    serde_json::from_slice(&body).map_err(|err| {
        human_errors::user(
            format!(
                "The workload key set at '{}' is not a JSON Web Key Set ({err}).",
                path.display()
            ),
            &["Save the orchestrator's /.well-known/jwks.json document there verbatim."],
        )
    })
}

/// One GET, capped and parsed, with the issuer named in every failure.
async fn fetch<T: DeserializeOwned>(
    http: &reqwest::Client,
    url: &str,
    what: &'static str,
) -> Result<T, Error> {
    let response = http
        .get(url)
        .send()
        .await
        .wrap_system_err(
            format!("We could not reach the orchestrator to fetch its {what}."),
            ADVICE_UNREACHABLE,
        )?
        .error_for_status()
        .wrap_system_err(
            format!("The orchestrator refused to hand over its {what}."),
            ADVICE_UNREACHABLE,
        )?;

    // Capped rather than `.json()`: an endpoint that streamed without end would
    // otherwise stream into this server's memory, and every enrolment waiting
    // on a refresh is behind this call.
    let body = body_within(response, MAX_JSON_BYTES)
        .await
        .wrap_system_err(
            format!("We could not read the orchestrator's {what}."),
            ADVICE_UNREACHABLE,
        )?;

    serde_json::from_slice(&body).wrap_system_err(
        format!("The orchestrator's {what} was not in a shape we could read."),
        ADVICE_UNREACHABLE,
    )
}

/// Logs the issuers that fetch their keys over plain HTTP, once per start-up.
///
/// `--check` refuses one that has not opted in; this is the other half of that
/// bargain — an installation which *has* opted in is reminded every time it
/// starts, because whoever can answer that request decides which signatures
/// this server accepts.
pub fn warn_about_insecure_issuers(workload: &crate::config::WorkloadConfig) {
    for issuer in &workload.issuers {
        if !issuer.allow_insecure_jwks {
            continue;
        }

        warn!(
            issuer = %issuer.name,
            url = %issuer.jwks_url.as_deref().or(issuer.discovery_url.as_deref()).unwrap_or_default(),
            "This installation fetches a workload issuer's signing keys over plain HTTP, because allow_insecure_jwks is set.",
        );
    }
}

/// How long a caller should wait before asking again, for the tests.
#[allow(dead_code)]
pub(super) const fn refresh_window() -> StdDuration {
    StdDuration::from_secs(60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorkloadIssuer;
    use crate::services::AppContext;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn issuer(name: &str) -> WorkloadIssuer {
        WorkloadIssuer {
            name: name.to_string(),
            issuer: None,
            jwks_url: None,
            discovery_url: None,
            jwks_file: None,
            audience: "rustak".to_string(),
            algorithms: Vec::new(),
            clock_skew: chrono::Duration::seconds(30),
            jwks_refresh: chrono::Duration::hours(1),
            allow_insecure_jwks: false,
        }
    }

    /// A key set with one (structurally valid, cryptographically irrelevant)
    /// key, so that a test can tell two fetches apart by their `kid`.
    fn key_set_body(kid: &str) -> serde_json::Value {
        serde_json::json!({
            "keys": [{
                "kty": "RSA",
                "use": "sig",
                "alg": "RS256",
                "kid": kid,
                "n": "sXchKj9Qm7qgTfxYkQqHGMe0zUqLE_nX3fjZFC7BuLQ",
                "e": "AQAB",
            }],
        })
    }

    async fn orchestrator(kid: &str) -> MockServer {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/.well-known/jwks.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(key_set_body(kid)))
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/.well-known/openid-configuration"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": server.uri(),
                "jwks_uri": format!("{}/.well-known/jwks.json", server.uri()),
            })))
            .mount(&server)
            .await;

        server
    }

    async fn requests_to(server: &MockServer, wanted: &str) -> usize {
        server
            .received_requests()
            .await
            .expect("the mock server records what it was asked for")
            .iter()
            .filter(|request| request.url.path() == wanted)
            .count()
    }

    #[tokio::test]
    async fn a_key_set_is_fetched_once_and_then_served_from_the_cache() {
        let server = orchestrator("k1").await;
        let services = AppContext::new_mock(|_| {}).await.unwrap();
        let configured = WorkloadIssuer {
            jwks_url: Some(format!("{}/.well-known/jwks.json", server.uri())),
            ..issuer("nomad")
        };

        let first = key_set(&services, &configured).await.unwrap();
        let second = key_set(&services, &configured).await.unwrap();

        assert_eq!(first.keys.len(), 1);
        assert_eq!(second.keys.len(), 1);
        assert_eq!(
            requests_to(&server, "/.well-known/jwks.json").await,
            1,
            "an enrolment per request must not become an orchestrator call per request",
        );
    }

    #[tokio::test]
    async fn a_discovery_document_is_read_for_the_url_rather_than_guessed() {
        let server = orchestrator("k1").await;
        let services = AppContext::new_mock(|_| {}).await.unwrap();
        let configured = WorkloadIssuer {
            discovery_url: Some(format!("{}/.well-known/openid-configuration", server.uri())),
            ..issuer("kubernetes")
        };

        key_set(&services, &configured).await.unwrap();

        assert_eq!(
            requests_to(&server, "/.well-known/openid-configuration").await,
            1
        );
        assert_eq!(requests_to(&server, "/.well-known/jwks.json").await, 1);
    }

    #[tokio::test]
    async fn an_unknown_key_sends_us_back_exactly_once_a_minute() {
        // The compromise this module exists for: a rotation is noticed by the
        // first token that needs it, and a token naming a key that will never
        // exist cannot be replayed into a request per presentation.
        let server = orchestrator("k1").await;
        let services = AppContext::new_mock(|_| {}).await.unwrap();
        let configured = WorkloadIssuer {
            jwks_url: Some(format!("{}/.well-known/jwks.json", server.uri())),
            ..issuer("nomad")
        };

        key_set(&services, &configured).await.unwrap();
        assert_eq!(requests_to(&server, "/.well-known/jwks.json").await, 1);

        assert!(refreshed(&services, &configured).await.unwrap().is_some());
        assert_eq!(requests_to(&server, "/.well-known/jwks.json").await, 2);

        assert!(
            refreshed(&services, &configured).await.unwrap().is_none(),
            "the second refetch inside the window is refused rather than made",
        );
        assert_eq!(requests_to(&server, "/.well-known/jwks.json").await, 2);
    }

    #[tokio::test]
    async fn a_key_set_on_disk_is_read_every_time_and_never_refetched() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jwks.json");
        std::fs::write(&path, serde_json::to_vec(&key_set_body("k1")).unwrap()).unwrap();

        let services = AppContext::new_mock(|_| {}).await.unwrap();
        let configured = WorkloadIssuer {
            jwks_file: Some(path.clone()),
            ..issuer("airgapped")
        };

        assert_eq!(key_set(&services, &configured).await.unwrap().keys.len(), 1);
        assert!(
            refreshed(&services, &configured).await.unwrap().is_none(),
            "a file is re-read anyway, so there is nothing to refetch",
        );

        std::fs::write(&path, "not a key set").unwrap();
        assert!(key_set(&services, &configured).await.is_err());
    }

    #[tokio::test]
    async fn an_orchestrator_that_will_not_answer_is_reported_rather_than_cached() {
        let services = AppContext::new_mock(|_| {}).await.unwrap();
        let configured = WorkloadIssuer {
            jwks_url: Some("http://127.0.0.1:1/jwks".to_string()),
            ..issuer("nomad")
        };

        assert!(key_set(&services, &configured).await.is_err());
    }

    #[test]
    fn the_refresh_window_is_the_one_the_documentation_promises() {
        assert_eq!(refresh_window(), StdDuration::from_secs(60));
        assert_eq!(REFRESH_EVERY, chrono::Duration::minutes(1));
    }
}

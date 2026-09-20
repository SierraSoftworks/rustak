//! An orchestrator a test can genuinely enrol against.
//!
//! It serves a real key set over HTTP, a real discovery document beside it, and
//! mints real RS256 assertions with the claims Nomad and Kubernetes actually
//! put in one. rustak fetches, caches, verifies and rotates against it exactly
//! as it would against a Nomad cluster.
//!
//! # Why not a shortcut
//!
//! The cheap way to reach the same paths is a branch in the verifier: accept
//! HS256 with a shared secret under `cfg(test)`, skip the fetch, twenty lines.
//! That branch *is* the algorithm-confusion vulnerability written down
//! deliberately, and it means the code under test is not the code that ships.
//! Nothing in `src/` outside this module knows a test is running — the same
//! bargain [`super::oidc`] makes, for the same reason.
//!
//! # Two keys, and one nobody has heard of
//!
//! The issuer publishes one key to begin with and holds a second back, so a
//! test can mint an assertion naming a `kid` the server does not hold, publish
//! the key, and watch the refetch accept it. A third key is never published at
//! all: that is the forgery, correct in every respect except the signature.

use std::sync::{Arc, LazyLock, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::config::{WorkloadAlgorithm, WorkloadIssuer};

/// Where the key set is published, as Nomad publishes it.
const JWKS_PATH: &str = "/.well-known/jwks.json";

/// The discovery document beside it.
const DISCOVERY_PATH: &str = "/.well-known/openid-configuration";

/// The key the issuer advertises from the start.
pub const KEY_ID: &str = "rustak-workload-1";

/// The key it holds back until a test asks it to rotate.
pub const ROTATED_KEY_ID: &str = "rustak-workload-2";

/// The audience a deployment asks for, and therefore the one every assertion
/// rustak accepts must name.
pub const AUDIENCE: &str = "rustak";

/// How long an issued assertion is good for, as a Nomad `ttl` would be.
const LIFETIME_SECONDS: i64 = 3600;

/// One key, in both halves it needs.
struct IssuerKey {
    pem: String,
    jwk: serde_json::Value,
}

/// Generated once per test process; see [`super::oidc`] for why.
static PRIMARY: LazyLock<IssuerKey> = LazyLock::new(|| key(KEY_ID));

/// The key the issuer publishes only after a rotation.
static ROTATED: LazyLock<IssuerKey> = LazyLock::new(|| key(ROTATED_KEY_ID));

/// A key the issuer never publishes: the forgery.
static UNADVERTISED: LazyLock<IssuerKey> = LazyLock::new(|| key(KEY_ID));

/// Generates a key and derives the JSON Web Key for its public half.
fn key(kid: &str) -> IssuerKey {
    use base64::Engine as _;
    use rsa::RsaPrivateKey;
    use rsa::pkcs1::{EncodeRsaPrivateKey as _, LineEnding};
    use rsa::traits::PublicKeyParts as _;

    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)
        .expect("generate a key for the orchestrator under test");

    let pem = key
        .to_pkcs1_pem(LineEnding::LF)
        .expect("encode the generated key")
        .to_string();

    // RFC 7518 §6.3.1, derived from the key rather than written down, so the
    // published set cannot drift from what the issuer signs with.
    let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let jwk = serde_json::json!({
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": kid,
        "n": encoder.encode(key.n().to_bytes_be()),
        "e": encoder.encode(key.e().to_bytes_be()),
    });

    IssuerKey { pem, jwk }
}

/// Which keys the issuer is publishing right now.
type Published = Arc<Mutex<Vec<serde_json::Value>>>;

/// An orchestrator's signing authority, served over HTTP.
///
/// Holds the server, so it must outlive every request made through it.
pub struct TestWorkloadIssuer {
    server: MockServer,
    published: Published,
}

impl TestWorkloadIssuer {
    /// Starts an issuer publishing one key.
    pub async fn start() -> Self {
        let server = MockServer::start().await;
        let published: Published = Arc::new(Mutex::new(vec![PRIMARY.jwk.clone()]));
        let serving = Arc::clone(&published);

        Mock::given(method("GET"))
            .and(path(JWKS_PATH))
            .respond_with(move |_: &Request| {
                let keys = serving.lock().map(|held| held.clone()).unwrap_or_default();

                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "keys": keys }))
            })
            .mount(&server)
            .await;

        let issuer = server.uri();

        Mock::given(method("GET"))
            .and(path(DISCOVERY_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "jwks_uri": format!("{issuer}{JWKS_PATH}"),
                "response_types_supported": ["id_token"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["RS256", "EdDSA"],
            })))
            .mount(&server)
            .await;

        Self { server, published }
    }

    /// The `iss` this issuer stamps into every assertion, which is also where
    /// its documents live.
    pub fn issuer(&self) -> String {
        self.server.uri()
    }

    /// Where the key set is published.
    pub fn jwks_url(&self) -> String {
        format!("{}{JWKS_PATH}", self.server.uri())
    }

    /// Where the discovery document is published.
    pub fn discovery_url(&self) -> String {
        format!("{}{DISCOVERY_PATH}", self.server.uri())
    }

    /// The `[auth.workload]` issuer an operator would write to trust this one.
    pub fn config(&self, name: &str) -> WorkloadIssuer {
        WorkloadIssuer {
            name: name.to_string(),
            issuer: Some(self.issuer()),
            jwks_url: Some(self.jwks_url()),
            discovery_url: None,
            jwks_file: None,
            audience: AUDIENCE.to_string(),
            algorithms: vec![WorkloadAlgorithm::Rs256, WorkloadAlgorithm::EdDsa],
            clock_skew: chrono::Duration::seconds(30),
            jwks_refresh: chrono::Duration::hours(1),
            allow_insecure_jwks: false,
        }
    }

    /// Publishes the second key, as a cluster rotating its signing keys does.
    ///
    /// # Panics
    ///
    /// Never: a poisoned lock leaves the set as it was, which fails the
    /// assertion the caller was about to make rather than the whole suite.
    pub fn rotate(&self) {
        if let Ok(mut held) = self.published.lock() {
            held.push(ROTATED.jwk.clone());
        }
    }

    /// The claims a Nomad `identity` block produces, as the maintainer's
    /// cluster emits them (verified 2026-09-20).
    pub fn nomad_claims(&self, namespace: &str, job: &str, task: &str) -> serde_json::Value {
        let now = chrono::Utc::now().timestamp();

        serde_json::json!({
            "iss": self.issuer(),
            "aud": [AUDIENCE],
            "sub": format!("global:{namespace}:{job}:sidecar:{task}:rustak"),
            "nomad_namespace": namespace,
            "nomad_job_id": job,
            "nomad_task": task,
            "nomad_allocation_id": "3f1d8a0e-0000-0000-0000-000000000001",
            "jti": format!("jti-{job}-{now}"),
            "iat": now,
            "nbf": now,
            "exp": now + LIFETIME_SECONDS,
        })
    }

    /// The claims a Kubernetes projected service-account token carries, with
    /// everything inside the one `kubernetes.io` claim.
    pub fn kubernetes_claims(&self, namespace: &str, account: &str) -> serde_json::Value {
        let now = chrono::Utc::now().timestamp();

        serde_json::json!({
            "iss": self.issuer(),
            "aud": [AUDIENCE],
            "sub": format!("system:serviceaccount:{namespace}:{account}"),
            "kubernetes.io": {
                "namespace": namespace,
                "pod": { "name": format!("{account}-7c9f2"), "uid": "8f0e-1" },
                "serviceaccount": { "name": account, "uid": "1b2c-3" },
            },
            "jti": format!("jti-{account}-{now}"),
            "iat": now,
            "nbf": now,
            "exp": now + LIFETIME_SECONDS,
        })
    }

    /// Signs a claim set with the key this issuer advertises.
    pub fn issue(&self, claims: serde_json::Value) -> String {
        sign(&PRIMARY.pem, Some(KEY_ID), claims)
    }

    /// Signs with the key that is published only after [`rotate`](Self::rotate).
    pub fn issue_rotated(&self, claims: serde_json::Value) -> String {
        sign(&ROTATED.pem, Some(ROTATED_KEY_ID), claims)
    }

    /// Signs with the issuer's own key but labels it with another `kid`.
    pub fn issue_with_kid(&self, kid: Option<&str>, claims: serde_json::Value) -> String {
        sign(&PRIMARY.pem, kid, claims)
    }

    /// Signs with a key this issuer does not publish, labelled as one it does.
    ///
    /// The forgery that matters: the key set is public, so a `kid` naming a
    /// legitimate key is not a secret and an attacker will reuse it. What they
    /// cannot do is produce a signature that verifies against it.
    pub fn forge(&self, claims: serde_json::Value) -> String {
        sign(&UNADVERTISED.pem, Some(KEY_ID), claims)
    }

    /// How many times the key set has been fetched.
    pub async fn jwks_fetches(&self) -> usize {
        self.server
            .received_requests()
            .await
            .expect("the mock server records what it was asked for")
            .iter()
            .filter(|request| request.url.path() == JWKS_PATH)
            .count()
    }
}

/// Signs a claim set as an RS256 assertion.
fn sign(pem: &str, kid: Option<&str>, claims: serde_json::Value) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = kid.map(str::to_string);

    jsonwebtoken::encode(
        &header,
        &claims,
        &jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes())
            .expect("read the orchestrator's signing key"),
    )
    .expect("sign a workload assertion")
}

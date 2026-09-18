//! An identity provider a test can genuinely sign in to.
//!
//! It serves a real discovery document over HTTP, a real key set derived from a
//! real RSA key, and mints real RS256 tokens — plus the authorization and token
//! endpoints of the code flow, with the proof-key check the specification
//! requires. rustak fetches, caches, verifies and rotates against it exactly as
//! it would against Entra or Auth0.
//!
//! # Why not a shortcut
//!
//! The cheap way to reach the same paths is a branch in the validator: accept
//! HS256 with a shared secret under `cfg(test)`, skip the fetches, twenty
//! lines. That branch is the algorithm-confusion vulnerability written down
//! deliberately, and it means the code under test is not the code that ships —
//! the guard stays in production, the tested path never exercises it, and
//! nothing notices when they drift. Nothing in `src/` outside this module knows
//! a test is running.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::config::OidcConfig;

/// Where the provider publishes its keys.
///
/// Named in the discovery document rather than assumed, so a test that broke
/// discovery parsing fails here rather than quietly working.
const JWKS_PATH: &str = "/jwks";

/// The path OpenID Connect reserves for discovery. Not ours to choose, which is
/// the point.
const DISCOVERY_PATH: &str = "/.well-known/openid-configuration";

/// Where a code is redeemed.
const TOKEN_PATH: &str = "/token";

/// The key identifier the provider advertises and stamps into every token.
pub const KEY_ID: &str = "rustak-test-provider";

/// The client rustak is registered as, and therefore the audience every token
/// it accepts must name.
pub const CLIENT_ID: &str = "rustak-under-test";

/// The authorization code the token endpoint will redeem.
pub const CODE: &str = "an-authorization-code";

/// How long an issued token is good for.
const LIFETIME_SECONDS: i64 = 3600;

/// The provider's key, in both halves it needs.
struct ProviderKey {
    pem: String,
    jwk: serde_json::Value,
}

/// Generated once per test process.
///
/// Once because RSA key generation would otherwise dominate a suite that runs
/// in a couple of seconds; generated rather than committed because a private
/// key in a repository is one that eventually gets copied somewhere real.
static PROVIDER_KEY: LazyLock<ProviderKey> = LazyLock::new(|| key(2048));

/// A second real key the provider does **not** advertise.
///
/// The forgery the tests care about: a token signed by somebody else, correct
/// in every other respect.
static UNADVERTISED_KEY: LazyLock<ProviderKey> = LazyLock::new(|| key(2048));

/// Generates a key and derives the JSON Web Key for its public half.
fn key(bits: usize) -> ProviderKey {
    use base64::Engine as _;
    use rsa::RsaPrivateKey;
    use rsa::pkcs1::{EncodeRsaPrivateKey as _, LineEnding};
    use rsa::traits::PublicKeyParts as _;

    let key = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, bits)
        .expect("generate a key for the identity provider under test");

    let pem = key
        .to_pkcs1_pem(LineEnding::LF)
        .expect("encode the generated key")
        .to_string();

    // RFC 7518 §6.3.1, derived from the key rather than written down, so the
    // published key set cannot drift from what the provider signs with.
    let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let jwk = serde_json::json!({
        "kty": "RSA",
        "use": "sig",
        "alg": "RS256",
        "kid": KEY_ID,
        "n": encoder.encode(key.n().to_bytes_be()),
        "e": encoder.encode(key.e().to_bytes_be()),
    });

    ProviderKey { pem, jwk }
}

/// The key a forgery is signed with.
pub fn unadvertised_key() -> &'static str {
    &UNADVERTISED_KEY.pem
}

/// What the authorization endpoint recorded for each code it issued: the nonce
/// and the proof-key challenge, either of which a caller may omit.
type Issued = Arc<Mutex<HashMap<String, (Option<String>, Option<String>)>>>;

/// An identity provider, served over HTTP.
///
/// Holds the server, so it must outlive every request made through it.
pub struct TestIdentityProvider {
    server: MockServer,
}

impl TestIdentityProvider {
    /// Starts a provider and publishes everything the code flow needs.
    pub async fn start() -> Self {
        Self::start_for("alice").await
    }

    /// As [`start`](Self::start), with the token endpoint signing in
    /// `username`.
    pub async fn start_for(username: &str) -> Self {
        let server = MockServer::start().await;
        let issuer = server.uri();

        Mock::given(method("GET"))
            .and(path(DISCOVERY_PATH))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "issuer": issuer,
                "authorization_endpoint": format!("{issuer}/authorize"),
                "token_endpoint": format!("{issuer}{TOKEN_PATH}"),
                "jwks_uri": format!("{issuer}{JWKS_PATH}"),
            })))
            .mount(&server)
            .await;

        // Only the public half. A provider that handed out its signing key is
        // not one anything should be tested against.
        Mock::given(method("GET"))
            .and(path(JWKS_PATH))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "keys": [PROVIDER_KEY.jwk] })),
            )
            .mount(&server)
            .await;

        // What the authorization endpoint recorded about each code it issued:
        // the nonce it was asked to echo and the proof-key challenge it has to
        // check at redemption. A real provider keeps exactly this.
        let issued: Issued = Arc::new(Mutex::new(HashMap::new()));

        // The authorization endpoint, so a browser-driven flow has somewhere
        // to be sent. It hands back a code the token endpoint will redeem, and
        // carries the caller's `state` back untouched.
        let recording = Arc::clone(&issued);

        Mock::given(method("GET"))
            .and(path("/authorize"))
            .respond_with(move |request: &Request| {
                let query: HashMap<_, _> = request.url.query_pairs().collect();

                let Some(redirect_uri) = query.get("redirect_uri") else {
                    return ResponseTemplate::new(400);
                };

                let separator = if redirect_uri.contains('?') { '&' } else { '?' };
                let state = query.get("state").map(AsRef::as_ref).unwrap_or("");
                let nonce = query.get("nonce").map(|value| value.to_string());
                let challenge = query.get("code_challenge").map(|value| value.to_string());

                // A distinct code per request, so two flows in one test cannot
                // redeem each other's — and prefixed with `CODE` so that a test
                // driving the exchange directly still finds one it can use.
                let code = if nonce.is_some() || challenge.is_some() {
                    let serial = recording.lock().map(|held| held.len()).unwrap_or(0);

                    format!("{CODE}-{serial}")
                } else {
                    CODE.to_string()
                };

                if let Ok(mut held) = recording.lock() {
                    held.insert(code.clone(), (nonce, challenge));
                }

                ResponseTemplate::new(302).insert_header(
                    "location",
                    format!("{redirect_uri}{separator}code={code}&state={state}").as_str(),
                )
            })
            .mount(&server)
            .await;

        let claims = claims_for(&issuer, username);
        let redeeming = Arc::clone(&issued);

        // The code flow, with the checks a provider actually makes: the code
        // has to be the one it issued, the verifier has to be present, and —
        // when the authorization request registered a challenge — it has to
        // hash to it. The nonce it was asked for goes into the ID token.
        Mock::given(method("POST"))
            .and(path(TOKEN_PATH))
            .respond_with(move |request: &Request| {
                let form = form_fields(&request.body);
                let code = form.get("code").cloned().unwrap_or_default();

                if !code.starts_with(CODE) {
                    return ResponseTemplate::new(400)
                        .set_body_json(serde_json::json!({ "error": "invalid_grant" }));
                }

                let Some(verifier) = form.get("code_verifier").filter(|value| !value.is_empty())
                else {
                    return ResponseTemplate::new(400)
                        .set_body_json(serde_json::json!({ "error": "invalid_request" }));
                };

                let recorded = redeeming
                    .lock()
                    .ok()
                    .and_then(|held| held.get(&code).cloned());
                let mut claims = claims.clone();

                if let Some((nonce, challenge)) = recorded {
                    if let Some(challenge) = challenge
                        && challenge != s256(verifier)
                    {
                        return ResponseTemplate::new(400)
                            .set_body_json(serde_json::json!({ "error": "invalid_grant" }));
                    }

                    if let Some(nonce) = nonce {
                        claims["nonce"] = serde_json::Value::String(nonce);
                    }
                }

                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "id_token": sign(&PROVIDER_KEY.pem, Some(KEY_ID), claims),
                    "refresh_token": "a-refresh-token",
                    "token_type": "Bearer",
                }))
            })
            .mount(&server)
            .await;

        Self { server }
    }

    /// The provider's issuer, which is also where its discovery document lives.
    pub fn issuer(&self) -> String {
        self.server.uri()
    }

    /// The configuration an operator would write to trust this provider.
    pub fn config(&self) -> OidcConfig {
        OidcConfig {
            endpoint: self.issuer(),
            client_id: CLIENT_ID.to_string(),
            client_secret: "a-client-secret".to_string(),
            scopes: vec!["email".to_string()],
            username_claim: "preferred_username".to_string(),
            groups_claim: "groups".to_string(),
            group_prefix: String::new(),
            strip_group_prefix: true,
            read_suffix: "_READ".to_string(),
            write_suffix: "_WRITE".to_string(),
            read_only_group: None,
            auto_create_groups: true,
            link_by_username: false,
            display_name: Some("The provider under test".to_string()),
        }
    }

    /// The claims this provider puts in an ID token.
    ///
    /// `sub` is deliberately not the username: real providers issue an opaque
    /// subject beside a human-readable name, and keeping them apart is how a
    /// test can tell which claim decides whose account a request belongs to.
    pub fn claims_for(&self, username: &str) -> serde_json::Value {
        claims_for(&self.issuer(), username)
    }

    /// A signed ID token, as the browser would present it.
    pub fn sign_in_as(&self, username: &str) -> String {
        self.issue(self.claims_for(username))
    }

    /// Signs an arbitrary claim set with the key this provider advertises.
    pub fn issue(&self, claims: serde_json::Value) -> String {
        sign(&PROVIDER_KEY.pem, Some(KEY_ID), claims)
    }

    /// Signs with the provider's own key but labels it with another `kid`.
    ///
    /// The signature is genuinely the provider's; only the label is a lie. That
    /// separates "we could not verify this" from "we could not work out what to
    /// verify it against".
    pub fn issue_with_kid(&self, kid: Option<&str>, claims: serde_json::Value) -> String {
        sign(&PROVIDER_KEY.pem, kid, claims)
    }

    /// Signs with a key the provider does not publish.
    ///
    /// The forgery that matters: the key set is public, so the `kid` naming a
    /// legitimate key is not a secret and an attacker will reuse it. What they
    /// cannot do is produce a signature that verifies against it.
    pub fn forge(&self, claims: serde_json::Value) -> String {
        sign(unadvertised_key(), Some(KEY_ID), claims)
    }

    /// How many times the key set has been fetched.
    pub async fn jwks_fetches(&self) -> usize {
        self.requests_to(JWKS_PATH).await
    }

    /// How many times the discovery document has been fetched.
    pub async fn discovery_fetches(&self) -> usize {
        self.requests_to(DISCOVERY_PATH).await
    }

    async fn requests_to(&self, wanted: &str) -> usize {
        self.server
            .received_requests()
            .await
            .expect("the mock server records what it was asked for")
            .iter()
            .filter(|request| request.url.path() == wanted)
            .count()
    }
}

/// The claims a provider would issue for somebody.
fn claims_for(issuer: &str, username: &str) -> serde_json::Value {
    let now = chrono::Utc::now().timestamp();

    serde_json::json!({
        "iss": issuer,
        "aud": CLIENT_ID,
        "sub": format!("subject-id-for-{username}"),
        "preferred_username": username,
        "name": display_name(username),
        "email": format!("{username}@example.com"),
        "groups": ["ops_WRITE"],
        "iat": now,
        "exp": now + LIFETIME_SECONDS,
    })
}

/// The fields of an `application/x-www-form-urlencoded` body.
fn form_fields(body: &[u8]) -> HashMap<String, String> {
    url::form_urlencoded::parse(body)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

/// The `S256` challenge for a verifier, as RFC 7636 defines it.
///
/// Written out here rather than called from `web::helpers::oidc::pkce` on
/// purpose: a provider that derived the challenge with the same code under test
/// would agree with it however wrong both were.
fn s256(verifier: &str) -> String {
    use base64::Engine as _;
    use sha2::{Digest as _, Sha256};

    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Signs a claim set as an RS256 ID token.
fn sign(pem: &str, kid: Option<&str>, claims: serde_json::Value) -> String {
    let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
    header.kid = kid.map(str::to_string);

    jsonwebtoken::encode(
        &header,
        &claims,
        &jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes())
            .expect("read the identity provider's signing key"),
    )
    .expect("sign an ID token")
}

/// The name a provider would show, distinct from the username on purpose:
/// `name` and `preferred_username` are read by different code, and a test
/// cannot tell them apart if they carry the same value.
fn display_name(username: &str) -> String {
    let mut characters = username.chars();

    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

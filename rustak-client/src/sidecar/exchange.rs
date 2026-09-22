//! Buying a rustak access token with the identity the orchestrator gave us.
//!
//! RFC 7523 §2.1: the assertion this sidecar's task already holds is posted to
//! `/oauth/token` and comes back as one of rustak's own access tokens. That is
//! what replaces `[service] token` for the control API.
//!
//! # What is cached, and what is never cached
//!
//! The **access token** is held until a minute before it expires. The
//! **assertion** is not held at all: it is re-read from
//! [`Source`](super::workload::Source) on every exchange, because both
//! orchestrators renew it by rewriting its file, and a copy taken at start-up
//! is a credential that works for an hour and then stops.
//!
//! # A refusal is not retried at full speed
//!
//! A server that answers `400` to an assertion will answer `400` to the same
//! assertion five seconds later. [`Credential`] is what turns that into a
//! widening wait and four log lines instead of one exchange per tick forever —
//! which is what cost the first live deployment a `429` lockout that outlived
//! the process that earned it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rustak_core::prelude::*;

use crate::http;

use super::assertion::{Assertion, Clock, read_fresh, unverified_subject};
use super::credential::{Credential, announce_acceptance, announce_refusal, fingerprint};
use super::link_health::{LinkHealth, humanised, note};
use super::workload::{Source, report_control_identity};

/// The RFC 7523 grant a token exchange asks for.
pub const JWT_BEARER_GRANT: &str = "urn:ietf:params:oauth:grant-type:jwt-bearer";

/// How long before an access token expires it is exchanged again.
const RENEW_BEFORE: chrono::Duration = chrono::Duration::seconds(60);

/// The last line of advice on every refusal the **server** made.
///
/// A marker, in the way `http::is_transport` uses one: "the credential was
/// refused" and "there was no credential to present" are different sentences,
/// and only the first of them may claim the control link is up.
const ADVICE_ANSWERED: &str =
    "This is the server's own answer, and not a guess made by this sidecar.";

/// Whether a failed exchange is one the server answered.
pub(crate) fn was_answered(err: &Error) -> bool {
    err.advice().contains(&ADVICE_ANSWERED)
}

/// How long to give the orchestrator when what was read is not worth
/// presenting.
///
/// Long enough for a rename to land, short enough that a start-up does not
/// visibly pause on it.
const SETTLE: Duration = Duration::from_millis(500);

/// What a token exchange answered.
#[derive(Debug, Deserialize)]
struct Granted {
    access_token: String,

    /// Seconds. Absent is treated as "expired already", which costs one extra
    /// exchange and never leaves a stale token in place.
    #[serde(default)]
    expires_in: Option<i64>,
}

/// An access token and when it stops being worth presenting.
#[derive(Debug, Clone)]
struct Held {
    token: Secret,
    renew_at: chrono::DateTime<chrono::Utc>,
}

/// The rustak access token a workload identity buys, kept until it is nearly
/// spent.
#[derive(Debug)]
pub struct AccessTokens {
    source: Source,
    endpoint: String,
    http: reqwest::Client,
    held: Mutex<Option<Held>>,

    /// The account `[service]` says this sidecar is, for the identity line when
    /// the exchanged token cannot be taken apart to read a `sub` out of.
    account: Option<String>,

    /// Whether the identity line has been written.
    ///
    /// The token is exchanged again every hour and by two callers at start-up
    /// — the harness and the feed task — and the line is a start-up line, so it
    /// is written once and never again.
    reported: AtomicBool,

    /// How many identity lines were written, for the test that asserts "once"
    /// — there is nothing else to read a log line back from.
    #[cfg(test)]
    reports: std::sync::atomic::AtomicUsize,

    /// Held for the duration of an exchange, so two callers asking at once buy
    /// one token rather than two.
    exchanging: tokio::sync::Mutex<()>,

    /// Whether the server is taking what we present, and how long to wait
    /// before asking again when it is not.
    credential: Credential,

    /// A fingerprint of the assertion last presented — never the assertion.
    presented: Mutex<Option<u64>>,

    /// The clock, injected so that tests decide what "now" is.
    clock: Clock,

    /// How long to give the orchestrator mid-renewal; zero in tests.
    settle: Duration,
}

impl AccessTokens {
    /// An exchanger against `endpoint`, reading its assertion from `source`.
    pub fn new(source: Source, endpoint: String, http: reqwest::Client) -> Self {
        Self {
            source,
            endpoint,
            http,
            held: Mutex::new(None),
            account: None,
            reported: AtomicBool::new(false),
            #[cfg(test)]
            reports: std::sync::atomic::AtomicUsize::new(0),
            exchanging: tokio::sync::Mutex::new(()),
            credential: Credential::new(),
            presented: Mutex::new(None),
            clock: Clock::default(),
            settle: SETTLE,
        }
    }

    /// Names the account `[service]` says this sidecar is.
    ///
    /// Only the identity line uses it, and only when the exchanged token
    /// cannot be taken apart: the `sub` the server put in the token it issued
    /// is the better answer, because it is the account rustak *resolved*
    /// rather than the one the file asked for.
    #[must_use]
    pub fn for_account(mut self, account: impl Into<String>) -> Self {
        self.account = Some(account.into());
        self
    }

    /// The clock and the mid-renewal wait a test wants.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self.settle = Duration::ZERO;
        self
    }

    /// Where the assertion comes from, for the start-up line.
    pub fn source(&self) -> &Source {
        &self.source
    }

    /// The access token being held right now, if one is worth presenting.
    ///
    /// Never exchanges: this is for a caller — a plugin reading its own
    /// configuration — that wants to ride on the credential the harness has
    /// already bought rather than provoke an exchange of its own.
    pub fn held(&self) -> Option<Secret> {
        self.live()
    }

    /// A live access token, exchanging for one when what is held is nearly
    /// spent.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the token cannot be read, the
    /// server cannot be reached, or the exchange is refused.
    pub async fn current(&self) -> Result<Secret, Error> {
        if let Some(held) = self.live() {
            return Ok(held);
        }

        // One exchange at a time. Nothing is awaited while the `held` lock is
        // taken, so this is the only thing either caller waits on.
        let _exchanging = self.exchanging.lock().await;

        // Whoever was ahead of us may have bought one while we waited, and a
        // sidecar needs a token rather than its own copy of one.
        if let Some(held) = self.live() {
            return Ok(held);
        }

        let assertion = self.assertion().await?;
        let granted = self.exchange(&assertion).await?;
        let renew_at = self.clock.now()
            + chrono::Duration::seconds(granted.expires_in.unwrap_or(0))
            - RENEW_BEFORE;
        let token = Secret::new(granted.access_token);

        if let Ok(mut held) = self.held.lock() {
            *held = Some(Held {
                token: token.clone(),
                renew_at,
            });
        }

        self.report_once(&assertion, &token);

        Ok(token)
    }

    /// A live access token for a control-API call, or [`None`] when there is
    /// not one to be had right now.
    ///
    /// Everything worth saying about a failure has already been said, at the
    /// level this credential's own history calls for — so a caller that gets
    /// [`None`] skips the call it wanted the token for and says nothing more.
    pub(crate) async fn for_call(&self, health: &LinkHealth, what: &str) -> Option<Secret> {
        if let Some(held) = self.live() {
            return Some(held);
        }

        let now = self.clock.now();

        // A credential the server has just refused is not worth presenting
        // again yet — unless the orchestrator has replaced it since, which is
        // the renewal this whole module is about.
        if !self.credential.due_at(now) && !self.replaced() {
            tracing::debug!(
                retry_in = %humanised(self.credential.retry_in_at(now)),
                "This sidecar's workload identity was refused; not exchanging it again yet.",
            );

            return None;
        }

        match self.current().await {
            Ok(token) => {
                announce_acceptance(self.credential.accepted_at(self.clock.now()));

                Some(token)
            }
            // The server was never reached, so this is the link being down and
            // not the credential being wrong: `LinkHealth` owns that sentence.
            Err(err) if http::is_transport(&err) => {
                note(health, what, &err);

                None
            }
            Err(err) => {
                let mark = self.presented.lock().ok().and_then(|held| *held);
                announce_refusal(
                    self.credential.refused_at(mark, self.clock.now()),
                    was_answered(&err),
                    &err,
                );

                None
            }
        }
    }

    /// How long until this sidecar's credential is worth presenting again.
    pub(crate) fn retry_in(&self) -> Duration {
        self.credential.retry_in_at(self.clock.now())
    }

    /// Throws away what is held, so the next call exchanges again.
    ///
    /// Called when the server refuses the access token: a signing key rotated,
    /// or somebody revoked it. Waiting for the cached expiry would leave the
    /// sidecar unable to report for up to an hour.
    pub fn invalidate(&self) {
        if let Ok(mut held) = self.held.lock() {
            *held = None;
        }
    }

    /// The assertion to present, read afresh and checked for a pulse.
    async fn assertion(&self) -> Result<Assertion, Error> {
        let assertion = read_fresh(|| self.source.read(), &self.clock, self.settle).await?;

        if let Ok(mut presented) = self.presented.lock() {
            *presented = Some(fingerprint(assertion.token()));
        }

        Ok(assertion)
    }

    /// Whether the source holds something other than what was refused.
    fn replaced(&self) -> bool {
        self.source
            .read()
            .ok()
            .is_some_and(|token| self.credential.changed(fingerprint(&token)))
    }

    /// Writes the identity line the first time a token comes back.
    ///
    /// The account is the `sub` of the access token — the username rustak
    /// resolved the workload identity to — falling back to what `[service]`
    /// configured and then to "-": a description of the endpoint is not an
    /// account, and printing one is what made this line unreadable.
    fn report_once(&self, assertion: &Assertion, granted: &Secret) {
        if self.reported.swap(true, Ordering::Relaxed) {
            return;
        }

        #[cfg(test)]
        self.reports.fetch_add(1, Ordering::Relaxed);

        let account = unverified_subject(granted)
            .or_else(|| self.account.clone())
            .unwrap_or_else(|| "-".to_string());

        report_control_identity(
            &self.source,
            assertion.issuer(),
            &account,
            assertion.expires_at(),
        );
    }

    /// The held token, if it is still worth presenting.
    fn live(&self) -> Option<Secret> {
        let held = self.held.lock().ok()?;
        let held = held.as_ref()?;

        (self.clock.now() < held.renew_at).then(|| held.token.clone())
    }

    /// One `POST /oauth/token` with the `jwt-bearer` grant.
    async fn exchange(&self, assertion: &Assertion) -> Result<Granted, Error> {
        let base = http::base_url(&self.endpoint, "control")?;
        let url = http::endpoint(&base, "/oauth/token")?;

        let response = self
            .http
            .post(url)
            .form(&[
                ("grant_type", JWT_BEARER_GRANT),
                ("assertion", assertion.token().expose()),
            ])
            .send()
            .await
            .map_err(|err| http::transport(err, "exchange this workload identity for a token"))?;

        let status = response.status();

        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();

            return Err(refused(
                status,
                &self.source,
                assertion,
                self.clock.now(),
                described(&body).as_deref(),
            ));
        }

        response.json().await.map_err(|err| {
            human_errors::user(
                format!("The server's token answer was not one we can read ({err})."),
                &["Check that [server] control names a rustak server's public listener."],
            )
        })
    }
}

/// The `error_description` an OAuth error object carries, when it carries one.
///
/// RFC 6749 §5.2: the server's own sentence about why it would not take this
/// credential. It names no secret — the caller is the token's holder — and it
/// is the difference between "refused" and "expired 58m12s ago".
fn described(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let description = parsed.get("error_description")?.as_str()?.trim();

    (!description.is_empty()).then(|| description.to_string())
}

/// What each way of being refused means for whoever is holding the token.
///
/// The expiry comes **first** when this end can see one: "the token you are
/// presenting died 58 minutes ago" is the answer, and everything about
/// `[auth.workload]` is a distraction from it.
fn refused(
    status: reqwest::StatusCode,
    source: &Source,
    assertion: &Assertion,
    now: chrono::DateTime<chrono::Utc>,
    description: Option<&str>,
) -> Error {
    let said = match description {
        Some(description) => format!(": {description}"),
        None => String::new(),
    };

    let message = match assertion.expired_for(now) {
        Some(ago) => format!(
            "The workload identity this sidecar presented expired {} ago; its source {source} is {}. The server refused it ({status}){said}.",
            humanised(ago.to_std().unwrap_or_default()),
            renewal(source),
        ),
        None => format!("The server refused this workload identity ({status}){said}."),
    };

    human_errors::user(message, advice(status, source, assertion, now))
}

/// How the credential this sidecar is holding gets replaced, if it does.
fn renewal(source: &Source) -> &'static str {
    match source.is_renewable() {
        true => {
            "a file the orchestrator rewrites on every renewal, so a renewal has not reached this sidecar"
        }
        false => "an environment variable, which the orchestrator cannot renew",
    }
}

/// What to tell an operator to do about it.
fn advice(
    status: reqwest::StatusCode,
    source: &Source,
    assertion: &Assertion,
    now: chrono::DateTime<chrono::Utc>,
) -> &'static [&'static str] {
    if assertion.expired_for(now).is_some() && !source.is_renewable() {
        return &[
            "Set `env = false, file = true` in the Nomad identity block: Nomad renews an identity by rewriting its file, and never by changing the environment.",
            "Then restart this task once, so that it picks up the file as its source.",
            ADVICE_ANSWERED,
        ];
    }

    match status.as_u16() {
        400 => &[
            "The server would not accept this workload identity: check `[auth.workload]` on the server, and that the audience matches.",
            "The sentence above is the server's own answer; its log says the same thing.",
            ADVICE_ANSWERED,
        ],
        429 => &[
            "Too many attempts. This is what a sidecar that retried a refused credential every tick leaves behind; the wait is now backed off.",
            ADVICE_ANSWERED,
        ],
        503 => &[
            "This installation cannot issue tokens right now.",
            ADVICE_ANSWERED,
        ],
        _ => &["The status above is the server's own.", ADVICE_ANSWERED],
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::{DateTime, Utc};

    use super::*;

    /// The instant these tests judge against; nothing here reads a wall clock.
    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_789_646_400 + seconds, 0).expect("an instant")
    }

    /// A workload assertion expiring at the given instant.
    fn assertion(expires_at: DateTime<Utc>) -> String {
        use base64::Engine as _;

        let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;

        format!(
            "{}.{}.not-a-signature",
            encoder.encode(br#"{"alg":"RS256","kid":"k1"}"#),
            encoder.encode(
                serde_json::json!({
                    "iss": "https://nomad.example.com",
                    "sub": "global:default:rustak-plugin-adsb:sidecar:adsb:rustak",
                    "exp": expires_at.timestamp(),
                })
                .to_string()
                .as_bytes(),
            ),
        )
    }

    /// A token exchange that answers `sub`, counting what it was asked.
    async fn exchanging() -> (wiremock::MockServer, tempfile::TempDir, PathBuf) {
        use base64::Engine as _;

        let encoder = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let granted = format!(
            "{}.{}.not-a-signature",
            encoder.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
            encoder.encode(br#"{"iss":"https://tak.example.com","sub":"svc.adsb"}"#),
        );

        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/oauth/token"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": granted,
                    "token_type": "Bearer",
                    "expires_in": 3600,
                })),
            )
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, assertion(at(3_600))).unwrap();

        (server, directory, path)
    }

    /// The exchanger a test drives, with its clock stopped at `at(0)`.
    fn tokens(path: &std::path::Path, uri: String) -> AccessTokens {
        AccessTokens::new(
            Source::File(path.to_path_buf()),
            uri,
            reqwest::Client::new(),
        )
        .with_clock(Clock::fixed(at(0)))
    }

    #[tokio::test]
    async fn reopening_the_feed_does_not_buy_another_token() {
        // The feed asks for a token before every opening, because one held open
        // for hours outlives the token that opened it. What must *not* happen
        // is a `/oauth/token` per reopening.
        let (server, _directory, path) = exchanging().await;
        let tokens = tokens(&path, server.uri());

        for _ in 0..5 {
            tokens.current().await.expect("a token");
        }

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "five openings, one exchange",
        );
    }

    #[tokio::test]
    async fn two_callers_asking_at_once_buy_one_token_between_them() {
        // The harness and the feed task both ask the instant a sidecar starts,
        // and both find nothing cached.
        let (server, _directory, path) = exchanging().await;
        let tokens = std::sync::Arc::new(tokens(&path, server.uri()));

        let asking: Vec<_> = (0..4)
            .map(|_| {
                let tokens = std::sync::Arc::clone(&tokens);

                tokio::spawn(
                    async move { tokens.current().await.map(|token| token.expose().len()) },
                )
            })
            .collect();

        for asked in asking {
            asked.await.expect("the task joins").expect("a token");
        }

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "four callers, one exchange",
        );
    }

    #[tokio::test]
    async fn the_identity_line_is_written_once_however_many_callers_ask() {
        let (server, _directory, path) = exchanging().await;
        let tokens = tokens(&path, server.uri()).for_account("svc.adsb");

        tokens.current().await.expect("a token");
        tokens.invalidate();
        tokens.current().await.expect("another token");

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "the invalidation did buy a second token",
        );
        assert_eq!(
            tokens.reports.load(Ordering::Relaxed),
            1,
            "and it was still one identity line",
        );
    }

    #[tokio::test]
    async fn an_exchange_asks_for_the_jwt_bearer_grant_and_caches_what_comes_back() {
        let (server, directory, _path) = exchanging().await;
        let path = directory.path().join("grant.jwt");
        std::fs::write(&path, assertion(at(3_600))).unwrap();

        let tokens = tokens(&path, server.uri());

        tokens.current().await.expect("a token");
        tokens.current().await.expect("the same token");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1, "a live token is not exchanged again");

        let body = String::from_utf8(requests[0].body.clone()).expect("a form body");
        assert!(
            body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer"),
            "{body}",
        );
    }

    #[tokio::test]
    async fn the_token_is_re_read_from_its_source_on_every_exchange() {
        // Both orchestrators rotate it by rewriting the file, so a copy held
        // from start-up would work for an hour and then stop.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "granted", "expires_in": 3600 }),
            ))
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        let first = assertion(at(3_600));
        let second = assertion(at(7_200));
        std::fs::write(&path, &first).unwrap();

        let tokens = tokens(&path, server.uri());

        tokens.current().await.expect("a token");
        tokens.invalidate();
        std::fs::write(&path, &second).unwrap();
        tokens.current().await.expect("another token");

        let presented: Vec<String> = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .map(|request| String::from_utf8(request.body.clone()).expect("a form body"))
            .collect();

        assert_eq!(presented.len(), 2);
        // A JWT is made of the characters form encoding leaves alone, so the
        // body carries it verbatim.
        assert!(presented[0].contains(&first), "{presented:?}");
        assert!(
            presented[1].contains(&second),
            "the second exchange must present the token the file holds now: {presented:?}",
        );
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn an_expired_token_is_still_presented_so_that_the_server_can_say_so() {
        // The orchestrator may be part way through a renewal, and presenting a
        // token we can see is dead earns a refusal and a lockout.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "granted", "expires_in": 3600 }),
            ))
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, assertion(at(-60))).unwrap();

        let tokens = tokens(&path, server.uri());

        // The second read is where the renewed token is found; the file is
        // rewritten by the reader itself, which is what `read_fresh` covers in
        // isolation. Here what matters is that the expired one still goes out
        // rather than the sidecar giving up on its own.
        tokens.current().await.expect("a token");

        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_refused_exchange_names_the_expiry_the_source_and_what_the_server_said() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(400).set_body_json(serde_json::json!({
                    "error": "invalid_grant",
                    "error_description": "exp: expired 58m12s ago",
                })),
            )
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, assertion(at(-3_480))).unwrap();

        let err = tokens(&path, server.uri()).current().await.unwrap_err();
        let rendered = err.to_string();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(
            rendered.starts_with(
                "The workload identity this sidecar presented expired 58m00s ago; its source"
            ),
            "the expiry comes first: {rendered}",
        );
        assert!(rendered.contains("token.jwt"), "{rendered}");
        assert!(rendered.contains("exp: expired 58m12s ago"), "{rendered}");
        assert!(
            !rendered.contains("eyJ"),
            "and no part of the token is in it: {rendered}",
        );
    }

    #[tokio::test]
    async fn an_expired_environment_credential_says_the_environment_cannot_be_renewed() {
        let refusal = refused(
            reqwest::StatusCode::BAD_REQUEST,
            &Source::Env("NOMAD_TOKEN_rustak".to_string()),
            &Assertion::read(Secret::new(assertion(at(-3_600)))),
            at(0),
            Some("exp: expired 1h00m ago"),
        );
        let rendered = refusal.to_string();

        assert!(
            rendered.contains(
                "its source NOMAD_TOKEN_rustak is an environment variable, which the orchestrator cannot renew"
            ),
            "{rendered}",
        );
        assert!(
            refusal
                .advice()
                .iter()
                .any(|line| line.contains("env = false, file = true")),
            "{:?}",
            refusal.advice(),
        );
    }

    #[tokio::test]
    async fn a_refusal_that_is_not_about_the_expiry_still_says_what_to_look_at() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "invalid_grant" })),
            )
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, assertion(at(3_600))).unwrap();

        let err = tokens(&path, server.uri()).current().await.unwrap_err();

        assert!(err.is(human_errors::Kind::User), "{err}");
        assert!(
            err.advice()
                .iter()
                .any(|line| line.contains("[auth.workload]")),
            "{err}",
        );
    }

    #[tokio::test]
    async fn a_refused_credential_is_not_presented_again_until_the_wait_has_run_out() {
        // The production defect's other half: a refusal reset the link's
        // backoff, so the same dead assertion went out every tick for 45
        // minutes and earned a 429 that outlived the process.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(
                wiremock::ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "invalid_grant" })),
            )
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, assertion(at(-3_600))).unwrap();

        let tokens = tokens(&path, server.uri());
        let health = LinkHealth::new();

        for _ in 0..5 {
            assert!(tokens.for_call(&health, "exchange it").await.is_none());
        }

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            1,
            "five calls, one exchange: the rest were inside the backoff",
        );
        assert_eq!(tokens.retry_in(), crate::sidecar::credential::RETRY_MIN);
    }

    #[tokio::test]
    async fn a_renewed_credential_is_tried_at_once_rather_than_serving_the_old_one_s_wait() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::body_string_contains("not-a-signature"))
            .respond_with(
                wiremock::ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "invalid_grant" })),
            )
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, assertion(at(-3_600))).unwrap();

        let tokens = tokens(&path, server.uri());
        let health = LinkHealth::new();

        assert!(tokens.for_call(&health, "exchange it").await.is_none());
        assert!(
            tokens.for_call(&health, "exchange it").await.is_none(),
            "inside the backoff",
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);

        // Nomad rewrites the file: a different credential, and the wait it
        // inherited was the old one's.
        std::fs::write(&path, assertion(at(-30))).unwrap();

        assert!(tokens.for_call(&health, "exchange it").await.is_none());
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            2,
            "a renewed credential is presented at once",
        );
    }

    #[tokio::test]
    async fn a_server_that_cannot_be_reached_is_the_links_problem_and_not_the_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, assertion(at(3_600))).unwrap();

        // Port 1 refuses the connection: a transport failure.
        let tokens = tokens(&path, "http://127.0.0.1:1".to_string());
        let health = LinkHealth::new();

        assert!(tokens.for_call(&health, "exchange it").await.is_none());

        assert!(health.is_down(), "the link is what failed");
        assert_eq!(
            tokens.retry_in(),
            Duration::ZERO,
            "and the credential is not the thing being backed off",
        );
    }

    #[tokio::test]
    async fn an_accepted_credential_forgets_the_refusals_before_it() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": "granted", "expires_in": 3600 }),
            ))
            .mount(&server)
            .await;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("token.jwt");
        std::fs::write(&path, assertion(at(3_600))).unwrap();

        let tokens = tokens(&path, server.uri());
        let health = LinkHealth::new();

        assert!(tokens.for_call(&health, "exchange it").await.is_some());
        assert_eq!(tokens.retry_in(), Duration::ZERO);
        assert!(
            tokens.held().is_some(),
            "and a plugin can ride on it without provoking an exchange",
        );
    }

    #[tokio::test]
    async fn a_credential_that_could_not_be_read_is_not_reported_as_the_server_refusing_it() {
        // The file is not there: nothing reached the server, so nothing may
        // claim the server answered.
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("never-written.jwt");

        let err = tokens(&path, "http://127.0.0.1:1".to_string())
            .current()
            .await
            .unwrap_err();

        assert!(!was_answered(&err), "{err}");
        assert!(err.to_string().contains("never-written.jwt"), "{err}");
    }

    #[test]
    fn the_grant_type_is_spelled_the_way_rfc_7523_spells_it() {
        assert_eq!(
            JWT_BEARER_GRANT,
            "urn:ietf:params:oauth:grant-type:jwt-bearer"
        );
    }

    #[test]
    fn an_error_object_without_a_description_is_not_one() {
        assert_eq!(
            described(r#"{"error":"invalid_grant","error_description":"exp: expired"}"#).as_deref(),
            Some("exp: expired"),
        );
        assert_eq!(described(r#"{"error":"invalid_grant"}"#), None);
        assert_eq!(described(r#"{"error_description":"  "}"#), None);
        assert_eq!(described("not json"), None);
    }
}

//! A server a test can sign in to.
//!
//! [`TestServer`] is an [`AppContext`] with the handles start-up would have
//! installed — the signing keys, the content store — already in place, plus the
//! two or three lines every endpoint test otherwise repeats: seed an account,
//! mint a session, put a bearer token on a request.
//!
//! # What is shared, and what is not
//!
//! Everything a test can observe is its own: a fresh in-memory database, a
//! fresh data directory, a fresh secret store, a fresh rate limiter. Two things
//! that cost real time and that no test asserts on are process-wide instead —
//! the RSA token signing key ([`keys`](super::keys)) and the argon2id cost
//! ([`use_testing_params`](rustak_core::identity::password::use_testing_params))
//! — because generating a key and hashing at 19 MiB once per test is what made
//! this suite unfinishable on a two-core runner under coverage. Both are still
//! the real algorithms on the real code path; only their cost changes.

use std::sync::Arc;

use rustak_api::{TokenResponse, UserKind};

use crate::auth::{JwtIssuer, tokens};
use crate::config::Config;
use crate::db::repos::{NewUser, UserRow};
use crate::prelude::*;

/// The host an installation under test answers on.
pub const TEST_HOST: &str = "localhost";

/// The base URL that follows from it, which is also the WebAuthn origin.
pub const TEST_ORIGIN: &str = "https://localhost";

/// A context with everything start-up installs.
pub struct TestServer {
    /// The context itself.
    pub context: AppContext,
    /// The rate limiter the routes share, so a test can reason about lockouts.
    pub limiter: Arc<crate::auth::RateLimiter>,
    /// Held because the content store lives under it; dropping it deletes the
    /// directory while the server still points at it.
    pub data_dir: tempfile::TempDir,
}

impl std::ops::Deref for TestServer {
    type Target = AppContext;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl TestServer {
    /// A server with the default configuration for a test.
    ///
    /// # Panics
    ///
    /// If the in-memory database, the signing keys or the content store cannot
    /// be built, none of which a test can carry on past.
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// As [`start`](Self::start), letting the caller adjust the configuration.
    ///
    /// # Panics
    ///
    /// As [`start`](Self::start).
    pub async fn start_with(f: impl Sized + FnOnce(&mut Config)) -> Self {
        // Before anything is hashed, including by the endpoints this server is
        // about to serve. Idempotent, so every test may call it.
        rustak_core::identity::password::use_testing_params();

        let data_dir = tempfile::tempdir().expect("a temporary directory for the test server");
        let path = data_dir.path().to_path_buf();

        let context = AppContext::new_mock(move |config| {
            config.server.domains = vec![TEST_HOST.to_string()];
            config.server.data_dir = path;
            config.web.public.allow_insecure_http = true;
            config.web.public.tls.mode = crate::config::TlsMode::None;

            f(config);
        })
        .await
        .expect("a mock context");

        let issuer = JwtIssuer::load_or_adopt(
            context.db(),
            context.secrets(),
            &context.config().auth,
            TEST_ORIGIN,
            &super::keys::JWT_SIGNING_KEY,
        )
        .await
        .expect("token signing keys");

        context
            .install_jwt(Arc::new(issuer))
            .expect("the signing keys are installed once");

        context
            .install_content(Arc::new(crate::store::ContentStore::new(
                context.config().content_dir(),
            )))
            .expect("the content store is installed once");

        let limiter = Arc::new(crate::auth::RateLimiter::new(
            &context.config().auth.rate_limit,
        ));

        Self {
            context,
            limiter,
            data_dir,
        }
    }

    /// The routes the public listener serves, for `test::init_service`.
    pub fn app(&self) -> impl FnOnce(&mut actix_web::web::ServiceConfig) + Clone {
        crate::web::server::services(self.context.clone(), self.limiter.clone())
    }

    /// Creates an account.
    ///
    /// # Panics
    ///
    /// If the account cannot be created, which a test cannot carry on past.
    pub async fn user(&self, username: &str, is_admin: bool) -> UserRow {
        let username = Username::parse(username).expect("a usable username in a test");

        let user = self
            .context
            .db()
            .users()
            .create(NewUser {
                kind: UserKind::Person,
                is_admin,
                ..NewUser::person(username)
            })
            .await
            .expect("create the account under test");

        crate::identity::groups::join_default(self.context.db(), user.id)
            .await
            .expect("put the account in the default channel");

        user
    }

    /// Creates an account and signs it in.
    ///
    /// # Panics
    ///
    /// As [`user`](Self::user), or if the session cannot be issued.
    pub async fn signed_in(&self, username: &str, is_admin: bool) -> (UserRow, TokenResponse) {
        let user = self.user(username, is_admin).await;
        let session = session_for(&self.context, &user, is_admin).await;

        (user, session)
    }
}

/// Mints a session for an account that already exists.
///
/// # Panics
///
/// If the token cannot be signed, which means the keys were never installed.
pub async fn session_for(context: &AppContext, user: &UserRow, is_admin: bool) -> TokenResponse {
    tokens::issue_session(context, user, is_admin, Some("test"))
        .await
        .expect("issue a session for the account under test")
}

/// The `Authorization` header value for a session.
pub fn bearer(session: &TokenResponse) -> String {
    format!("Bearer {}", session.token)
}

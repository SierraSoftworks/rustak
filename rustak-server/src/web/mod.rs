//! The HTTP surface: the admin API, the embedded UI, and the listeners that
//! serve them.
//!
//! # What is here in M0
//!
//! The public listener and `/api/v1`. The Marti listener, the OAuth2 endpoints
//! and the TAK-compatible routes arrive with enrolment in M2; the module tree
//! is laid out so that they are added beside [`api`] rather than through it.
//!
//! # Why the public listener always serves TLS
//!
//! Everything this listener carries is a credential: a bearer token, a passkey
//! assertion, an enrolment token typed into a device. Plaintext is therefore
//! not a default with a warning, it is a configuration error — `[web.public]`
//! has to say `allow_insecure_http` *and* `tls.mode = "none"` before a socket
//! is bound without it. See [`tls`] for what each mode does.
//!
//! # Why there are no cookies
//!
//! The admin API takes our own bearer token and nothing else. A cookie is
//! attached by the browser to every request whatever page caused it, which is
//! the whole of what cross-site request forgery is; a header is not, so there
//! is no token to double-submit and no `SameSite` to reason about. The
//! `/login/*` pages that TAK clients expect do use cookies, and they arrive in
//! M2 scoped to those paths alone.

pub mod api;
pub mod helpers;
pub mod server;
pub mod telemetry;
pub mod tls;
pub mod ui;

pub use server::build_public;

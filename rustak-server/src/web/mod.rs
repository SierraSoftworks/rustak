//! The HTTP surface: the admin API, the embedded UI, and the listeners that
//! serve them.
//!
//! # The two listeners
//!
//! [`build_public`] binds `[web.public]`: the admin UI, `/api/v1`, the OAuth2
//! endpoints, the enrolment endpoints and the whole TAK surface, with no client
//! certificate asked for. [`build_marti`] binds `[web.marti]`: the TAK surface
//! alone, with a client certificate **required** — which is both the
//! authentication and the device identity on that port.
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
//! `/login/*` pages that TAK clients expect do use cookies, and they arrive
//! scoped to those paths alone.

pub mod api;
pub mod helpers;
pub mod server;
pub mod telemetry;
pub mod tls;
pub mod ui;

pub use server::{build_marti, build_public};

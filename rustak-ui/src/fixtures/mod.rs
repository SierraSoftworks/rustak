//! Baked-in sample data for offline previews.
//!
//! Appending `?demo` to the URL (for example when running `trunk serve` with no
//! server behind it) makes the app render fixture data instead of calling the
//! API, so the interface can be built and reviewed before `/api/v1` exists.
//!
//! The substitution happens inside [`crate::api`] and [`crate::auth`] rather
//! than in the pages. A page written against the real client therefore works in
//! demo mode without knowing demo mode exists, and cannot drift out of it by
//! adding a call nobody remembered to stub.
//!
//! Demo mode is a development convenience, so everything below the flag itself
//! is compiled only into debug builds: a release bundle contains no fixtures and
//! no path that could reach them.

#[cfg(debug_assertions)]
mod data;
#[cfg(debug_assertions)]
mod store;

#[cfg(debug_assertions)]
pub use store::*;

/// Returns true when the current URL asks for demo mode (`?demo`).
#[cfg(debug_assertions)]
pub fn is_demo() -> bool {
    web_sys::window()
        .and_then(|window| window.location().search().ok())
        .map(|search| search.contains("demo"))
        .unwrap_or(false)
}

/// Demo mode is unavailable in release builds.
#[cfg(not(debug_assertions))]
pub fn is_demo() -> bool {
    false
}

/// Serves a call from the demo store when demo mode is active.
///
/// Written as a macro so each call site reads as one line above the real
/// request, and so the whole thing vanishes from a release build rather than
/// relying on the optimiser to notice that it cannot be reached.
macro_rules! demo {
    ($($body:tt)*) => {
        #[cfg(debug_assertions)]
        if $crate::fixtures::is_demo() {
            return { $($body)* };
        }
    };
}

pub(crate) use demo;

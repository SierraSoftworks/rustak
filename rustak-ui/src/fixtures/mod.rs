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
mod certificates;
#[cfg(debug_assertions)]
mod data;
#[cfg(debug_assertions)]
mod missions;
#[cfg(debug_assertions)]
mod packages;
#[cfg(debug_assertions)]
mod profiles;
#[cfg(debug_assertions)]
mod store;

#[cfg(debug_assertions)]
pub use certificates::*;
#[cfg(debug_assertions)]
pub use missions::*;
#[cfg(debug_assertions)]
pub use packages::*;
#[cfg(debug_assertions)]
pub use profiles::*;
#[cfg(debug_assertions)]
pub use store::*;

/// A valid, empty zip archive: the end-of-central-directory record and nothing
/// else.
///
/// Demo mode has no package builder behind it — assembling a Mission Package
/// is the server's job, and a second implementation here would be one more
/// thing to keep honest — so every download in demo mode is this. It is a
/// *valid* archive rather than arbitrary bytes so that a browser handed one
/// opens it and finds it empty, instead of reporting a corrupt file and
/// leaving the reader wondering which of the two they are looking at.
#[cfg(debug_assertions)]
pub fn empty_zip() -> Vec<u8> {
    let mut bytes = vec![0x50, 0x4b, 0x05, 0x06];
    bytes.extend(std::iter::repeat_n(0u8, 18));
    bytes
}

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

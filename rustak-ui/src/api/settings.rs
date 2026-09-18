//! The server's own settings, as resolved from the configuration file and the
//! wizard together.
//!
//! Three of the four are read-only, and for the same reason each time: the
//! value is settled by `config.toml` before the process has a listener to
//! serve this endpoint from, so a form that appeared to change it would be
//! lying. The exception is the upload ceiling, which the server will accept a
//! change to — unless the configuration file pins it, in which case the `PUT`
//! is a `409` rather than a value the next start would override.

use rustak_api::{FileSettings, MartiSettings, ServerSettings, TlsStatus};

use crate::api::{ApiError, get_json, post_json, put_json};
// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// The resolved settings. Values set in the configuration file win over the ones
/// the wizard wrote, so this is the only honest answer to "what is it actually
/// using".
pub async fn get() -> Result<ServerSettings, ApiError> {
    demo!(Ok(fixtures::server_settings()));

    get_json("/settings").await
}

/// What the public listener is presenting, and what happens to it next.
pub async fn tls() -> Result<TlsStatus, ApiError> {
    demo!(Ok(fixtures::tls_status()));

    get_json("/settings/tls").await
}

/// Orders a certificate now rather than waiting for the renewal job.
///
/// Worth a button because the useful moment for it is exactly when the last
/// order failed: the page shows the authority's own error, an operator fixes
/// whatever it named, and the next attempt is otherwise hours away.
pub async fn renew_tls() -> Result<TlsStatus, ApiError> {
    demo!(fixtures::renew_tls());

    post_json("/settings/tls/renew", &()).await
}

/// The upload ceiling, and whether the configuration file is what set it.
pub async fn files() -> Result<FileSettings, ApiError> {
    demo!(Ok(fixtures::file_settings()));

    get_json("/settings/files").await
}

/// Changes the upload ceiling.
///
/// It is a *limit*, not a suggestion: the same number is advertised to clients
/// through `/files/api/config` and enforced inside every upload reader, so
/// changing it changes what a client is told as well as what it may send.
pub async fn set_files(upload_size_limit_mb: u32) -> Result<FileSettings, ApiError> {
    demo!(fixtures::set_file_settings(upload_size_limit_mb));

    put_json(
        "/settings/files",
        &FileSettings {
            upload_size_limit_mb,
            from_config_file: false,
        },
    )
    .await
}

/// What the TAK-compatible surface tells clients about itself.
pub async fn marti() -> Result<MartiSettings, ApiError> {
    demo!(Ok(fixtures::marti_settings()));

    get_json("/settings/marti").await
}

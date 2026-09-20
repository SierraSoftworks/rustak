//! The `rustak-plugin-ais` binary: the library's [`AisSidecar`] under the
//! sidecar harness.
//!
//! Everything this process does — the command line, the configuration file,
//! telemetry, the CoT stream, registration and the shutdown signal — belongs to
//! [`rustak_client::sidecar::run`]. What is ours is in `lib.rs`, which is also
//! what `rustak-server`'s integration suite drives.

use rustak_client::sidecar::run;
use rustak_plugin_ais::AisSidecar;

#[tokio::main]
async fn main() {
    run::<AisSidecar>().await;
}

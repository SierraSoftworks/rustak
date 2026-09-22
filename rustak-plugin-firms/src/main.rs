//! The `rustak-plugin-firms` binary: the library's [`FirmsSidecar`] under the
//! sidecar harness.
//!
//! The command line, the configuration file, telemetry, the CoT stream,
//! registration and the shutdown signal all belong to
//! [`rustak_client::sidecar::run`]. What is ours is in `lib.rs`.

use rustak_client::sidecar::run;
use rustak_plugin_firms::FirmsSidecar;

#[tokio::main]
async fn main() {
    run::<FirmsSidecar>().await;
}

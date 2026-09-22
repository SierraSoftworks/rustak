//! The `rustak-plugin-esb` binary: the library's [`EsbSidecar`] under the
//! sidecar harness, which owns the command line, the configuration file,
//! telemetry, the CoT stream, registration and the shutdown signal.

use rustak_client::sidecar::run;
use rustak_plugin_esb::EsbSidecar;

#[tokio::main]
async fn main() {
    run::<EsbSidecar>().await;
}

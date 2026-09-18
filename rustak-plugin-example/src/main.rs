//! `rustak-plugin-example` — the copy-and-rename template for a TAK sidecar.
//!
//! A real plugin connects as a service identity, publishes/subscribes CoT
//! over the stream, and reports heartbeats to the server's control API by
//! implementing `rustak_client::sidecar::Sidecar` and calling its `run()`
//! harness (see `.claude/plan/plan.md` → Architecture → "Plugin (sidecar)
//! contract"). M0 only proves the binary builds, links against
//! `rustak-client`/`rustak-core`/`rustak-cot`, and reports who it is; the
//! `Sidecar` implementation lands in a later milestone.

const NAME: &str = env!("CARGO_PKG_NAME");
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[allow(clippy::print_stdout)]
fn main() {
    println!("{NAME} {VERSION}");
}

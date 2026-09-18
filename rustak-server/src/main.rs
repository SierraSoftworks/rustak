//! `rustak` — thin CLI entry point.
//!
//! M0 only proves the binary builds and reports its version; `clap` argument
//! parsing (`--config`, `--env`, `--check`), telemetry bootstrap and the
//! actual server run loop are wired up by a later M0 implementation brief
//! (see `.claude/plan/design/01-foundations-storage-ci.md` §3.4).

#[allow(clippy::print_stdout)]
fn main() {
    // Binaries normally log through `tracing` once telemetry is wired up
    // (`rustak_core::telemetry`); that doesn't exist yet in M0, and a bare
    // version string is conventionally written to stdout regardless (as
    // `cargo --version` and friends do), so the workspace lint is relaxed
    // here rather than for the crate as a whole.
    println!("rustak {}", env!("CARGO_PKG_VERSION"));
}

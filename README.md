# rustak

A lightweight, single-binary Rust TAK server backed by embedded SQLite, aiming for first-class
compatibility with **ATAK-CIV** (EUD) and **CloudTAK** (web UI). It is a from-scratch, permissively
licensed (MIT) implementation — no code is shared with TAK Server or OpenTAKServer.

This repository is a Cargo workspace: `rustak-cot` (CoT/TAK protocol), `rustak-api` (shared DTOs),
`rustak-core` (config/telemetry/identity foundations), `rustak-client` (sidecar SDK), `rustak-server`
(the `rustak` binary), `rustak-plugin-example` (a sidecar template), and `rustak-ui` (the Yew admin
SPA, built separately with Trunk since it targets `wasm32-unknown-unknown`).

## Building

The UI is embedded into the server binary at compile time, so build it first:

```sh
cd rustak-ui && trunk build && cd ..
cargo build
```

`cargo build` (with no other arguments) builds only `rustak-server`, the binary you actually run;
`cargo build --workspace` builds every crate, including `rustak-plugin-example`.

## Project plan

Architecture, milestones and implementation conventions live in [`.claude/plan/plan.md`](.claude/plan/plan.md).

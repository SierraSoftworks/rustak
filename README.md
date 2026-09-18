# rustak

A lightweight, single-binary Rust TAK server backed by embedded SQLite, aiming for first-class
compatibility with **ATAK-CIV** (EUD) and **CloudTAK** (web UI). It is a from-scratch, permissively
licensed (MIT) implementation — no code is shared with TAK Server or OpenTAKServer.

Running the official TAK Server or OpenTAKServer for a small personal deployment (hike tracking,
HAM radio integration) is cost- and complexity-prohibitive, and OpenTAKServer is unstable and only
partially compatible with CloudTAK. rustak targets one process, one SQLite database, and a config
file with every key documented — no plaintext credential listener, no local user passwords, and a
one-time setup token instead of a default admin account. See
[`.claude/plan/plan.md`](.claude/plan/plan.md) → Context and Decisions for the full reasoning.

This repository is a Cargo workspace: `rustak-cot` (CoT/TAK protocol), `rustak-api` (shared DTOs),
`rustak-core` (config/telemetry/identity foundations), `rustak-client` (sidecar SDK), `rustak-server`
(the `rustak` binary), `rustak-plugin-example` (a sidecar template), and `rustak-ui` (the Yew admin
SPA, built separately with Trunk since it targets `wasm32-unknown-unknown`).

> **Status.** Under active development; not yet ready to run end to end. M0 (the workspace
> skeleton, config, storage, crypto and the admin API) is most of the way done; the server does not
> yet have a wired-up run loop (see the table below and [`docs/deployment.md`](docs/deployment.md)
> for exactly what that means today). Nothing here should be pointed at real ATAK/CloudTAK traffic
> yet.

## Feature status

Current state per milestone, as verified by the implementation notes under
[`.claude/plan/status/`](.claude/plan/status/) — not aspirational. A milestone with no status file
listed has no verified implementation yet, whatever a design document says it will contain.

| # | Milestone | Exit criterion | Status |
|---|---|---|---|
| M0 | Workspace, config, storage, crypto, admin web server + UI shell, full CI/CD, file-length lint | Green pipeline on `main`; multi-arch `ghcr.io/sierrasoftworks/rustak:latest` published; a tagged pre-release produces binaries + Homebrew formula | **In progress.** Done and verified: workspace skeleton and CI pipeline (`M0-01`, `M0-02`); `rustak-api` DTOs and `rustak-core` (`M0-03`, `M0-04`); server config schema + `config.example.toml` + `--check` (`M0-06`); SQLite layer (`M0-07`); crypto (`M0-08`); services/job host (`M0-09`); content store, append-log, root CA, JWT issuer (`M0-10`); admin web server, `/api/v1`, OIDC + passkey auth, setup wizard API (`M0-11`); Yew UI skeleton (`M0-13`); `rustak-client` sidecar SDK + `rustak-plugin-example` (`M0-15`). **Not yet done:** the `main.rs`/`lib.rs::run`/`runtime.rs` bootstrap that actually starts the listeners (today `rustak --config … --check` validates a config file, but there is no run loop yet); the Playwright e2e specs. The exit criterion (published image, green full pipeline) has not been met. |
| M1 | `rustak-cot` crate + streaming core: TLS listener, negotiation, hub/router, ping, latest-SA replay, append-only CoT history; EUD interop-harness decision; `interop/rust` suites | Fake-EUD integration suites green in CI; TLS 1.3 `!ECDH` check green; EUD-harness decision recorded | **In progress.** The EUD interop harness decision is recorded (`M1-00`: build ATAK's `commoncommo` from `atak-civ` at image-build time, kept out of this repository per its GPL-3.0 licence) and its CI image-build job is delivered (`M1-04`, not yet confirmed by a real CI run). The CoT XML model (`rustak-cot`) and the streaming listener/hub/router are under active development with no status file yet. |
| M2 | PKI, enrollment, Marti basics (`tls/*`, one-time enrollment tokens + QR, client passwords, passkeys + setup wizard already land in M0), groups, contacts, `/files/api/config`, ACME; `interop/node-tak` suite in CI | node-tak suite green on PRs; ATAK enrols via QR | **Not started.** Briefs exist (`M2-01` PKI issuance, `M2-02` identity/credentials) with no status files yet. |
| M3 | Data packages + device profiles | node-tak files scenarios green; EUD harness package share | Not started. |
| M4 | Data Sync: full mission API; `interop/cloudtak` compose nightly | node-tak mission scenarios green; CloudTAK compose Data Sync round-trip green | Not started. |
| M5 | OAuth2 server + OIDC federation, admin UI polish | CloudTAK login (node-tak + compose); browser SSO + passkeys for the Yew UI (e2e) | Not started. |
| M6 | Services API + example sidecar, docs, release pipeline | Sidecar publishes CoT to a channel; `brew install` works | Not started — though the sidecar SDK and `rustak-plugin-example` this milestone documents already exist from M0 (`M0-15`); see [`docs/plugins.md`](docs/plugins.md). |

Every capability marked "planned" in the docs below (ACME, the CoT stream, the Marti API,
enrollment) follows this table: it exists in the design and, usually, in validated configuration,
but has no status file verifying a working implementation yet.

## Quick start

The UI is embedded into the server binary at compile time, so build it first:

```sh
cd rustak-ui && trunk build && cd ..
cargo build
```

`cargo build` (with no other arguments) builds only `rustak-server`, the binary you actually run;
`cargo build --workspace` builds every crate, including `rustak-plugin-example`.

Copy [`config.example.toml`](config.example.toml) — every key is documented with its default — and
validate a candidate configuration without starting anything:

```sh
cp config.example.toml config.toml
cargo run -- --config config.toml --check
```

Full deployment instructions (Docker, a `docker-compose.yml` with CloudTAK, a systemd unit, TLS
modes, first-run setup, and backup) are in [`docs/deployment.md`](docs/deployment.md) — including
what does and does not work yet.

## Documentation

- [`docs/deployment.md`](docs/deployment.md) — configuring, running and backing up a rustak server.
- [`docs/ci.md`](docs/ci.md) — the CI/CD pipeline, and how to reproduce every check locally.
- [`docs/interop.md`](docs/interop.md) — the automated suites that prove compatibility with ATAK
  and CloudTAK.
- [`docs/plugins.md`](docs/plugins.md) — writing a sidecar with `rustak-client`.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) — workspace layout, local checks, conventions, and how to
  propose a change.

## Project plan

Architecture, decisions and the milestone plan live in
[`.claude/plan/plan.md`](.claude/plan/plan.md); code standards (the <300-functional-line rule,
error handling, tracing, testing, licensing) are in
[`.claude/plan/conventions.md`](.claude/plan/conventions.md). Implementation is orchestrated
per-milestone through self-contained briefs (`.claude/plan/briefs/`) and the notes each one leaves
behind (`.claude/plan/status/`) — see [`.claude/plan/README.md`](.claude/plan/README.md) for how
that directory is organised.

# M0-15 — `rustak-client::sidecar` + `rustak-plugin-example` — complete

Brief: `.claude/plan/briefs/M0-15-client-sidecar-example.md`
Design: `design/01-foundations-storage-ci.md` §1.2 (crate deps), §8 step 15;
`plan.md` → Architecture → "Plugin (sidecar) contract". Depends on M0-04.

## What was built

| File | Functional lines (limit 300) | Contents |
|---|---:|---|
| `rustak-client/src/lib.rs` | 1 | module list; doc pointer to `docs/plugins.md` |
| `rustak-client/src/sidecar/mod.rs` | 105 | `Sidecar` trait (`NAME`/`VERSION` consts, `Settings` associated type, `start`/`tick`/`on_event`/`stop`), `SidecarContext<S>`, `SidecarEvent`, `pub use async_trait` |
| `rustak-client/src/sidecar/config.rs` | 168 | `SidecarConfig<S>` (`[service]`, `[server]`, `[sidecar]`, `[settings]`), `ServiceConfig::identity`, `ServerConfig::endpoints`, `HarnessConfig`, `NoSettings` |
| `rustak-client/src/sidecar/run.rs` | 103 | clap `Args` (`--config`/`--env`/`--check`), `run::<S>()`, `run_with`, `serve`, `drive` |
| `rustak-plugin-example/src/main.rs` | 70 | ~100-line plugin: settings, heartbeat `tick`, event dispatch, `main` |
| `rustak-plugin-example/config.example.toml` | — | every key with its default; loaded by the crate's own test suite |
| `docs/plugins.md` | — | identity model, credentials, copy-the-crate recipe, `Sidecar` methods, running/testing, the M6 control API |

30 tests (26 `rustak-client` unit, 3 `rustak-plugin-example` unit, 1 doctest),
every file with its single trailing column-0 `#[cfg(test)] mod tests`.

`rustak-client/src/sidecar.rs` (the M0-01 stub) was replaced by the
`sidecar/` directory — it shows as a deletion in `git status`.

## Decisions

- **`#[async_trait]` rather than native AFIT.** `rustak-server`'s own traits
  (`Cache`, `Queue`, `KeyValueStore`, `AuditStore`) already use it, a public
  trait with a bare `async fn` trips `async_fn_in_trait` under `-D warnings`,
  and boxing a future at heartbeat rates costs nothing. `async_trait` is
  re-exported from `rustak_client::sidecar`, so a plugin crate does not add the
  dependency itself.
- **The plugin's settings are a typed associated type**, not a raw `toml::Table`:
  `SidecarConfig<S::Settings>` parses `[settings]` with the plugin's own
  `deny_unknown_fields` type at *load* time, which is what keeps the
  "`config.example.toml` is a test, not documentation" property the server has.
  `NoSettings` is the type for a plugin with nothing to configure.
- **`SidecarEvent` is ours and `#[non_exhaustive]`.** M0 has no stream, so
  `on_event` is never called by the harness, but the *shape* is fixed now:
  `Connected`/`Disconnected`/`Cot(Box<rustak_cot::proto::TakMessage>)`. The
  payload is a type that exists today rather than a placeholder, the box keeps
  `large_enum_variant` quiet, and the wildcard arm that `#[non_exhaustive]`
  obliges a plugin to write is what makes M1's replacement additive.
- **The context is given only to `start`.** A plugin that needs it keeps it
  (`self.context = Some(ctx)`); one that does not is spared threading it through
  every call. It is `Clone` (config behind an `Arc`) and carries the
  `ServiceIdentity`, the published `ServiceDescriptor`, the `Shutdown` and the
  `sidecar{service,uid}` span.
- **An error from any trait method stops the process**, reported through
  `rustak_core::errors::report_and_exit`. Documented on the trait: a failure a
  plugin expects to recover from is one it swallows itself.
- **The first `tick` is immediate**, not one interval later (tokio `interval`
  semantics, with `MissedTickBehavior::Delay` so an overrunning plugin falls
  behind rather than stampedes). An operator restarting a plugin sees it do
  something. The `select!` is `biased;` so a cancelled sidecar never takes one
  more tick.
- **`--check`** mirrors `rustak --check`: load, validate, log and exit 0,
  without starting. It reports through tracing rather than stdout, because a
  library has no business writing to a binary's stdout (`clippy::print_stdout`).
- **Unresolved `${{ env.X }}` values are refused by name.** `service.token` and
  the three `[server]` endpoints are checked with
  `rustak_core::config::env::is_unresolved`; the token is held as a
  `rustak_core::identity::Secret`, so a `Debug` dump of the whole config (or of
  the context) redacts it. Both are tested.
- **The harness never logs the plugin's `[settings]`.** The descriptor is
  published, so it is logged in full; `[settings]` are whatever the plugin says
  they are and may hold an upstream API key, so a harness that printed them
  would make that a trap rather than a decision. The trait docs and
  `docs/plugins.md` point a plugin holding a credential at
  `rustak_core::identity::Secret`.
- **Half a client certificate is refused at start-up** rather than at the TLS
  handshake, naming the missing half.

## A finding for `rustak-core` (telemetry in debug builds)

`tracing-batteries` sets `Metadata::enabled_by_default = false` under
`cfg(debug_assertions)`, and the dynamic filter it installs gates **the stdout
writer** as well as the exporters. A debug build that calls
`rustak_core::telemetry::bootstrap` therefore logs *nothing at all* —
`cargo run -p rustak-plugin-example` was completely silent before this was
found.

`run_with` works around it by turning the session on immediately after
bootstrap:

```rust
session.enable().store(true, std::sync::atomic::Ordering::Relaxed);
```

This changes nothing in a release build (already enabled) and attaches no extra
battery in a debug one (`TelemetryOptions::from_env` adds Sentry only when a DSN
is configured, and analytics never). **The server bootstrap brief (M0-12) will
hit exactly the same thing**, so the right long-term home is probably
`rustak_core::telemetry` — either `bootstrap` calling `Metadata::with_debug_builds`
or a `TelemetryOptions { debug_builds: bool }` — rather than two copies of this
line. `rustak-core` was out of scope for this brief, so it was not changed.

## Manifest changes

`rustak-client/Cargo.toml` gained `async-trait`, `chrono` (the `[sidecar]`
durations use `rustak_core::config::duration::humane`) and `clap`, plus a
`tempfile` dev-dependency for the `--check` test — all `workspace = true`.
`rustak-plugin-example/Cargo.toml` is unchanged; its `Dockerfile` was not
touched (its `--config /data/plugin.toml` entrypoint still matches).

Design 01 §1.2 lists `rustak-client` without `clap`/`chrono`/`async-trait`; the
brief's "`run::<S: Sidecar>(args)` entrypoint with clap" supersedes it for
`clap`, and the other two follow from the harness shape.

## Exit checks

### `cargo run -p rustak-plugin-example -- --config rustak-plugin-example/config.example.toml`, then `kill -INT`

```
exit status: 0
   Compiling rustak-client v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-client)
   Compiling rustak-plugin-example v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-example)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.06s
     Running `target/debug/rustak-plugin-example --config rustak-plugin-example/config.example.toml`
2026-09-18T10:55:21.644414Z  INFO rustak_client::sidecar::run: Loaded the configuration for rustak-plugin-example 0.1.0. descriptor=ServiceDescriptor { name: ServiceName(example), display_name: None, version: Some("0.1.0"), capabilities: [], endpoints: ServiceEndpoints { stream: None, marti: None, control: None } } file=rustak-plugin-example/config.example.toml
2026-09-18T10:55:21.644585Z  INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is starting as example. uid=SERVICE-example stream=None
2026-09-18T10:55:21.644651Z  INFO sidecar{service=example uid=SERVICE-example}: rustak_client::sidecar::run: The sidecar has started. interval=5s
2026-09-18T10:55:21.645903Z  INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is alive. heartbeats=1
2026-09-18T10:55:26.647171Z  INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is alive. heartbeats=2
2026-09-18T10:55:30.876742Z  INFO rustak_core::runtime: Received a shutdown signal; draining connections.
2026-09-18T10:55:30.876840Z  INFO sidecar{service=example uid=SERVICE-example}: rustak_client::sidecar::run: The sidecar is stopping.
2026-09-18T10:55:30.876880Z  INFO sidecar{service=example uid=SERVICE-example}: rustak_plugin_example: The example sidecar is stopping. heartbeats=2
```

(A 12-second run at the example file's `tick = "5s"`. ANSI colour codes
stripped.)

### `cargo test -p rustak-client -p rustak-plugin-example`

```
     Running unittests src/lib.rs (target/debug/deps/rustak_client-40f94b3ebedb7908)
running 26 tests
test result: ok. 26 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
     Running unittests src/main.rs (target/debug/deps/rustak_plugin_example-2304aa6b05575898)
running 3 tests
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests rustak_client
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

### `cargo clippy --workspace --all-targets -- -D warnings`

**Fails on `rustak-server`, which is another agent's in-flight work** (`git
status` shows unstaged `rustak-server/src/{jobs,pki,services,store,auth}` files
that do not compile yet):

```
error: this bound is already specified as the supertrait of `Services`   (× 22, rustak-server/src/jobs/*.rs)
error: use of `println!`                                                  (rustak-server/src/pki/ca.rs)
error: very complex type used. Consider factoring parts into `type` definitions  (rustak-server/src/pki/ca.rs)
error: could not compile `rustak-server` (lib) due to 13 previous errors
error: could not compile `rustak-server` (lib test) due to 24 previous errors
```

Nothing in that list is in a file this brief owns (one `collapsible_if` in
`sidecar/run.rs`'s test module was found this way and fixed). Scoped to the
crates this brief touches and everything they depend on:

```
$ cargo clippy -p rustak-api -p rustak-cot -p rustak-core -p rustak-client -p rustak-plugin-example --all-targets -- -D warnings
    Checking rustak-client v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-client)
    Checking rustak-plugin-example v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-plugin-example)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.98s
```

### `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`

Same `rustak-server` compile failure, so run scoped:

```
$ RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-api -p rustak-cot -p rustak-core -p rustak-client -p rustak-plugin-example --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.39s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 4 other files
```

(`sidecar::run` is both a module and a function; the intra-doc links to it are
written `[`run()`]` so rustdoc does not have to guess.)

### `cargo fmt --all --check`

```
(no output)
```

### `./scripts/check-file-length.sh`

```
(no output)
```

## For the orchestrator

- `rustak-client/src/sidecar.rs` is **deleted**; `rustak-client/src/sidecar/` is new.
- The workspace-wide clippy and doc gates cannot pass until the in-flight
  `rustak-server` work compiles; re-run them after integration.
- The telemetry-in-debug-builds finding above is worth folding into
  `rustak-core` before M0-12 writes the same line a second time.

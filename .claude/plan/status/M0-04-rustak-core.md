# M0-04 — `rustak-core` — complete

Brief: `.claude/plan/briefs/M0-04-rustak-core.md`
Design: `design/01-foundations-storage-ci.md` §2.2 (file table), §3.3 (config shape it must load);
`design/03-identity-pki-acme-auth.md` §4–§5 (`GroupSet`/`Principal` semantics).

## What was built

`rustak-core/src/` now matches design 01 §2.2, with the deltas the brief names. Files owned, with
functional-line counts (limit 300):

| File | Lines | Contents |
|---|---:|---|
| `lib.rs` | 7 | module list; crate docs carry the start-up ordering (`load_env_file` → `telemetry::bootstrap` → `Shutdown` → `config::load`) as a `no_run` doctest |
| `prelude.rs` | 6 | `human_errors` + `ResultExt`/`OptionExt`, `tracing_batteries::prelude::*`, serde traits incl. `DeserializeOwned`, `identity::*`, `Shutdown` |
| `errors.rs` | 22 | `ADVICE_REPORT_DEV`, `ADVICE_FILE_ACCESS`, `ADVICE_RESTART_AFTER_FIXING`; `report_and_exit(&Error, Option<Arc<Session>>) -> !` (pretty-print → record when `Kind::System` → flush telemetry → exit 1) |
| `config/mod.rs` | 68 | `load<T>`, `load_str<T>`, `load_env_file` (FIFO guard) |
| `config/interpolation.rs` | 138 | **verbatim lift** of `../automate/agent/src/parsers/interpolation.rs`, tests included |
| `config/env.rs` | 18 | `resolve(expr)`, `is_unresolved(value)`, `UNRESOLVED_MARKER` |
| `config/duration.rs` | 117 | `humane` / `humane_option` serde adapters + public `parse`/`format` |
| `config/listen.rs` | 123 | `ListenAddr` (`":8446"` → `0.0.0.0`, `"[::]:8446"`, serde-as-string, `to_socket_addrs()`) |
| `telemetry.rs` | 71 | `Session` re-export, `TelemetryOptions{sentry_dsn, analytics_url, stdout}` + `from_env()`, `bootstrap`, `shutdown` (40 × 50 ms `Arc::try_unwrap` loop), `testing_session` behind `cfg(any(test, feature = "testing"))` |
| `runtime.rs` | 74 | `Shutdown` over `CancellationToken` (`listen_for_signals`, `cancelled`, `cancel`, `child`, `is_cancelled`, `token`), `with_grace(what, fut, timeout)` |
| `identity/mod.rs` | 15 | `pub use rustak_api::identity::*;` + `rustak_api::credential::CredentialKind` + submodule re-exports |
| `identity/secret.rs` | 73 | `Secret` (zeroize-on-drop, `Debug`/`Display` redacted, `constant_time_eq`), `generate_token`, `generate_password` (Crockford base32, 5×4) |
| `identity/password.rs` | 83 | `PasswordHash` (PHC newtype, redacted `Debug`), `hash`/`verify`/`verify_dummy`, `lookup_hint`, `hash_blocking`/`verify_blocking`/`verify_dummy_blocking` |
| `identity/groups.rs` | 160 | `GroupSet` (two 256-bit `BitVec<u8, Msb0>`), `set`/`clear`/`contains`/`positions`/`intersect`/`to_bytes`/`from_bytes`/`names`, `can_reach`, `GroupIndex` |
| `identity/principal.rs` | 89 | `Principal`, `PrincipalKind`, `AuthMethod` |
| `service.rs` | 80 | `pub use rustak_api::service::*`, `ServiceIdentity`, `impl From<&ServiceIdentity> for ServiceDescriptor` |

150 tests in total (135 unit + 14 doctests + rstest cases), every file with its single trailing
column-0 `#[cfg(test)] mod tests`.

## Brief requirements, point by point

- **Verbatim lift of `interpolation.rs`.** `diff` against the automate original shows exactly three
  changes: a module-doc header recording the provenance, the doc example's import path
  (`automate::parsers::interpolation` → `rustak_core::config::interpolation`), and an
  `#[allow(clippy::collapsible_match)]` on `parse_expression` with a comment saying why — the
  current clippy fires `collapsible_match` on automate's `match`/`if` shape, and rewriting it would
  have cost the "verbatim" property that keeps the two copies from drifting.
- **`duration.rs` patterned on `serde_duration.rs`.** Same module shape (`humane` / `humane_option`
  mirroring `minutes` / `minutes_option`), same refusals at both ends (negative, fractional,
  unrepresentable), same `#[serde(default)]`-is-load-bearing note. It accepts `"30d" | "12h" |
  "15m" | "45s"` plus a bare integer of seconds instead of automate's fixed minutes, because
  rustak's config spans a 90-second idle timeout and a 3650-day CA validity in one file.
- **`telemetry.rs` from automate's `main.rs`.** The `Arc::try_unwrap` reclaim loop is the same
  40 × 50 ms wait, for the same reason; `bootstrap` composes the same OpenTelemetry/Sentry/Analytics
  batteries, with the Sentry DSN from `option_env!("RUSTAK_SENTRY_DSN")` overridden by the runtime
  env var and suppressed when empty.
- **`identity/password.rs` delta.** argon2id at `Argon2::default()` (RFC 9106 second recommendation,
  m = 19 MiB / t = 2 / p = 1), with `hash_blocking` / `verify_blocking` on `spawn_blocking` —
  plus `verify_dummy_blocking`, because the "no such user" path that the constant-time equaliser
  exists for is itself async.
- **`identity/principal.rs` delta.** `AuthMethod` is exactly `ClientCert{fingerprint, serial}`,
  `Bearer{jti, scope}`, `Basic{credential_id, kind}`, `Passkey{credential_id}`, `SetupToken`. There
  is no anonymous constructor and no anonymous variant; `PrincipalKind` is `{Person, Service}` only
  (design 01 §2.2 listed an `Anonymous` kind, which the plan's later "remove anonymous principals"
  delta overrides). A test records the constraint so that re-adding one is a deliberate act.
- **`config::load_env_file` FIFO guard.** `std::fs::metadata` (which does not open the path) →
  `NotFound` is `Ok(())`, `!is_file()` logs a warning and returns `Ok(())`, only a regular file
  reaches `dotenvy::from_path_override`. Covered by a directory test and, under `cfg(unix)`, a real
  `mkfifo` test — a regression there hangs the suite rather than failing it, which is the loudest
  available signal.
- **`${{ env.X }}` doctest.** `config/mod.rs`'s module doctest loads a TOML snippet containing
  `client_secret = "${{ env.RUSTAK_DOC_CLIENT_SECRET }}"` and asserts the unset-variable marker
  survives. It deliberately does not set an environment variable: `std::env::set_var` is `unsafe`
  in edition 2024 and the workspace forbids `unsafe`, so the tests that need a *set* variable use
  `PATH` instead.

## Coordination with M0-03 (`rustak-api`)

`rustak-api/src/identity/` landed in the working tree while this brief was in progress, so
**no competing types were left behind**: `rustak-core::identity` re-exports
`rustak_api::identity::*` and `rustak_api::credential::CredentialKind`, and the temporary local
`identity/newtypes.rs` placeholder that had been written against design 01 §2.1 was deleted before
this brief finished. There is nothing here for the orchestrator to reconcile.

The `rustak-api` surface `rustak-core` depends on, so a later change there is a visible break:

- `identity::{Username, DeviceUid, ServiceName, GroupName, Direction}` — `parse`, `as_str`,
  `ServiceName::uid()`, `GroupName::anon()`, `Direction::{In, Out, Both}` and
  `Direction::includes(Direction) -> bool` (used in place of the `is_in()`/`is_out()` helpers the
  local placeholder had).
- `identity::{UserId, CredentialId}` — `From<i64>`.
- `credential::CredentialKind` — carried by `AuthMethod::Basic`.
- `service::{ServiceDescriptor, ServiceEndpoints, Capability, …}` — glob re-exported from
  `rustak_core::service`; `ServiceDescriptor::new(ServiceName)` is used by
  `From<&ServiceIdentity>`.

## Notes for later briefs

- **`rustak-core/Cargo.toml` gained a `testing` feature** (empty; gates
  `telemetry::testing_session`). `rustak-server` and `rustak-client` should take
  `rustak-core = { workspace = true, features = ["testing"] }` as a *dev*-dependency when their
  integration tests need a `Session`.
- **`scripts/check-file-length.sh` and uncommitted renames.** The script iterates `git ls-files`,
  so while `rustak-core/src/config.rs` → `config/mod.rs` and `identity.rs` → `identity/` are
  unstaged it was handed paths that no longer exist and exited 2 with an `awk` error. A
  `[ -f "$file" ] || continue` guard landed in that script from another session while this brief
  was finishing, and it now exits 0. The remaining gap is the mirror image: `git ls-files` does not
  list *untracked* files either, so this crate's new `config/` and `identity/` trees are not
  covered by the script until they are committed. The run recorded below covers them explicitly.
- `ListenAddr::to_socket_addrs` resolves at *bind* time, not parse time, so a container hostname
  that is not up yet is a start-up condition rather than a config-file syntax error. The server's
  `validate()` should call it only where it is about to bind.
- `config::env::is_unresolved` is how a config type refuses a secret whose environment variable was
  not set, by name. `SecretStore`/`JwtIssuer` in M2 should use it rather than treating the literal
  `${{ env.… }}` text as key material.

## Exit checks

### `cargo test -p rustak-core`

```
running 135 tests
...
test result: ok. 135 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.18s

   Doc-tests rustak_core

running 14 tests
test rustak-core/src/config/mod.rs - config (line 29) ... ok
test rustak-core/src/config/env.rs - config::env::is_unresolved (line 77) ... ok
test rustak-core/src/lib.rs - (line 23) - compile ... ok
test rustak-core/src/config/duration.rs - config::duration (line 52) ... ok
test rustak-core/src/identity/secret.rs - identity::secret::Secret (line 48) ... ok
test rustak-core/src/config/duration.rs - config::duration::humane (line 193) ... ok
test rustak-core/src/config/listen.rs - config::listen::ListenAddr (line 55) ... ok
test rustak-core/src/identity/groups.rs - identity::groups::GroupSet (line 53) ... ok
test rustak-core/src/config/duration.rs - config::duration::humane_option (line 240) ... ok
test rustak-core/src/config/env.rs - config::env::resolve (line 46) ... ok
test rustak-core/src/config/interpolation.rs - config::interpolation::interpolate (line 25) ... ok
test rustak-core/src/prelude.rs - prelude (line 10) ... ok
test rustak-core/src/service.rs - service::ServiceIdentity (line 31) ... ok
test rustak-core/src/runtime.rs - runtime::Shutdown (line 37) ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s
```

exit status 0.

### `cargo clippy -p rustak-core --all-targets -- -D warnings`

```
    Checking rustak-core v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-core)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.21s
```

exit status 0.

### `RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-core --no-deps`

```
 Documenting rustak-core v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-core)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.05s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_core/index.html
```

exit status 0.

### `./scripts/check-file-length.sh`

First run, before the `[ -f "$file" ] || continue` guard landed in that script from another
session:

```
awk: can't open file rustak-core/src/config.rs
 source line number 5
```

exit status 2 — the uncommitted-rename limitation described above, not a file over the limit.
After that guard, re-run unchanged:

```
(no output)
```

exit status 0.

Because `git ls-files` lists neither deleted nor untracked paths, that run does not yet see this
crate's new `config/` and `identity/` trees. The same check over tracked *plus* untracked `.rs`
files, with the same exemptions and the same `awk` counter:

```
checked 46 existing tracked+untracked .rs files; over-limit=0
```

exit status 0. Largest file in this crate is `identity/groups.rs` at 160 functional lines.

### `cargo fmt -p rustak-core -- --check`

Clean. `cargo check --workspace --all-targets` also passes against `rustak-api` as it stands.

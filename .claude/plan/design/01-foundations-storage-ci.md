I have everything I need: the automate reference is fully mapped (config/interpolation, SQLite+migrations+kv/queue/audit, crypto, services, JobHost, web/api/OIDC/principal, UI auth/api/fixtures, e2e harness, all workflows and packaging files), the rustak repo is empty apart from `.git` (remote `SierraSoftworks/rustak`), the local toolchain is Rust 1.96 / trunk 0.21.14 / cross 0.2.5 / node 24, and every dependency's current stable version and feature set is verified against the registry (including the compatibility pins: tokio-rusqlite 0.8 → rusqlite ^0.40, protox 0.9 → prost 0.14, yew-router 0.20 → yew 0.23, tokio-rustls 0.26 / actix `rustls-0_23` / instant-acme / rcgen → rustls 0.23 + aws-lc-rs). Here is the plan.

---

# rustak M0 + storage foundations — implementation plan

## 0. Baseline facts this plan relies on

| Item | Verified value |
|---|---|
| Toolchain | rustc/cargo 1.96 stable; edition 2024; workspace `rust-version = "1.88"` (MSRV floor set by actix-web 4.15, filt-rs 1.1.3, zip 8.6, jsonwebtoken 11.1) |
| Stable versions (Sept 2026) | tokio 1.53.1, actix-web 4.15.0, actix-ws 0.4.0, actix-multipart 0.8.2, reqwest 0.13.5, rusqlite 0.40.2, tokio-rusqlite 0.8.0, include_dir 0.7.4, serde 1.0.229, serde_json 1.0.151, toml 1.1.6, chrono 0.4.45, uuid 1.26.1, quick-xml 0.42.0, prost/prost-build/prost-types 0.14.4, protox 0.9.1, bytes 1.12.1, tokio-util 0.7.19, futures 0.3.34, futures-concurrency 7.7.1, async-trait 0.1.92, inventory 0.3.24, clap 4.6.7, dotenvy 0.15.7, human-errors 0.2.4, tracing 0.1.44, opentelemetry 0.32.0, filt-rs 1.1.3, jsonwebtoken 11.1.0, argon2 0.6.0, aes-gcm 0.11.1, sha2 0.11.0, hmac 0.13.0, base64 0.23.1, hex 0.4.3, rand 0.10.2, zeroize 1.9.0, bitvec 1.1.1, **rustls 0.23.45** (0.24 is dev-only), tokio-rustls 0.26.5, rustls-pki-types 1.15.1, rcgen 0.14.10, x509-parser 0.18.1, instant-acme 0.8.5, p12-keystore 0.3.2, pem 4.0.0, **rsa 0.9.10** (0.10 is rc), **zip 8.6.0** (9 is pre), rstest 0.27.0, wiremock 0.6.5, tempfile 3.27.0, yew 0.23.0, yew-router 0.20.0, gloo-net 0.7.0, gloo-utils 0.3.0, gloo-timers 0.4.0, wasm-bindgen 0.2.128, web-sys/js-sys 0.3.105, **trunk 0.21.14** (0.22 is beta), @playwright/test ^1.62 |
| TLS crypto provider | One provider workspace-wide: **aws-lc-rs** (default of rustls, tokio-rustls, reqwest 0.13, instant-acme; opt-in for rcgen/x509-parser/jsonwebtoken). automate already cross-builds aws-lc-rs for aarch64-musl via jsonwebtoken, so the cross path is proven. |
| protoc | Not needed anywhere: `rustak-cot` uses `protox` + `prost_build::Config::compile_fds`; tracing-batteries' opentelemetry-proto ships pre-generated code. automate's CI still installs protoc — rustak's CI will not, and step 1 verifies this. |

---

## 1. Workspace design

### 1.1 Root `Cargo.toml`

```toml
[workspace]
resolver = "3"
members = ["rustak-*"]
exclude = ["rustak-ui"]          # wasm32 target; built with trunk (own Cargo.lock)
default-members = ["rustak-server"]

[workspace.package]
version = "0.1.0"                # rewritten by the `version` CI job from the release tag
edition = "2024"
rust-version = "1.88"
license = "MIT"
authors = ["Sierra Softworks"]
repository = "https://github.com/SierraSoftworks/rustak"
homepage = "https://github.com/SierraSoftworks/rustak"
publish = false

[workspace.dependencies]
# ---- internal ----
rustak-cot    = { path = "rustak-cot" }
rustak-api    = { path = "rustak-api" }
rustak-core   = { path = "rustak-core" }
rustak-client = { path = "rustak-client" }
rustak-server = { path = "rustak-server" }

# ---- async / http ----
tokio = { version = "1.53.1", features = ["rt-multi-thread", "macros", "net", "io-util", "time", "fs", "signal", "sync", "tracing"] }
tokio-util = { version = "0.7.19", features = ["codec", "rt"] }
tokio-stream = "0.1.19"
futures = "0.3.34"
futures-concurrency = "7.7.1"
async-trait = "0.1.92"
bytes = "1.12.1"
actix-web = { version = "4.15.0", features = ["rustls-0_23"] }
actix-ws = "0.4.0"
actix-multipart = "0.8.2"
reqwest = { version = "0.13.5", default-features = false, features = ["rustls", "json", "form", "multipart", "stream", "http2", "charset"] }
url = "2.5.8"

# ---- tls / pki ----
rustls = { version = "0.23.45", default-features = false, features = ["aws_lc_rs", "logging", "std", "tls12"] }
tokio-rustls = { version = "0.26.5", default-features = false, features = ["aws_lc_rs", "logging", "tls12"] }
rustls-pki-types = { version = "1.15.1", features = ["std"] }
rcgen = { version = "0.14.10", default-features = false, features = ["crypto", "pem", "aws_lc_rs", "x509-parser"] }
x509-parser = { version = "0.18.1", features = ["verify-aws"] }
instant-acme = { version = "0.8.5", default-features = false, features = ["aws-lc-rs", "hyper-rustls"] }
p12-keystore = "0.3.2"
pem = "4.0.0"
rsa = { version = "0.9.10", features = ["getrandom", "sha2"] }   # RSA key generation (CA + JWT keys); rcgen cannot generate RSA

# ---- storage ----
rusqlite = { version = "0.40.2", features = ["bundled", "chrono", "serde_json"] }
tokio-rusqlite = "0.8.0"
include_dir = "0.7.4"

# ---- serialization ----
serde = { version = "1.0.229", features = ["derive"] }
serde_json = "1.0.151"
toml = "1.1.6"
chrono = { version = "0.4.45", features = ["serde"] }
uuid = { version = "1.26.1", features = ["v4", "serde"] }
quick-xml = { version = "0.42.0", features = ["serialize"] }
prost = "0.14.4"
prost-types = "0.14.4"
prost-build = "0.14.4"
protox = "0.9.1"
zip = { version = "8.6.0", default-features = false, features = ["deflate-flate2-zlib-rs"] }   # pure Rust, cross-friendly

# ---- identity / crypto ----
jsonwebtoken = { version = "11.1.0", features = ["aws_lc_rs"] }
argon2 = "0.6.0"
aes-gcm = "0.11.1"
sha2 = "0.11.0"
hmac = "0.13.0"
base64 = "0.23.1"
hex = "0.4.3"
rand = "0.10.2"
zeroize = { version = "1.9.0", features = ["derive"] }
filt-rs = { version = "1.1.3", features = ["serde"] }
bitvec = "1.1.1"

# ---- telemetry / errors / cli ----
tracing = "0.1.44"
tracing-batteries = { git = "https://github.com/sierrasoftworks/tracing-batteries-rs.git", features = ["opentelemetry", "sentry", "analytics", "testing", "human_errors"] }
human-errors = { version = "0.2.4", features = ["pretty", "force_backtraces"] }
opentelemetry = "0.32.0"
clap = { version = "4.6.7", features = ["derive", "cargo", "string", "env"] }
dotenvy = "0.15.7"
inventory = "0.3.24"

# ---- dev ----
rstest = "0.27.0"
wiremock = "0.6.5"
tempfile = "3.27.0"
pretty_assertions = "1.4.1"

[workspace.lints.rust]
unsafe_code = "forbid"
unused_must_use = "deny"
rust_2018_idioms = { level = "warn", priority = -1 }

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
dbg_macro = "warn"
todo = "warn"
print_stdout = "warn"          # binaries log through tracing; `main.rs` allows print_stderr for the pretty error
large_futures = "warn"

# The tests generate RSA keys (CA, JWT signing key, test IdP); bignum math is
# unusably slow unoptimised (same override automate carries).
[profile.dev.package.num-bigint-dig]
opt-level = 3
[profile.dev.package.rsa]
opt-level = 3

[profile.release]
lto = "thin"
codegen-units = 1
```

Every crate manifest uses only `foo = { workspace = true }` (or `foo.workspace = true`), inherits `[package]` fields with `.workspace = true`, and has `[lints] workspace = true`.

### 1.2 Per-crate manifests (responsibilities + dependency lists)

| Crate | Kind | Depends on (workspace) | Notes |
|---|---|---|---|
| `rustak-api` | lib, wasm-safe | serde, serde_json, chrono, uuid (no `v4`) | JSON contract + validated identity newtypes (`Username`, `DeviceUid`, `ServiceName`, `GroupName`, `Direction`, `MissionGuid`, typed row ids). **No** tokio/tracing/rusqlite. `uuid` here without `v4`/`js`; the UI turns on `uuid/js` itself if it ever mints ids. |
| `rustak-cot` | lib, no I/O | prost, prost-types, bytes, quick-xml, chrono, serde (fixtures/tests: rstest, pretty_assertions); build-deps: protox, prost-build | M0 ships only the build pipeline + `TakControl`/`TakMessage` skeleton; M1 fills the model. |
| `rustak-core` | lib | rustak-api, tokio (rt, signal, sync), tokio-util (rt: `CancellationToken`), tracing, tracing-batteries, human-errors, serde, serde_json, toml, chrono, dotenvy, argon2, rand, zeroize, sha2, base64, bitvec, url, uuid; dev: rstest, tempfile | Config loader, interpolation, telemetry, prelude, runtime `Shutdown`, credential primitives, `Principal`/`GroupSet`, service identity. |
| `rustak-client` | lib | rustak-core, rustak-cot, rustak-api, tokio, tokio-rustls, rustls, rustls-pki-types, reqwest, futures, tokio-util, bytes, serde, serde_json, url, tracing, human-errors | M0: `sidecar` module (`Sidecar` trait + `run()` harness that loads config, builds telemetry, waits for shutdown); stream/marti/control clients in M1/M2/M6. |
| `rustak-server` | lib + bin `rustak` | rustak-api, rustak-core, rustak-cot, actix-web, actix-ws, actix-multipart, tokio, tokio-util, futures, futures-concurrency, async-trait, reqwest, rustls, tokio-rustls, rustls-pki-types, rcgen, rsa, x509-parser, instant-acme, p12-keystore, pem, rusqlite, tokio-rusqlite, include_dir, serde, serde_json, toml, chrono, uuid, quick-xml, zip, jsonwebtoken, argon2, aes-gcm, sha2, hmac, base64, hex, rand, zeroize, filt-rs, bitvec, tracing, tracing-batteries, human-errors, opentelemetry, clap, dotenvy, inventory, url; dev: rustak-client, rstest, wiremock, tempfile, pretty_assertions; build-deps: none (std only) | `[lib] name = "rustak_server"`, `[[bin]] name = "rustak" path = "src/main.rs"`. `[features] testing = []` exposes mocks to integration tests in `tests/`. |
| `rustak-plugin-example` | bin | rustak-client, rustak-core, rustak-cot, tokio, serde, tracing, human-errors, clap | ~100 lines implementing `Sidecar`; has its own `config.example.toml` and `Dockerfile`. |
| `rustak-ui` | bin (wasm), excluded | rustak-api (path), yew 0.23 `csr`, yew-router 0.20, gloo-net 0.7, gloo-utils 0.3, gloo-timers 0.4 `futures`, futures 0.3, wasm-bindgen 0.2, wasm-bindgen-futures 0.4, js-sys 0.3, web-sys 0.3 (Window, Location, History, Storage, UrlSearchParams, Crypto, HtmlInputElement, HtmlSelectElement, InputEvent, Navigator, Clipboard, Element), base64 0.23, serde, serde_json, chrono `wasmbind`, log, wasm-logger, console_error_panic_hook | Cannot use `workspace = true` (excluded) — versions are pinned here and its `Cargo.lock` is committed; dependabot gets a second cargo entry for `/rustak-ui`. |

Dependency direction (one adjustment to the draft): `rustak-api ← rustak-core ← rustak-client ← rustak-plugin-*`; `rustak-cot` is a leaf; `rustak-server` uses all of api/core/cot (+ client as dev-dep); `rustak-ui ← rustak-api`. The adjustment: **validated identity newtypes live in `rustak-api` and `rustak-core::identity` re-exports them**, exactly as automate keeps `TenantId` in `automate-api` and re-exports it in the agent prelude. This is the only way the UI and the server share one `Username` type without making the API crate depend on tokio.

### 1.3 `rustak-cot/build.rs` (protox + prost-build, no protoc)

```rust
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("proto");
    println!("cargo:rerun-if-changed={}", proto_dir.display());

    // protox is a pure-Rust protoc: it parses the .proto files into a
    // FileDescriptorSet so prost-build never has to shell out to `protoc`.
    let descriptors = protox::compile([proto_dir.join("tak_protocol_v1.proto")], [&proto_dir])?;

    prost_build::Config::new()
        .bytes(["."])                   // `bytes` fields (detail xml) as `bytes::Bytes`, zero-copy on decode
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .compile_fds(descriptors)?;      // writes $OUT_DIR/rustak.cot.v1.rs
    Ok(())
}
```

`rustak-cot/proto/tak_protocol_v1.proto` is the clean-room definition (`syntax = "proto3"; package rustak.cot.v1;` — the package name is not on the wire; only field numbers/types are). `src/proto/mod.rs` = `include!(concat!(env!("OUT_DIR"), "/rustak.cot.v1.rs"));` plus a unit test that round-trips a `TakMessage`. M0 defines `TakControl{minProtoVersion=1,maxProtoVersion=2,contactUid=3}`, `TakMessage{takControl=1,cotEvent=2}`, `CotEvent` with the documented field numbers and an empty `Detail` — enough to prove the pipeline; M1 completes it.

### 1.4 `rustak-server/build.rs`

Verbatim port of `automate/agent/build.rs` with the path changed to `../rustak-ui/dist` (creates the directory so `include_dir!` compiles on a fresh clone; `cargo:rerun-if-changed=../rustak-ui/dist`).

### 1.5 Supporting root files

- `.cargo/config.toml`: `[target.aarch64-unknown-linux-musl] linker = "rust-lld"` (lift).
- `.gitignore`: lift automate's, renamed paths (`rustak-ui/dist/`, `*.sqlite*`, `*.sqlite.key`, `e2e/node_modules/` …, `data/`).
- `Cross.toml`: see §7.
- `config.example.toml` (server) at the root, `rustak-plugin-example/config.example.toml` for the sidecar.
- `LICENSE` (MIT), `README.md`, `docs/{deployment.md, plugins.md, compat/*.md}` (appendix A moves here).

---

## 2. `rustak-api` and `rustak-core`

### 2.1 `rustak-api/src/` (every file < 300 functional lines)

| File | Contents |
|---|---|
| `lib.rs` | module list + `pub use` of every DTO |
| `identity/mod.rs` | re-exports |
| `identity/username.rs` | `Username` (trim; reject empty, `/`, control chars, whitespace, `>190`, reserved prefix `!`; **case preserved**, `Username::key()` lowercases for comparison; serde as string; `FromStr`). CN preservation matters for TAK client certs. |
| `identity/uid.rs` | `DeviceUid` (ATAK `clientUid`; non-empty, no whitespace, ≤ 256), `ServiceName` (`^[a-z0-9][a-z0-9-]{1,62}$`) with `fn uid(&self) -> DeviceUid` = `SERVICE-<name>`, `MissionGuid` (uuid string) |
| `identity/group.rs` | `GroupName` (`__ANON__` constant), `Direction { In, Out, Both }` with TAK wire strings |
| `identity/ids.rs` | `define_id!(UserId, DeviceId, GroupId, CertificateId, CredentialId, ServiceId, MissionId, ResourceId, ProfileId)` → `#[repr(transparent)] i64` newtypes with `From<i64>/Into<i64>`, Display, serde as number |
| `auth.rs` | `AuthMetadata { mode: AuthMode }`, `AuthMode::{Oidc{authorization_endpoint, client_id, scopes}, Local}`, `TokenExchangeRequest{code, redirect_uri}`, `TokenRefreshRequest{refresh_token}`, `LocalLoginRequest{username, password}`, `TokenResponse{token, refresh_token, expires_in, token_type}`, `Me{username, display_name, email, kind, is_admin, via}` , `AuthVia::{Bearer, ClientCert, Basic, Session}` |
| `setup.rs` | `SetupStatus{needs_setup, has_admin, has_ca, has_server_name, setup_completed, version}`, `CreateAdminRequest{username, password, display_name}`, `InitCaRequest{common_name, organization, key_type}`, `CaKeyType::{Rsa2048, EcdsaP256}`, `CaSummary{subject, fingerprint, not_before, not_after}`, `ServerSettingsRequest{name, domains, base_url}` |
| `user.rs` | `User{id, username, kind, display_name, email, is_admin, disabled, source, created_at, last_seen_at}`, `UserKind::{Person, Service}`, `UserSource::{Local, Oidc, Service}`, `UserPatch{disabled, is_admin, display_name}` |
| `group.rs` | `Group{id, name, bitpos, description, source}`, `GroupMembership{group, direction}` |
| `device.rs` | `Device{id, uid, username, callsign, platform, version, device_model, first_seen_at, last_seen_at, last_ip}` |
| `credential.rs` | `CredentialKind::{DevicePassword, EnrollmentToken, ServiceToken}`, `Credential{id, kind, label, expires_at, max_uses, uses, last_used_at, revoked_at, created_at}`, `CreateCredentialRequest{kind, label, expires_in_days, max_uses}`, `CredentialCreated{credential, secret, enroll_url}` |
| `certificate.rs` | `CertificateKind::{Ca, Server, Client, Service}`, `Certificate{id, kind, serial, fingerprint, subject_cn, san, not_before, not_after, revoked_at, source}` |
| `service.rs` | `ServiceDescriptor{name, display_name, version, capabilities: Vec<Capability>, endpoints: ServiceEndpoints}`, `Capability` (string newtype), `ServiceEndpoints{stream, marti, control}`, `ServiceState::{Unknown, Healthy, Degraded, Unhealthy}`, `ServiceStatus{state, message, last_heartbeat_at}`, `Heartbeat{state, message, metrics: serde_json::Value}`, `ServiceSummary` |
| `audit.rs` | lift `AuditOutcome`, `AuditRecord` (drop `tenant`), new `AuditCategory::{Authentication, Enrollment, Administration, Pki, Stream, Mission, Package, Profile, Service, System}` with `as_str/label/parse/ALL` |
| `health.rs` | `Health{status, version, uptime_seconds, database: ComponentStatus}` |
| `settings.rs` | `ServerSettings{name, domains, base_url, node_id, setup_completed_at}` |
| `error.rs` | `ApiErrorBody{error, code: Option<String>}` (the `{"error": …}` shape automate's UI already parses) |

### 2.2 `rustak-core/src/`

| File | Contents (≈ functional lines) |
|---|---|
| `lib.rs` | `pub mod config; pub mod telemetry; pub mod runtime; pub mod identity; pub mod service; pub mod prelude; pub mod errors;` |
| `prelude.rs` | `pub use human_errors::{self, ResultExt}; pub use tracing_batteries::prelude::*; pub use serde::{Serialize, Deserialize, de::DeserializeOwned}; pub use crate::identity::*; pub use crate::runtime::Shutdown;` |
| `errors.rs` | shared advice slices (`ADVICE_REPORT_DEV`, `ADVICE_FILE_ACCESS`), `pub fn report_and_exit(err: &human_errors::Error, session: Option<&Session>) -> !` (pretty print, record if `Kind::System`, exit 1) |
| `config/mod.rs` | `pub fn load<T: DeserializeOwned>(path: impl Into<PathBuf>) -> Result<T>` (read → `interpolate` with `env::resolve` → `toml::from_str`, automate's error advice), `load_str<T>`, `load_env_file(path)` — **adds a guard**: only load when `metadata.is_file()`, so a FIFO like automate's `.env` cannot block startup (the bug automate's e2e script works around). (≈110) |
| `config/interpolation.rs` | verbatim lift of `agent/src/parsers/interpolation.rs` (205 functional lines + rstest cases) |
| `config/env.rs` | `resolve(expr) -> Result<String>`: `env.X` → value or the literal `${{ env.X }}` left in place (automate behaviour), anything else → user error (≈40) |
| `config/duration.rs` | `humane` / `humane_option` serde adapters for `chrono::Duration` accepting `"30d" \| "12h" \| "15m" \| "45s"` and plain integer seconds; refuse negative/fractional (patterned on automate's `serde_duration.rs`) (≈120) |
| `config/listen.rs` | `ListenAddr` (`"host:port"`, `":8446"` → `0.0.0.0`, `"[::]:8446"`), serde string, `to_socket_addrs()`; replaces automate's inline `split_once(':')` (≈80) |
| `telemetry.rs` | `pub use tracing_batteries::Session;` `TelemetryOptions{ sentry_dsn: Option<String>, analytics_url: Option<String>, stdout: bool }`, `pub fn bootstrap(app: &'static str, version: &'static str, opts) -> Arc<Session>` (OpenTelemetry `.with_stdout`, optional Sentry, optional Analytics), `pub async fn shutdown(session: Arc<Session>)` (the 40×50 ms `Arc::try_unwrap` loop from automate `main.rs`), `#[cfg(any(test, feature = "testing"))] pub fn testing_session(app)` (≈90). DSN source: `option_env!("RUSTAK_SENTRY_DSN")` overridden by the `RUSTAK_SENTRY_DSN` env var at runtime — telemetry must exist before the config file is parsed so config errors are reported. |
| `runtime.rs` | `Shutdown` = clonable wrapper over `tokio_util::sync::CancellationToken`: `Shutdown::new()`, `listen_for_signals()` (spawns a task awaiting `ctrl_c()` and, on unix, `SignalKind::terminate()`, then `cancel()`; second signal → `std::process::exit(130)`), `cancelled()`, `child()`, `is_cancelled()`; `pub async fn with_grace(fut, timeout)` (≈90) |
| `identity/mod.rs` | `pub use rustak_api::identity::*;` + submodules |
| `identity/secret.rs` | `Secret(String)` (zeroize on drop, `Debug` = `Secret(***)`, `expose()`), `generate_secret(bytes: usize) -> Secret` (rand → base64url), `generate_password(words: usize)`-style readable device passwords (≈70) |
| `identity/password.rs` | `PasswordHash` (PHC string newtype), `hash(secret) -> Result<PasswordHash>` (argon2id, `Argon2::default()`), `verify(secret, &PasswordHash) -> bool`, `lookup_hint(secret) -> String` (first 16 hex of sha256 — lets a credential row be found before running argon2), `pub async fn hash_blocking / verify_blocking` wrappers using `spawn_blocking` (≈110) |
| `identity/groups.rs` | `GroupSet { bits_in: BitVec, bits_out: BitVec }`, `set(bitpos, Direction)`, `can_reach(sender: &GroupSet, receiver: &GroupSet) -> bool` (`sender.in ∧ receiver.out ≠ 0`), `to_bytes()/from_bytes()` (BLOB storage), `names()` via a `GroupIndex` (bitpos ↔ name) (≈120) |
| `identity/principal.rs` | `Principal { user_id: UserId, username: Username, kind: PrincipalKind, device: Option<DeviceUid>, groups: Arc<GroupSet>, is_admin: bool, via: AuthMethod }`, `PrincipalKind::{Person, Service, Anonymous}`, `AuthMethod::{ClientCert{serial}, Bearer{jti}, Basic{credential_id}, StreamAuth, Anonymous}`; `Principal::anonymous()` (≈100) |
| `service.rs` | runtime identity for sidecars: `ServiceIdentity { name: ServiceName, uid: DeviceUid, credential: Option<Secret>, cert: Option<PathBuf>, key: Option<PathBuf>, truststore: Option<PathBuf> }` + `impl From<&ServiceIdentity> for ServiceDescriptor` helpers; re-export `rustak_api::service::*` (≈70) |

---

## 3. `rustak-server` bootstrap

### 3.1 File list (M0)

```
rustak-server/
├── Cargo.toml, build.rs
├── migrations/0001_kv_queues_audit.sql … 0007_profiles.sql       (§4.5)
├── src/main.rs           clap Args{--config (default config.toml), --env (default .env), --check}; env file;
│                         telemetry::bootstrap("rustak", CARGO_PKG_VERSION); rustak_server::run(args); telemetry::shutdown; exit code
├── src/lib.rs            pub mods; `pub async fn run(config: Config, session: Arc<Session>, shutdown: Shutdown) -> Result<()>`;
│                         `pub async fn build_context(...) -> Result<AppContext>` (used by integration tests)
├── src/prelude.rs        rustak_core::prelude::* + crate::{config::Config, services::{AppContext, Services}, db::{KeyValueStore, Queue, Cache, AuditStore}, jobs::{Job, JobContext}}
├── src/runtime.rs        `run_all(ctx)`: builds public HttpServer(s) [+ marti in M2, listeners in M1], JobHost, checkpoint task;
│                         `(web, jobs, housekeeping).try_join()`; a cancelled `Shutdown` stops actix via `ServerHandle::stop(true)`;
│                         any Err cancels the token so siblings stop; final `db.close()` (WAL TRUNCATE checkpoint)
├── src/config/{mod,server,storage,web,stream,auth,pki,acme,retention}.rs   (§3.3)
├── src/services/mod.rs   AppContext + Services trait;   services/mock.rs (cfg(test) or feature "testing")
├── src/db/{mod,connection,migrations,row,kv,queue,queue_sqlite,cache,partition,audit}.rs ; db/repos/{mod,users,groups,devices,certificates,credentials,services,oauth_keys,oauth_tokens,settings}.rs
├── src/crypto/{mod,key,store,context,keyfile}.rs
├── src/files/store.rs    ContentStore (content-addressed dir)         ← storage foundation, used from M3
├── src/pki/{mod,keys,ca}.rs                                            ← minimal: create/load root CA for the setup wizard
├── src/auth/{mod,jwt,local}.rs                                         ← RS256 issue/verify, local username/password login
├── src/web/{mod,server,tls,ui,telemetry,principal}.rs ; web/helpers/{mod,request,json}.rs ; web/helpers/oidc/{mod,discovery,validate,exchange,claims}.rs
├── src/web/api/{mod,middleware,error,health,auth,me,setup,users,audit,settings}.rs
├── src/jobs/{mod,job,runnable,host,audit_prune,wal_checkpoint}.rs
├── src/testing/{mod,oidc}.rs   (cfg(test)/feature "testing")
└── tests/bootstrap.rs     in-process: temp dir → run() → /robots.txt 200, /api/v1/health, setup wizard, cancel token → exits < 10 s
```

### 3.2 `AppContext` / `Services`

rustak is single-tenant, so automate's `ServicesContainer<D>`/`TenantDb` generics collapse:

```rust
#[derive(Clone)]
pub struct AppContext {
    config: Arc<Config>,
    db: Database,                       // §4.1
    secrets: Arc<SecretStore>,
    content: Arc<ContentStore>,
    session: Arc<Session>,
    http_client: reqwest::Client,       // user agent "SierraSoftworks/rustak"
    shutdown: Shutdown,
    started_at: DateTime<Utc>,
    jwt: Arc<JwtKeys>,                  // loaded/created at startup from oauth_keys
}

pub trait Services: Send + Sync + 'static {
    fn config(&self) -> Arc<Config>;
    fn session(&self) -> &Session;
    fn secrets(&self) -> &SecretStore;
    fn db(&self) -> &Database;
    fn content(&self) -> &ContentStore;
    fn http_client(&self) -> reqwest::Client;
    fn shutdown(&self) -> &Shutdown;
    fn kv(&self) -> impl KeyValueStore + Clone + Send + Sync + 'static;
    fn queue(&self) -> impl Queue + Clone + Send + Sync + 'static;
    fn cache(&self) -> impl Cache + Clone + Send + Sync + 'static;
    fn audit(&self) -> impl AuditStore + Clone + Send + Sync + 'static;
}
impl Services for AppContext { … }   impl<S: Services> Services for &S { … }   // as automate
```

`AppContext::new_mock(|cfg| …)` (in-memory DB, `SecretStore::ephemeral()`, `Testing` battery, temp `ContentStore`) mirrors automate.

### 3.3 Config schema (`deny_unknown_fields` on every struct; defaults written out in `impl Default`, as automate documents)

```toml
# config.example.toml — every key shown with its default unless marked (required)

[server]
name = "rustak"                       # shown to clients (Marti version/config), overridable from the setup wizard
domains = ["tak.example.com"]        # public host names; first is the canonical one used in QR codes and .pref files
# base_url = "https://tak.example.com:8446"   # inferred from server.domains / request Host when omitted
trust_proxy = false                   # honour X-Forwarded-* only behind a trusted proxy
data_dir = "./data"                   # database, key file, content store, ACME cache live here

[storage]
# database = "<data_dir>/rustak.sqlite"
# content_dir = "<data_dir>/content"
reader_connections = 2                # read-only WAL connections; 0 = reads share the writer
busy_timeout = "5s"
checkpoint_interval = "5m"            # PASSIVE wal_checkpoint job cadence

[web.public]                          # browser UI, /api/v1, /oauth/*, /login/*, /Marti/api/tls/* (enrollment)
listen = [":8446"]                    # add ":443" for a browser-friendly port (needs CAP_NET_BIND_SERVICE)
[web.public.tls]
mode = "none"                         # "none" (dev / behind a TLS proxy) | "files" | "acme"
# cert_file = "/etc/rustak/fullchain.pem"
# key_file  = "/etc/rustak/privkey.pem"

[web.marti]                           # full Marti API over the internal CA (M2)
enabled = true
listen = ":8443"
client_cert = "optional"              # "optional" | "required"

[stream.tls]                          # CoT stream, internal CA, client cert required (M1)
enabled = true
listen = ":8089"
idle_timeout = "90s"
[stream.tcp]
enabled = false
listen = ":8087"
allow_anonymous = false

[auth]
# oidc = { endpoint = "https://id.example.com", client_id = "rustak", client_secret = "${{ env.RUSTAK_OIDC_CLIENT_SECRET }}",
#          scopes = ["profile", "email", "offline_access"], username_claim = "preferred_username", groups_claim = "groups" }
# user_acl  = 'true'                  # filt-rs over method/path/client_ip/headers.*/claims.*; who may sign in to the UI (default: deny)
# admin_acl = 'claims.groups contains "tak-admins"'   # OR the users.is_admin flag set by the wizard/CLI (default: deny)
access_token_ttl = "1h"
refresh_token_ttl = "30d"
# secret_key = "${{ env.RUSTAK_SECRET_KEY }}"        # AES-256 key (base64/hex); generated into <database>.key when absent
# previous_secret_keys = []

[pki]
ca_common_name = "rustak CA"
organization = "rustak"
key_type = "rsa-2048"                 # "rsa-2048" | "ecdsa-p256" — RSA by default for TAK ecosystem truststores
ca_validity = "3650d"
client_cert_validity = "365d"
server_cert_validity = "397d"

[acme]                                # public certificate for [web.public] when tls.mode = "acme" (M2)
enabled = false
directory = "https://acme-v02.api.letsencrypt.org/directory"
# contact_email = "ops@example.com"
# domains = []                        # defaults to server.domains
challenge = "http-01"                 # "http-01" | "tls-alpn-01"
renew_before = "30d"

[retention]
cot_history = "7d"
cot_history_max_rows = 2000000
audit = "90d"
audit_max_entries = 100000
archived_missions = "30d"
content_orphans = "24h"
```

Rust shape: `Config { server: ServerConfig, storage: StorageConfig, web: WebConfig{ public: PublicWebConfig, marti: MartiWebConfig }, stream: StreamConfig{ tls, tcp }, auth: AuthConfig, pki: PkiConfig, acme: AcmeConfig, retention: RetentionConfig }`, one file per section; `Config::load(path)` delegates to `rustak_core::config::load::<Config>` then `validate()` (e.g. `acme.enabled` requires `web.public.tls.mode = "acme"`, non-empty `server.domains` when ACME on). Tests: the `include_str!("../../config.example.toml")` schema test, misplaced-key-refused test, defaults-agree-with-serde test — all lifted from `agent/src/config.rs`.

### 3.4 `main.rs` → `run()` → graceful shutdown

1. `Args::parse()`; `rustak_core::config::load_env_file(args.env)` (exit 2 on error).
2. `let session = telemetry::bootstrap("rustak", env!("CARGO_PKG_VERSION"), TelemetryOptions::from_env())`.
3. `let shutdown = Shutdown::new(); shutdown.listen_for_signals();`
4. `let result = rustak_server::run(Config::load(args.config)?, session.clone(), shutdown).await` — `--check` returns after `Config::load` + `validate`.
5. `telemetry::shutdown(session).await; exit(if err {1} else {0})`.

`run()`: `rustls::crypto::aws_lc_rs::default_provider().install_default().ok()` (defensive: prevents the "no default CryptoProvider" panic if a transitive dep ever enables `ring`); `Database::open`; `SecretStore::load`; `ContentStore::open`; `JwtKeys::load_or_create`; `AppContext::new`; `runtime::run_all(ctx).await`.

`runtime::run_all`: builds `HttpServer` per configured public `listen` entry (one `HttpServer` with multiple `.bind()`/`.bind_rustls_0_23()` calls — one server, N sockets) with `.disable_signals().shutdown_timeout(10)`; obtains `server.handle()`; spawns `{ shutdown.cancelled().await; handle.stop(true).await }`; `JobHost::run(ctx)` selects on `shutdown.cancelled()` around `dequeue_any` and then `JoinSet::shutdown()`; `(web, jobs, checkpoints).try_join().await` from `futures_concurrency` — first `Err` cancels the token and is returned; on clean cancellation everything returns `Ok(())`; then `ctx.db().close().await` runs `PRAGMA wal_checkpoint(TRUNCATE)`.

---

## 4. SQLite layer

### 4.1 Connection handling — recommendation: **one writer + a tiny read-only pool**

```rust
#[derive(Clone)]
pub struct Database { writer: Arc<tokio_rusqlite::Connection>, readers: Arc<Readers> /* Vec<Connection> + AtomicUsize round-robin; empty for :memory: */ }

impl Database {
    pub async fn open(cfg: &StorageConfig) -> Result<Self>;         // file: WAL + pool
    pub async fn open_in_memory() -> Result<Self>;                  // tests: readers = writer
    pub async fn read<T: Send + 'static>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static) -> Result<T>;
    pub async fn write<T: Send + 'static>(&self, f: impl FnOnce(&mut Transaction<'_>) -> rusqlite::Result<T> + Send + 'static) -> Result<T>; // BEGIN IMMEDIATE … COMMIT
    pub async fn checkpoint(&self, mode: Checkpoint /* Passive | Truncate */) -> Result<()>;
    pub async fn close(self) -> Result<()>;
}
```

Why not a single connection: `tokio-rusqlite` serialises every closure onto one thread, so a burst of CoT history writes would also queue every web read behind them. Why not a big pool: SQLite has one writer anyway; readers only need a couple of connections under WAL, and each extra connection is a thread. Two readers (`storage.reader_connections`) is the default; `0` collapses to the writer (also what `:memory:` uses, since an in-memory database is private to its connection).

Pragmas (`db/connection.rs`, per connection unless noted): `busy_timeout = 5s` (set first, as automate explains), `journal_mode = WAL` (file only; per-database; warn-and-continue if it does not stick — automate's NFS reasoning), `synchronous = NORMAL` only when WAL applied, **`foreign_keys = ON`** (the schema uses `REFERENCES`, unlike automate), `temp_store = MEMORY`, `journal_size_limit = 67108864`, readers opened with `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX` and `query_only = 1`. The writer also runs `PRAGMA optimize` at close.

### 4.2 Migration runner (`db/migrations.rs`)

- `static MIGRATIONS: Dir = include_dir!("$CARGO_MANIFEST_DIR/migrations");` files named `NNNN_snake_name.sql`; the runner sorts by numeric prefix and asserts contiguity from 1 (a unit test also asserts the filename regex).
- Table: `schema_migrations (id INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL) STRICT`.
- Each file runs in its own transaction via `execute_batch`, then the row is inserted — same loop as automate's `initialize()`, with the file's name in the error message.
- Test-only: `open_in_memory_at_migration(n)`, `upgrade()`, `schema_of(table)` (columns + indexes) — lifted, plus `PRAGMA foreign_key_check` and `PRAGMA integrity_check` after a full migrate, and the "upgraded == fresh" parity test.
- Rule for later milestones: never edit an applied `000N` file; add `0008_…sql`. Rebuilding a table (SQLite cannot alter PKs) follows automate's `kv_migrated` pattern.

### 4.3 Repository pattern (`db/repos/*.rs`, one aggregate per file)

```rust
pub struct UsersRepo<'a> { db: &'a Database }
impl Database { pub fn users(&self) -> UsersRepo<'_> { UsersRepo { db: self } } }

#[derive(Debug, Clone)]
pub struct UserRow { pub id: UserId, pub username: Username, pub kind: UserKind, pub display_name: Option<String>, pub email: Option<String>,
                     pub is_admin: bool, pub disabled: bool, pub source: UserSource, pub oidc_subject: Option<String>,
                     pub password_hash: Option<PasswordHash>, pub created_at: DateTime<Utc>, pub updated_at: DateTime<Utc>, pub last_seen_at: Option<DateTime<Utc>> }
const COLUMNS: &str = "id, username, kind, display_name, email, is_admin, disabled, source, oidc_subject, password_hash, created_at, updated_at, last_seen_at";
impl UserRow { fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> }

impl UsersRepo<'_> {
    pub async fn create(&self, new: NewUser) -> Result<UserRow>;                    // INSERT … RETURNING <COLUMNS>
    pub async fn get(&self, id: UserId) -> Result<Option<UserRow>>;
    pub async fn get_by_username(&self, name: &Username) -> Result<Option<UserRow>>;
    pub async fn upsert_oidc(&self, subject: &str, username: &Username, profile: OidcProfile) -> Result<UserRow>;
    pub async fn list(&self, page: Page) -> Result<Vec<UserRow>>;
    pub async fn count_admins(&self) -> Result<u64>;
    pub async fn set_disabled / set_admin / touch_last_seen(...);
}
```

Conventions: no ORM; SQL strings live beside the row type; `db/row.rs` provides `json_col::<T>(row, idx)`, `id_col::<T: From<i64>>`, `bool_col`, `opt_ts`; timestamps are always bound from Rust as `chrono::DateTime<Utc>` (RFC 3339 with millis) — never `CURRENT_TIMESTAMP` (second resolution, and `DATETIME` is not a STRICT type). DTO conversion (`UserRow → rustak_api::User`) lives in `web/api/users.rs`, keeping repos free of wire concerns. `kv`/`queues`/`audit_log` keep automate's trait-based API (`KeyValueStore`, `Queue`, `Cache`, `AuditStore`, `Partition`) with the `tenant` column removed; `queue_sqlite.rs` holds the impl so `queue.rs` (traits + message types) stays under 300 lines.

### 4.4 JSON-vs-column rule

A value gets its own **column** when it is filtered, sorted or joined on; constrained (`UNIQUE`, `REFERENCES`, `NOT NULL`, `CHECK`); or updated independently of its siblings. A value is stored as **`TEXT` JSON with `CHECK (json_valid(...))`** when it is an opaque, read-and-written-whole blob owned by exactly one row (raw CoT XML is `TEXT` XML; mission `externalData`/`feeds`, ATAK `details` dicts, JWK/ACME credential envelopes, `Sealed` secrets, per-device active-channel state, profile preference maps). Arrays that must be searched get a child table (`resource_keywords`, `group_members`), never delimited strings. Group reachability bit-vectors are `BLOB`. All tables are `STRICT`; pure-key tables are `WITHOUT ROWID`.

### 4.5 Initial schema DDL (7 files)

**`0001_kv_queues_audit.sql`** (lifted from automate, single-tenant, STRICT)
```sql
CREATE TABLE kv (
  partition  TEXT NOT NULL, key TEXT NOT NULL,
  value      TEXT NOT NULL CHECK (json_valid(value)),
  updated_at TEXT NOT NULL,
  PRIMARY KEY (partition, key)) STRICT, WITHOUT ROWID;

CREATE TABLE queues (
  partition TEXT NOT NULL, key TEXT NOT NULL,
  payload TEXT NOT NULL CHECK (json_valid(payload)),
  scheduled_at TEXT NOT NULL, hidden_until TEXT NOT NULL,
  reserved_by TEXT, traceparent TEXT, tracestate TEXT, idempotency_key TEXT,
  attempts INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (partition, key)) STRICT, WITHOUT ROWID;
CREATE INDEX idx_queues_partition_hidden ON queues (partition, hidden_until);
CREATE INDEX idx_queues_hidden_scheduled ON queues (hidden_until, scheduled_at);

CREATE TABLE audit_log (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  occurred_at TEXT NOT NULL, category TEXT NOT NULL, action TEXT NOT NULL, outcome TEXT NOT NULL,
  actor TEXT, subject TEXT, message TEXT,
  detail TEXT CHECK (detail IS NULL OR json_valid(detail))) STRICT;
CREATE INDEX idx_audit_subject  ON audit_log (subject, id DESC);
CREATE INDEX idx_audit_category ON audit_log (category, id DESC);
CREATE INDEX idx_audit_actor    ON audit_log (actor, id DESC);
```

**`0002_identity.sql`**
```sql
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  username TEXT NOT NULL COLLATE NOCASE,
  kind TEXT NOT NULL CHECK (kind IN ('person','service')),
  display_name TEXT, email TEXT,
  is_admin INTEGER NOT NULL DEFAULT 0 CHECK (is_admin IN (0,1)),
  disabled INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0,1)),
  source TEXT NOT NULL CHECK (source IN ('local','oidc','service')),
  oidc_subject TEXT, password_hash TEXT,
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL, last_seen_at TEXT) STRICT;
CREATE UNIQUE INDEX idx_users_username ON users (username);
CREATE UNIQUE INDEX idx_users_oidc_subject ON users (oidc_subject) WHERE oidc_subject IS NOT NULL;

CREATE TABLE groups (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, bitpos INTEGER NOT NULL, description TEXT,
  source TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual','oidc','system')),
  created_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_groups_name ON groups (name);
CREATE UNIQUE INDEX idx_groups_bitpos ON groups (bitpos);
INSERT INTO groups (id, name, bitpos, description, source, created_at)
  VALUES (1, '__ANON__', 0, 'Default channel', 'system', strftime('%Y-%m-%dT%H:%M:%fZ','now'));

CREATE TABLE group_members (
  group_id INTEGER NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
  user_id  INTEGER NOT NULL REFERENCES users(id)  ON DELETE CASCADE,
  direction TEXT NOT NULL CHECK (direction IN ('IN','OUT','BOTH')),
  source TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual','oidc')),
  PRIMARY KEY (group_id, user_id)) STRICT, WITHOUT ROWID;
CREATE INDEX idx_group_members_user ON group_members (user_id);

CREATE TABLE devices (
  id INTEGER PRIMARY KEY, uid TEXT NOT NULL,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  callsign TEXT, platform TEXT, version TEXT, device_model TEXT, os TEXT,
  active_groups TEXT CHECK (active_groups IS NULL OR json_valid(active_groups)),  -- PUT /groups/active state, per clientUid
  incognito INTEGER NOT NULL DEFAULT 0,
  first_seen_at TEXT NOT NULL, last_seen_at TEXT NOT NULL, last_ip TEXT) STRICT;
CREATE UNIQUE INDEX idx_devices_uid ON devices (uid);
CREATE INDEX idx_devices_user ON devices (user_id);

CREATE TABLE certificates (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('ca','server','client','service')),
  source TEXT NOT NULL DEFAULT 'internal' CHECK (source IN ('internal','acme','file')),
  serial TEXT NOT NULL, fingerprint TEXT NOT NULL, subject_cn TEXT NOT NULL,
  san TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(san)),
  user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
  device_id INTEGER REFERENCES devices(id) ON DELETE SET NULL,
  issuer_id INTEGER REFERENCES certificates(id) ON DELETE SET NULL,
  cert_pem TEXT NOT NULL,
  key_sealed TEXT CHECK (key_sealed IS NULL OR json_valid(key_sealed)),          -- Sealed envelope; CA/server/service keys only
  not_before TEXT NOT NULL, not_after TEXT NOT NULL,
  revoked_at TEXT, revocation_reason TEXT, created_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_certificates_fingerprint ON certificates (fingerprint);
CREATE UNIQUE INDEX idx_certificates_issuer_serial ON certificates (issuer_id, serial);
CREATE INDEX idx_certificates_user ON certificates (user_id);
CREATE INDEX idx_certificates_device ON certificates (device_id);
CREATE INDEX idx_certificates_expiry ON certificates (kind, not_after) WHERE revoked_at IS NULL;

CREATE TABLE credentials (
  id INTEGER PRIMARY KEY,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('device_password','enrollment_token','service_token')),
  label TEXT NOT NULL,
  secret_hash TEXT NOT NULL,             -- argon2id PHC string (never sealed: a hash is not recoverable)
  lookup_hint TEXT NOT NULL,             -- sha256(secret)[..16] hex: find the candidate row before running argon2
  max_uses INTEGER, uses INTEGER NOT NULL DEFAULT 0,
  expires_at TEXT, last_used_at TEXT, revoked_at TEXT,
  created_by TEXT, created_at TEXT NOT NULL) STRICT;
CREATE INDEX idx_credentials_user ON credentials (user_id, kind) WHERE revoked_at IS NULL;
CREATE INDEX idx_credentials_hint ON credentials (lookup_hint);

CREATE TABLE services (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,     -- users.kind = 'service'
  display_name TEXT, description TEXT, version TEXT,
  capabilities TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(capabilities)),
  endpoints TEXT CHECK (endpoints IS NULL OR json_valid(endpoints)),
  config TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config)),          -- control-API per-service config KV
  status TEXT NOT NULL DEFAULT 'unknown' CHECK (status IN ('unknown','healthy','degraded','unhealthy')),
  status_message TEXT, last_heartbeat_at TEXT,
  enabled INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL, updated_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_services_name ON services (name);
CREATE UNIQUE INDEX idx_services_user ON services (user_id);
```

**`0003_auth_pki.sql`**
```sql
CREATE TABLE oauth_keys (                       -- our signing keys
  kid TEXT PRIMARY KEY,
  alg TEXT NOT NULL CHECK (alg IN ('RS256','HS256')),
  purpose TEXT NOT NULL CHECK (purpose IN ('access_token','mission_token')),
  public_jwk TEXT CHECK (public_jwk IS NULL OR json_valid(public_jwk)),
  private_sealed TEXT NOT NULL CHECK (json_valid(private_sealed)),
  created_at TEXT NOT NULL, retired_at TEXT) STRICT, WITHOUT ROWID;
CREATE INDEX idx_oauth_keys_active ON oauth_keys (purpose, created_at DESC) WHERE retired_at IS NULL;

CREATE TABLE oauth_tokens (                     -- refresh tokens, auth codes, pending IdP states
  id TEXT PRIMARY KEY,                          -- jti / code / state (32 random bytes, base64url)
  kind TEXT NOT NULL CHECK (kind IN ('refresh','code','idp_state')),
  user_id INTEGER REFERENCES users(id) ON DELETE CASCADE,
  client_id TEXT, scope TEXT, redirect_uri TEXT,
  token_hash TEXT,                              -- sha256 of the opaque secret handed out (high entropy → no argon2)
  data TEXT CHECK (data IS NULL OR json_valid(data)),   -- nonce, PKCE, sealed IdP refresh token
  created_at TEXT NOT NULL, expires_at TEXT NOT NULL, consumed_at TEXT) STRICT, WITHOUT ROWID;
CREATE INDEX idx_oauth_tokens_user ON oauth_tokens (user_id, kind);
CREATE INDEX idx_oauth_tokens_expires ON oauth_tokens (expires_at);

CREATE TABLE acme_accounts (
  id INTEGER PRIMARY KEY, directory_url TEXT NOT NULL, contact TEXT NOT NULL, account_url TEXT NOT NULL,
  credentials_sealed TEXT NOT NULL CHECK (json_valid(credentials_sealed)),   -- instant_acme::AccountCredentials, sealed
  created_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_acme_accounts_directory_contact ON acme_accounts (directory_url, contact);
```

**`0004_cot.sql`**
```sql
CREATE TABLE cot_latest (
  uid TEXT PRIMARY KEY, type TEXT NOT NULL, callsign TEXT,
  user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
  device_id INTEGER REFERENCES devices(id) ON DELETE SET NULL,
  group_bits BLOB NOT NULL,                     -- sender's IN groups at send time (GroupSet bytes) for replay reachability
  time TEXT NOT NULL, start TEXT NOT NULL, stale TEXT NOT NULL,
  lat REAL, lon REAL, hae REAL, ce REAL, le REAL,
  xml TEXT NOT NULL, received_at TEXT NOT NULL) STRICT, WITHOUT ROWID;
CREATE INDEX idx_cot_latest_stale ON cot_latest (stale);
CREATE INDEX idx_cot_latest_type ON cot_latest (type);
CREATE INDEX idx_cot_latest_device ON cot_latest (device_id);

CREATE TABLE cot_history (
  id INTEGER PRIMARY KEY, uid TEXT NOT NULL, type TEXT NOT NULL, time TEXT NOT NULL,
  lat REAL, lon REAL, hae REAL, xml TEXT NOT NULL, received_at TEXT NOT NULL) STRICT;
CREATE INDEX idx_cot_history_uid_time ON cot_history (uid, time);
CREATE INDEX idx_cot_history_received ON cot_history (received_at);      -- retention prune
```

**`0005_files.sql`**
```sql
CREATE TABLE resources (                        -- metadata; bytes live at <content_dir>/<hash[0..2]>/<hash>
  id INTEGER PRIMARY KEY, hash TEXT NOT NULL, uid TEXT NOT NULL, name TEXT NOT NULL,
  mime_type TEXT NOT NULL, size INTEGER NOT NULL, tool TEXT, creator_uid TEXT,
  submitter_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
  submission_time TEXT NOT NULL, expiration INTEGER,          -- epoch ms; NULL = never (TAK sends -1)
  is_mission_package INTEGER NOT NULL DEFAULT 0,
  manifest TEXT CHECK (manifest IS NULL OR json_valid(manifest)),
  groups TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(groups)),
  deleted_at TEXT, created_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_resources_hash ON resources (hash);
CREATE UNIQUE INDEX idx_resources_uid ON resources (uid);
CREATE INDEX idx_resources_name ON resources (name);
CREATE INDEX idx_resources_tool_time ON resources (tool, submission_time DESC);

CREATE TABLE resource_keywords (
  resource_id INTEGER NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  keyword TEXT NOT NULL COLLATE NOCASE,
  PRIMARY KEY (resource_id, keyword)) STRICT, WITHOUT ROWID;
CREATE INDEX idx_resource_keywords_keyword ON resource_keywords (keyword);
```

**`0006_missions.sql`**
```sql
CREATE TABLE missions (
  id INTEGER PRIMARY KEY, guid TEXT NOT NULL, name TEXT NOT NULL COLLATE NOCASE,
  description TEXT, chat_room TEXT, base_layer TEXT, bbox TEXT, bounding_polygon TEXT, path TEXT, classification TEXT,
  tool TEXT NOT NULL DEFAULT 'public',
  keywords TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(keywords)),
  creator_uid TEXT, owner_user_id INTEGER REFERENCES users(id) ON DELETE SET NULL,
  create_time TEXT NOT NULL,
  default_role TEXT NOT NULL DEFAULT 'MISSION_SUBSCRIBER',
  invite_only INTEGER NOT NULL DEFAULT 0,
  password_hash TEXT,                                          -- argon2id (we are the only verifier)
  expiration INTEGER,                                          -- epoch s; NULL = none
  groups TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(groups)),
  external_data TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(external_data)),
  feeds TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(feeds)),
  token_kid TEXT REFERENCES oauth_keys(kid),
  archived_at TEXT, deleted_at TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_missions_guid ON missions (guid);
CREATE UNIQUE INDEX idx_missions_name_live ON missions (name) WHERE deleted_at IS NULL;
CREATE INDEX idx_missions_tool ON missions (tool);

CREATE TABLE mission_subscriptions (
  id INTEGER PRIMARY KEY, mission_id INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  client_uid TEXT NOT NULL, user_id INTEGER REFERENCES users(id) ON DELETE SET NULL, username TEXT,
  role TEXT NOT NULL CHECK (role IN ('MISSION_OWNER','MISSION_SUBSCRIBER','MISSION_READONLY_SUBSCRIBER')),
  token_jti TEXT, created_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_mission_subs_client ON mission_subscriptions (mission_id, client_uid);
CREATE INDEX idx_mission_subs_user ON mission_subscriptions (user_id);

CREATE TABLE mission_changes (
  id INTEGER PRIMARY KEY, mission_id INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  type TEXT NOT NULL,                     -- CREATE_MISSION | DELETE_MISSION | ADD_CONTENT | REMOVE_CONTENT | CREATE_MISSION_FEED | DELETE_MISSION_FEED
  timestamp TEXT NOT NULL, server_time TEXT NOT NULL, creator_uid TEXT,
  content_uid TEXT, content_hash TEXT,
  detail TEXT CHECK (detail IS NULL OR json_valid(detail))) STRICT;
CREATE INDEX idx_mission_changes_mission_ts ON mission_changes (mission_id, timestamp);
CREATE INDEX idx_mission_changes_uid ON mission_changes (mission_id, content_uid);

CREATE TABLE mission_contents (
  mission_id INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  resource_id INTEGER NOT NULL REFERENCES resources(id) ON DELETE CASCADE,
  creator_uid TEXT, timestamp TEXT NOT NULL,
  keywords TEXT NOT NUL

L DEFAULT '[]' CHECK (json_valid(keywords)),
  PRIMARY KEY (mission_id, resource_id)) STRICT, WITHOUT ROWID;
CREATE INDEX idx_mission_contents_resource ON mission_contents (resource_id);

CREATE TABLE mission_uids (
  mission_id INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  uid TEXT NOT NULL, creator_uid TEXT, timestamp TEXT NOT NULL,
  keywords TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(keywords)),
  details TEXT CHECK (details IS NULL OR json_valid(details)),   -- {type, callsign, title, iconsetPath, color, location}
  PRIMARY KEY (mission_id, uid)) STRICT, WITHOUT ROWID;
CREATE INDEX idx_mission_uids_uid ON mission_uids (uid);

CREATE TABLE mission_layers (
  id INTEGER PRIMARY KEY, mission_id INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  uid TEXT NOT NULL, name TEXT, type TEXT NOT NULL,             -- GROUP | UID | CONTENTS | MAPLAYER | ITEM
  parent_uid TEXT, after_uid TEXT,
  data TEXT CHECK (data IS NULL OR json_valid(data)),
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_mission_layers_uid ON mission_layers (mission_id, uid);

CREATE TABLE mission_logs (
  id INTEGER PRIMARY KEY, log_id TEXT NOT NULL,
  mission_id INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  content TEXT NOT NULL, creator_uid TEXT, entry_uid TEXT,
  content_hashes TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(content_hashes)),
  keywords TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(keywords)),
  servertime TEXT NOT NULL, dtg TEXT, created_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_mission_logs_log_id ON mission_logs (log_id);
CREATE INDEX idx_mission_logs_mission_time ON mission_logs (mission_id, servertime);

CREATE TABLE mission_invitations (
  id INTEGER PRIMARY KEY, mission_id INTEGER NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
  invitee_type TEXT NOT NULL CHECK (invitee_type IN ('clientUid','callsign','userName','group','team')),
  invitee TEXT NOT NULL, creator_uid TEXT,
  role TEXT NOT NULL DEFAULT 'MISSION_SUBSCRIBER', token_jti TEXT,
  created_at TEXT NOT NULL, accepted_at TEXT) STRICT;
CREATE UNIQUE INDEX idx_mission_invitations_target ON mission_invitations (mission_id, invitee_type, invitee);
```

**`0007_profiles.sql`**
```sql
CREATE TABLE profiles (
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, description TEXT,
  apply_on TEXT NOT NULL CHECK (apply_on IN ('enrollment','connection')),
  enabled INTEGER NOT NULL DEFAULT 1, priority INTEGER NOT NULL DEFAULT 0,
  groups TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(groups)),           -- [] = everyone
  preferences TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(preferences)), -- key -> {class, value}; rendered to .pref
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_profiles_name ON profiles (name);

CREATE TABLE profile_files (
  id INTEGER PRIMARY KEY, profile_id INTEGER NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
  path TEXT NOT NULL, hash TEXT NOT NULL, size INTEGER NOT NULL, mime_type TEXT,
  created_at TEXT NOT NULL) STRICT;
CREATE UNIQUE INDEX idx_profile_files_path ON profile_files (profile_id, path);
```

### 4.6 Content-addressed store (`files/store.rs`, foundation for M3)

`ContentStore { root }` with `put(AsyncRead) -> (sha256 hex, size)` (stream to `<root>/tmp/<uuid>` while hashing, then atomic `rename` to `<root>/<aa>/<hash>`; no-op if exists), `open(hash) -> tokio::fs::File`, `exists`, `remove`, `iter()` for the orphan GC job. Refcount is derived from `resources.hash` ∪ `profile_files.hash`.

### 4.7 Testing approach

- `Database::open_in_memory()` for every repo/unit test; `AppContext::new_mock` for handler tests.
- Migration tests: `open_in_memory_at_migration(n)` + seed + `upgrade()`; fresh-vs-upgraded `schema_of()` parity across every table; `foreign_key_check` and `integrity_check` empty; filename/contiguity assertions.
- Repo tests: one `#[cfg(test)] mod tests` per repo file exercising create/get/list/constraint violations (unique username case-insensitive, FK cascade on user delete removes devices/credentials).
- Queue/kv/audit tests lifted from automate minus the tenant cases.
- Integration test (`tests/bootstrap.rs`, feature `testing`): temp dir, real file DB, `run()` in a task, HTTP smoke, cancel, assert exit and that `rustak.sqlite-wal` is truncated.

---

## 5. Crypto (`rustak-server/src/crypto/`)

Lift **verbatim** from `automate/agent/src/crypto.rs` (split into four files to satisfy the length rule): `key.rs` (`SecretKey`, `KeyId`, `decode_key_bytes`, encodings/tests), `store.rs` (`Sealed`, `SecretStore::{new, load, ephemeral, active_key_id, seal, open, seal_json, open_json, key_for}` + tests), `keyfile.rs` (`key_file_for`, `load_or_create_key`, `write_key_file` 0600, `warn_if_world_readable` + tests), `context.rs` (rustak's `SecretContext`). Only edits: `SecretStore::load(auth: &AuthConfig, database: &Path)` reads `auth.secret_key`/`auth.previous_secret_keys`; key-id domain string `"rustak/secret-key-id/v1"`; advice text says `[auth]`.

```rust
pub enum SecretContext<'a> {
    CaKey { certificate: CertificateId },            // "rustak/v1/ca-key/{id}"
    ServerCertKey { certificate: CertificateId },    // "rustak/v1/server-key/{id}"   (internal + ACME-issued)
    ServiceCertKey { certificate: CertificateId },   // "rustak/v1/service-key/{id}"  (bundles we generate for sidecars/manual packages)
    AcmeAccount { account: i64 },                    // "rustak/v1/acme-account/{id}"
    JwtSigningKey { kid: &'a str },                  // "rustak/v1/jwt-key/{kid}"
    MissionTokenKey { kid: &'a str },                // "rustak/v1/mission-key/{kid}"
    IdpRefreshToken { token: &'a str },              // "rustak/v1/idp-refresh/{jti}"  (M5)
    ServiceSecret { service: &'a ServiceName, key: &'a str }, // per-service config values marked secret (M6)
}
```

Rule of three: *secrets we must read back* (private keys, ACME credentials, IdP refresh tokens) → `Sealed` with context-as-AAD; *low-entropy secrets we only verify* (device passwords, enrollment tokens, local admin password, mission passwords) → argon2id PHC in `credentials.secret_hash` / `users.password_hash` / `missions.password_hash`, never sealed; *high-entropy random tokens we only verify* (our refresh tokens, auth codes) → plain sha256 in `oauth_tokens.token_hash`. argon2 runs in `spawn_blocking` (`rustak_core::identity::password::verify_blocking`).

---

## 6. Admin web server, `/api/v1`, UI skeleton

### 6.1 Server wiring (`web/`)

- `web/server.rs`: `build_public(ctx) -> Result<Server>`: `HttpServer::new(move || App::new().app_data(Data::new(ctx.clone())).wrap(telemetry::TracingLogger).service(api::configure()).route("/robots.txt", get().to(ui::robots)).default_service(get().to(ui::serve)))`, binds every `web.public.listen` entry (`bind` or `bind_rustls_0_23(addr, ServerConfig)` per `tls.mode`), `.disable_signals()`. `web/tls.rs`: `ServerConfig` from PEM files (`files` mode); `acme` mode wired in M2.
- `web/ui.rs` and `web/telemetry.rs`: lifted verbatim (path `../rustak-ui/dist`, placeholder text "rustak").
- `web/helpers/request.rs`: lifted (`client_ip`, `is_https`, `base_url` reading `server.base_url`/`trust_proxy`).
- `web/helpers/oidc/*`: lifted and split (`discovery.rs` discovery+JWKS cache via `Cache`; `validate.rs` `validate_token`/`needs_jwks_refresh`/`verify_token`; `exchange.rs` `exchange_code`/`refresh_tokens`/`token_request`; `claims.rs` `AdminRequestFilter`, `username_from_claims`, `filterable_claims`, `groups_from_claims`).
- `web/principal.rs`: `Principal` (from core) stored in request extensions; extractors `Authenticated(Principal)` and `Administrative(Principal)` (the latter refuses non-admins in `from_request`, as automate's `scope.rs` does).

### 6.2 Auth model for the admin UI (deviation from automate, in line with "rustak is the identity authority")

The server **always issues its own RS256 JWT + opaque refresh token**, whether the person signed in locally or through OIDC:

- `GET  /api/v1/auth/metadata` → `AuthMode::Oidc{…}` when `[auth].oidc` is set, else `AuthMode::Local` (no 404 — the UI picks the form).
- `POST /api/v1/auth/token {code, redirect_uri}` → exchange with IdP (lifted), `validate_token`, `users().upsert_oidc(...)`, `user_acl` check, then `jwt.issue(principal)` + `oauth_tokens(kind='refresh')` → `TokenResponse`.
- `POST /api/v1/auth/local {username, password}` → `users().get_by_username` (source=local, has `password_hash`), `verify_blocking`, constant-time failure path, audit `Authentication/login`, → `TokenResponse`.
- `POST /api/v1/auth/refresh {refresh_token}` → lookup by sha256, rotate (consume + issue), → `TokenResponse`.
- `api_auth` middleware (`web/api/middleware.rs`): `bearer_token` (lifted) → `jwt.verify` (issuer = base URL, audience `rustak-admin`, `sub` = username, `jti`) → load user (refuse `disabled`) → build `Principal` → `user_acl`/`admin_acl` via filt-rs over `AdminRequestFilter` (claims = our JWT claims + `is_admin` flag from DB) → insert into extensions. 401 vs 403 semantics exactly as automate documents.
- `auth/jwt.rs`: `JwtKeys { kid, encoding: EncodingKey, decoding: DecodingKey, public_jwk }`, `load_or_create(db, secrets)` (RSA-2048 via `rsa` in `spawn_blocking` → PKCS#8 PEM → `Sealed` with `JwtSigningKey{kid}` → `oauth_keys` row), `issue(&Principal, ttl) -> String` with header exactly `{"alg":"RS256","typ":"JWT"}` and flat claims (`sub, iss, aud, iat, nbf, exp, jti, scope`) — the same token format CloudTAK parses in M5, so nothing changes later.

### 6.3 `/api/v1` routes in M0

| Route | Guard | Purpose |
|---|---|---|
| `GET /health` | public | `Health{status, version, uptime_seconds, database}` (`SELECT 1` through a reader) |
| `GET /auth/metadata`, `POST /auth/token`, `POST /auth/local`, `POST /auth/refresh` | public | §6.2 |
| `GET /setup/status` | public | `SetupStatus` (counts admins, `certificates.kind='ca'`, settings) |
| `POST /setup/admin` | public **only while `count_admins()==0`**, else 409 | creates local admin (person/local/is_admin, argon2) and returns a `TokenResponse`; audit `Administration/bootstrap-admin` |
| `POST /setup/server` | admin | writes `ServerSettings` to `kv` partition `settings` (config-file values win when set) and mints `node_id` |
| `POST /setup/ca` | admin | `pki::ca::create(&InitCaRequest)` → rcgen `CertificateParams` (CA:true, keyCertSign/cRLSign, validity from `[pki]`), key via `rsa` or rcgen ECDSA, cert PEM + `Sealed` key into `certificates(kind='ca')`; 409 if a CA exists |
| `POST /setup/complete` | admin | stamps `setup_completed_at` |
| `GET /me` | authenticated | `Me` from `Principal` |
| `GET /users`, `PATCH /users/{username}` | admin | list / disable / promote (lifted `admin.rs` shape) |
| `GET /audit?category&subject&before&limit` | admin | `AuditQuery` (lifted) |
| `GET /settings` | admin | resolved `ServerSettings` |

`web/api/error.rs`: `json_error(status, msg)` (lifted) and `impl ResponseError for ApiError` mapping `human_errors::Kind::User → 400`, `System → 500` with the `{"error": …}` body; every JSON response uses `content_type("application/json")` exactly (a helper `json_ok<T>()` so the CloudTAK "no charset" rule is already honoured).

### 6.4 `rustak-ui` skeleton

```
rustak-ui/
├── Cargo.toml, Cargo.lock, Trunk.toml  (dist = "dist"; serve port 8081; proxies /api/v1, /oauth, /login, /Marti → http://127.0.0.1:8446)
├── index.html                         (title "rustak", data-bin="rustak-ui", styles.scss)
├── styles.scss                        sections: 1 tokens (automate's palette, re-branded) · 2 base · 3 primitives (btn, badge, alert, form, table, status-pill) ·
│                                      4 chrome (app bar, shell, footer) · 5 pages (landing, auth, setup wizard steps, dashboard cards) · 6 responsive
└── src/
    ├── main.rs                        (lifted)
    ├── app.rs                         Route: Landing "/", AuthCallback "/auth/callback", Setup "/setup", AdminRoot "/admin", Dashboard "/admin/",
    │                                  Devices, Credentials, Users, Groups, Services, Missions, Packages, Profiles, Activity, Settings (all "/admin/…"),
    │                                  cfg(debug) DemoControls, NotFound; AuthStatus adds `NeedsSetup`; use_auth resolves setup status before /me
    ├── api/mod.rs                     transport (build/send/refresh-once/json_response/demo! macro) — lifted, trimmed (<300)
    ├── api/{auth,setup,users,audit,settings,health}.rs   one module per DTO group (each < 100 lines)
    ├── auth.rs                        lifted (popup OIDC, sessionStorage keys `rustak.admin.*`, single-flight refresh) + `login_local(username, password)`
    ├── fixtures/{mod,data,store}.rs   demo mode (`?demo`) with a seeded admin, a few users, audit rows, health, setup complete
    ├── components/{mod,admin_shell,app_bar,alert,form,page_title,status_pill,layout,secret_input,helpers}.rs   (lifted subset; nav lists all pages)
    └── pages/{mod,landing,login,auth_callback,setup,dashboard,users,activity,settings,not_found,protected}.rs
        + stubs devices/credentials/groups/services/missions/packages/profiles.rs (PageTitle + "Arrives in M{n}" note)
```

Login page renders the SSO button or the username/password form based on `AuthMetadata.mode`; the setup page is a three-step wizard (admin → server → CA) driven by `SetupStatus`, reachable without a session for step 1 only.

---

## 7. e2e harness, file-length lint, CI/CD

### 7.1 e2e (`e2e/`)

- `package.json` (`@playwright/test ^1.62`, `@types/node`), `tsconfig.json`, `playwright.config.ts`: lifted; `RUSTAK_E2E_PORT` default **18446**, `webServer.command = "node scripts/start-server.mjs"`, readiness `GET /robots.txt`, `gracefulShutdown SIGTERM`.
- `scripts/start-server.mjs`: lifted from `start-agent.mjs`; binary `target/{debug,release}/rustak`, scratch dir `rustak-e2e-*`, generated config:
  ```toml
  [server] name = "rustak-e2e" data_dir = "<dir>"
  [web.public] listen = ["127.0.0.1:18446"]
  [web.public.tls] mode = "none"
  [web.marti] enabled = false
  [stream.tls] enabled = false
  [stream.tcp] enabled = false
  [auth] user_acl = 'true' admin_acl = 'true'
  ```
  Runs with `--env <dir>/.env.absent` and `cwd = <dir>` (the FIFO guard in core makes this belt-and-braces).
- `tests/helpers.ts`: `waitForApp` on `TrunkApplicationStarted` (lifted) + `bootstrapAdmin(request)` (POST `/setup/admin`, tolerate 409 then `/auth/local`) + `signIn(page, token)` (init script writing `rustak.admin.token` to sessionStorage).
- Specs: `smoke.spec.ts` (robots, `/` not 500 and contains `TrunkApplicationStarted`, dashboard renders), `setup.spec.ts` (wizard end-to-end on a fresh server: admin → server name → CA → dashboard), `auth.spec.ts` (local login, wrong password, sign-out), `navigation.spec.ts`.

### 7.2 `scripts/check-file-length.sh`

```bash
#!/usr/bin/env bash
# Fails when any Rust source file has more than MAX functional lines.
# Functional = not blank, not a comment line, and before the top-level `#[cfg(test)]`
# that introduces the trailing `mod tests` block. Test-only trees are exempt.
set -euo pipefail
MAX="${MAX_FUNCTIONAL_LINES:-300}"; status=0
while IFS= read -r file; do
  case "$file" in */tests/*|*/testing/*|*/fixtures/*|*_tests.rs) continue;; esac
  count=$(awk '
    /^#\[cfg\(test\)\]/ { exit }            # column-0 attribute: everything after is the test module
    /^[[:space:]]*$/       { next }          # blank
    /^[[:space:]]*\/\//    { next }          # // and /// and //! comments
    { n++ } END { print n+0 }' "$file")
  if [ "$count" -gt "$MAX" ]; then printf '%s: %d functional lines (limit %d)\n' "$file" "$count" "$MAX"; status=1; fi
done < <(git ls-files '*.rs' ':!:rustak-ui/dist')
exit $status
```
Convention this enforces: the `#[cfg(test)] mod tests` block is the last item and its attribute sits at column 0 (a companion `--report` flag prints the ten longest files). It runs in the `lint` job and is documented in `CONTRIBUTING.md`; it deliberately does not count `.scss`/`.ts`.

### 7.3 GitHub Actions

**`rust.yml`** (push `main`, PR, release) — job graph:

```
deduplicate ──┬─ version ────────────────────────────────┐
              ├─ lint   (fmt --check; clippy --workspace --all-targets -D warnings; check-file-length.sh; cargo doc -D warnings)
              ├─ test   (cargo test --workspace --no-fail-fast, -Cinstrument-coverage → grcov → codecov)
              ├─ ui     (rustup wasm32; cargo binstall trunk@0.21.14; cargo fmt/clippy in rustak-ui for wasm32; trunk build → ui-dist-e2e; trunk build --release → ui-dist)
              ├─ e2e    (needs ui; download ui-dist-e2e → cargo build -p rustak-server → npm ci → playwright)
              └─ build  (needs version, ui; matrix crate × target, see below) ──────┬─ ci (always(); verifies every result, saves merge-tree marker)
                                                                                     ├─ docker-build (main pushes + releases; matrix crate × platform) ─ docker-publish (per crate)
                                                                                     └─ tap (release only; formula `rustak`, aliases major minor)
```

Build matrix: dimension `crate` = `[{name: rustak-server, bin: rustak, docker: true}, {name: rustak-plugin-example, bin: rustak-plugin-example, docker: true}]` × `include` targets `x86_64-unknown-linux-musl` (ubuntu-24.04, `musl-tools`, cargo), `aarch64-unknown-linux-musl` (ubuntu, cross, chown fix-up lifted), `x86_64-apple-darwin`, `aarch64-apple-darwin` (macos-latest), `x86_64-pc-windows-msvc` (windows-latest, `.exe`) — 10 jobs, command `${builder} build --release --target ${target} -p ${crate.name}`, artifact and release asset `${bin}-${os}-${arch}${ext}`. No `setup-protoc` step anywhere. `version` rewrites the single `^version =` line in the root `Cargo.toml` (`[workspace.package]`) and uploads it; `build` downloads it to the root before compiling.

Docker: `docker-build` matrix `crate × [linux/amd64, linux/arm64]`, `file: ${crate.name}/Dockerfile`, image `ghcr.io/sierrasoftworks/${bin}`, tags `type=raw,value=latest,enable={{is_default_branch}}`, `type=ref,event=branch`, `type=semver,pattern={{version}}|{{major}}.{{minor}}|{{major}}`; `if: github.event_name == 'release' || (github.event_name == 'push' && github.ref == 'refs/heads/main')` so `ghcr.io/sierrasoftworks/rustak:latest` exists on a green `main` (M0 exit criterion). `docker-publish` runs `buildx imagetools create` per crate from the digest artifacts.

`rustak-server/Dockerfile`:
```dockerfile
FROM ubuntu:24.04
LABEL org.opencontainers.image.source=https://github.com/SierraSoftworks/rustak
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
ADD ./rustak /usr/local/bin/rustak
VOLUME /data
EXPOSE 443 8446 8443 8089 8087
ENTRYPOINT ["/usr/local/bin/rustak", "--config", "/data/config.toml"]
```
(`rustak-plugin-example/Dockerfile` is the same with its binary and `--config /data/plugin.toml`.)

`Cross.toml`: musl targets need nothing pre-built (rusqlite bundled and aws-lc-sys compile with the image's C toolchain); keep a `[target.aarch64-unknown-linux-musl] pre-build = ["apt-get update && apt-get install -y cmake"]` entry only if step 14's first cross build fails on aws-lc-sys.

Other workflows: `changelog.yml` and `security_audit.yml` lifted (audit also on `push` touching `Cargo.lock`/`rustak-ui/Cargo.lock`); `.github/release-drafter.yml` lifted with autolabeler `files: ["*.md", "docs/**"]`; `.github/dependabot.yml`: cargo `/` (groups `opentelemetry` = `opentelemetry*`,`tracing*`,`tonic`; `protobuf` = `prost*`,`protox`; `rustls` = `rustls*`,`tokio-rustls`,`rcgen`,`aws-lc-*`,`instant-acme`; `actix` = `actix-*`), cargo `/rustak-ui` (group `yew` = `yew*`,`gloo-*`,`wasm-bindgen*`,`web-sys`,`js-sys`), github-actions `/`, npm `/e2e` — all daily.

---

## 8. Ordered M0 implementation steps (each with its verification)

| # | Step | Verify |
|---|---|---|
| 1 | Root `Cargo.toml`, `.cargo/config.toml`, `.gitignore`, LICENSE, README; `cargo new` all six crates with placeholder `lib.rs`/`main.rs`; `rustak-ui` from `cargo new` + `Trunk.toml` + `index.html` | `cargo metadata --format-version 1 \| jq '.workspace_members \| length' == 6`; `cargo build` builds only `rustak-server`; `cargo build --workspace` green |
| 2 | Commit the full `.github/` set (rust.yml with lint/test/ui/build/ci, changelog, audit, dependabot, release-drafter) and `scripts/check-file-length.sh` | Pipeline green on the empty skeleton **without protoc installed**; `deduplicate` cache hit on a re-run |
| 3 | `rustak-api` identity newtypes + DTO modules with unit tests | `cargo test -p rustak-api`; `cargo check -p rustak-api --target wasm32-unknown-unknown` |
| 4 | `rustak-core`: interpolation (lift), config loader + env resolver + FIFO guard, durations, `ListenAddr`, telemetry bootstrap/shutdown, `Shutdown`, secret/password/groups/principal, service identity | `cargo test -p rustak-core`; a doctest that loads a TOML with `${{ env.X }}` |
| 5 | `rustak-cot`: `proto/tak_protocol_v1.proto` skeleton, `build.rs` (protox), `proto/mod.rs` round-trip test | `env -i PATH=/usr/bin:/bin HOME=$HOME cargo build -p rustak-cot` succeeds (proves no protoc); `cargo test -p rustak-cot` |
| 6 | Server `config/*` + root `config.example.toml` + `--check` flag | `cargo test -p rustak-server config::`; `cargo run -- --config config.example.toml --check` exits 0; a misplaced key is refused |
| 7 | `db/`: connection + pragmas, migration runner, `0001–0007` SQL, row helpers, kv/queue/cache/partition/audit (lift), repos for users/groups/credentials/certificates/services/oauth_keys/oauth_tokens/settings | `cargo test -p rustak-server db::`; parity, `foreign_key_check`, WAL-applied test on a temp file; `sqlite3 data/rustak.sqlite .schema` matches |
| 8 | `crypto/` (lift) + `SecretContext` | lifted tests green; relocation-detection test for `CaKey{1}` vs `CaKey{2}` |
| 9 | `services/` (AppContext, Services, mock), `jobs/` (Job/JobRunnable/JobHost/audit_prune/wal_checkpoint) with shutdown-aware loop | lifted job tests; a test cancelling `Shutdown` makes `JobHost::run` return within 1 s |
| 10 | `files/store.rs`, `pki/{keys,ca}.rs`, `auth/{jwt,local}.rs` | put/open/hash tests; CA round-trip parsed by `x509-parser` (CA:true, key usage); JWT header bytes are exactly `{"alg":"RS256","typ":"JWT"}`, verify rejects wrong `aud`/expired |
| 11 | `web/`: servers, tls (files), ui, telemetry, helpers, oidc (lift+split), principal, api (health/auth/me/setup/users/audit/settings), `TestIdentityProvider` (lift) | actix `test::init_service` tests per endpoint; OIDC forgery suite (lifted); setup-admin 409 after first admin; `/robots.txt` before catch-all |
| 12 | `main.rs` + `lib.rs::run` + `runtime.rs`; `tests/bootstrap.rs` | in-process: start → health 200 → cancel → returns Ok in < 10 s; manual: `cargo run` then SIGTERM exits 0, `-wal` file truncated, no "could not reclaim session" warning |
| 13 | `rustak-ui`: routes, shell, protected gate, login (local + SSO), setup wizard, dashboard, users, activity, fixtures/demo, styles | `trunk build` from `rustak-ui/`; `trunk serve` with `?demo` renders every page; `cargo clippy --target wasm32-unknown-unknown -- -D warnings` |
| 14 | e2e harness + specs; wire `e2e` job | `cd rustak-ui && trunk build && cd .. && cargo build -p rustak-server && cd e2e && npx playwright test` green locally and in CI |
| 15 | `rustak-client::sidecar` + `rustak-plugin-example` (loads config via core, logs descriptor, waits for shutdown); Dockerfiles; `Cross.toml`; docker/tap jobs | `cargo run -p rustak-plugin-example -- --config rustak-plugin-example/config.example.toml`; cross build of both bins for aarch64-musl locally (`cross build --release --target aarch64-unknown-linux-musl -p rustak-server`) |
| 16 | Push to `main` → `ghcr.io/sierrasoftworks/rustak:latest` multi-arch; publish pre-release `v0.0.1-alpha.1` | release has 10 assets; `docker run --rm ghcr.io/sierrasoftworks/rustak:0.0.1-alpha.1 --help`; Homebrew formula updated in the tap (if actions-tap skips pre-releases, verify with `v0.0.1`) |
| 17 | `docs/deployment.md`, `docs/plugins.md`, move appendix A to `docs/compat/`, `CONTRIBUTING.md` (file-length rule, build order) | `check-file-length.sh` passes over the whole tree |

---

## 9. Risks and mitigations

| Risk | Mitigation |
|---|---|
| **tokio-rusqlite single writer vs CoT history writes** — every position report becomes a write; a naive per-event `INSERT` serialises the hot path behind fsyncs | The stream hub never awaits the DB: it pushes to a bounded `mpsc`; one `cot_store` task drains it into batched `write()` transactions (≤ 200 rows / 250 ms), coalescing `cot_latest` upserts per uid. WAL + `synchronous=NORMAL` makes a batch one fsync. 50 EUDs at 1 msg/2 s is ~25 rows/s — well inside budget. `Database::write` is the only writer, so there is never `SQLITE_BUSY` between our own connections. |
| **WAL with many readers** — long read transactions block checkpoints and grow the WAL | Reader pool (default 2) with short, closure-scoped transactions; `wal_checkpoint(PASSIVE)` job every 5 min; `TRUNCATE` at shutdown; `journal_size_limit`; a warning when the `-wal` file exceeds 256 MB. Large downloads stream from the content dir, never from a DB cursor. |
| **`include_dir!` compile-time coupling** — server built before `trunk build` silently serves a 500 | `build.rs` creates the dir; smoke spec asserts `/` is not 500; CI orders `ui → e2e/build`; `cargo:rerun-if-changed`; `CONTRIBUTING.md` documents the order (same approach as automate). |
| **cross + protox / aws-lc-sys** | protox removes the protoc dependency entirely. aws-lc-sys needs a C compiler (present in cross images) and cmake for some targets; automate already cross-builds aws-lc-rs for aarch64-musl, so this is expected to work. Fallback if step 15 fails: add `cmake` to `Cross.toml` `pre-build`; second fallback: switch the workspace to `ring` (`rustls/ring`, `tokio-rustls/ring`, `rcgen/ring`, `instant-acme/ring`, `x509-parser/verify`, `jsonwebtoken/rust_crypto`, `reqwest/rustls-no-provider` + explicit provider install). The `install_default()` call in `run()` protects against a mixed-provider panic either way. |
| **Yew 0.23 status** | yew 0.23.0 / yew-router 0.20.0 are the current stable pair and are what automate ships on today; cadence is slow but the API surface used (function components, hooks, router, context) is stable. gloo-net 0.7 keeps the `RequestBuilder → build()/json() → send()` shape automate uses. Pin `trunk@0.21.14` (`cargo binstall trunk --version 0.21.14`) since 0.22 is beta. |
| **rustls has no RSA key exchange** — if ATAK's `DEFAULT:!ECDH` really excludes ECDHE on TLS 1.2, rustls cannot talk to it unless TLS 1.3 negotiates | Not an M0 blocker but must be tested first thing in M1 with a real ATAK against `:8089`. If TLS 1.3 is not negotiated, the stream listener (only) falls back to an OpenSSL acceptor (`tokio-openssl`, vendored) behind a cargo feature; the web listeners stay on rustls. Recorded in `docs/compat/streaming.md`. |
| **Duplicate `reqwest` versions** — tracing-batteries pins reqwest 0.12; the workspace uses 0.13 | Accept the duplicate (compile-time only). Alternative: pin 0.12 in the workspace until tracing-batteries bumps. |
| **tracing-batteries is an unpinned git dependency** | Pin `rev` in `[workspace.dependencies]` and bump deliberately; dependabot does not track git deps. |
| **STRICT tables** — `DATETIME`/`CURRENT_TIMESTAMP` habits from automate break | Rule in `db/row.rs` docs: timestamps are `TEXT` bound from `chrono`; a migration test rejects any `CURRENT_TIMESTAMP` default via `sqlite_master` scan. |
| **RSA key generation cost** (CA, JWT key, test IdP) | `spawn_blocking`; `[profile.dev.package.rsa/num-bigint-dig] opt-level = 3`; one `LazyLock` key per test process (automate's pattern). |
| **`default-members = ["rustak-server"]`** hides other crates from bare `cargo test` | CI always passes `--workspace`; `CONTRIBUTING.md` says so. |
| **File-length lint false positives** (helpers under `#[cfg(test)]` mid-file) | Convention: exactly one column-0 `#[cfg(test)]` per file, last item; the script exempts `tests/`, `testing/`, `fixtures/`. |
| **Setup wizard bootstrap window** — `POST /setup/admin` is public until the first admin exists | It is a single-shot: the handler re-checks `count_admins()` inside the write transaction (unique constraint + 409); an audit row is written; docs recommend running the first start on localhost or behind a proxy. |
| **Binding `:443` by default needs privileges** | Default `listen = [":8446"]`; docs show `":443"` with `CAP_NET_BIND_SERVICE` (systemd) or Docker. **Deviation from the draft's "both bound by default" — please confirm.** |

## 10. Decisions that deviate from the draft (for confirmation)

1. Identity newtypes live in `rustak-api` (re-exported by `rustak-core`), so `rustak-core` depends on `rustak-api`.
2. The admin UI is authenticated with **our own** RS256 JWT + refresh token for both local and OIDC sign-in (automate uses the IdP ID token as the bearer). This unifies `api_auth`, is what CloudTAK will consume in M5, and lets the local admin created by the wizard work with no IdP.
3. `[web.public].listen` defaults to `[":8446"]` only.
4. Extra helper table `resource_keywords` and a `certificates.source`/`san` pair added beyond the listed table set; per-device active channels are JSON on `devices`, not a table.
5. Sentry DSN comes from `option_env!("RUSTAK_SENTRY_DSN")`/env, not the config file, so telemetry exists before config parsing.
6. The CA is created by the M0 setup wizard (minimal `pki/ca.rs`) rather than waiting for M2, defaulting to RSA-2048 via the `rsa` crate.

### Critical Files for Implementation
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/Cargo.toml` — workspace package/dependency/lint tables every crate inherits
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/db/connection.rs` — writer + read-only pool, pragmas, `read`/`write`/`close`
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/db/migrations.rs` — `include_dir!` runner over `rustak-server/migrations/0001–0007*.sql`
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server/src/runtime.rs` — listeners + HttpServers + JobHost under one `Shutdown` token with WAL checkpoint on exit
- `/Users/bpannell/dev/gh/SierraSoftworks/rustak/.github/workflows/rust.yml` — dedup → version/lint/test/ui/e2e → crate×target build matrix → ci → docker (per crate) → tap

Reference sources to lift from: `/Users/bpannell/dev/gh/SierraSoftworks/automate/agent/src/{crypto.rs,db/sqlite.rs,db/audit.rs,job.rs,parsers/interpolation.rs,web/helpers/oidc.rs,web/api/mod.rs,testing/oidc.rs}`, `/Users/bpannell/dev/gh/SierraSoftworks/automate/ui/src/{auth.rs,api.rs,app.rs}`, `/Users/bpannell/dev/gh/SierraSoftworks/automate/e2e/scripts/start-agent.mjs`, `/Users/bpannell/dev/gh/SierraSoftworks/automate/.github/workflows/rust.yml`.

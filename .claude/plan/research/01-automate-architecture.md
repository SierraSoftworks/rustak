I have a complete picture. Here's the architectural map.

---

# Architectural map: `SierraSoftworks/automate`

All paths relative to `/Users/bpannell/dev/gh/SierraSoftworks/automate`.

---

## 1. Crate layout and build

### `agent/Cargo.toml` (package `automate`, edition 2024)

| Concern | Crate + version/features |
|---|---|
| Web framework | `actix-web = "4.13.0"` |
| Async runtime | `tokio = { version = "1.52.3", features = ["net","io-std","rt-multi-thread","time","fs","tracing"] }` |
| SQLite | `rusqlite = { version = "0.37.0", features = ["bundled","chrono"] }` + `tokio-rusqlite = "0.7.0"` |
| Migrations | **hand-rolled** — a `const MIGRATIONS: &[&str]` array, no crate |
| TLS | none for listeners; `reqwest = { version = "0.12.27", features = ["rustls-tls"] }` for clients only. `openssl-sys` `vendored` behind an optional `openssl-vendored` feature (used by release builds only) |
| OIDC/JWT | `jsonwebtoken = { version = "11.0.0", features = ["aws_lc_rs"] }`, `oauth2 = "5.0.0"` |
| Serialization | `serde 1.0.219` (`alloc`,`derive`), `serde_json 1.0.150`, `toml = "1.1.2"` |
| XML / feeds | `feed-rs = "2.3.1"` (RSS/Atom), `calcard = "0.3.4"` (iCalendar), `scraper = "0.27.0"` + `htmd = "0.5.4"` + `html-escape = "0.2.13"` (HTML) — **no generic XML crate; no quick-xml** |
| Config | `toml`, `dotenvy = "0.15"`, `clap = "4.6.1"` (`cargo`,`derive`,`string`) |
| Logging/tracing | `tracing = "0.1.44"` + `tracing-batteries` (git, `sierrasoftworks/tracing-batteries-rs`, features `analytics`, `opentelemetry`, `testing`, `human_errors`) |
| Errors | `human-errors = { version = "0.2.4", features = ["pretty","force_backtraces"] }` |
| Static assets | `include_dir = "0.7.4"` |
| Crypto | `aes-gcm = "0.11"`, `sha2 = "0.11.0"`, `sha256 = "1.6.0"`, `hmac = "0.13.0"`, `hex`, `base64 = "0.23"`, `rand = "0.10"`, `zeroize = "1.9"` |
| Plugin registry | `inventory = "0.3.22"` (used for jobs, integrations, webhook sources) |
| Filter DSL | `filt-rs = { version = "1.1.3", features = ["serde"] }` |
| Scheduling | `croner = { version = "3.0.1", features = ["serde"] }` |
| Misc | `async-trait`, `futures`, `futures-concurrency = "7.7.1"`, `regex`, `urlencoding`, `uuid` (`serde`,`v4`) |
| Dev-deps | `rstest = "0.26.1"`, `wiremock = "0.6"`, `rsa = "0.9.10"` (`getrandom`), `base64` |

Domain-specific (drop for rustak): `todoist-api`, `rust-ynab`, `calcard`, `feed-rs`, `htmd`, `scraper`.

### `api/Cargo.toml` (`automate-api`)
Deliberately tiny so it compiles to wasm: **only** `chrono 0.4.45` (`serde`), `serde 1.0.219` (`alloc`,`derive`), `serde_json 1.0.150`. Documented rule in `api/src/lib.rs`: "free of any web-framework, database, or UI dependencies."

### `ui/Cargo.toml` (`automate-ui`, excluded from workspace)
`yew = { version = "0.23", features = ["csr"] }`, `yew-router = "0.20"`, `gloo-net = "0.5"`, `gloo-utils = "0.2"`, `gloo-timers = "0.3"` (`futures`), `wasm-bindgen 0.2`, `wasm-bindgen-futures 0.4`, `js-sys 0.3`, `web-sys 0.3` with an explicit feature list (`Window, Location, History, Storage, UrlSearchParams, Crypto, HtmlInputElement, HtmlSelectElement, HtmlTextAreaElement, InputEvent, Navigator, Clipboard, Element`), `futures 0.3` (for the single-flight refresh), `base64 0.22`, `chrono` (`serde`,`wasmbind`), `log`/`wasm-logger`/`console_error_panic_hook`, `pulldown-cmark 0.13.4` (`default-features = false`, `html`), plus `automate-api` and `filt-rs` (`visitor` feature, for client-side filter parsing/completion).

### UI embedding — the pattern worth copying verbatim
- `agent/src/web/ui.rs`: `static ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../ui/dist");` with `serve()` (SPA fallback to `index.html`), `robots()`, and an extension→content-type table.
- `agent/build.rs`: does two things only — `std::fs::create_dir_all("../ui/dist")` so `include_dir!` compiles on a fresh clone, and `println!("cargo:rerun-if-changed=../ui/dist")` because `include_dir!` doesn't track its own inputs. It warns rather than fails.
- `ui/dist/` is gitignored wholesale (no `.gitkeep`, because `trunk build` wipes the dir).
- Ordering is load-bearing: `trunk build` → `cargo build -p automate`.

### `ui/Trunk.toml`
```toml
[build]
target = "index.html"
dist = "dist"
[watch]
watch = ["src", "styles.scss", "index.html"]
[serve]
port = 8081
[[proxy]] backend = "http://127.0.0.1:8080/api/v1"
[[proxy]] backend = "http://127.0.0.1:8080/integrations"
[[proxy]] backend = "http://127.0.0.1:8080/oauth"
```
`ui/index.html` is a bare shell with `<link data-trunk rel="scss" href="styles.scss" />` and `<link data-trunk rel="rust" data-bin="automate-ui" />`.

### Root `Cargo.toml`
`resolver = "3"`, `members = ["agent","api"]`, `exclude = ["ui"]`, plus `[profile.dev.package.num-bigint-dig] opt-level = 3` and the same for `rsa` (test RSA keygen would otherwise dominate the suite).

### `.cargo/config.toml`
One line: `[target.aarch64-unknown-linux-musl] linker = "rust-lld"`.

### `Cross.toml`
Per-target `pre-build` installing `protobuf-compiler` (needed by the OTel stack) and `libssl-dev:$CROSS_DEB_ARCH` for the gnu targets.

### `Dockerfile`
`ubuntu:24.04`, installs `ca-certificates`+`openssl`, `ADD ./automate /usr/local/bin/automate`. Deliberately **not** self-building: the binary is built by CI and copied in.

### `.github/workflows/`
- **`rust.yml`** — jobs: `deduplicate` (caches a success marker keyed on `git rev-parse HEAD^{tree}` so an already-passed merge tree short-circuits CI), `version` (sed-rewrites `agent/Cargo.toml` version from the release tag, uploads it as an artifact), `test` (`cargo test --no-fail-fast` under `-Cinstrument-coverage`, grcov → codecov), `ui` (installs trunk via `cargo-binstall`, builds **twice** — a debug bundle for e2e since `?demo` is compiled out of release, and a release bundle), `e2e` (downloads the debug bundle into `ui/dist`, `cargo build -p automate`, Playwright/chromium), `build` matrix, `ci` aggregator, `docker-build`/`docker-publish` (per-platform digests → `buildx imagetools create` manifest list → ghcr.io), `tap` (`SierraSoftworks/actions-tap@v1` with `aliases: major minor`, gated on `github.event_name == 'release'`).
- Build matrix targets: `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-musl` (cargo + musl-tools), `aarch64-unknown-linux-musl` (**cross**), `x86_64-apple-darwin`, `aarch64-apple-darwin`. All non-Windows use `--features openssl-vendored`.
- **`changelog.yml`** — `release-drafter/release-drafter@v7.7.0` with the same merge-tree dedup cache.
- **`security_audit.yml`** — nightly `rustsec/audit-check@v2.0.0`.
- `.github/dependabot.yml` — daily cargo + github-actions, with an `opentelemetry` group (`opentelemetry*`, `tracing*`, `tonic`).

---

## 2. Backend bootstrap

### `agent/src/main.rs`
Top-level modules (from `main.rs`): `collectors`, `config`, `connection_refresh`, `connections`, `crypto`, `db`, `filter`, `integrations`, `job`, `jobs`, `parsers`, `prelude`, `publishers`, `runs`, `serde_duration`, `services`, `testing` (`#[cfg(test)]`), `users`, `web`, `webhook_index`, `webhook_payload`, `webhooks`, `workflow_store`, `workflow_toml`, `workflows`.

| Module | One-line role |
|---|---|
| `config.rs` | TOML schema + `${{ env.X }}` interpolation + `.env` loading |
| `prelude.rs` | Single re-export hub: `Config`, `Cache/KeyValueStore/Queue`, `Filter/Filterable`, `Job/JobContext`, `CronJob`, `Services`, `TenantId`, `human_errors::ResultExt`, `serde` traits, `tracing_batteries::prelude::*` |
| `services/` | `AppContext` (root, cross-tenant) → `AppServices` (tenant-scoped); DI seam with `Services` trait |
| `db/` | SQLite, KV/Queue/Cache/Audit traits, `TenantDb` scoped handle, `Partition<D,T>` |
| `crypto.rs` | AES-256-GCM `SecretStore`/`Sealed`/`SecretContext`, key rotation, key file |
| `filter.rs` | Thin re-export of `filt-rs` + `serde_json::Value → FilterValue` bridge |
| `job.rs` | `Job`/`JobRunnable` traits, `inventory` registry, `JobHost` consumer loop |
| `jobs/` | Concrete job impls + `cron.rs` scheduler (domain) |
| `workflows.rs` | `ConfigurableWorkflow`/`WorkflowType` registry (self-describing forms) |
| `workflow_store.rs` | Per-tenant workflow records, webhook index coupling |
| `runs.rs` | One overwritten run record per workflow (last run/failure/consecutive) |
| `users.rs` | `UserRegistry` in the system tenant |
| `webhook_index.rs` | Hashed-token → `(tenant, workflow)` index in the system tenant |
| `connections.rs` | Sealed external-service credentials per tenant |
| `connection_refresh.rs` | Cross-tenant OAuth grant renewal sweep |
| `integrations/` | `Integration` trait + `inventory` registry for setup wizards |
| `web/` | actix-web server, `/api/v1`, wizards, webhooks, SPA serving |
| `webhooks/` | `WebhookSource` trait + shared routing/fan-out (domain handlers alongside) |
| `parsers/` | `interpolate`, HTML→markdown, iCalendar, key/value pairs |
| `serde_duration.rs` | `chrono::Duration` ⇄ minutes serde adapters |
| `collectors/`, `publishers/` | Domain (Todoist/GitHub/Spotify/RSS) |
| `testing.rs`, `testing/oidc.rs` | Test fixtures + a real in-process OIDC provider |

`main()`: `clap::Parser` args (`--config`, `--env` defaulting to `.env`) → `Config::load_env_file` → build `Arc<Session>` with OpenTelemetry/Sentry/Analytics batteries → `run()` → `human_errors::pretty` on failure → `shutdown_session()` → `exit(1)`.

`run()` is the whole bootstrap:
```rust
let config = Config::load(args.config.unwrap_or_else(|| "config.toml".into()))?;
let db = db::SqliteDatabase::open(&config.web.database).await?;
let secrets = crypto::SecretStore::load(&config.web.auth, Path::new(&database_path))?;
let context = services::AppContext::new(config, db, secrets, session.clone());

(crate::web::run_web_server(context.clone()),
 crate::job::JobHost::run(context.clone()))
    .race()             // futures_concurrency::future::Race
    .await
    .or_user_err(&[...])?;
```

**Shutdown** (`shutdown_session`) is the interesting bit: `Session::shutdown` consumes the session, so `main` polls `Arc::try_unwrap` 40× at 50 ms intervals waiting for in-flight tasks to drop their clones, then flushes telemetry, else warns. `JobHost` keeps a `tokio::task::JoinSet` specifically so dropping the host aborts in-flight jobs and releases those clones.

### Config loading (`agent/src/config.rs`)
1. `dotenvy::from_path_override(path)` if the file exists (`.env` **overrides** process env).
2. Read file → `crate::parsers::interpolate(&contents, |expr| …)` — `${{ env.NAME }}` only; an **unset** variable is left verbatim (`${{ env.NAME }}`) rather than erroring, and `crypto::SecretKey::from_encoded` explicitly detects a leftover `${{` to give a good message. `\${{ }}` escapes.
3. `toml::from_str::<Config>` — every struct carries `#[serde(deny_unknown_fields)]`, which makes `config.example.toml` a real schema test (`the_documented_example_configuration_is_valid` does `include_str!("../../config.example.toml")`).
4. Validation is mostly type-level; ACLs default to `Filter::new("false")` via a `static DEFAULT_ADMIN_ACL: LazyLock<Filter>` — **deny by default**. `WebConfig::Default` is written out by hand because derived `Default` wouldn't apply serde's `#[serde(default = "...")]` functions.

### HTTP server (`agent/src/web/mod.rs`)
```rust
let registry = Arc::new(Registry::new(&context.config())?);   // fail fast on duplicate integration id
let services = context.tenant(TenantId::local());
// address parsed by split_once(':'), empty host → "0.0.0.0"
HttpServer::new(move || App::new()
    .app_data(web::Data::new(services.clone()))
    .app_data(web::Data::new(context.clone()))
    .app_data(web::Data::from(registry.clone()))
    .wrap(telemetry::TracingLogger::<AppServices>::new())
    .service(api::configure())                        // /api/v1
    .service(integrations::configure())               // /integrations/{id}/setup/*
    .service(integrations::configure_oauth_callback())// /oauth/{id}/callback
    .route("/webhooks/w/{token}", web::post().to(webhooks::deliver))
    .route("/webhooks/{source}", web::post().to(webhooks::deliver_source))
    .route("/robots.txt", web::get().to(ui::robots))
    .default_service(web::get().to(ui::serve))        // SPA fallback
).bind((addr, port))?.run().await
```
`/robots.txt` registered **before** the catch-all is what the e2e harness uses as a genuine readiness probe.

`/api/v1` scoping (`agent/src/web/api/mod.rs::configure`): a public `web::scope("/auth")` with `metadata`/`token`/`refresh`, then `web::scope("").wrap(from_fn(api_auth::<S>))` holding everything else. Note the routing-order comment: `/workflows/export` and `/workflows/import` are registered ahead of `/workflows/{workflow}`.

`base_url` / `trust_proxy` live in `agent/src/web/helpers/request.rs`: `client_ip()`, `is_https()`, `base_url()`. Forwarding headers (`X-Forwarded-For`, `-Proto`, `-Host`) are consulted **only** when `web.trust_proxy` is set; left-most entry taken; `web.base_url` wins outright. Tested both ways.

Telemetry middleware `agent/src/web/telemetry.rs` — custom `TracingLogger<S>` `Transform` that redacts `SENSITIVE_QUERY_PARAMS` (`code`,`state`,`id_token`,`access_token`,`refresh_token`,`token`,`client_secret`) and `SENSITIVE_HEADERS` (`authorization`, `proxy-authorization`, `cookie`, `set-cookie`) and extracts W3C trace context.

### Background work alongside the server
`JobHost::run(context)` (`agent/src/job.rs:313`):
1. Build `HashMap<&'static str, &'static dyn JobRunnable>` from `inventory::iter::<JobRegistration>`; duplicate partition = startup error.
2. `handler.setup(services)` once per registered job (local tenant).
3. `for tenant in context.database().tenants()` → `CronJob::reconcile(&services)` — per-tenant failures logged and skipped.
4. `reserve_for = max(handler.timeout())`, default 5 min.
5. A `tokio::task::JoinSet` is spawned with two permanent cross-tenant tasks: `Self::prune_audit_log(context)` (daily) and `connection_refresh::run(context)` (every 10 min).
6. Loop: reap completed tasks, `context.database().dequeue_any_global(reserve_for)` → narrow to the message's tenant → look up handler → `tasks.spawn(Self::process(...))`. Unknown partition ⇒ message completed (dropped) with a warning. Dequeue error ⇒ 5 s sleep.

---

## 3. Storage layer

**Crate:** `rusqlite 0.37` (`bundled`) behind `tokio-rusqlite 0.7`. **No pool** — a single `Arc<tokio_rusqlite::Connection>` shared everywhere; tokio-rusqlite owns one background thread with the connection and `call()` marshals closures onto it.

**Pragmas** — `agent/src/db/sqlite.rs::configure(connection, storage)`, run for every connection:
- `busy_timeout(5s)` (`BUSY_TIMEOUT`), set first so the WAL switch can wait.
- `PRAGMA journal_mode = WAL` via `query_row` so the *actual* resulting mode is read back; if it isn't WAL (network FS), it logs a specific warning and **skips** `synchronous = NORMAL` (because NORMAL + rollback journal is unsafe). `Storage::Memory` skips both.
- Foreign keys deliberately not enabled, with a comment saying why.

**Migrations** — hand-rolled: `const MIGRATIONS: &[&str]` (11 entries), a `migrations (id INTEGER PRIMARY KEY)` table, `SELECT COALESCE(MAX(id),0)`, then `.skip(latest)` applying each in a transaction with `execute_batch` + `INSERT INTO migrations`. Tests get `open_in_memory_at_migration(version)` and `upgrade()` so a migration can be exercised against realistic older data.

**Schema** (3 tables):
```sql
kv     (tenant, partition, key, value TEXT, PRIMARY KEY (tenant, partition, key))
queues (tenant, partition, key, payload TEXT, scheduledAt, hiddenUntil, reservedBy,
        traceparent, tracestate, idempotencyKey, PRIMARY KEY (tenant, partition, key))
  INDEX idx_queues_tenant_partition_hidden (tenant, partition, hiddenUntil)
  INDEX idx_queues_hidden_scheduled        (hiddenUntil, scheduledAt)
audit_log (id INTEGER PRIMARY KEY AUTOINCREMENT, tenant, occurredAt, category,
           action, outcome, subject, actor, message, detail TEXT)
  INDEX idx_audit_tenant  (tenant, id DESC)
  INDEX idx_audit_subject (tenant, subject, id DESC)
```
**Everything else is a JSON blob in `kv.value`.** Columns exist only for what is queried/ordered/indexed. Partitions are `/`-delimited hierarchical names (`github/releases`, `connections`, `runs`, `users`, `webhook-tokens`, `oauth-state`, `cron`). Migration 6 renamed partitions wholesale; migration 7 rewrapped bare values into JSON objects using `json_valid`/`json_type` guards for idempotency.

**Account-scoped handle pattern** (the README's "no method naming another account"):
- `SqliteDatabase` (root) has `tenant(TenantId) -> TenantDb`, `tenants()`, `dequeue_any_global()`, `audit_all()`, `prune_audit_log()` — the only cross-tenant surface.
- `TenantDb { connection: Arc<Connection>, tenant: TenantId }` implements `KeyValueStore`, `Queue`, `Cache`, `AuditStore`; **every statement carries `WHERE tenant = ?`** and no method takes a tenant parameter. `TenantDb::tenant()` returns its own name only.
- `AppContext` (root) holds `config`, `SqliteDatabase`, `Arc<SecretStore>`, shared `reqwest::Client`, `Arc<Session>`; `AppContext::tenant(TenantId) -> AppServices` where `type AppServices = ServicesContainer<TenantDb>`.
- `Services` trait (`agent/src/services/mod.rs`) exposes `config()`, `session()`, `secrets()`, `tenant()`, `kv()`, `queue()`, `cache()`, `audit()`, `http_client()` — all `-> impl Trait + Clone + Send + Sync + 'static`. There is a blanket `impl<S: Services> Services for &S` so borrowed handles stand in for owned ones.
- HTTP side: the `Scoped` extractor (`agent/src/web/api/scope.rs`) builds `AppServices` from `Principal::effective()`; the `Administrative` extractor yields the root `AppContext` and refuses non-admins. The stated design goal is "reviewing which endpoints can see across tenants is a matter of searching for one type."

**Traits** (`agent/src/db/mod.rs`): `KeyValueStore` (`get/list/set/insert/remove/partitions/scan/partition`), `Queue` (`enqueue/dequeue/dequeue_any/complete/reserve/peek/purge/partitions/partition`), `Cache` (`cached(partition,key,builder,ttl)` — blanket-implemented over any `KeyValueStore` in `db/cache.rs`, storing a `CacheItem{value, expires_at}`), `AuditStore`. `StateKey{partition,key}` names one address. `Partition<D,T>` (`db/partition.rs`) is a typed, curried view over any of the three.

**Encryption at rest** (`agent/src/crypto.rs`, 959 lines — the single most liftable file):
- AES-256-GCM. `Sealed { v: u8, kid: String, n: String, c: String }` stored as a plain JSON object inside a KV record — no special handling anywhere.
- `SecretContext` enum (`Connection{tenant,connection}`, `WebhookSecret{tenant,workflow}`) rendered as `automate/v1/connection/{tenant}/{id}` and used as **GCM AAD**, deliberately *not* stored in the envelope: it is reconstructed from the row's identity, so a ciphertext moved to another row fails its tag. This is the core idea.
- `SecretKey` zeroizes on drop, refuses to `Debug`-print, `id()` is `SHA256("automate/secret-key-id/v1" || key)[..4]` hex.
- `SecretStore::new(active, previous)` keyed by `KeyId`; rotation = move old key to `previous_secret_keys`, values re-seal lazily on write. `seal`/`open`/`seal_json`/`open_json`.
- `load_or_create_key(configured, database_path)` + `key_file_for(db)` = `<database>.key` beside the DB (`KEY_FILE_SUFFIX`), generated on first run.
- Explicit design note: sealing is *not* hidden behind serde, so every exposure point is greppable.

**Audit log** (`agent/src/db/audit.rs` + `api/src/audit.rs`): builder `AuditEntry::new(category, action, outcome).subject().actor().message().detail(json)`; `AuditQuery { category, subject, before, limit }` paginating on the autoincrement id (not timestamp). Categories: `WorkflowRun`, `WebhookDelivery`, `WorkflowConfig`, `Connection`, `Authentication`, `Administration`. Bounded by *both* `[audit].retain_days` (90) and `max_entries_per_account` (10 000) — the doc comment explains why one alone is insufficient. Deliberately **not** written for ordinary runs; `agent/src/runs.rs` keeps one overwritten `RunState` per workflow instead, with the input payload redacted and size-capped (`runs::keepable`).

---

## 4. Auth

### OIDC end-to-end (`agent/src/web/helpers/oidc.rs`, 1074 lines; `agent/src/web/api/auth.rs`)
The browser is a **public** client, the agent is the **confidential** one. **No PKCE** (the client secret already binds the exchange). The ID token is the API bearer; it is **never a cookie**, so there is no CSRF surface on `/api/v1` (stated in the module doc).

- `OidcDiscovery { issuer, authorization_endpoint, token_endpoint, jwks_uri }` fetched from `{endpoint}/.well-known/openid-configuration`, cached **1 hour** in `Cache` partition `oidc:discovery`.
- JWKS cached **24 hours** in partition `oidc:jwks`. Rotation is on-demand: `needs_jwks_refresh(key_set, token)` decodes the header, and if the `kid` is absent, `jwks(..., force_refresh = true)` removes the KV entry and refetches once before rejecting.
- `validate_token` → `verify_token(client_id, issuer, key_set, token)`:
  - Rejects `HS256/384/512` outright (algorithm confusion).
  - Requires a `kid`; `DecodingKey::from_jwk`.
  - `validation.set_audience(&[client_id])`, `set_issuer(&[issuer])`, `validate_exp`, `validate_nbf`, and crucially `validation.set_required_spec_claims(&["exp","aud","iss"])` — with a comment explaining that `jsonwebtoken`'s default would accept a token that simply *omits* `aud`.
- Endpoints: `GET /api/v1/auth/metadata` (returns `authorization_endpoint`, `client_id`, `scopes` with `openid` always first — so the browser never reads discovery cross-origin), `POST /api/v1/auth/token` (`{code, redirect_uri}` → `exchange_code` → `{token, refresh_token?}`), `POST /api/v1/auth/refresh` (`{refresh_token}` → same shape). All three answer `404` when no provider is configured, `502` on provider failure, and refresh answers `401` on failure.

### ACL filter expression language
**Not hand-rolled** — external crate `filt-rs 1.1.3`. `agent/src/filter.rs` re-exports `Filter`, `FilterValue`, `Filterable` and adds only `json_to_filter_value` (the orphan-rule bridge; JSON objects and `null` both map to `FilterValue::Null`, arrays to `Tuple`). `Filter` is `Deserialize`-able directly from a TOML/JSON string.

The variable surface is `AdminRequestFilter` in `web/helpers/oidc.rs`:
```rust
impl Filterable for AdminRequestFilter<'_> {
    fn get(&self, key: &str) -> FilterValue<'_> {
        match key {
            "method" => ..., "path" => ..., "client_ip" => ...,
            k if k.starts_with("headers.") => self.headers.get(&k[8..])...,
            k if k.starts_with("claims.")  => json_to_filter_value(self.claims?.get(&k[7..])),
            _ => FilterValue::Null,
        }
    }
}
```
Protocol claims are excluded from the user-facing surface via `EXCLUDED_CLAIMS` (`exp, nbf, iat, iss, aud, jti, nonce, at_hash, c_hash, azp, auth_time`). Example expressions from the config tests: `claims.email == "admin@example.com"`, `client_ip in ["127.0.0.1"]`, `true`, `false`.

### `user_acl` / `admin_acl` gates and the middleware
`api_auth<S>` (`agent/src/web/api/mod.rs:~180-335`), an actix `from_fn` middleware. Sequence:
1. If OIDC configured: require `Authorization: Bearer` (`bearer_token()` accepts both `Bearer ` and `bearer `), `validate_token` → claims. Else `None`.
2. Build `AdminRequestFilter`; `user_acl().matches()` false ⇒ **403, not 401** (with a long comment: a 401 would bounce the UI through a sign-in that cannot change the outcome).
3. `is_admin = admin_acl().matches()`.
4. `account_for(config, claims)` — the username claim only becomes the tenant when `multi_tenant` is on; otherwise `local_tenant(config)` (`web.auth.local_user` or `TenantId::local()`).
5. `UserRegistry::new(context.tenant(TenantId::system())).record_sign_in(...)` — returns `Ok(None)` for a **suspended** account ⇒ 403. A registry write failure is logged, never fatal.
6. Impersonation: `IMPERSONATE_HEADER = "x-impersonate-user"`. Refused with **400** when `!multi_tenant` (an explicit message rather than silently doing nothing). Otherwise `resolve_impersonation` → `principal.impersonating(subject)` and `record_impersonation` writes an audit entry **only for unsafe methods** (`req.method().is_safe()` short-circuits) into the *impersonated* account's log, naming the admin as actor.
7. `req.extensions_mut().insert(principal)`.

`Principal` (`agent/src/web/principal.rs`) is the model to copy: `actor` / `effective` / `is_admin` / `user`. `is_admin` is a property of the actor only — `impersonating()` leaves it untouched, so acting as an admin grants nothing. `to_admin_user()` reports the effective account with `impersonated_by` set. Nine unit tests cover exactly these invariants.

### OAuth2 "setup wizard" state-cookie CSRF (`agent/src/web/helpers/wizard.rs`)
- `SETUP_STATE_COOKIE = "automate_setup_state"`, 10-minute max-age, `HttpOnly`, `SameSite::Lax`, `secure` only over HTTPS, **path-scoped to the callback's directory** (`integrations::state_cookie_path` takes the callback path up to the last `/`).
- `state_matches(expected, provided)` requires both present, non-empty and equal.
- `with_cleared_state(path, response)` clears the cookie one-shot on *every* callback outcome, so a `state` cannot be replayed.
- `public_wizard_outcome()` returns `Allowed | Denied | AdminOnly`: an integration with its own `acl` is self-service (evaluated with `claims: None` since a top-level navigation carries no bearer); without one it is admin-gated, and when OIDC is on it returns `AdminOnly` because a bearer cannot ride a top-level navigation.
- Server-rendered pages are `html_page`/`html_action_page`/`error_page`, everything escaped with `html_escape::encode_text` / `encode_double_quoted_attribute`.
- **Crucially**, the account is *not* read from the callback request: `agent/src/integrations/state.rs` (`PendingAuthorizations`, partition `oauth-state` in the system tenant, 10-minute `LIFETIME`) records the initiating tenant server-side at `begin()` and the callback `claim()`s it. The doc comment spells out the attack this prevents.

---

## 5. API contract crate (`api/`)

Pure serde DTOs, no framework/db/UI deps, compiled for both native and wasm. Modules: `audit`, `connection`, `ids`, `integration`, `kv`, `queue`, `run`, `tenant`, `user`, `webhook`, `wordlist`, `workflow`.

Conventions:
- `#[serde(default, skip_serializing_if = "Option::is_none")]` on every optional; `skip_serializing_if = "Vec::is_empty"` / `"Map::is_empty"` on collections.
- Enums are `#[serde(rename_all = "kebab-case")]` (audit, run) or `"snake_case"` / `"lowercase"` (connection status, queue status), tagged unions use `#[serde(tag = "kind", rename_all = "snake_case")]` (`WorkflowTrigger`, `FieldKind`).
- Every enum also has `as_str()`, `label()` and `parse()` so the same strings serve wire, storage and UI.
- **Versioning:** there is no version field or module. The single version lives in the URL prefix (`/api/v1`). The stability story is instead structural: `deny_unknown_fields` on config, and note the `agent/src/web/api/mod.rs` comment about `WorkflowType::type_id()` being distinct from `Job::partition()` precisely because "a partition is a routing detail that has been renamed before whereas this ends up inside stored records, where a rename is a migration."
- **Error shape:** one helper, `web::api::json_error(status, message)` → `{"error": "<message>"}`, mirrored on the client by `struct ServerError { error: String }`. `web/integrations.rs` has a matching `json_error`/`html_error` pair so the same handler can answer either surface, switching on `err.is(human_errors::Kind::User)` (show the message) vs `Kind::System` (log, record, generic message).

Two id types worth lifting wholesale:
- `api/src/ids.rs` — `WordId<const N: usize>(u64)` over a 2048-word BIP-39 list (11 bits/word); `WorkflowId = WordId<3>`, `ConnectionId = WordId<2>`. Documented as *names, never credentials*; generation is random with retry-on-collision (`ID_ATTEMPTS = 8`).
- `api/src/webhook.rs` — `WebhookToken([u8; 16])`, 128-bit, constant-time comparison, generated agent-side (the crate carries no RNG so it stays wasm-clean).
- `api/src/tenant.rs` — `TenantId(String)` with reserved `!system` / `!local` (leading `!` is unrepresentable in an OIDC username), `new()` validating, `from_storage()` bypassing validation for reads.

---

## 6. Yew UI

`ui/src/`: `main.rs`, `app.rs`, `api.rs`, `auth.rs`, `search.rs`, `util.rs`, `components/` (24 files), `pages/` (14), `fixtures/` (3).

**Entry** (`main.rs`): `console_error_panic_hook::set_once()`, `wasm_logger::init`, `yew::Renderer::<app::App>::new().render()`.

**Router** (`app.rs`): `#[derive(Routable)] enum Route` with `/`, `/auth/callback`, `/admin`, `/admin/`, `/admin/connections|workflows|activity|users`, plus `#[cfg(debug_assertions)] /demo/controls` and `/demo/controls/:control`, and `#[not_found] #[at("/404")]`. `App` wraps `BrowserRouter`; `AppInner` runs the `use_auth()` hook and provides `ContextProvider<AuthHandle>`, with `<Switch<Route> key={generation} />` — the generation counter is bumped on impersonation change so every page *remounts* and drops the previous account's data.

`AuthStatus`: `Loading | Disabled | SignedIn(AdminUser) | NeedsLogin | Forbidden | Error(String)`, resolved by probing `GET /api/v1/me` (204 ⇒ `Disabled`, 401 ⇒ `NeedsLogin`, 403 ⇒ `Forbidden`). `AuthHandle { status, user, acting_as, login, signout, act_as }`. `pages/protected.rs` gates children on that status so they only mount (and only fetch) once access is granted.

**API client** (`ui/src/api.rs`, 649 lines):
- `const API_BASE = "/api/v1"`, relative to origin.
- `enum Verb { Get, Post, Put, Patch, Delete }` exists solely so a request can be *rebuilt* for the post-refresh retry.
- `build(verb, url, token, body)` attaches `Authorization: Bearer` and **always** attaches `X-Impersonate-User` when set ("sent on every call rather than on the ones a page thinks are account-specific, because a page that forgot it would silently write to the wrong account").
- `send()`: on `401` → `auth::refresh_session()` → rebuild and retry **once**; on refresh failure `clear_token()` and return the original 401.
- `enum ApiError { Unauthorized, Forbidden, Network(String), Server(String) }`, `error_from_response` maps 401/403 then reads `{"error": …}`.

**OIDC popup flow** (`ui/src/auth.rs`, 419 lines):
- Storage keys: `automate.admin.token`, `automate.admin.refresh`, `automate.admin.impersonate`, `automate.oidc.state` in **sessionStorage**; `automate.oidc.popup_result` in **localStorage** (popups don't share sessionStorage with their opener).
- `begin_login()`: mint `state` (`crypto.getRandomValues`, 24 bytes, base64url), `fetch_metadata()`, `build_authorize_url()`, `window.open(..., "popup,width=480,height=720")`, then **poll** the localStorage handoff slot every 300 ms up to 2000 times (~10 min), also checking `popup.closed()`.
- `complete_callback()`: reads `?code&state`, compares against stored state, POSTs to `/api/v1/auth/token`; if `is_popup()` writes tokens to the handoff slot and `window.close()`; otherwise stores them and `history.replace_state` to scrub the code from the address bar.
- `refresh_session()` is wrapped in a bespoke `single_flight` module (`Shared<LocalBoxFuture>` + generation counter behind a `thread_local!`) — because providers rotate refresh tokens and several polling loops can 401 simultaneously. This is the reason `futures` is in `ui/Cargo.toml`.

**Demo mode** (`ui/src/fixtures/`): `is_demo()` checks `location.search.contains("demo")`. The substitution is a `macro_rules! demo!` invoked as the first line of each `api.rs` function — so pages never branch on demo mode and *cannot* forget a stub. All of `fixtures/data.rs` (796 lines of sample data) and `fixtures/store.rs` (a mutable `RefCell` store so pausing/deleting *sticks* for the tab) are `#[cfg(debug_assertions)]`, and `is_demo()` is a `const false` in release. `util::nav_href()` carries `?demo` across full-page navigations. There is also a debug-only control gallery at `/demo/controls` rendering every shared component in every state (`ui/src/pages/demo/specimens.rs`, 1389 lines).

**Styles:** a single hand-written `ui/styles.scss` (2953 lines), **no CSS framework** — the header calls it "a lightweight, typography-centric theme drawing on Element Plus for its neutral palette". Organised in 8 numbered sections (design tokens → base/reset → primitives → layout chrome → pages → partition browser → control gallery → responsive), SCSS variables for brand/neutrals/status, `@mixin card` and `@mixin focus-ring`. Loaded by Trunk via `<link data-trunk rel="scss">`. Class naming is BEM-ish (`btn btn--small btn--primary`, `loading-note`).

**Component conventions** (`ui/src/components/mod.rs` doc): all text through Yew `{value}` interpolation (auto-escaped); JSON rendered as plain text inside `<pre><code>`. `AdminShell` owns the app bar, page title row, an injectable `PageActions` slot (pages push a refresh button into the shared header via context), a shared search query + completion `SearchVocabulary`, and wraps children in `Protected`.

**Build output:** `ui/dist`, consumed by `include_dir!("$CARGO_MANIFEST_DIR/../ui/dist")`.

---

## 7. Testing

**Rust tests are all in-file `#[cfg(test)] mod tests`** — there is no `agent/tests/*.rs`; `agent/tests/data/` holds fixture files only (`calendar_large.ics`, `github_notifications.json`, `github_releases.json`, `xkcd.rss.xml`, `youtube.atom.xml`), reached via `testing::get_test_file_path/contents` built from `env!("CARGO_MANIFEST_DIR")`.

Helpers (`agent/src/testing.rs`, `#[cfg(test)] mod testing` in `main.rs`):
- `mock_services()` → `ServicesContainer::new_mock()`.
- `AppContext::new_mock(|config| …)` and `ServicesContainer::new_custom_mock(|config, db| …)` — both go through `SqliteDatabase::open_in_memory()` + `SecretStore::ephemeral()` + `Session::new(...).with_battery(tracing_batteries::Testing)`, explicitly "so that tests construct their services the same way the running agent does".
- `SqliteDatabase::open_in_memory_at_migration(n)` + `upgrade()` for migration tests against realistic data.
- `static GITHUB_APP_PRIVATE_KEY: LazyLock<String>` — a real RSA-2048 key **generated once per test process** (never committed), which is why the workspace bumps `opt-level` for `rsa`/`num-bigint-dig`.
- `agent/src/testing/oidc.rs` (351 lines) — `TestIdentityProvider`: a `wiremock::MockServer` serving a **real** discovery document and JWKS and minting **real RS256** tokens. The module doc explicitly argues against the `#[cfg(test)]` shortcut ("that branch is the algorithm-confusion vulnerability written down deliberately… the thing under test is not the thing that ships").
- actix handler tests use `test::init_service(App::new().app_data(...).service(configure()))` behind a local `macro_rules! app!`.
- `agent/src/web/api/tenancy_tests.rs` (1135 lines) — a dedicated cross-tenant isolation suite proving two *real* sign-ins can't see each other's records, plus an admin impersonating each in turn.

**`e2e/`** — Playwright (`@playwright/test ^1.62.1`, TypeScript, chromium only, `fullyParallel: false`, `workers: 1`).
- `e2e/playwright.config.ts`: `webServer.command = "node scripts/start-agent.mjs"`, readiness on `${baseURL}/robots.txt`, port **8099** (deliberately not 8080 so a run can't hit the developer's real database), `gracefulShutdown: { signal: "SIGTERM" }` so the launcher can clean up, `timeout: 60s` / `expect: 15s` because the wasm bundle is megabytes.
- `e2e/scripts/start-agent.mjs`: does **not** build; resolves `target/{debug,release}/automate` (newest wins, `AUTOMATE_E2E_BINARY` overrides), sweeps stale `automate-e2e-*` temp dirs older than 6 h, `mkdtemp`s a scratch dir, writes a minimal `config.toml` with `user_acl = 'true'` / `admin_acl = 'true'`, spawns with `--config <scratch> --env <nonexistent path>` and `cwd: scratchDir`, removes the directory on exit/SIGINT/SIGTERM/SIGHUP.
- Specs: `smoke`, `navigation`, `activity`, `connections`, `dynamic-form`, `impersonation`, `webhooks`, `workflows`.
- `e2e/tests/helpers.ts`: a `base.extend` fixture installing an `addInitScript` that latches the `TrunkApplicationStarted` event into `window.__automateStarted` (turning a race into a poll), `waitForApp`/`gotoApp`, `uniqueName(prefix)`, and API-level `createConnection`/`deleteConnection` so preconditions don't go through the UI.
- `e2e/README.md` documents two real traps: the repo's root `.env` is a **named pipe** (`Path::exists()` is true for a FIFO, so the agent blocks forever), and debug builds print nothing because telemetry is disabled under `debug_assertions` — hence never gate readiness on log output.

---

## 8. Reusable for rustak (TCP/TLS listeners, long-lived connections, fan-out, X.509)

**What is *not* there — be aware you're building these from scratch:**
- **No rustls/TLS server code at all.** `rustls` appears in `Cargo.lock` only transitively via `reqwest`'s `rustls-tls` (client side). No `TlsAcceptor`, no `TcpListener`, no `tokio::net` server usage (`tokio`'s `net` feature is enabled but unused for listening — actix owns the socket).
- **No X.509 issuance.** No `rcgen`, no `x509-parser`, no CA machinery. `openssl-sys` is vendored only to satisfy a transitive dependency on the release targets.
- **No in-process pub/sub.** No `tokio::sync::broadcast`, no `watch`, no `Notify`. The only `tokio::sync` uses are `Mutex` in `services/alphavantage.rs` (rate limiting) and `oneshot` inside a `db/sqlite.rs` contention test. Fan-out is exclusively *through SQLite*.
- **No graceful-shutdown signal handling** beyond the `Race` + `JoinSet`-drop + `Arc::try_unwrap` telemetry flush in `main.rs`. There is no `ctrl_c()` handler; actix handles SIGINT itself.

**What *is* directly liftable:**

1. **The SQLite queue as a job/fan-out substrate** — `queues(tenant, partition, key, payload, scheduledAt, hiddenUntil, reservedBy, traceparent, tracestate, idempotencyKey)`. Visibility-timeout semantics: `dequeue` does `SELECT … WHERE hiddenUntil < CURRENT_TIMESTAMP ORDER BY scheduledAt LIMIT 1` then `UPDATE … SET reservedBy = ?, hiddenUntil = ?` inside one transaction; `complete` deletes `WHERE … AND reservedBy = ?` (so a lapsed reservation can't delete someone else's work); `reserve()` narrows the timeout post-dequeue and doubles as retry backoff; `enqueue` is an `INSERT … ON CONFLICT DO UPDATE` keyed on the idempotency key. `dequeue_any_global` polls with a 1 s sleep when empty and documents its fairness limitation honestly. Two indexes: tenant-leading for scoped consumers, `(hiddenUntil, scheduledAt)` for the global one.

2. **The `inventory` plugin-registry pattern, used three times** — copy it for TAK message handlers / CoT types:
   - `job.rs`: `Job` (generic, testable) + object-safe `JobRunnable` with a blanket impl that deserializes the payload; `JobRegistration(&'static dyn JobRunnable)`; `inventory::collect!` + `macro_rules! register_job!`; duplicate partition detected at startup.
   - `integrations/mod.rs`: `Integration` trait (`instances`, `acl`, `callback_path`, `begin_setup`, `complete_setup`, `connections`, `disconnect`, `refresh`) + `Registry::new(&config)?`.
   - `webhooks/routing.rs`: `WebhookSource` trait — the module doc is a textbook justification for extracting a shared fan-out loop out of N near-duplicate handlers, reducing each provider to four expressions (what it signs with, how it signs, which account a delivery names, which connections it maps to). This is the closest existing analogue to "fan out a message to every subscribed client".

3. **`AppContext` → `AppServices` scoping + `Scoped`/`Administrative` extractors** — the cleanest way to make "this connection may only touch its own records" a *type* property. Maps well onto per-client TAK connection state.

4. **`crypto.rs` in its entirety** — AES-256-GCM with context-as-AAD, key file beside the database, keyed rotation. For rustak this is exactly what you want for storing CA private keys and client enrolment secrets. `SecretContext` becomes e.g. `Ca { … }` / `ClientCert { … }`.

5. **`filt-rs` ACL evaluation + `Filterable`** — for TAK you'd expose `client_ip`, `claims.*`, plus cert subject/SAN fields; the `AdminRequestFilter` prefix-matching pattern (`headers.` / `claims.`) generalises directly.

6. **`human-errors` + `ResultExt` discipline** — `wrap_user_err(msg, &[advice…])` / `or_system_err(&[…])`, and the `err.is(human_errors::Kind::User)` split that decides whether a message is shown or logged-and-generalised. Pervasive; adopt from day one or not at all.

7. **`tracing-batteries` `Session`** — one `Arc<Session>` threaded through `Services::session()`, `record_human_error(&err)` at every swallow point, and the `shutdown_session` flush dance. Note it forces `protoc` into your CI and cross images.

8. **`serde_duration::{minutes, minutes_option}`**, `parsers::interpolate` (the `${{ env.X }}` engine, ~40 lines, fully generic over the handler), `db::Partition<D,T>`, `api/src/ids.rs` word-ids, `api/src/webhook.rs` token type.

---

## Flagged: domain-coupled vs. generic

**Tightly coupled to automate's domain — do not lift:** `collectors/*`, `publishers/*`, `webhooks/{github,grey,grafana,honeycomb,sentry,tailscale,terraform,todoist,azure_monitor}.rs` (~8k lines), `jobs/*` (except `cron.rs`'s reconciliation shape), `integrations/{todoist,ynab,github_app}.rs`, `services/{github,github_app,alphavantage}.rs`, `parsers/{calendar,html}.rs`, `workflow_toml.rs`, `webhook_payload.rs`, `connections.rs`'s `ConnectionKind` variants, and the `[connections.*]` config sections.

**Genuinely generic infrastructure:** `crypto.rs`, `db/{mod,sqlite,cache,partition,audit}.rs`, `services/mod.rs`, `job.rs` (minus the registered jobs), `filter.rs`, `web/{mod,ui,telemetry,principal}.rs`, `web/api/{mod,scope,auth,user,kv,queue,audit,admin}.rs`, `web/helpers/*`, `integrations/{mod,state}.rs`, `webhooks/routing.rs` (as a fan-out template), `users.rs`, `runs.rs`, `webhook_index.rs`, `parsers/interpolation.rs`, `serde_duration.rs`, `config.rs`'s `[web]`/`[web.auth]`/`[audit]` shape, the whole `api/` crate minus `workflow.rs`/`connection.rs` specifics, `ui/{api,auth,app,util}.rs` + `fixtures/mod.rs` + `components/{alert,form,layout,page_title,status_pill,menu_button,refresh_button,secret_input,json_highlight}.rs`, and the entire `e2e/` harness.

**Two staleness warnings:**
- `.github/copilot-instructions.md` describes the auth flow as *server-driven PKCE with an `HttpOnly` session cookie and a double-submit `X-CSRF-Token`, with routes `/api/v1/auth/login` and `/api/v1/auth/callback`*. That is **no longer true** — the code implements a browser popup + bearer-in-sessionStorage with no PKCE and no CSRF token (see `agent/src/web/api/auth.rs` and `ui/src/auth.rs`). It also claims a `src/filter/` directory and a `src/services.rs` file that are actually `filter.rs` and `services/`. Take the code, not that document.
- `config.toml` and `database.sqlite` exist in the working tree (both gitignored), and the repo root `.env` is a **named pipe** — never `cat` it or run a recursive grep from the root.

# rustak — a lightweight Rust-native TAK server

> Status: IN PROGRESS (2026-09-19). M0–M6 landed; both interop suites 9/9; the three independent reviews
> (`.claude/plan/reviews/`) are actioned except the stream-side robustness items (M1-10, in flight). Remaining:
> first pre-release tag (user decision), running the manual checklists in `docs/compat/` on real devices, and the
> backlog in `.claude/plan/backlog.md`.

## Context

Running the official TAK Server or OpenTAKServer for a tiny personal deployment (hike tracking,
HAM radio integration) is cost- and complexity-prohibitive, and OpenTAKServer is unstable and
only partially compatible with CloudTAK. We are building **rustak**: a single-binary Rust TAK
server backed by embedded SQLite, structured like `../automate` (actix-web + tokio, OIDC,
Yew/Trunk SPA embedded via `include_dir`, TOML config with `${{ env.X }}`), targeting
first-class compatibility with **ATAK-CIV** (EUD) and **CloudTAK** (web UI). Our own Yew UI is
for bootstrap, enrollment, configuration and admin only.

Reference material was researched from source (checked out under the session scratchpad
`refs/`: `CloudTAK`, `node-tak`, `node-CoT`, `OpenTAKServer`, sparse `atak-civ`, sparse
`takserver`). automate's architecture map is in the appendix.

## Decisions (confirmed with user)

| Topic | Decision |
|---|---|
| OIDC / CloudTAK | rustak is the identity authority: full OAuth2 server (`/oauth/token` password grant + authorize-code flow federating to the OIDC IdP, TAK-Server-style `/login/*`). Users/groups from OIDC claims; users mint **per-device passwords / enrollment tokens** in the Yew UI. CloudTAK (13.90 has no OIDC backend route) logs in with username + device password today; works unchanged once CloudTAK ships OIDC. |
| License | Permissive (MIT/Apache-2.0). **Clean-room** protobuf definitions written from documented field numbers/types; no copying from GPL atak-civ/TAK Server. (crates.io name `rustak` is taken; irrelevant unless publishing.) |
| Plugin model | **TAK-native sidecars**: a plugin is an authenticated TAK client (service identity with cert/device password, group-scoped) speaking CoT on the stream + Marti API, like CloudTAK Connections. Plus a small `/api/v1` control surface (service registry, health, config KV, server-event feed). |
| TLS exposure | **Built-in ACME (Let's Encrypt) + BYO cert files** for the public HTTPS listener(s); internal CA issues certs for the mTLS Marti listener and the TLS stream. |
| Secure by default | **No plaintext TCP stream listener at all** (8087/stcp removed; only TLS :8089 with client cert required). No `<auth>` credential message, no anonymous stream access. Marti :8443 requires a client cert. Admin UI auth = OIDC or **passkeys (WebAuthn)**; there are no local user passwords. First-run bootstrap uses a one-time setup token. ATAK enrolment uses **one-time enrollment tokens** (QR). The only reusable secret is an opt-in, expiring **client password** a user mints explicitly for clients that cannot do better (CloudTAK's `/oauth/token` password grant + Basic `signClient`); it is scoped to those two endpoints, audited, and revocable. Public listener TLS defaults to the internal CA (`internal`) or ACME; plaintext HTTP requires an explicit `allow_insecure_http = true` (dev/e2e only). |
| Time-series storage | Append-only, binary (protobuf, TAK-frame encoded) **segment files per stream** with metadata/index rows in SQLite for anything that grows continuously (CoT history / location tracks, later telemetry). SQLite holds state and metadata, never high-rate streams. |
| Interop in CI | Compatibility is proven by automated suites, not ad-hoc agent testing: node-tak contract suite on every PR; CloudTAK full-stack compose and a commoncommo-based EUD harness nightly; Rust fake-EUD suites on every PR. An exploration brief in M1 settles the ATAK-side harness. |
| Code quality | Modular crates/modules; **< 300 lines of functional code per file** (tests may exceed); current stable dependencies; `clippy -D warnings`; CI enforces file-length check. |
| Deferred | Video streaming/recording, voice, federation, QUIC, ExCheck, iTAK-specific quirks (best-effort only). |

## Architecture

### Workspace (flat, one top-level folder per crate)

```
rustak/
├── Cargo.toml               # [workspace] members = ["rustak-*"], exclude = ["rustak-ui"], resolver = "3",
│                            #   default-members = ["rustak-server"], [workspace.package], [workspace.dependencies], [workspace.lints]
├── rustak-cot/              # lib: CoT XML model + TAK Protocol v1 (clean-room prost) + frame codecs. No I/O, no server deps.
├── rustak-api/              # lib: serde DTOs for the admin API (server ↔ UI). wasm-safe: serde/chrono/uuid only.
├── rustak-core/             # lib: shared foundation for every binary — config loading + `${{ env.X }}`, telemetry Session
│                            #   bootstrap, human-errors prelude, credential/identity primitives, service-descriptor types.
├── rustak-client/           # lib: sidecar SDK — TAK stream client (TCP/TLS, negotiation, ping, reconnect), typed Marti API
│                            #   client (missions, files, groups, contacts, enrollment), control-API client (register/heartbeat/
│                            #   config/events). Built on rustak-cot + rustak-core. Also used by the server's integration tests.
├── rustak-server/           # lib + bin `rustak`: listeners, SQLite, PKI/ACME, Marti API, OAuth2, admin API, embedded UI.
│                            #   `src/lib.rs` exposes the app for in-process integration tests; `src/main.rs` is thin.
├── rustak-ui/               # Yew SPA (Trunk). Excluded from the workspace (wasm target); embedded via include_dir!("../rustak-ui/dist").
├── rustak-plugin-example/   # bin: minimal sidecar template (service identity → connect → publish/subscribe CoT → heartbeat).
│                            #   Copy-and-rename recipe for rustak-plugin-adsb / -ais / -meshtastic / -meshcore later.
├── e2e/                     # Playwright (harness lifted from automate)
├── docs/                    # compat/*.md (wire contracts), plugins.md (sidecar recipe), deployment.md
├── scripts/                 # check-file-length.sh (CI: < 300 functional lines/file), dev helpers
└── .github/workflows/       # rust.yml (test+coverage, ui, e2e, cross matrix, docker, tap), security_audit.yml, changelog.yml
```

Dependency direction: `rustak-cot` ← `rustak-core` ← `rustak-client` ← `rustak-plugin-*`; `rustak-server` depends on
`rustak-cot`, `rustak-core`, `rustak-api` (and `rustak-client` as a dev-dependency). `rustak-api` ← `rustak-ui`.
All crate versions come from `[workspace.package]`; all third-party versions from `[workspace.dependencies]`
(`dep = { workspace = true }` in each crate); clippy/rustc lints from `[workspace.lints]`. A new plugin is:
`cargo new rustak-plugin-<name>` (auto-picked-up by the `rustak-*` glob), depend on `rustak-client`, implement
the `Sidecar` trait from `rustak-client::sidecar`, and add a CI matrix entry.

### Listeners (single process)

| Listener | Default | TLS | Auth | Serves |
|---|---|---|---|---|
| public HTTPS | `:8446` (add `:443` for browsers/ACME) | ACME / BYO / internal CA (never plaintext unless `allow_insecure_http`) | Bearer (our JWT) for `/api/v1` and Marti; Basic **only** on `/Marti/api/tls/*` and `/oauth/token` (enrollment token or client password); cookies only on `/login/*` | Yew admin UI, `/api/v1`, `/oauth/*`, `/login/*`, passkey ceremonies, `/Marti/api/tls/*` enrollment + enrollment profile, full Marti (for CloudTAK `webtak`). TAK's "8446 / webtak" role. |
| Marti mTLS | `:8443` | internal-CA server cert, client cert **required** | client cert (CN→user, fingerprint→device+revocation) | Full Marti API (`/Marti/**`, `/files/api/config`) |
| TAK stream TLS | `:8089` | internal-CA server cert, client cert **required**, TLS 1.3 offered | client cert | CoT stream; TAK Protocol v1 negotiation |

There is deliberately **no** plaintext TCP/stcp stream input and no anonymous access; a device that cannot
present a certificate must enrol first. Ports are configurable; CloudTAK is pointed at
`url=ssl://host:8089`, `api=https://host:8443`, `webtak=https://host:8446`.

### Server module tree (`rustak-server/src/`)

```
main.rs            thin: clap args (--config, --env) → rustak_core::config::load → rustak_server::run()
lib.rs             run(): build AppContext, (web, listeners, jobs).race(); re-exports for integration tests
prelude.rs         re-exports (Config, Services, human_errors::ResultExt, tracing…)
config/            mod.rs Config (uses rustak_core::config loader); web.rs, listeners.rs, auth.rs, pki.rs, acme.rs, storage.rs, retention.rs (all deny_unknown_fields)
services/          AppContext (config, db, secrets, pki, hub, http_client, session); Services trait; extractors
db/                sqlite.rs (open, WAL pragmas, hand-rolled migration runner), migrations/*.sql (include_dir), kv.rs, queue.rs, audit.rs, repos/ (one file per aggregate)
crypto.rs          AES-256-GCM SecretStore/Sealed with context-as-AAD, key file beside DB (lifted from automate)
pki/               ca.rs (root CA create/load, rcgen), issue.rs (CSR→client cert), server_cert.rs, revoke.rs, p12.rs (PKCS#12 for manual packages), acme.rs (instant-acme, HTTP-01/TLS-ALPN-01, renewal job), tls_config.rs (rustls ServerConfig builders, client-cert verifier → principal)
identity/          users.rs, groups.rs (IN/OUT, bitpos, __ANON__), members.rs (+ per-device active state), devices.rs (EUD/uid ↔ user), credentials.rs (one-time enrollment tokens; opt-in expiring client passwords; service tokens — argon2id, uses/expiry, purpose scoping), provisioning.rs (OIDC JIT + group-claim mapping), services.rs (plugin identities)
auth/              oidc/ (discovery/JWKS/validate/PKCE — from automate + nonce), passkeys.rs (WebAuthn registration/authentication via `webauthn-rs`; admin bootstrap and local sign-in), oauth_server/ {token.rs (password grant for client passwords only; refresh; code), authorize.rs (code flow → IdP), jwt.rs (RS256, fixed 27-byte header, flat claims), login.rs (/login/*)}, principal.rs, resolve.rs (cert → bearer → basic(enrollment/oauth paths only) → Principal; per-listener policy), ratelimit.rs, acl.rs (filt-rs), mission_token.rs (HS256), audit.rs
stream/            listener_tls.rs (tokio-rustls, client cert required, principal from peer cert), connection.rs (per-conn task: codec state machine, negotiation, ping/timeout), subscription.rs (uid, callsign, groups bitvec, latest SA, incognito), hub.rs (registry + fan-out), router.rs (marti/dest, mission dest, group reachability, flow tags, echo suppression), control.rs (t-x-c-t, t-x-takp-q, t-x-c-i-*), replay.rs (latest reachable SA on connect), notify.rs (builders: t-x-d-d, t-x-g-c, t-x-m-*), mission_hook.rs (MissionIngest trait)
cot_store/         latest.rs (per-uid latest in SQLite), history.rs (per-uid append-only segment files via `store::append_log`, time index rows in SQLite; serves /cot/xml/{uid}/all, /cot/sa, mission /cot), retention.rs (segment pruning)
store/             append_log.rs (generic append-only segment writer/reader: `<data_dir>/streams/<kind>/<key>/<segment>.log`, frames = varint length + protobuf payload (TAK frame format for CoT), crash-safe open truncates to the last complete frame, sparse time index persisted to SQLite `stream_segments`), content.rs (content-addressed file store)
missions/          model.rs, service.rs, roles.rs, subscriptions.rs, changes.rs, contents.rs, layers.rs, logs.rs, invitations.rs, keywords.rs, archive.rs, cot.rs
files/             store.rs (content-addressed dir, sha256), metadata.rs (Resource), search.rs, package.rs (MissionPackage manifest read/write)
profiles/          model.rs, builder.rs (zip + MANIFEST), prefs.rs (.pref writer), service.rs
marti/             actix scopes: mod.rs (auth middleware), version.rs, tls.rs, oauth.rs, groups.rs, contacts.rs, subscriptions.rs, cot.rs, sync.rs (/Marti/sync/*), files.rs, profiles.rs, video.rs (stubs), missions/ {mod.rs, crud.rs, subscription.rs, contents.rs, changes.rs, layers.rs, logs.rs, invitations.rs}
web/               mod.rs (HttpServer builders per listener), ui.rs (include_dir SPA), telemetry.rs, helpers/, api/ (v1 admin API: auth, me, users, devices, credentials+qr, groups, missions, packages, profiles, services, events (SSE), audit, settings, setup)
plugins/           registry.rs, health.rs, events.rs (server-event bus → SSE)
jobs/              host.rs (SQLite queue consumer, from automate), acme_renew.rs, cert_expiry.rs, retention.rs, mission_expiry.rs, audit_prune.rs
testing/           mock services, test CA, TestIdentityProvider (from automate), fake EUD client built on `cot`
```

### `rustak-cot` crate (`rustak-cot/src/`)

```
lib.rs        Event, Point, Detail (raw XML tree + typed accessors), Type/How constants
xml/          parse.rs (quick-xml → Event), write.rs (Event → XML, declaration + no trailing newline), detail/ {contact.rs, group.rs, takv.rs, track.rs, status.rs, precision.rs, marti.rs (dest), chat.rs, fileshare.rs, link.rs, mission.rs}
proto/        rustak.proto (clean-room TakMessage/CotEvent/Detail/… — field numbers as facts), build.rs (prost-build), convert.rs (Event ⇄ TakMessage per detail-mapping rules)
codec/        xml_frame.rs (scan `</event>`), proto_frame.rs (0xBF + varint), negotiate.rs (t-x-takp-v/q/r builders/parsers), frame.rs (enum Frame, tokio_util::codec Decoder/Encoder)
fixtures/     own sample messages (no copied captures)
```

### Storage

SQLite (rusqlite bundled + tokio-rusqlite, WAL, busy_timeout, STRICT tables) holds **state and
metadata**: users, devices, certificates, credentials, passkeys, groups, group_members,
device_group_state, missions and their sub-aggregates, resources, cot_latest, profiles, services,
settings, kv, queues, audit_log, acme_accounts, stream_segments. Two things never live in SQLite:

- **File content** (data packages, attachments, profile files): content-addressed directory
  `<data_dir>/content/<sha256>` with metadata rows in SQLite; streamed from disk.
- **Time-series streams** (CoT history / location tracks now; telemetry later): a generic
  **append-only segment log** (`store::append_log`) — one directory per stream key (e.g. per CoT
  uid), rolling segment files, each record a varint-length-prefixed **protobuf** payload (for CoT the
  `TakMessage` frame we already speak on the wire, so no second schema), crash-safe open by
  truncating to the last complete frame, sparse time index + segment metadata in SQLite
  (`stream_segments`: key, path, first/last time, count, bytes). Readers seek via the index;
  retention deletes whole segments. This keeps SQLite write volume to metadata only and keeps
  files small and fast to parse.

Background work uses automate's SQLite queue table + `JobHost`.

### Identity & auth model (secure by default)

- **Users**: created on first OIDC sign-in (username claim, JIT) or by an admin; `kind ∈ {person, service}`; admin via `admin_acl` on claims or an explicit override. **No local passwords exist.** Local (non-OIDC) sign-in to the Yew UI is by **passkey** (WebAuthn, `webauthn-rs`); the first-run wizard requires a one-time setup token and then registers the first admin's passkey (or configures OIDC).
- **Groups (channels)**: name + direction (IN = write, OUT = read) + bitpos; `__ANON__` default; membership manual or mapped from an OIDC `groups` claim (`_READ`/`_WRITE` suffix convention); per-device active state.
- **Credentials** (all argon2id-hashed, shown once, labelled, audited, revocable):
  - `EnrollmentToken` — **one-time**, short TTL (15 min default), consumed on successful `signClient`; embedded in `tak://com.atakmap.app/enroll?host=&username=&token=` QR codes or typed into ATAK's "quick connect". This is the default and only recommended path for EUDs.
  - `ClientPassword` — **opt-in**, expiring (90-day default), only for clients that cannot use anything better today (CloudTAK's `/oauth/token` password grant and its Basic `signClient/v2`); accepted **only** on those endpoints, never for UI or Marti access; the UI labels it as a compatibility credential.
  - `ServiceToken` — for sidecars' control API (mTLS cert is their primary identity).
- **Client certs**: CSR from ATAK/CloudTAK (key kept) → issued by the internal CA with **our** subject (`CN=<username>` + configured O/OU), EKU clientAuth, 128-bit serial, SKI/AKI, configurable validity; persisted (fingerprint, serial, user, device, credential used). The stream and Marti listeners require the cert; a custom rustls `ClientCertVerifier` checks chain, expiry, revocation and `require_known_cert` **at handshake**. Revoking a cert disconnects live sessions.
- **Our JWT** (RS256): header exactly `{"alg":"RS256","typ":"JWT"}`; flat claims only (`sub`, `iss`, `aud`, `iat`, `nbf`, `exp`, `jti`, `scope`, optional `dev`); issued to the UI (after OIDC/passkey), to CloudTAK (`/oauth/token`), and to sidecars; verified RS256-only; `jti` revocation; rotating refresh tokens with family revocation on reuse. Groups are looked up by `sub`, never embedded.
- **Mission tokens**: HS256 JWT with a dedicated sealed secret (`SUBSCRIPTION|INVITATION|ACCESS`, `MISSION_NAME`, `MISSION_GUID`), read from `MissionAuthorization` then `Authorization`.
- **Hardening**: rate limiting + lockout on every credential endpoint, constant-time dummy verification for unknown users, `Secure`/`HttpOnly`/`SameSite` cookies only on `/login/*`, bearer-only APIs (no CSRF surface), TLS on every listener, no plaintext unless explicitly allowed, all key material sealed at rest.

### Plugin (sidecar) contract

Service identity = user of kind `service` with its own cert/device password and group
scoping; connects to `:8089` like any EUD (uid `SERVICE-<name>`), uses Marti API with its cert.
Control API (`/api/v1/services/*`, service-token auth): register/list, heartbeat+status,
per-service config KV, `GET /api/v1/events` SSE feed (client connect/disconnect, mission
changes, group changes, package uploads). Documented in `docs/plugins.md`.

`rustak-client` layout: `stream/` (connect string parsing, TCP/TLS connector with truststore +
client cert, negotiation to protobuf, ping/timeout, reconnect with backoff, `Stream<Item=Event>`
+ `Sink`), `marti/` (typed client per Marti area, mission tokens, multipart uploads),
`control/` (service registration, heartbeat, config, SSE events), `enroll.rs` (CSR → cert via
device password), `sidecar/` (`Sidecar` trait + `run(config)` harness: loads config via
`rustak-core`, enrolls/loads identity, connects, drives the trait's `on_event`/`tick`, reports
health). `rustak-plugin-example` is ~100 lines implementing `Sidecar`.

### CI/CD & releases (first-class from M0, modelled on `../automate/.github/workflows`)

| Workflow | Jobs (adapted from automate) |
|---|---|
| `rust.yml` (push/PR/release) | `deduplicate` (merge-tree success marker cache) → `version` (rewrite `[workspace.package].version` from the release tag, upload artifact) → `lint` (`cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`, `scripts/check-file-length.sh`) → `test` (`cargo test --workspace --no-fail-fast` with `-Cinstrument-coverage` → grcov → codecov) → `ui` (cargo-binstall trunk; build **debug** bundle for e2e and **release** bundle for embedding) → `e2e` (Playwright against `rustak-server` debug build + debug UI) → `build` matrix **per binary crate** (`rustak-server`, `rustak-plugin-*`) × targets `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl` (cross), `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`; artifacts named `<crate>-<target>` → `ci` aggregator → `docker-build` (per-platform digests, one image per binary: `ghcr.io/sierrasoftworks/rustak`, `ghcr.io/sierrasoftworks/rustak-plugin-<name>`) → `docker-publish` (`buildx imagetools create` multi-arch manifest, tags `latest`, version, major, minor; on `main` and releases) → `tap` (`SierraSoftworks/actions-tap@v1`, formula `rustak`, aliases major/minor; release only) → release assets uploaded to the GitHub Release. |
| `changelog.yml` | `release-drafter/release-drafter@v7.7.0` drafting release notes from PR labels (same dedup cache). |
| `security_audit.yml` | nightly `rustsec/audit-check@v2`. |
| `.github/dependabot.yml` | daily cargo + github-actions + npm (e2e, ui tooling), grouped `opentelemetry`/`tracing`. |

Supporting files: `Dockerfile` per binary crate (`ubuntu:24.04` + `ca-certificates`, `ADD` the CI-built binary, `VOLUME /data`, `EXPOSE 443 8443 8446 8089 8087`, `ENTRYPOINT ["rustak", "--config", "/data/config.toml"]`), `Cross.toml` (pre-build installs only what is needed; prefer `protox` (pure-Rust protoc) in `rustak-cot/build.rs` so cross images need no `protobuf-compiler`), `.cargo/config.toml` (`rust-lld` for aarch64 musl), `release-drafter.yml` label→section config, and `docs/deployment.md` (docker-compose example including CloudTAK, Homebrew install, systemd unit).

## Implementation strategy — orchestrated agent fleet

The main session acts as **orchestrator**: it never writes feature code itself; it briefs, sequences and
reviews child agents, and keeps `.claude/plan/` (in the repo) as the shared, versioned knowledge base so
briefs stay short and nothing is retyped.

### `.claude/plan/` layout (created in M0 step 1, committed)

```
.claude/plan/
├── README.md                 # how agents use this directory; conventions for artefacts
├── plan.md                   # this plan (copied; kept current by the orchestrator)
├── conventions.md            # code standards: <300 functional lines/file, module layout, error handling (human-errors),
│                             #   tracing, testing patterns, commit-message convention, GitButler usage, no GPL copying
├── research/                 # the seven verified research reports (copied verbatim from the session scratchpad):
│   ├── 01-automate-architecture.md            (what to lift from ../automate, with paths)
│   ├── 02-tak-protocol-atak-web-research.md   (protocol overview; superseded where 05/07 differ)
│   ├── 03-cloudtak-node-tak-contract.md       (CloudTAK hard requirements, node-tak types, gotchas)
│   ├── 04-opentakserver-source-map.md         (OTS route table; what it gets wrong)
│   ├── 05-takserver-streaming-auth-verified.md (streaming/auth/routing/notifications — authoritative)
│   ├── 06-takserver-http-api-verified.md      (Marti HTTP contracts — authoritative, 1900 lines)
│   └── 07-atak-client-verified.md             (ATAK client behaviour — authoritative)
├── compat/                   # distilled wire contracts, one file per area, written by design agents from research/:
│   ├── streaming.md  enrollment.md  missions.md  files.md  profiles.md  groups.md  oauth.md  cloudtak.md
├── design/                   # design-agent outputs (copied from the session scratchpad `designs/`):
│   ├── 01-foundations-storage-ci.md            (workspace manifests, core/api crates, Database, schema DDL, config, web/UI skeleton, e2e, CI, 17 M0 steps)
│   ├── 02-protocol-streaming.md                (rustak-cot file map + clean-room .proto, codecs, hub/router/negotiation, rustak-client stream, 15 M1 steps)
│   ├── 03-identity-pki-acme-auth.md            (pki/, identity/, auth/, oauth_server/, oidc/, ACME, threat model, endpoint tables, M2+M5 steps)
│   └── 04-marti-api-missions-files-profiles.md (marti/ layout, envelope/type constants, missions/files/profiles/cot query, admin API+UI, M2–M4 steps)
├── briefs/                   # per-task briefs handed to implementation agents (Mx-NN-<slug>.md), each self-contained
└── status/                   # per-milestone progress notes + interop test logs written by agents for the orchestrator
```

### Agent roles and model selection

| Role | Model / effort | Used for |
|---|---|---|
| Design agent | Opus 5 or Fable 5.1, high | Turning a plan section + `research/` into a file-level design in `design/` and a `compat/*.md` contract |
| Implementation agent | Opus 5, medium–high | One brief → one coherent change set on its own GitButler branch (`feat/<slug>`), with tests, `cargo clippy -D warnings`, file-length check; writes `status/` note |
| Reviewer agent | Fable 5.1 or Opus 5, high | Independent review of a change set against the brief, `compat/*.md` and `conventions.md`; reports findings, does not fix |
| Operational agent | Sonnet 5, low–medium | Mechanical work: porting automate's CI workflows/Dockerfile/Cross.toml, scaffolding crates, copying fixtures, formatting, dependency bumps, doc moves |
| Interop tester | Opus 5, medium | Drives `rustak-client` fake EUDs and CloudTAK's docker-compose against a running server; records results in `status/` |

Rules of engagement: every brief names the files it may touch, the `compat/` contract it must satisfy, the tests
it must add, and the exit check (`cargo test -p <crate>`, clippy, file-length). Agents work in parallel only on
disjoint crates/modules; the orchestrator integrates via GitButler (one branch per brief, stacked when dependent)
and runs the full workspace check before merging. Anything an agent could not verify goes in its `status/` note,
never silently assumed. GPL sources under `refs/` are read for facts only; no code or comment text is copied.

### Pipeline per milestone

1. Orchestrator writes `briefs/Mx-*.md` from `design/` (parallelisable set + dependency order).
2. Implementation agents run in parallel on disjoint areas; operational agents handle scaffolding first.
3. Reviewer agent per brief (or per merged group) → orchestrator triages findings → fix-up briefs.
4. Interop tester runs the milestone's gate (fake EUD tests, CloudTAK compose, manual ATAK checklist) → `status/`.
5. Orchestrator updates `plan.md`/`compat/` with anything learned, tidies history, commits the checkpoint.

## Design artefacts and reconciled decisions

The four design documents (session scratchpad `designs/01–04`, to be committed under `.claude/plan/design/`)
are the file-level source of truth for implementation briefs. Decisions they introduced or reconciled:

| Area | Decision |
|---|---|
| Crate deps | Validated identity newtypes (`Username`, `DeviceUid`, `GroupName`, typed ids…) live in `rustak-api` and are re-exported by `rustak-core` (so UI and server share one type without tokio in the API crate). Dependency order: `rustak-api ← rustak-core ← rustak-client ← rustak-plugin-*`; `rustak-cot` is a leaf; `rustak-server` uses api/core/cot (+client as dev-dep). |
| Admin UI auth | The server issues **its own RS256 JWT + rotating refresh token** for both local login and OIDC popup sign-in (`/api/v1/auth/{metadata,token,login,refresh,logout}`); ACLs evaluated at sign-in/refresh; per-request checks read the user row. Same token format CloudTAK consumes via `/oauth/token`, so one bearer type everywhere. |
| Bootstrap | First run prints/writes a **setup token**; `/api/v1/setup/*` (admin, server name/hostname, cert source, CA init, OIDC, first device password + QR) is gated by it and returns 410 afterwards. Wizard-managed settings live in a `settings` table; TOML wins when set. |
| Config naming | Foundations naming is canonical: `[web.public] listen = [":8446"]`, `[web.public.tls] mode = "none|files|acme|internal"`, `[web.marti] listen = ":8443" client_cert = "optional"`, `[stream.tls]`, `[stream.tcp] auth = "credentials|anonymous"`, `[auth]`, `[auth.oidc]`, `[pki]`, `[acme]`, `[retention]`, `[storage]`. The identity design's extra fields (`name_entries`, `server_names`, `server_ips`, `require_known_cert`, `csr_*`, `p12_*`, `channels_marker_eku`, `rate_limit`, `setup_token_file`, `anon_group_default`, `link_by_username`, group-mapping fields) are folded into those sections. Default public listen is `:8446` only; ACME mode requires `:443` (TLS-ALPN-01) or `:80` (HTTP-01) to be bound and validation enforces it; `internal` mode (our CA) is the LAN-only story with a trust-bootstrap data package for ATAK. |
| TLS | rustls (aws-lc-rs) everywhere. ATAK's `DEFAULT:!ECDH` cipher list applies to its **mission-package HTTPS transfers (curl → Marti :8443)**, not the :8089 stream (verified in M1-00: the stream SSL context sets no cipher list). Since rustls offers only ECDHE suites in TLS 1.2, **TLS 1.3 must be enabled on :8443 and :8446** (and is on :8089 too); 1.2 kept for CloudTAK/WinTAK/iTAK. CI runs `openssl s_client -cipher 'DEFAULT:!ECDH'` against :8443 and :8446; escape hatch (unplanned) is an OpenSSL acceptor feature. Root CA and internal server certs RSA-2048 by default (ecdsa-p256 selectable); client certs get the CSR's key but **our** subject (`CN=<user>` + configured O/OU), EKU clientAuth (+ optional TAK channels marker OID), 128-bit serial, SKI/AKI. Revocation enforced in a custom rustls `ClientCertVerifier` via an in-memory cache; `require_known_cert` rejects CA-signed certs missing from the DB. |
| Secrets | `Sealed` (AES-GCM, context-as-AAD, lifted from automate) for CA/server/JWT/mission-token/ACME keys; argon2id for device passwords, enrollment tokens, local passwords (with a 5-minute verified-secret cache and an ip+user rate limiter); sha256 for refresh tokens. |
| Groups | Memberships per user with direction IN/OUT (+ source manual/oidc); **active state per device** (`device_group_state`) driving `PUT /groups/active?clientUid=`; effective bit-vector = memberships ∩ active; `__ANON__` bitpos 1; `GET /groups/all` returns IN+OUT with `active`; `t-x-g-c` to the user's other devices (all devices when CloudTAK omits `clientUid`). |
| Storage | `Database` = one tokio-rusqlite writer + small read-only pool (WAL, `synchronous=NORMAL`, foreign keys on, STRICT tables, RFC 3339 millisecond timestamps bound from Rust); hand-rolled migration runner over `include_dir!` SQL files with parity/fk tests; CoT history via a batched writer task fed by a bounded channel (`try_send`, never blocks the router). |
| Streaming | Sharded `parking_lot::RwLock` hub (no actor), routing on the sender's task, bounded per-connection writer queues with drop-then-close; `EncodedEvent` lazily caches XML/proto once per relayed message; spec-strict typed-detail extraction; oversize protobuf (>64 KiB) substituted with a `b-f-t-r` pointer like TAK. Mission publish and notifications go through `MissionIngest`/`Notifier` traits so `stream/` and `missions/` stay decoupled. |
| Marti | D1–D14 in design 04: mounted on both listeners; `Authorization: Bearer` is an identity token only if it verifies as our RS256 JWT, otherwise a mission token candidate (`MissionAuthorization` first); one JSON helper enforcing exact `Content-Type`; never 3xx; UUID-shaped mission names rejected; dates padded millis except `Group.created` (date) and `Files.Time`; squashed changes computed in Rust; HS256 mission tokens with a dedicated sealed secret; `.pref` group `com.atakmap.app.civ_preferences` (parameterised). |
| Build | No `protoc` anywhere (`protox`); `zip` with pure-Rust deflate; `trunk` pinned to the current stable; per-binary Dockerfiles; cross for aarch64-musl. |

**Deltas the briefs must apply over designs 01–04** (later user direction): remove `listener_tcp.rs`, `auth_tcp.rs`, `codec/auth.rs` (client side too), `[stream.tcp]`, `TcpAuthMode`, `Purpose::StreamAuth`, anonymous principals and `LocalPassword`; add `auth/passkeys.rs` (`webauthn-rs`, `passkeys` table, `/api/v1/auth/passkey/{register,login}/{start,finish}`, wizard registers the first admin's passkey); rename `DevicePassword` → `ClientPassword` (opt-in, expiring, scoped to `/oauth/token` + `/Marti/api/tls/*`); `[web.marti] client_cert = "required"`; `[web.public.tls] mode` defaults to `internal` with `allow_insecure_http` opt-out; replace `cot_history` table with `store::append_log` segments + `stream_segments` index (design 02 §2.3 writer task now appends to segment files); add `interop/` suites and CI jobs (design 01 §7.3 gains `interop-node-tak` on PR, `interop-cloudtak` + `interop-eud` nightly).

## Kickoff (first orchestrator actions after approval)

1. **M0 step 1 (operational agent, Sonnet 5)**: create the workspace skeleton exactly as design 01 §1 (root `Cargo.toml`, six crates, `rustak-ui` + `Trunk.toml`, `.cargo/config.toml`, `.gitignore`, `LICENSE` MIT, `README.md`), plus `.claude/plan/` with `README.md`, `plan.md` (this file), `conventions.md`, `research/01–07`, `design/01–04` copied from the session scratchpad. Commit on `feat/workspace-skeleton`.
2. **M0 step 2 (operational agent)**: port automate's `.github/` set per design 01 §7.3 (lint/test/ui/e2e/build matrix/docker/tap/changelog/audit/dependabot/release-drafter) + `scripts/check-file-length.sh`; verify the pipeline is green on the skeleton.
3. **M0 steps 3–17** as briefs in `.claude/plan/briefs/M0-NN-*.md` (parallel groups: `rustak-api`+`rustak-core` | `rustak-cot` build pipeline | server config+db+crypto+services+jobs | web+api+UI skeleton | e2e; then bootstrap/plugin-example/docker/release), each with an implementation agent and a reviewer; interop/e2e gate; checkpoint commit and pre-release tag.
4. Then M1 (design 02 steps 1–15 minus TCP, plus `store::append_log`; **exploration brief**: "EUD interop harness — evaluate building commoncommo's example client in Docker vs. an Android-emulator ATAK job vs. a pytak/takproto client; deliver a recommendation, a build recipe and a licensing note to `.claude/plan/status/`"), M2 (design 03 M2.1–M2.10 with passkeys/client-password deltas + design 04 M2 steps + `interop/node-tak`), M3 (design 04), M4 (design 04 + `interop/cloudtak`), M5 (design 03 M5.1–M5.5), M6 (services API + example sidecar + docs).

## Milestones

| # | Milestone | Exit criterion |
|---|---|---|
| M0 | Skeleton: workspace, config, db+migrations, crypto, services, web server + UI shell, e2e harness, **full CI/CD** (lint/test/coverage/ui/e2e/build matrix/docker/tap/release-drafter/audit/dependabot), file-length lint | Green pipeline on `main`; multi-arch `ghcr.io/sierrasoftworks/rustak:latest` published; a tagged pre-release produces binaries + Homebrew formula |
| M1 | `rustak-cot` crate + streaming core: TLS listener (test CA + issued client certs), negotiation, hub/router (broadcast, dest, groups, flow tags), ping, latest-SA replay, append-only CoT history store; **interop exploration brief** (EUD harness) and `interop/rust` suites | Fake-EUD integration suites green in CI; TLS 1.3 `!ECDH` check green; EUD-harness decision recorded in `.claude/plan/status/` |
| M2 | PKI + enrollment + Marti basics: CA, server certs, `tls/*` (JSON+XML), one-time enrollment tokens + QR, opt-in client passwords, passkeys + setup wizard, mTLS principal, version/config, groups, contacts, clientEndPoints, `/files/api/config`, ACME; **`interop/node-tak` suite in CI** | node-tak suite green on PRs (login, enrollment, mTLS probe, channels); ATAK enrols via QR (EUD harness or manual checklist) |
| M3 | Data packages + device profiles: `/Marti/sync/*`, files metadata, `b-f-t-r` relay, profiles on enrollment/connection, manual config packages; node-tak suite extended to files | node-tak files scenarios green; EUD harness package share; CloudTAK compose smoke (nightly) passes packages |
| M4 | Data Sync: full mission API, tokens, subscriptions, changes, `t-x-m-*`, `<dest mission>`, layers, logs, invitations, archive, `/cot`; node-tak suite extended to missions; **`interop/cloudtak` compose nightly** | node-tak mission scenarios green; CloudTAK compose Data Sync round-trip green |
| M5 | OAuth2 server + OIDC federation (`/oauth/authorize`, `/login/*`), group claim mapping, admin UI polish | CloudTAK login with client password (node-tak + compose); browser SSO + passkeys for Yew UI (e2e) |
| M6 | Services API + example sidecar, docs, release pipeline (tap, docker, cross) | Sidecar publishes CoT to a channel (interop/rust); `brew install` works |

## Verification

- Unit tests in-file (`#[cfg(test)]`), `rustak-cot` golden + property tests with our own fixtures, fuzz targets (nightly), migration tests at each version.
- Integration tests (`rustak-server/tests/`): in-process server with test CA + `TestIdentityProvider` + fake EUDs (`rustak-client`, TLS with issued certs) covering negotiation, routing by group, enrollment (Basic enrollment token → CSR → cert → mTLS probe; revoke → handshake failure), passkey ceremonies, mission flows with tokens, package upload/download, profile delivery, contract tests asserting exact JSON/XML bytes and `Content-Type`, and a JSON-schema oracle transcribed from node-tak's TypeBox types.
- e2e: Playwright against a debug build (`?demo` fixtures for UI-only), incl. setup wizard with passkey registration (Playwright virtual authenticator).
- **Interop suites in CI** (`interop/`):
  - `interop/node-tak/` — TypeScript suite using `@tak-ps/node-tak` + `@tak-ps/node-cot` (the exact libraries CloudTAK uses) against a rustak started with a test CA: `/oauth/token` password grant (client password), `tls/config` + `signClient/v2`, `/Marti/api/version` mTLS probe, groups, contacts, missions (create → token/guid → subscribe → contents → changes → layers → logs → delete by `?guid=`), files (`missionupload`, `sync/search`, `files/metadata`), stream connect + ping/pong + `t-x-m-c` receipt. Runs on **every PR**.
  - `interop/cloudtak/` — docker-compose (Postgres + CloudTAK API + rustak with a BYO cert trusted via `NODE_EXTRA_CA_CERTS`) driven through CloudTAK's own REST API (server config, login, channels, data sync, packages) with API-level assertions and a Playwright smoke of its UI. Runs **nightly** and on demand (label), since it is heavy.
  - `interop/eud/` — ATAK-side harness, **decided by M1-00** (`.claude/plan/status/M1-00-eud-interop-harness-exploration.md`): build ATAK's networking core `commoncommo` from atak-civ (pinned commit, quictls OpenSSL from `takthirdparty`) in a CI Docker image cached in ghcr (GPL test tool, nothing vendored) and drive the stock `commotest` CLI script (`estream:`/`sstream:`/`chatsend:`/`smpsend:`/`proto:`…), parsing its `commo-log.txt`/`commo-xml.txt` for enrollment, TLS, protobuf negotiation, ping/pong, SA/chat routing and mission-package transfer; plus one pytak/goatak enrollment smoke as an independent witness. The Android-emulator route is rejected (no APK supply). Nightly; ~25–45 min cold image build, ~10 min per run.
  - `interop/rust/` — `rustak-client` fake-EUD scenarios (multi-client reachability, negotiation, disconnect) on every PR (these are the `rustak-server/tests/stream_*.rs` suites).
- Quality gates in CI: `cargo fmt --check`, `cargo clippy --all-targets -D warnings`, `cargo doc -D warnings`, `cargo audit`, file-length script, `openssl s_client -cipher 'DEFAULT:!ECDH'` TLS 1.3 check against :8443/:8446 (ATAK package transfers) and :8089.
- Manual checks are reserved for what CI cannot reach (WinTAK/iTAK, real phones) and are listed in `docs/compat/` as checklists, not gates.

---

## Appendix A — Compatibility contract (verified digest)

Distilled from the seven research reports (full detail in `.claude/plan/research/`; reports 05/06/07 are
verified against TAK Server 5.7 and ATAK-CIV source and win over 02 where they differ).

### A.1 Streaming (ATAK ⇄ server) — verified (05, 07)
- **XML framing**: inbound split on literal `</event>`; bytes before the first `<event` are discarded (this is how a stray `<auth>` is tolerated); 8 MiB per-message cap. Outbound `<?xml version="1.0" encoding="UTF-8"?>\n<event …>…</event>`, no trailing newline, messages head-to-tail. **Protobuf frame** `0xBF` + LEB128 length + `TakMessage` (mesh framing `0xBF ver 0xBF` never appears on streams). ATAK caps frames at 64 KiB and resyncs on bad magic; TAK Server replaces >64 KiB outbound protobuf with a `b-f-t-r` pointing at `/Marti/api/cot/xml/{uid}`.
- **Connect sequence (TLS)**: handshake (client cert required) → auth (cert CN → user; or `<auth><cot username password uid/></auth>` as the very first bytes on TCP/`file` auth, no reply, close on failure) → replay latest SA of reachable peers (XML) → **one** `t-x-takp-v` (`<TakControl><TakProtocolSupport version="1"/><TakServerVersionInfo serverVersion=… apiVersion="3"/></TakControl>`, `how=m-g`, stale +60 s) → client `t-x-takp-q` (`TakRequest/@version=="1"`, reuses uid, then stops sending) → `t-x-takp-r` `<TakResponse status="true"/>` → both directions protobuf, never XML again. No request ⇒ XML forever (ATAK gives up waiting after 60 s and stays XML). CloudTAK never negotiates and needs full `<event>…</event>` pairs.
- **Keepalive**: ATAK pings (`t-x-c-t`, uid `<uid>-ping`, `how=m-g`) after 15 s silence, repeats every 4.5 s, drops at 25 s. Reply only to the pinger: `<event version='2.0' uid='takPong' type='t-x-c-t-r' how='h-g-i-g-o' time start stale=+20s><point ce='9999999' le='9999999' hae='0' lat='0' lon='0'/></event>` (no detail). Control types consumed, never relayed: `t-b*`, `t-x-c-f`, `t-x-c-t(-r)`, `t-x-takp-q`, `t-x-c-m`, `t-x-c-i-e/-d` (incognito). Messages lacking `<point lat>` are dropped. Unknown control ⇒ no-op.
- **Subscription state**: first message with `<contact endpoint>` fixes `clientUid`=event uid and `callsign`; each SA (has callsign+endpoint+uid) refreshes latest-SA cache, `team=__group/@name`, `role=__group/@role`, `takv=platform:version`. Streaming endpoint sentinel is `*:-1:stcp`.
- **Routing**: `<marti><dest …/>` matched by first attribute in order callsign → publish (unimplemented) → uid → mission(+path,+after) → mission-guid → group (sender must hold that IN group, else reject). `<marti>` always stripped before relay. `callsign="All Streaming"` ⇒ discard explicit list ⇒ implicit broadcast. **Reachability: deliver iff ∃G: sender has IN(G) ∧ receiver has OUT(G)** (IN = may publish, OUT = may receive). Implicit broadcast excludes the source connection; explicit still applies reachability. Add `_flow-tags_ TAK-Server-<id>="<time>"`, drop already-tagged inbound. Mission dest: require MISSION_WRITE subscription, relay to connected mission subscribers (minus sender), persist uid + ADD_CONTENT change, emit `t-x-m-c`. GeoChat `b-t-f` is ordinary routed CoT (DMs use `<dest callsign>`; ATAK matches conversations on `__chat@id`/`chatgrp`); undeliverable `b-t-f` ⇒ `b-t-f-s` back to sender.
- **Disconnect**: `t-x-d-d` `<detail><link relation='p-p' uid='{clientUid}' type='{lastSA type}'/></detail>` (point 0, ce/le 9999999, stale +20 s) to reachable peers. **Group change**: `t-x-g-c` (uid `<gen>.<clientUid>`, `<link relation='p-p'/>`) to the user's *other* devices; ATAK then re-fetches groups with `sendLatestSA=true` and clears server items.
- **XML↔protobuf**: typed sub-message only when the element has exactly the expected attribute set (`contact`: callsign[+endpoint]; `__group`: name+role; `precisionlocation`; `status`: battery; `takv`: device+platform+os+version; `track`: speed+course); else whole element stays in `xmlDetail` (children concatenated, no wrapper). Decode: xmlDetail wins on name collision. Field numbers: CotEvent type=1 access=2 qos=3 opex=4 uid=5 sendTime=6 startTime=7 staleTime=8 how=9 lat=10 lon=11 hae=12 ce=13 le=14 detail=15 caveat=16 releasableTo=17; Detail xmlDetail=1 contact=2 group=3 precisionLocation=4 status=5 takv=6 track=7; TakMessage takControl=1 cotEvent=2 (+ server-only submissionTime=3 creationTime=4). Package `atakmap.commoncommo.protobuf.v1`. Keep millisecond times.
- **TLS**: ATAK verifies the server chain against its enrollment truststore only (no hostname check on 8089), OpenSSL default TLS versions, no cipher restriction on the stream (the `!ECDH` list applies only to package downloads); client cert + key from PKCS#12; no ALPN. Client certs need chain-to-CA only (no EKU checks in ATAK; we still issue EKU clientAuth).

### A.2 CloudTAK (node-tak) hard requirements
- `GET /files/api/config` → `{"uploadSizeLimit": N}` (server setup gate). `GET /Marti/api/version` plain text under mTLS (every login/session check).
- `POST /oauth/token` form `grant_type=password&username&password` → `200 {"access_token"}`; 401/403 on bad creds. JWT header 3-byte-aligned, flat payload, `sub` = username.
- `GET /Marti/api/tls/config` XML root literally `ns2:certificateConfig` with ≥2 `nameEntry`; `POST /Marti/api/tls/signClient/v2?clientUid=&version=` with **Basic** auth (also accept Bearer), PEM CSR body → `{"signedCert": <bare base64>, "ca0", "ca1"}`; XML variant `<enrollment><signedCert/><ca/>…</enrollment>` when `Accept: application/xml` (ATAK).
- Every JSON response `Content-Type: application/json` **exactly** (no charset). Never redirect (3xx treated as success). `Mission` arrays (`externalData`, `feeds`, `mapLayers`, `uids`, `contents`) always present.
- `/Marti/api/groups/all` (`type: com.bbn.marti.remote.groups.Group`, `bitpos` required), `PUT /Marti/api/groups/active` (accept missing `clientUid`), `t-x-g-c` to trigger refresh.
- Missions addressed by GUID (`/missions/guid/{guid}/…`) and by name; create returns `token` + `guid`; `MissionAuthorization: Bearer` header; `DELETE /Marti/api/missions?guid=`; `group` param comma-joined or repeated; `allowGroupChange` accepted.
- `/Marti/sync/search` → `{resultCount, results:[{UID,Name,Hash,PrimaryKey,SubmissionDateTime,SubmissionUser,CreatorUid,Keywords,MIMEType,Size(str),EXPIRATION(str),Tool}]}`; `/Marti/sync/missionupload` multipart `assetfile`, `Groups`/`groups`; `/Marti/api/files/metadata?missionPackage=true&name=`.
- Stream: CoT XML only (no protobuf negotiation with CloudTAK — it never sends `t-x-takp-q`), full `<event>…</event>` (never self-closing), `t-x-takp-v` optional, 15 s handshake budget.
- Video: `GET /Marti/api/video` → `{"videoConnections": []}`; reject writes.

### A.3 ATAK enrollment / profiles / channels / packages — verified (07, 06)
- **Enrollment (commoncommo, port 8446)**: `GET /Marti/api/tls/config` (Basic; XML root `certificateConfig` with `nameEntries/nameEntry@name,@value`) → CSR subject `CN=<username>` then nameEntries in order, RSA (nominally 4096-bit), SHA-256 → `POST /Marti/api/tls/signClient/v2?clientUid=<uid>&version=<atak ver>` (Basic; `Accept: application/xml`; `Content-Type: application/octet-stream`; body = base64 CSR without armour) → **200 only** (201 = failure) `<?xml …?><enrollment><signedCert>…</signedCert><ca>…</ca>…</enrollment>` (bare base64; any non-`signedCert` child is a CA). TAK Server validates CN == authenticated user (case-insensitive) and RDN set == CN + nameEntries; issues KU digitalSignature|keyAgreement|nonRepudiation, EKU clientAuth (+ OID 1.2.840.113549.1.9.7 when `version` present), 31-bit random serial, validity `validityDays` (default 365) with notBefore −720 min, subject verbatim.
- **Trust**: QR/"quick connect" enrollment verifies 8446 with **public CAs + hostname check** and then stores the returned CA chain as the stream truststore. Manual enrollment (existing connection) uses the stored truststore, or **disables peer verification when none exists**. There is no "trust anyway" prompt; `SERVER_NOT_TRUSTED` hard-fails. After success ATAK stores the client P12 keyed by host+port, sets `connectString=host:port:ssl`, `enrollForCertificateWithTrust=true`, `enrollUseTrust=false`, does **not** set `useAuth`, fetches `GET /Marti/api/tls/profile/enrollment?clientUid=` (Basic; 200 zip or 204), then reconnects.
- **QR**: `tak://com.atakmap.app/enroll?host=<host[:port[:quic]]>&username=&token=` (all three required; port = streaming port, default 8089, protocol forced `ssl`; enrollment still hits 8446; token is the Basic password). `tak://com.atakmap.app/import?url=<urlencoded>` imports any file (packages, `.pref`, `.p12`).
- **Device profiles**: `GET /Marti/api/device/profile/connection?syncSecago=&clientUid=` (8443, cert) and `/device/profile/tool/{tool}` fire only when pref `deviceProfileEnableOnConnect=true` (default false) — our enrollment profile sets it. `/tls/profile/tool/{tool}/file?relativePath=` (Basic). Responses: 200 zip (`profile.zip`, Mission Package `fileN/<name>` + `MANIFEST/manifest.xml` with `uid`, `name`, `onReceiveImport=true`, `onReceiveDelete=true`) | 204 nothing | 304 via `If-Modified-Since`/`Last-Modified` (RFC 1123).
- **`.pref`**: `<?xml version='1.0' standalone='yes'?><preferences><preference version="1" name="…">…`; groups `cot_streams`/`cot_inputs`/`cot_outputs` are connection loaders, everything else is a SharedPreferences name — ATAK-CIV's default is **`com.atakmap.app.civ_preferences`** (aliases `com.atakmap.app_preferences`, `com.atakmap.civ_preferences`). `class` is literally `class java.lang.String|Boolean|Integer|Float|Long` (missing attribute crashes import). `cot_streams` keys read: `count`, `description<i>`, `connectString<i>`, `enabled<i>`, `useAuth<i>`, `compress<i>`, `cacheCreds<i>`, `caPassword<i>`, `clientPassword<i>`, `caLocation<i>`, `certificateLocation<i>`, `enrollForCertificateWithTrust<i>`, `enrollUseTrust<i>`, `expiration<i>` (no username/password keys). `.p12` in a package land in `<atak>/cert/`; `caLocation`/`certificateLocation` resolve there; passwords are stored then scrubbed from prefs.
- **Channels**: `GET /Marti/api/groups/all?useCache=true[&sendLatestSA=true]` on connect and on `t-x-g-c`; envelope `type` must be `com.bbn.marti.remote.groups.Group`; each group needs `name`, `direction`, `type`, `created` (TAK emits `yyyy-MM-dd`), **`bitpos ≥ 0`** (else dropped) and optional `active`(default true), `description`, `distinguishedName`. `PUT /Marti/api/groups/active?clientUid=<device uid>` with lower-case `content-type: application/json` and a **bare JSON array** of groups (`created` as epoch millis here). TAK Server: `useCache=false` returns OUT groups only; `useCache=true` returns the active-group cache incl. IN; `clientUid` present ⇒ `t-x-g-c` to the user's other devices. UI gated by prefs `prefs_enable_channels` (bool) and `prefs_enable_channels_host-<host>` ("true" string).
- **Packages**: ATAK: `GET /Marti/sync/missionquery?hash=` (text URL | 404) → `POST /Marti/sync/missionupload?hash=&filename=&creatorUid=` (multipart `assetfile`, `application/x-zip-compressed`; response body **is** the URL used as `senderUrl`) → `PUT /Marti/api/sync/metadata/{hash}/tool` body `private|public`. Server list: `GET /Marti/sync/search?keywords=missionpackage[&tool=]` — body must contain `resultCount`; `results[]` Title-case keys `UID, Name, Hash, PrimaryKey(int-parsable), SubmissionDateTime(yyyy-MM-dd'T'HH:mm:ss.SSS'Z')` mandatory, `SubmissionUser, CreatorUid, Keywords, MIMEType, Size` optional; TAK Server emits `Size`/`PrimaryKey` as strings, `EXPIRATION`, `Tool`, `Groups`. Download `GET /Marti/sync/content?hash=[&offset=]` (ATAK appends `&receiver=<callsign>` on `senderUrl`). `b-f-t-r`: `<fileshare filename senderUrl sizeInBytes sha256 senderUid senderCallsign name [peerHosted] [httpsPort]/>` + `<ackrequest uid ackrequested="true" tag/>`, `how=h-e`, stale 10 s; ack `b-f-t-a` `<ackresponse uid senderUid success tag reason sha256 sizeInBytes/>`. Server does not rewrite `senderUrl` (client already points at our `/Marti/sync/content`). Manifest: `MANIFEST/manifest.xml`, `<MissionPackageManifest version="2"><Configuration><Parameter name="uid|name|remarks|onReceiveDelete|onReceiveImport|deleteWithPackage|onReceiveAction" value=/></Configuration><Contents><Content zipEntry= ignore=><Parameter name="uid|name|localpath|isCoT|contentType|visible|refContent"/></Content></Contents>`; paths relative to the dir containing `MANIFEST/`.
- **HTTP client posture**: https ⇒ client cert + no Basic (8443); Basic only on 8446 or explicit; no User-Agent; `Accept` only where set; gzip accepted on list calls. `/Marti/api/version/config` parsed as `{version:int, type:"ServerConfig", data:{version:string}}`; `/Marti/api/clientEndPoints` needs `type: com.bbn.marti.remote.ClientEndpoint`, items `uid, callsign, lastEventTime, lastStatus ∈ Connected|Disconnected` (parse failure rejects the whole list), fetched once per connect string.

### A.4 Marti HTTP contracts — verified (06, 03)
- **Envelope** `{"version":"3","type":…,"data":…,"nodeId":…}` (`messages` omitted); `type` is FQCN for groups/users/clientEndPoints/subscription/role/logs, simple name for `Mission`, `MissionChange`, `MissionLayer`, `Resource`, `ServerConfig`, `SubscriptionInfo`, literal `MissionSubscription`/`MissionInvitation`/`MapLayer`. Bare (no envelope): `/contacts/all`, `/missions/{n}/contacts`, `/cot/matchUid`, `/util/user/roles`, `/util/isAdmin`, `/home`, `/version`, `/version/info`, `/node/id`, `/files/api/config`. Errors: JSON `{status:"NOT_FOUND",code:1,message}` (404/400/409/410/401/403/500 table in report 06 §1.5); container errors are HTML. `API_VERSION` request header, default 2 (≥3: forbidden mission GET returns stripped 200; subscription response embeds `mission`). Dates: `yyyy-MM-dd'T'HH:mm:ss.SSS'Z'` for Mission/MissionChange/LogEntry/Resource? (Resource `submissionTime`, MissionSubscription/Invitation `createTime`, ClientEndpoint `lastEventTime` use unpadded `.S'Z'`). Emit `Content-Type: application/json` exactly (CloudTAK), never redirect.
- **Version**: `/Marti/api/version` text (TAK: `5.7-RELEASE-14`; ATAK's plain matcher wants "TAK Server" — emit `TAK Server rustak-<semver>`), `/version/config` → `data:{version, api:"3", hostname}`, `/version/info` `{major,minor,patch,branch,variant}`, `/node/id`. `/files/api/config` → `{"uploadSizeLimit":<int MB>}` (CloudTAK setup gate).
- **OAuth**: `POST /oauth/token` form `grant_type=password&username&password` → `200 {"access_token","token_type":"Bearer","expires_in"}` (no refresh/scope); errors `400 {"error":"invalid_grant"}`. TAK JWT header `{"alg":"RS256","kid":"<uuid>"}` (60 bytes), claims `sub`=username, `aud`=[username], `iat`, `nbf`, `exp`, `jti`, no `iss`. TAK honours bearer only on 8446/8447; also sets chunked `access_token_N` cookies (WebTAK). No `token_key`/JWKS endpoint exists. OIDC federation via `/login/auth` → IdP → `/login/redirect?code&state` (state cookie, sha256 compare) → cookies; `/login/authserver`, `/login/.well-known/openid-configuration` `{authorization_endpoint, token_endpoint}`, `/logout`. Group mapping from `groupsClaim` (default `groups`): bare ⇒ IN+OUT, `_READ` ⇒ OUT only, `_WRITE` ⇒ IN only; `usernameClaim` → `email` → `sub`.
- **Groups**: `Group` JSON `{name, direction, created:"yyyy-MM-dd", type:"SYSTEM|LDAP", bitpos, active, [description], [distinguishedName]}`; normalise `bitpos<0→0`, missing created/type/direction. `PUT /groups/active[?clientUid]` body `Group[]` → 200 empty; triggers disconnect SA, re-auth, latest-SA resend, and `t-x-g-c` when `clientUid` given. `/groups/groupCacheEnabled` → `ApiResponse<Boolean>`. `/util/user/roles` bare `["ROLE_ANONYMOUS",…]` + `ROLE_READONLY` when the user has no IN group.
- **Contacts**: `/contacts/all` bare `[{filterGroups, notes, callsign, team, role, takv, uid(=clientUid)}]` (all keys present). `/clientEndPoints?secAgo&showCurrentlyConnectedClients&showMostRecentOnly&group…` → `ClientEndpoint{callsign, uid, username, team, role, lastStatus, lastEventTime}`. `/subscriptions/all` → `SubscriptionInfo` (all ~33 keys present, nulls allowed).
- **Missions**: list `GET /missions?tool(default public)&passwordProtected=false&defaultRole=false` (invite-only excluded); `GET /missions/{name|guid/{guid}}?password&changes&logs&secago&start&end` (password ⇒ ACCESS token in `token`); create `PUT|POST /missions/{name}?creatorUid&group(repeatable|comma, default __ANON__)&description&chatRoom&baseLayer&bbox&boundingPolygon&path&classification&tool=public&password&defaultRole&expiration&inviteOnly&allowGroupChange|allowDupe` (+ optional JSON body overriding params, or a package zip body) → **201** with `token`+`ownerRole`; update → 200. Delete by name or `DELETE /missions?guid=` (archive to esync first; `deepDelete` needs MISSION_DELETE). `Mission` JSON per report 06 §7.11 (`uids`/`contents` are `MissionAdd` lists `{data,timestamp,creatorUid,keywords,[details]}`; `passwordProtected` always; `defaultRole{type,permissions}`; `groups` names). `/contents` PUT body `{hashes[],uids[],paths{},after}` / DELETE `?hash|uid`; `/contents/missionpackage` (zip body, 409 conflicts list); `/changes?squashed=true` (current-state delta) vs `false` (history); `/cot` → `<?xml …?><events>…</events>` `application/xml`; `/archive` zip (`cot/<uid>.cot`, `contents/<n>_<name>`, `MANIFEST/manifest.xml` with mission params + `<Groups>` + `<Role>`); `/keywords*`; subscription `PUT …/subscription?uid|topic&password&secago…` → **201** `MissionSubscription{token, mission?, clientUid, username, createTime, role}` (roles: token INVITATION|SUBSCRIPTION|ACCESS, BCrypt password, invite-only lookup, else default role; password on unprotected mission ⇒ 403); GET/DELETE(`disconnectOnly=true`)/POST; `/subscriptions` (uids), `/subscriptions/roles` (tokens nulled), `/role` GET/PUT, `/token` (201 ACCESS), invitations (`/invitations?clientUid`, `/all/invitations` names, `/invite/{clientUid|callsign|userName|group|team}/{invitee}`), logs (`/missions/logs/entries` POST/PUT 201, GET/DELETE by id, `/missions/{n}/log`; `LogEntry{id, content, creatorUid, entryUid, missionNames[], servertime, dtg, created, contentHashes[], keywords[]}`), layers (`MissionLayer{uid,name,type:GROUP|UID|CONTENTS|MAPLAYER|ITEM,parentUid,mission_layers[],uids[],contents[],maplayers[]}`, NON_EMPTY), `/password`, `/expiration`, `/externaldata`, `/maplayers`, `/feed`, `/copy`, `/send`, `/parent`, `/children`, `/kml`, `/pagedmissions`, `/missioncount`, `GET /Marti/api/sync/search` (Resource JSON).
- **Mission tokens**: HS256 (TAK keys it with the RSA private key bytes; we use a dedicated secret) claims `jti`, `iat`, `sub`=`SUBSCRIPTION|INVITATION|ACCESS`, `iss`, `[exp]`, `<TYPE>`=id, `MISSION_NAME`, `MISSION_GUID`. Read `MissionAuthorization` first, then `Authorization`, `Bearer ` case-sensitive; admin ⇒ MISSION_OWNER; SUBSCRIPTION ⇒ that subscription's role, INVITATION ⇒ invitation's role (only on subscribe), ACCESS ⇒ default role; name must match. `MissionChange` JSON `{type, timestamp, serverTime, missionName, missionGuid, isFederatedChange, contentUid, creatorUid, details{type,callsign,title,iconsetPath,color,attachments,name,category,location{lat,lon}}, contentResource, logEntry, missionFeed, mapLayer, externalData}`; types `CREATE_MISSION|DELETE_MISSION|ADD_CONTENT|REMOVE_CONTENT|CREATE_DATA_FEED|DELETE_DATA_FEED`. Stream `t-x-m-*` templates per report 05 §7 (child **elements** inside `<MissionChange>`, `<mission type name guid authorUid tool [token]>` attributes; `t-x-m-r` uses `type="INVITE"`).
- **Enterprise Sync**: `POST /Marti/sync/upload` (raw body or multipart `assetfile|resource`; params = Metadata field names incl. aliases `MIME`, `name`; arrays comma-split; `Groups` validated) → 200 `text/json` Title-case `Metadata` (`Size`/`PrimaryKey` strings, `EXPIRATION`). `GET /Marti/sync/search` → `text/json` `{resultCount:int, results:[Metadata]}` (params case-insensitive; unknown ⇒ 400). `GET|HEAD /Marti/sync/content?hash|uid&offset&length` (headers `api-version: 3`, MIME, `Content-Disposition: inline; filename=`, gzip if accepted, 206). `POST /Marti/sync/missionupload` (multipart only; `filename` required; `keyword` default `missionpackage`; `tool` default public; `groups`/`Groups`; client `hash` ignored; duplicate ⇒ 403) → `text/plain` `https://host:port/Marti/sync/content?hash=<hash>`; `GET /Marti/sync/missionquery?hash=` same URL | 404. `/Marti/sync/delete?hash|PrimaryKey` (GET/POST/DELETE) → HTML 200. `PUT /Marti/api/sync/metadata/{hash}/{tool|mimetype}` (text body), `/keywords` (JSON array), `/expiration?expiration=`. `/Marti/api/files/metadata?missionPackage&name&mission&page&limit` → `type:"Files"`, items map-of-strings `{Name, User, Creator, Size:"12kB", Time:<Date.toString()>, MimeType, Keywords:"a,b", Expiration|"none", Hash, Groups:"a,b"}`; `/files/metadata/count`; `GET|HEAD|DELETE /Marti/api/files/{hash}`; `PUT /files/{hash}/metadata`. `/Marti/sync/{hash}/metadata` does **not** exist.
- **Device profiles (server)**: `GET /Marti/api/tls/profile/enrollment?clientUid` (Basic), `/device/profile/connection?syncSecago&clientUid`, `/device/profile/tool/{tool}?clientUid[&syncSecago]`, `/tls|device/profile/tool/{tool}/file?relativePath…` → 204 | 200 zip | 304; TAK's generated `.pref` uses group `com.atakmap.app.civ_preferences`, sets `prefs_enable_channels`, `prefs_enable_channels_host-<host>`, and LDAP-derived `locationCallsign`/`locationTeam`/`atakRoleType`. Admin CRUD `/Marti/api/device/profile/**` (we expose ours under `/api/v1`).
- **CoT query**: `/Marti/api/cot/xml/{uid}` (single `<event>` with XML header, `<marti>` stripped, 404 empty), `/cot/xml/{uid}/all?secago|start|end` and `GET|POST /cot` (uids body) and `/cot/sa?start&end&left&bottom&right&top` (≤24 h) → `<events>`; `/cot/matchUid?search` bare string array.
- **Legacy still hit by ATAK**: `GET /Marti/GetTime`, `POST /Marti/ErrorLog`, `/Marti/vcm` (video, stub), `/Marti/api/video` (`{"videoConnections":[]}`), KML servlets (501 for now). `/Marti/**` DELETE/OPTIONS are denied by default in TAK unless allowed per route; unknown `/Marti/**` paths are admin-only.

## Appendix B — automate patterns to lift (paths relative to `../automate`)

Generic infrastructure worth copying/adapting: `agent/src/crypto.rs` (AES-GCM SecretStore),
`agent/src/db/{mod,sqlite,cache,partition,audit}.rs` (SQLite open/pragmas/migrations, KV/Queue/Audit
traits), `agent/src/services/mod.rs` (AppContext→scoped services), `agent/src/job.rs` (JobHost +
inventory registry), `agent/src/filter.rs` + `web/helpers/oidc.rs::AdminRequestFilter` (filt-rs ACLs),
`agent/src/web/{mod,ui,telemetry,principal}.rs`, `agent/src/web/api/{mod,scope,auth}.rs` (bearer
middleware, OIDC metadata/token/refresh), `agent/src/web/helpers/*` (request base_url/trust_proxy,
wizard state cookie), `agent/src/integrations/state.rs` (server-side pending auth state),
`agent/src/parsers/interpolation.rs` (`${{ env.X }}`), `agent/src/config.rs` shape
(`deny_unknown_fields`, example-config test), `agent/build.rs` + `web/ui.rs` (include_dir SPA),
`agent/src/testing.rs` + `testing/oidc.rs` (TestIdentityProvider), `api/src/{ids,tenant,audit}.rs`,
`ui/src/{api,auth,app,util}.rs` + `fixtures/` (demo mode macro) + shared components, `ui/styles.scss`
structure, `e2e/` harness (Playwright, start-agent script, readiness on `/robots.txt`),
`.github/workflows/*`, `Cross.toml`, `Dockerfile`, `.cargo/config.toml`.

Domain-specific (do not lift): collectors, publishers, webhooks/*, jobs/*, integrations/{todoist,ynab,github_app}, workflow_*.

Dependency baseline (automate, Sept 2026): actix-web 4.x, tokio 1.5x, rusqlite 0.3x bundled + tokio-rusqlite, jsonwebtoken 11, oauth2 5, serde/serde_json/toml, tracing + tracing-batteries (needs `protoc`), human-errors, include_dir, aes-gcm/sha2/hmac/base64/rand/zeroize, filt-rs, croner, clap, dotenvy, inventory, reqwest (rustls-tls), rstest/wiremock. Add for rustak: rustls + tokio-rustls, rcgen, x509-parser, rsa/pkcs8/der, p12-keystore, quick-xml, prost + prost-build + protox, instant-acme, zip, argon2, webauthn-rs (passkeys), uuid, actix-ws/SSE. Use latest **stable** releases at implementation time (design 01 §0 lists the verified versions).

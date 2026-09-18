# M0-06 — `rustak-server` config module and `config.example.toml` — complete

Brief: `.claude/plan/briefs/M0-06-server-config.md`
Design: `design/01-foundations-storage-ci.md` §3.3 (canonical section naming), reconciled with
`design/03-identity-pki-acme-auth.md` §8 (the identity/PKI/ACME keys) and the plan's
"Deltas the briefs must apply over designs 01–04".

## What was built

`rustak-server/src/config/` — one file per section, plus a `validate.rs` for the rules that span
more than one of them. Functional-line counts (limit 300):

| File | Lines | Contents |
|---|---:|---|
| `mod.rs` | 91 | `Config`, `load`/`load_str`/`validate`, the `data_dir`-relative path resolvers, `issuer()`, `Config::testing`, the example-file tests |
| `server.rs` | 45 | `[server]`: `name`, `domains`, `base_url`, `trust_proxy`, `data_dir`; `canonical_domain()`, `base_url()` |
| `storage.rs` | 66 | `[storage]`: `database`, `content_dir`, **`streams_dir`**, `reader_connections`, `busy_timeout`, `checkpoint_interval`; path resolution against `data_dir` |
| `web.rs` | 100 | `[web.public]` (+ `plain_bind`, `allow_insecure_http`), `[web.public.tls]` (`TlsMode`), `[web.marti]` (`ClientCertMode`) |
| `stream.rs` | 39 | `[stream.tls]` only — **no `[stream.tcp]`** |
| `auth.rs` | 175 | `[auth]` + `[auth.rate_limit]`, deny-by-default ACLs, redacting `Debug` |
| `oidc.rs` | 92 | `[auth.oidc]`, including every group-mapping field; redacting `Debug` |
| `pki.rs` | 161 | `[pki]` incl. `name_entries`, `server_names`/`server_ips`, `require_known_cert`, `csr_*`, `p12_*`, `channels_marker_eku`; `KeyType` |
| `acme.rs` | 137 | `[acme]`, `AcmeDirectory` (alias or URL), `AcmeChallenge` |
| `retention.rs` | 59 | `[retention]` |
| `validate.rs` | 199 | every cross-section rule, with its own tests |
| `main.rs` | 59 | clap `--config` / `--env` / `--check`; env file, load, validate, exit 0/1 |

`config.example.toml` at the repository root was rewritten from the M0-01 placeholder: every key,
with its default, in the order the structs declare them.

87 tests, all in-file under a single trailing column-0 `#[cfg(test)] mod tests`.

## Decisions worth recording

- **`validate.rs` is a twelfth file the brief did not name.** The brief's file list is design 01
  §3.1's. Folding the rules into `mod.rs` would have put `Config`, the loader, the resolvers, the
  testing fixture *and* six validation rules with their advice text in one file; splitting them
  keeps `mod.rs` at 91 lines and puts each rule's test next to the rule. `Config::validate()` is
  still the public entry point. Same reasoning for `oidc.rs`: `[auth.oidc]` is 14 keys of
  federation policy, which is a different responsibility from `[auth]`'s own credential policy.
- **`Config::testing` takes the data directory as a parameter.** The brief asked for
  `pub fn testing() -> Config` "or similar" with a temp `data_dir`. A `TempDir` created inside the
  function would be dropped — and the directory deleted — before the caller could use it, so the
  caller makes it and keeps it: `Config::testing(dir.path())`. It sets every listener to port 0
  (so concurrent suites do not race for a fixed port), `mode = "none"` with
  `allow_insecure_http = true`, and `user_acl = 'true'`. `admin_acl` is left denying, because
  administrator access is meant to come from the `users.is_admin` column the wizard sets, and that
  is the path the tests should exercise.
- **`Config::testing` is not behind `#[cfg(feature = "testing")]`.** `rustak-server`'s own
  integration tests under `tests/` cannot easily turn on a feature of the crate they are testing,
  and the function pulls in nothing a release build would not already have. `Cargo.toml` is not
  mine to edit, so this needed no manifest change.
- **`organization` *and* `name_entries` both exist**, because design 01 has the first and the
  brief adds the second. `PkiConfig::subject_entries()` resolves them: `name_entries` when it is
  non-empty, otherwise the single `O=<organization>` entry, and nothing at all when
  `organization` is blank. `name_entries` refuses `CN`, which is the username.
- **`client_password_ttl` has no "never expires".** Design 03 §8 had
  `device_password_ttl = ""` meaning no default expiry; the brief renames it and gives it 90d.
  A credential that can be replayed forever is what enrollment certificates exist to avoid, so
  the key is a plain duration and `validate()` requires it to be positive.
- **ACME defaults to `tls-alpn-01`.** Design 01 §3.3 wrote `http-01` and design 03 §8 wrote
  `tls-alpn-01`. The plan's reconciliation lists `:443 (TLS-ALPN-01)` first, and it is the choice
  that needs no plaintext port at all, so that is the default. Neither works against the default
  `listen = [":8446"]`, which is exactly what `validate()` refuses.
- **`[web.marti]` has no `allow_basic`/`allow_bearer`.** Design 03 §8 had them, but they only
  made sense alongside `client_cert = "optional"`, which the plan's delta removes. The brief lists
  only `client_cert` for this section, so the other two are gone.
- **`[auth] local_login` is gone** (the plan's delta removes `LocalPassword`; local sign-in is by
  passkey, which M0-11/M2 configure), and `trust_proxy` lives on `[server]` rather than
  `[web.public]`, because design 01 §3.3's naming is canonical.

## `validate()` — the rules, and what each one prevents

| Rule | Prevented failure |
|---|---|
| `[web.public] listen` is non-empty | a server with nothing to serve the UI, the API or enrollment on |
| `mode = "none"` requires `allow_insecure_http` | credentials, tokens and certificate enrollment over plaintext because one key was left at a permissive default |
| `mode = "files"` requires `cert_file` **and** `key_file` | a start-up failure naming a file that was never configured |
| `[acme] enabled` must agree with `mode = "acme"` | ordering a certificate that is never served, or serving one that is never ordered |
| ACME needs names (`[acme] domains`, else `[server] domains`) | an order with nothing to request |
| ACME needs `accept_tos` | an order the authority refuses; rustak does not accept somebody else's terms on an operator's behalf |
| `tls-alpn-01` needs `:443` in `listen`; `http-01` needs `plain_bind` or `:80` | a challenge the authority connects to a closed port for — discovered at the first renewal, in production |
| credential TTLs and `[auth.rate_limit] window` are positive; `attempts > 0` | a rate limiter that locks out every credential on its first use |
| `csr_min_rsa_bits >= 2048`, PKI validities positive, `server_cert_renew_before < server_cert_validity` | client certificates issued from weak keys that outlive the change that allowed them; a certificate due for renewal the moment it is issued |
| `[pki] name_entries` are `["type", "value"]` pairs and never `CN` | a subject rustak cannot issue, or a second common name |
| no two enabled listeners bind the same address (port 0 exempt) | "address already in use" at start-up instead of a configuration error naming both sections |

## `config.example.toml` is a test

Three tests keep the file and the schema from drifting apart, in both directions:

- `the_documented_example_configuration_loads_and_validates` — `include_str!` the file, run it
  through `Config::load_str` (interpolation included) **and** `validate()`, so the file we ship is
  one `rustak --check` accepts.
- `the_example_file_documents_every_key` — serialises a `Config` with **every `Option` filled in**
  and asserts each key it emits appears in the example. This is the direction
  `deny_unknown_fields` cannot catch: a key the server knows and the file never mentions.
- `an_empty_file_is_the_written_out_default` — `Config::load_str("") == Config::default()`, over
  the whole tree, which is the property every hand-written `impl Default` exists for. Each section
  has the same assertion locally.

Plus the two lifted from `../automate/agent/src/config.rs`: a misplaced key
(`admin_acl` under `[server]`) is reported by name rather than silently ignored, and the
defaults-agree test above. `[stream.tcp]` has its own refusal test in two places, because somebody
porting a TAK Server configuration has to be *told* there is no plaintext stream rather than left
believing port 8087 is open.

## Notes for later briefs

- **`Config::load` validates.** Anything calling `rustak_core::config::load::<Config>` directly
  would skip `validate()`; call `Config::load` (or `Config::load_str`).
- **Path resolution belongs to `Config`**, not to the caller: `database_path()`, `content_dir()`,
  `streams_dir()`, `setup_token_file()` all resolve against `[server] data_dir`, and a *relative*
  configured path resolves against the data directory too (not the process's working directory).
- **`ListenAddr::to_socket_addrs()` is not called during validation**, per M0-04's note: a
  container hostname that is not up yet is a start-up condition, not a config syntax error.
  `runtime::run_all` is where that resolution — and its failure — belongs.
- **`AcmeDirectory::url()`** is what `instant-acme` should be handed; the alias is kept rather
  than resolved so that a file we write back says what the operator wrote. `AcmeConfig::contacts()`
  returns `mailto:` URIs.
- **`OidcConfig::scopes()`** guarantees `openid`; use it rather than the raw field.
- **`AuthConfig::{user_acl, admin_acl}()`** return a deny-everybody `Filter` when unset. Do not
  treat `None` as "allow".
- **`[auth] secret_key` may hold an unresolved `${{ env.X }}` marker.** Per M0-04,
  `rustak_core::config::env::is_unresolved` is how `SecretStore` should refuse it by name.
- **`main.rs` is deliberately incomplete**: it parses arguments, loads the env file, loads and
  validates the configuration, and — without `--check` — prints a line saying the run loop is not
  wired yet. M0-12 owns that seam. `--check` deliberately does **not** bootstrap telemetry or touch
  the data directory, so it is safe to run repeatedly in a deployment pipeline on a machine that is
  not the server.
- `AuthConfig`, `OidcConfig` and `PkiConfig` have **hand-written `Debug` impls** that redact
  `secret_key`, `client_secret` and `p12_password` (and render the ACLs, which `filt_rs::Filter`
  cannot `Debug` itself). A new secret-bearing field must be added to those impls, not just to the
  struct; each has a test asserting the value does not appear in the rendering.

## Exit checks

Run against the working tree at the end of this brief. `rustak-server/src/db/` and
`src/db/repos/` were being written by another agent throughout; where a check is affected by that,
it is noted and the same check is shown against an isolated harness that compiles **these** files
with `#[path]` and nothing else.

### `cargo test -p rustak-server config::`

```
running 87 tests
...
test result: ok. 87 passed; 0 failed; 0 ignored; 0 measured; 175 filtered out; finished in 0.00s

     Running unittests src/main.rs
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

exit status 0.

### `cargo run -p rustak-server -- --config config.example.toml --check`

```
config.example.toml is valid: rustak would listen on 0.0.0.0:8446, with data in ./data.
```

exit status 0. The failure paths were exercised the same way:

| Input | Exit | First line |
|---|---:|---|
| a path that does not exist | 1 | `We could not read your config file 'nope.toml'.` |
| `[web.public.tls] mode = "none"` alone | 1 | `` `[web.public.tls] mode = "none"` would serve the admin UI, the API, enrollment and OAuth tokens over plaintext HTTP.`` |
| `mode = "acme"` with the default `listen = [":8446"]` | 1 | ``The ACME `tls-alpn-01` challenge is answered on port 443, and nothing is bound there.`` |

Each prints the `human-errors` advice block beneath it.

### `cargo clippy -p rustak-server --lib --bins -- -D warnings`

```
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.45s
```

exit status 0.

`--all-targets` currently fails with five `clippy::redundant_closure` errors, **all** in
`rustak-server/src/db/migrations.rs` (that brief's file, mid-flight):

```
rustak-server/src/db/migrations.rs:368:43
rustak-server/src/db/migrations.rs:369:49
rustak-server/src/db/migrations.rs:387:28
rustak-server/src/db/migrations.rs:390:25
rustak-server/src/db/migrations.rs:438:29
```

Nothing under `src/config/` or in `src/main.rs`. The isolated harness, which compiles exactly these
files with `--all-targets` and the workspace's lint set spelled out on the command line
(`-D warnings -W clippy::all -W clippy::dbg_macro -W clippy::todo -W clippy::print_stdout
-W clippy::large_futures -D unused_must_use -W rust-2018-idioms`), finishes clean.

### `RUSTDOCFLAGS="-D warnings" cargo doc -p rustak-server --no-deps`

Fails on one broken intra-doc link in `rustak-server/src/db/repos/groups.rs:29` (the other
brief's file). The same command over the isolated harness — which documents these files and the
real `main.rs` — succeeds:

```
 Documenting rustak-server v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s)
   Generated .../doc/rustak_server/index.html and 1 other file
```

Making the section modules `pub` (as `rustak-core::config` does) was needed to get there:
`[module documentation](self)` in a private module is a `rustdoc::private_intra_doc_links` error
under `-D warnings`. `main.rs` refers to `rustak_server::run` in prose rather than as a link,
because that item does not exist until M0-12.

### `cargo test -p rustak-server --doc config`

```
running 1 test
test rustak-server/src/config/mod.rs - config (line 28) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

exit status 0.

### `./scripts/check-file-length.sh`

```
(no output)
```

exit status 0. As M0-04 noted, `git ls-files` does not list untracked paths, so that run does not
yet cover this brief's new files. The same `awk` counter applied to them explicitly:

| File | Functional lines |
|---|---:|
| `config/validate.rs` | 199 |
| `config/auth.rs` | 175 |
| `config/pki.rs` | 158 |
| `config/acme.rs` | 137 |
| `config/web.rs` | 100 |
| `config/oidc.rs` | 92 |
| `config/mod.rs` | 91 |
| `config/storage.rs` | 66 |
| `config/retention.rs` | 59 |
| `main.rs` | 59 |
| `config/server.rs` | 45 |
| `config/stream.rs` | 39 |

Limit 300; largest is 199.

### `rustfmt --edition 2024 --check` over `src/config/*.rs` and `src/main.rs`

Clean. (`cargo fmt -p rustak-server` cannot run while `src/db/repos/mod.rs` names modules whose
files have not landed yet, so the files were checked directly.)

# M7-01 — `plain_bind`, ACME handles, wildcards at `--check` — complete

Brief: `.claude/plan/briefs/M7-01-acme-plain-listener.md`
Read first: `conventions.md`; `plan.md` → Listeners and the TLS row; `design/03` §3 (the
`:80` plain listener and the `301`, lines 29/281/505/563); status `M2-10-acme.md` (the resolver
slot and the token map, and the backlog lines they left), `M2-13-tls-files-reload.md` (how
`web/tls.rs` and `runtime.rs` build and hot-swap a listener today); `config/{acme,validate,web}.rs`,
`web/{mod,server,tls}.rs`, `runtime.rs`, `services/{mod,late}.rs`, `pki/acme/**`.

## What was built

All three backlog lines are closed: the plaintext listener exists, the two process-wide statics
are gone, and `--check` refuses a wildcard.

| File | Functional lines | Contents |
|---|---:|---|
| `web/plain.rs` *(new)* | 175 | `build_plain`, `build_plain_on`, `PlainSocket`, `plain_server`; `Origin` (canonical host, served names, port) and the `301` handler |
| `pki/acme/state.rs` *(new)* | 52 | `AcmeState`: the resolver slot and the `http-01` token map, with a `Debug` that counts answers and prints none |
| `pki/acme/mod.rs` | 54 (was 63) | `RESOLVER`, `publish_resolver` and `resolver()` deleted; `pub mod state` + `pub use state::AcmeState`; module doc rewritten ("Why the resolver is a handle") |
| `pki/acme/challenge.rs` | 115 (was 127) | `TOKENS`/`publish`/`withdraw`/`answer` deleted; `routes(Arc<AcmeState>)` returns a configurator and registers the handle as app data; `Responder` holds the handle; module doc kept and rewritten for it |
| `pki/acme/renew.rs` | 250 (was 254) | `run(services, forced)` — the resolver comes from `services.acme()` rather than a parameter |
| `services/mod.rs` | 225 (was 218) | `pub type AcmeState`, the `acme: Arc<AcmeState>` field, `Services::acme()`, both impls, the `Debug` field, the module's "The ACME slot" section |
| `web/tls.rs` | 222 (was 219) | `resolve(…, acme_state: &AcmeState)`; `acme()` publishes into it instead of the static |
| `web/server.rs` | 141 (was 140) | `http01_routes(context.acme())`; `cannot_bind` is `pub(super)` so the plaintext listener reports a refused socket the same way |
| `web/mod.rs` | 9 | `pub mod plain`, two re-exports, a paragraph on the third listener |
| `runtime.rs` | 244 (was 236) | binds the plaintext listener after the public one; `serve_marti` generalised to `serve_optional(context, server, what)`; the join is six components |
| `config/acme.rs` | 177 (was 156) | `AcmeConfig::validate_wildcards` + `ADVICE_WILDCARD`; `unorderable`'s doc says validation refuses wildcards first |
| `config/validate.rs` | 285 (was 284) | one line calling it, in `acme()` before `public_names` |
| `config/web.rs` | 148 (was 142) | `PublicWebConfig::https_port` (the port a redirect carries, `None` for 443); `plain_bind`'s doc points at `web::plain` |
| `jobs/acme_renew.rs` | 67 | **one line**: `acme::run(&services, job.forced)` — see **Deviations** |

**19 new unit tests** (10 `web::plain`, 5 `pki::acme::state`, 2 `services`, 1 `config::web`,
1 `config::validate`), the five `pki::acme::challenge` ones rewritten for the handle, and
**4 integration tests**
in `rustak-server/tests/acme_plain_listener.rs`. `tests/acme_directory.rs` was updated to use the
handle (its challenge mock is now told which context to ask).

## How it fits together

```
start-up ──► AppContext::new            ─► acme: Arc<AcmeState>   (empty, always valid)
             web::tls::resolve(…, &context.acme())
               └ mode = "acme": HotSwapCertResolver ─► state.publish_resolver(…)
             web::build_public(context, tls)
               └ services(): .configure(acme::http01_routes(context.acme()))
             web::build_plain(context)              ─► None unless [web.public] plain_bind
               ├ acme::http01_routes(context.acme())
               └ default_service ─► 301 https://<host><path>
             runtime: six components joined, one Shutdown, one drain budget

renewal  ──► AcmeRenewJob ─► acme::run(&services, forced)
               ├ state = services.acme()
               ├ Responder::new(state)   ─► http-01: state.publish/withdraw
               │                         ─► tls-alpn-01: state.resolver()
               └ install(…, state.resolver().as_ref())
```

### Decisions worth knowing

- **The slot is a plain field, not a `Late<…>`.** The brief pointed at the `Late` pattern, and
  `AcmeState` does not fit it: what arrives late is the *resolver inside* the handle, not the
  handle, and the handle has to exist before `web::tls::resolve` runs so that it has somewhere to
  publish. Making it `Late` would also mean every test harness installing one before the challenge
  route could answer — `TestServer` and `AppContext::new_mock` build no listener — for a value that
  is an empty map and an empty slot. It is created in `AppContext::new` instead, and
  `Services::acme()` never fails. The lateness that mattered is still enforced: `state.resolver()`
  is `None` until `mode = "acme"` publishes one, and every caller already treats that as "nothing
  to swap".
- **The redirect never echoes an unvalidated `Host`.** `Origin` keeps the requested host only when
  it matches `[server] domains`, `[acme] domains` or the host of `[server] base_url`, and sends
  anything else to the canonical name. A host that is not a host at all (spaces, slashes, quotes)
  is discarded before it can reach a `Location`. The one case that echoes is an installation with
  no name configured anywhere: there is nothing to compare against and nowhere else to send
  anybody, so it redirects to what was asked for and warns once at start-up.
- **`[server] base_url` wins over the listen port.** The brief said "the first public TLS
  listener's port when it is not 443", which is right when rustak is reached directly and wrong
  behind a proxy: an operator whose proxy publishes 443 while rustak binds 8446 has said so in
  `base_url`, and a redirect to `:8446` would send every browser somewhere unreachable. So a
  configured `base_url` decides host *and* port on its own (no port in it means the default), and
  `PublicWebConfig::https_port()` — first address, `None` for 443 — is the fallback.
- **The query string travels with the path.** `https://<host><path>` in the brief; dropping
  `?a=1` silently breaks every deep link, so the `Location` carries `path_and_query`.
- **Every method is redirected, including `POST`.** A `301` may change the method, which is fine
  here: the point is that no credential endpoint answers on this port at all, not that a POST
  completes across the redirect. `tests/acme_plain_listener.rs` asserts it for
  `POST /api/v1/auth/token`.
- **The wildcard rule lives in `config/acme.rs`**, beside `unorderable`, as
  `AcmeConfig::validate_wildcards` — the same shape `TlsConfig::validate_files` already uses, and
  the reason `validate.rs` stayed under 300 functional lines. `unorderable` still strips `*.` and
  still judges `*.tak.lan` as private, because it is public API and the wildcard rule runs first
  anyway; the "a wildcard is accepted" row was removed from its test, and the new refusal is
  covered in `config::validate`.

## Deviations from the brief's file list

Two files outside **Files you own** were touched, both unavoidable and both minimal:

1. **`rustak-server/src/web/tls.rs`** — the brief says to thread the handle to "the TLS acceptor
   builders", which is this file; it is where `publish_resolver` was called and where two tests
   read the static back. No other M7 agent owns `web/**`.
2. **`rustak-server/src/jobs/acme_renew.rs`** — **one line**, the `acme::run` call site. Deleting
   the statics changes `run`'s signature, and the job is the production caller. M7-02 owns
   `jobs/**` but adds `jobs/cloudtak_sweep.rs` and lines to `jobs/mod.rs`; `acme_renew.rs` is not
   in its brief, so the two diffs do not overlap. **If M7-02 did touch this file, this one line is
   the thing to check.**

Nothing else outside the list changed: `jobs/mod.rs`, `pki/mod.rs` (its `pub use acme::http01_routes`
still compiles), `pki/tls/files.rs` (whose own process-wide slot is M2-13's and out of scope),
`testing/**`, `.claude/plan/{plan,backlog}.md` and every CI file are untouched.

## What was not done

- **`pki::tls::files`'s process-wide slot stays.** The brief names the ACME statics only, and
  `files.rs` is outside `pki/acme/**`. It has the same shape and the same weakness (one process,
  one listener); worth a backlog line if a second server in one process is ever wanted for
  `mode = "files"` too.
- **No end-to-end ACME run over the plaintext port.** `tests/acme_directory.rs` drives the real
  client against a mock directory, which fetches the challenge over its own transport rather than
  over a socket; `tests/acme_plain_listener.rs` proves the socket answers what the handle holds.
  The two halves are covered, the join between them is still only covered by the staging checklist
  in `docs/deployment.md`.
- **Nothing binds port 80 in a test.** The integration suite binds `:0` on loopback and hands the
  socket to `build_plain_on`, as `enroll_flows` does for Marti. A real `:80` bind needs
  `CAP_NET_BIND_SERVICE` and is the operator's to check.

## Exit checks

```
$ cargo fmt --check
(no output, exit 0)

$ cargo clippy --workspace --all-targets -- -D warnings
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 13.48s

$ cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.50s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 6 other files

$ ./scripts/check-file-length.sh
(no output, exit 0)

$ cargo test -p rustak-server        # lib
test result: ok. 1781 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 29.22s

$ cargo test -p rustak-server --test acme_plain_listener
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.32s

$ cargo test -p rustak-server --test acme_directory
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.22s
```

Every other integration suite in the crate ran in the same `cargo test -p rustak-server` and
reported `0 failed`. (The working tree also carries four other agents' in-flight changes; the
suites above are the ones this brief touches.)

## For the backlog

- `pki/tls/files.rs` still publishes its `FilesCertificate` into a process-wide slot, for the same
  reason `pki/acme` used to. Same fix, one file.
- `[web.public] plain_bind` takes one address, unlike `listen`. A dual-stack deployment that wants
  `:80` on both families needs two, or a wildcard `[::]` bind. Worth a `Vec<ListenAddr>` if anyone
  asks.
- `dns-01` is now refused with a message that names it. If a wildcard is ever wanted, that is the
  challenge to implement, and `AcmeConfig::validate_wildcards` is the rule to delete.

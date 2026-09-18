# M2-13 — `[web.public.tls] mode = "files"`: reload on change, tolerate files that arrive later — complete

Brief: `.claude/plan/briefs/M2-13-tls-files-reload.md`
Read first: `conventions.md`; status `M2-10-acme.md` (the resolver slot, `web::tls::resolve`,
the `settings/tls` DTO) and `M0-12-runtime-bootstrap.md`;
`rustak-server/src/{web/tls.rs,pki/tls/resolver.rs,pki/acme/{mod,renew}.rs,jobs/{mod,acme_renew,wal_checkpoint}.rs,config/web.rs}`;
`docs/deployment.md` → TLS modes.

## What was built

`mode = "files"` now behaves like `mode = "acme"`: the listener is built behind a
`HotSwapCertResolver`, binds whether or not the pair is on disk, and a background job swaps a
renewed pair in without a restart.

| File | Functional lines | Contents |
|---|---:|---|
| `pki/tls/files.rs` *(new)* | 273 | `Stamp`/`Fingerprint` (size, mtime, inode), `load` (read → parse → prove the key is the leaf's → validity window), `FilesCertificate` (`reload`, `status`), the process-wide slot (`publish`, `for_config`), `report` |
| `jobs/tls_files.rs` *(new)* | 86 | `TlsFilesJob` — every `reload_interval`, on the blocking pool; `TLS_FILES_PARTITION`, `TLS_FILES_RELOAD_KEY`, `TlsFilesTask { forced }` |
| `web/tls.rs` | 219 (was 211) | `from_files` → `files`: bootstrap-on-missing, `require_files_at_start`, publishes the resolver |
| `config/web.rs` | 142 (was 100) | `reload_interval` (default `30s`), `require_files_at_start` (default `false`), `missing_at_start`, `reload_every`, `validate_files` |
| `config/validate.rs` | 283 (was 282) | **one line**: `TlsMode::Files => public.tls.validate_files()` |
| `web/api/settings.rs` | 104 (was 86) | files branch in `tls` and in `renew_tls` |
| `rustak-api/src/settings.rs` | 111 (was 99) | `TlsStatus::{loaded_at, cert_file, key_file, note}`, additive |
| `pki/acme/renew.rs` | unchanged count | **one line**: `..TlsStatus::fixed(source)` so the literal still compiles |

One-line registrations: `pki/tls/mod.rs` (`pub mod files;`), `jobs/mod.rs` (`pub mod tls_files;`
+ one `pub use`). **`jobs/mod.rs` must land in the same commit as `jobs/tls_files.rs`** — those two
lines were swept into another landing once already and broke the build.

> **`origin/main` is broken until `config/web.rs` lands.** The `[web.public.tls]` block of
> `config.example.toml` — including `reload_interval` and `require_files_at_start` — was swept into
> `dd4177a` without the two fields that make it parse, and every config struct carries
> `deny_unknown_fields`. On `origin/main` today,
> `config::tests::the_documented_example_configuration_loads_and_validates` fails and
> `rustak --config config.example.toml --check` reports `unknown field reload_interval`. The
> working tree is correct; `config/web.rs` closes it.

**27 new unit tests** (11 in `pki::tls::files`, 6 in `jobs::tls_files`, 3 in `config::web`, 1 in
`config::validate`, 3 in `web::tls`, 2 in `web::api::settings`, 1 in `rustak-api`).

## How it fits together

```
start-up ──► web::tls::resolve (mode = "files")
               │  files::load(cert_file, key_file)
               │    ok  ─► info!, resolver holds the pair
               │    err ─► require_files_at_start ? return the error
               │                                 : warn! naming both paths,
               │                                   bootstrap() — the internal CA certificate
               └► files::publish(FilesCertificate::new(paths, resolver, loaded, failure))

job host  ──► TlsFilesJob::setup  ─► armed one reload_interval out, only when
                                     mode = "files" and reload_interval > 0
TlsFilesJob::handle
               ├ re-arm (+reload_interval), unless this run was forced
               └► spawn_blocking(FilesCertificate::reload)
                     ├ stat both files ─► absent?        Waiting   (debug!)
                     ├ same fingerprint as last attempt? Unchanged (debug!)
                     ├ load ─► ok:  resolver.install(...)  Swapped  (info!)
                     └       └ err: record on the row     Rejected (warn!)

GET  /api/v1/settings/tls        ─► files::report(config)   (source "files", both paths,
POST /api/v1/settings/tls/renew  ─► TlsFilesJob forced       loaded_at, not_after, note,
                                    202 + the status          last_error, attempts)
```

### Decisions worth knowing

- **A missing pair is not a failure, an unusable one is.** `state` is `missing` with a `note`
  ("…waiting for the certificate files to appear") when neither file is on disk, and `failed` with
  `last_error` when they are there and cannot be served. An operator watching a first deploy and an
  operator debugging a half-written renewal are asking different questions, and one state for both
  answers neither.
- **The fingerprint of a *rejected* pair is remembered too.** Otherwise the same broken bytes are
  re-read, re-parsed and re-logged every thirty seconds for as long as nobody notices. The next
  change to either file — the rest of the write landing — is tried immediately.
- **The key is proved against the leaf before the swap**, through `CertifiedKey::from_der`, which
  parses the chain with webpki and checks the public key. A renewal caught between its two writes
  therefore keeps the *old* certificate serving instead of breaking every handshake until the next
  interval. This is the failure the whole design is arranged around.
- **`Fingerprint` carries the inode as well as size and mtime** (`#[cfg(unix)]`; zero elsewhere).
  An agent that renames a new file over the old one within the same second changes neither of the
  other two reliably.
- **The reload runs on `spawn_blocking`.** These files are frequently on a mount an agent is writing
  to, and a stall there must not hold a tokio worker.
- **`Loaded` carries the fingerprint taken *before* the read**, so a pair rewritten while it is
  being read is looked at again rather than recorded as the one being served.
- **`for_config` filters the process-wide slot by the configured paths.** One running server builds
  one listener, so this is always the published one; in a test process that has built two it is
  what keeps the admin API from answering with another test's paths. The slot exists for the same
  reason ACME's does — it is built where the listener is built and needed by a queue job.
- **`config/validate.rs` got exactly one line.** The rule and its advice live in
  `TlsConfig::validate_files` (`config/web.rs`), following `PkiConfig::validate_name_entries`:
  both are facts about how the values are used, the advice is most of the code, and `validate.rs`
  was at 282 of its 300 functional lines with another agent editing it. It is now at 283.
- **`--check` refuses a `require_files_at_start = true` whose files are absent**, and accepts the
  same configuration with the default — which is the deployment this brief exists for.
- **`needs_attention()` was deliberately *not* changed.** `rustak-ui/src/pages/settings_tls.rs`
  asserts it is false for `files`, and the UI is another agent's ground; a `files` listener waiting
  for its pair is now reportable through `state` and `note` instead. See the backlog item.

## Testing

- `pki/tls/files.rs` — a pair read with its validity window; a key that is not the leaf's refused
  by name; a missing file named; an empty chain refused before rustls sees it; **a renewed pair
  swapped in and byte-compared at the resolver**; **a broken new pair ignored with the previous one
  still presented, counted, and not re-read**; **absent at start, waited for, then picked up**; an
  expired certificate reported as `expiring`; a start-up failure reported as `failed` while an
  absent pair reads as `missing`; `Debug` never rendering a key.
- `jobs/tls_files.rs` — nothing scheduled when the listener does not read from disk, and nothing
  when `reload_interval = "0"`; the schedule armed one interval out; a run with no listener to swap
  still re-arms; a forced run does not; **a renewal written to disk reaches the resolver in one
  run**.
- `config/web.rs` — the defaults (`30s`, `false`); nothing watched in the other modes or with the
  check off; the promised-but-absent file named, and not named under the default.
- `config/validate.rs` — a `files` configuration whose files do not exist validates; the same with
  `require_files_at_start = true` does not, and the message names the file.
- `web/tls.rs` — a promised pair that is not there names the path; **a pair that has not been
  written yet still binds, reports `missing`, and serves the pair once it appears**.
- `web/api/settings.rs` — a files listener reports `source: "files"`, both paths and the note;
  `renew` queues a forced re-read and answers `202`.
- `rustak-api` — the new fields round-trip and are omitted when unset.

## Exit checks

Run on 2026-09-19 against the whole workspace, with three other agents editing concurrently.
All green.

```
$ cargo fmt --all -- --check
(clean — no output)

$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 46.73s
(clean)

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
(clean)

$ MAX_FUNCTIONAL_LINES=300 bash scripts/check-file-length.sh
(clean — exit 0)

$ cargo test --workspace --no-fail-fast
test result: ok. 1682 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 27.20s
   (rustak-server lib — includes the 26 new server-side tests)
test result: ok. 142 passed; 0 failed; …   (rustak-api)
… 41 test binaries, every one `ok`, 0 failed anywhere.

$ ./target/debug/rustak --config config.example.toml --check
config.example.toml is valid: rustak would listen on 0.0.0.0:8446, with data in ./data.

$ ./target/debug/rustak --config files-waiting.toml --check          # rc=0
#   [web.public.tls] mode = "files", cert_file/key_file under a directory that does not exist
files-waiting.toml is valid: rustak would listen on 0.0.0.0:8446, with data in …/data.

$ ./target/debug/rustak --config files-fail-fast.toml --check        # rc=1, as intended
#   the same file plus require_files_at_start = true
error(usr):  `[web.public.tls] require_files_at_start` is set and
             …/secrets/fullchain.pem is not there, so rustak would refuse to start.
 • Write the certificate chain and its private key to those paths before starting rustak.
 • Or leave `require_files_at_start = false`, which binds the listener with a certificate
   from rustak's own CA and swaps the files in as soon as they appear.
```

### The live check

Not a test in the suite — a real server, a real handshake, and a pair written while it ran.
`mode = "files"`, `reload_interval = "5s"`, `cert_file`/`key_file` pointing at an empty directory;
`openssl s_client` before and after writing an ECDSA pair with `CN=sidecar.example.com`.

```
--- the warning it logged ---
WARN server.run:web.tls.resolve: rustak_server::web::tls: The public certificate files are not
     usable yet, so the listener starts with one from this installation's own authority and will
     swap them in as soon as they appear. Watch GET /api/v1/settings/tls for it.
     cert_file=…/live/secrets/fullchain.pem key_file=…/live/secrets/privkey.pem
     reason=We could not read the certificate chain at …/live/secrets/fullchain.pem:
     I/O error: No such file or directory (os error 2) interval=Some(TimeDelta { secs: 5 })

--- what it presents before the files exist ---
subject=CN=localhost, O=rustak                 ← this installation's own authority

--- what it presents after the sidecar wrote them (no restart, 5s later) ---
subject=CN=sidecar.example.com

--- what it logged about the swap ---
INFO job.run{job.name="housekeeping/tls-files" job.delay=5014 job.attempts=1}:
     rustak_server::jobs::tls_files: The public listener is now presenting the certificate on
     disk. Existing connections keep the one they negotiated with; every new handshake gets this
     one. not_after=Some(2026-12-17T23:29:37Z)
```

Local toolchain is rustc 1.96.0; CI's stable is newer and remains the authority for new lints.

## Deviations from the brief

1. **`pki/tls/files.rs` is a new file, and `pki/tls/mod.rs` gained one `pub mod` line.** The brief
   gives me `pki/tls/resolver.rs` "(additive)" and `jobs/tls_files.rs`. The resolver needed nothing
   added — `install`, `current` and `is_ready` already do everything this mode wants — and the
   loading, the fingerprinting and the status belong beside it rather than inside a job module that
   `web/tls.rs` would then have to call into at start-up. The registration line is the same
   one-line kind the brief sanctions for `jobs/mod.rs`.
2. **One line in `pki/acme/renew.rs`**, which the brief does not list as mine: its `TlsStatus`
   literal does not compile once the DTO has four more fields, so it ends with
   `..TlsStatus::fixed(source)`. That file was not named as contended.
3. **Two documentation-only edits in `rustak-api/src/settings.rs`** beyond the additive fields: the
   `TlsCertificateState` and `Missing` comments said only ACME had more than one state, which is no
   longer true. No behaviour changed, and `needs_attention` was left exactly as it was.
4. **`POST /settings/tls/renew` queues the re-read rather than doing it inline**, matching the ACME
   branch's contract (`202`, poll the `GET`) and keeping file I/O out of a request handler. It works
   whether or not the timed check is switched on, which is what makes `reload_interval = "0"` a
   usable configuration.
5. **No audit entry for a swap.** ACME records `acme.issued`; this records nothing, because the
   brief asks for logging and the audit surface for "the certificate changed" wants to cover the
   internal daily rotation too. Backlog below.

## Backlog items this leaves

- **The admin UI does not show a `files` listener's paths, `loaded_at` or `note`, and
  `TlsStatus::needs_attention()` is still ACME-only** — so a files installation whose pair never
  arrives, or whose renewal is unusable, shows no banner. `rustak-ui/src/pages/settings_tls.rs`
  also titles any `last_error` "The last order failed.", which is the wrong sentence for a file
  that could not be read. (`rustak-ui/src/pages/settings_tls.rs`, `rustak-api/src/settings.rs`.)
  Found by M2-13.
- **A certificate swap is not audited.** `acme.issued` exists; there is no `tls.files.reloaded`,
  and no entry for the internal certificate's daily reissue either. One `pki`-category entry
  covering all three would answer "when did the certificate this listener presents last change?"
  from the audit log. (`jobs/tls_files.rs`, `pki/server_cert.rs`.) Found by M2-13.
- **`mode = "internal"` is the only mode still built with a fixed `ServerConfig`**, so a reissued
  internal certificate is not served until a restart. It now has the two pieces it would need — a
  resolver and a job that installs into one. (`web/tls.rs`.) Found by M2-13.

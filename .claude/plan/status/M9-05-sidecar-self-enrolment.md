# M9-05 — A sidecar enrols its own certificate from a one-time enrolment token

**Status: complete.** Every deliverable in the brief is implemented, both of the
coordinator's mid-task requirements are folded in, and every exit check is green.

`docs/plugins.md` has always promised that "a sidecar enrols when it has no certificate
rather than on every start". The harness now does it: a plugin whose `[service]
certificate`/`key` name files that are not there, and that holds a one-time enrolment
token, enrols before the CoT stream is opened, writes the three PEMs (the key with mode
`0600`), logs the certificate's subject and expiry, and carries on start-up with them.

## What landed

| Area | Files |
|---|---|
| Self-enrolment | `rustak-client/src/sidecar/enrolment.rs` (new: `ensure`, `check`, path resolution, the `x509` summary) |
| Start-up wiring, `--enroll` | `rustak-client/src/sidecar/run.rs` (`Args::enroll`, `serve` made public, two calls) |
| Configuration | `rustak-client/src/sidecar/config.rs` (`account`, `enrollment_token`, `pki_dir`, `ENROLLMENT_TOKEN_ENV`, two accessors) |
| Module wiring | `rustak-client/src/sidecar/mod.rs` (one `mod` line, two re-export lines) |
| Writing files | `rustak-client/src/enroll.rs` (additive: `write_files`, `write_identity`, `write_truststore`, the `0600` `write_pem`) |
| Dependency | `rustak-client/Cargo.toml` — one line, `x509-parser = { workspace = true }` |
| End-to-end test | `rustak-server/tests/sidecar_enrolment.rs` (new suite) |
| Example configs | `rustak-plugin-{example,ais,adsb}/config.example.toml` |
| Docs | `docs/plugins.md` ("Credentials", "Running one" + new "The first start", "Two credentials"), `docs/deployment.md` ("The sidecar images") |

`Cargo.lock` is updated as a build artefact. Nothing outside the brief's "Files you own"
was touched; no `git`/`but` command was run.

## The exact names a deployment needs

| | Value |
|---|---|
| Enrolment token, environment | **`RUSTAK_ENROLLMENT_TOKEN`** (read directly, and the variable `${{ env.… }}` would name) |
| Enrolment token, file | **`[service] enrollment_token`** — takes precedence over the variable when set |
| Service token, environment | **`RUSTAK_SERVICE_TOKEN`** (unchanged; `[service] token` is what reads it) |
| PKI directory | **`[service] pki_dir`** — default: **the directory the configuration file is in** (so `--config /data/plugin.toml` → `/data`) |
| Account to enrol as | **`[service] account`** — default: `[service] name` |
| Flag | **`--enroll`** (mutually exclusive with `--check`) |
| Files written | `<pki_dir>/<name>.pem`, `<pki_dir>/<name>.key` (mode `0600`), `<pki_dir>/truststore.pem` — each overridden by `[service] certificate` / `key` / `truststore` when that names a path |

## How it behaves

1. **When it enrols.** `[service] certificate` **and** `key` must both name existing files
   for the sidecar to be considered enrolled. Otherwise, with a token present, it enrols.
2. **Where it enrols.** `[server] marti` if set, else `[server] control` — rustak's public
   listener serves `/Marti/api/tls/*` beside the control API. Neither set is a `Kind::User`
   failure before anything is dialled. Unresolved `${{ env.… }}` endpoints are refused by
   name first (`ServerConfig::endpoints`).
3. **As whom.** `username` = `[service] account` or the service name; `client_uid` =
   `ServiceName::uid()`, i.e. `SERVICE-<name>` — taken from the same function the descriptor
   uses rather than re-formatted, so the two halves cannot drift.
4. **Truststore.** The CA chain the server answered with is written as the truststore and
   used from then on, *unless* `[service] truststore` names a file that **already exists** —
   that is the operator's own CA, distributed out of band, and it is kept (with an `info`
   line) and is also what verifies the server during the enrolment call itself. With no such
   file, the enrolment call verifies the server against the platform's roots, which is right
   for the public listener behind an ACME certificate. *(Reading note: the brief says "unless
   `[service] truststore` was given explicitly". Keying on "given **and present**" rather
   than "given" is deliberate: every shipped `config.example.toml` names a truststore path,
   so keying on the setting alone would leave a first start with no truststore at all.)*
5. **A leftover token.** When the files exist, the token is not read at all — only its
   presence is, for an `info` line saying it was not used. An unresolved
   `${{ env.… }}` expression in that state is therefore *not* an error, which is what keeps
   a second start working after the variable is dropped.
6. **Failure is fatal.** A refusal, an unreachable server or an unwritable path ends
   start-up with a `Kind::User` error that wraps the cause: "Could not enrol '<account>' at
   '<server>'." plus the refusal's own message and advice (`human_errors` merges the advice
   of the whole chain, so "An enrolment token is one-time…" survives the wrapping). Nothing
   half-written is left behind — the key is written only after the signing call succeeds.
7. **`--enroll`.** Does step 1 and exits 0, logging the three paths. Idempotent: a sidecar
   that already has a certificate exits 0 having done nothing. With no certificate *and* no
   token it fails rather than reporting success (an init container that enrolled nothing is
   a deployment that breaks later).
8. **`--check`.** Touches no network and writes nothing. See below.

## The two mid-task requirements

**(1) `[service] pki_dir`.** The key is spelled exactly that. `pki_dir = "/data"` with
nothing else about the identity puts all three files in `/data` on the first start and reads
them back from `/data` on every later start with no token present — covered by
`a_pki_dir_holds_the_three_files_and_is_read_back_without_a_token`, which also asserts the
second start makes **no** request at all.

**(2) `--check` accepts a "will enrol" identity.** Confirmed as a real defect first: against
the pre-change binary, `--check` on a file naming `/data/*.pem` refused with *"Could not read
the truststore at '/data/truststore.pem'"*, because `SidecarContext::from_config` builds the
HTTPS client and reads the material. `enrolment::check` now runs first on the `--check` path:
when the certificate and key are not there **and** the file says it means to enrol (a
`pki_dir` and/or a token), it logs

> The configuration is valid; this identity will be enrolled into '/data' on the first start.

and takes the not-yet-written paths off the configuration so the rest of validation proceeds.
It also resolves the enrolment token, so an `${{ env.… }}` expression whose variable is unset
is refused **by name** — a real check of a pre-enrolment file. A file with neither a
`pki_dir` nor a token is left exactly as it was, so a missing certificate is still reported
in the words start-up already had. Tests:
`check_accepts_a_deployment_whose_identity_has_not_been_enrolled_yet` (with `[server] stream`
ssl:// *and* `control` set, and asserting nothing was written),
`check_refuses_an_enrolment_token_whose_variable_was_never_set`,
`check_takes_the_files_that_are_not_there_yet_off_the_configuration`,
`check_leaves_a_sidecar_that_never_meant_to_enrol_alone`.

## Credential handling

- The **private key is generated in-process** by `rcgen` inside `enroll::signing_request` and
  written locally; only the signing request (public half) crosses the wire. Said plainly in
  `docs/plugins.md` ("The first start", "Two credentials") and `docs/deployment.md`.
- The key file is created with `O_CREAT|mode(0o600)` **and** `set_permissions(0o600)`
  afterwards, because `create` leaves an existing file's mode alone — a re-enrolment over a
  key somebody once made readable must not inherit that. Asserted in
  `enroll::tests::the_private_key_is_written_so_that_only_this_process_can_read_it`, which
  seeds a `0644` file first. The certificate and truststore are public material and keep the
  umask, so an init container can write for a sidecar running as another user.
- The token is a `rustak_core::identity::Secret` end to end: read through the same
  `optional_secret` deserialiser as `[service] token`, never formatted into a log line or an
  error message, redacted in `Debug` and zeroised on drop
  (`an_enrolment_token_is_a_secret_like_any_other` asserts the `Debug` dump).

## Two decisions worth naming

- **`serve` is now public** (`rustak_client::sidecar::serve`). It is the start-up sequence
  `run_with` ends in, minus the `report_and_exit` that would take the calling process with
  it. The integration suite needs to exercise a *first start*, not just the loop, and
  `drive` starts after enrolment has already happened. This is additive.
- **`x509-parser` added to `rustak-client`** (one line, workspace version, already in the
  tree through `rustak-server`). It is how the start-up line can say the certificate's
  subject and expiry. A certificate whose encoding cannot be read is a `warn` line, not a
  failure to start: the file is written either way and refusing to run over a log line would
  be the worse failure.

## Tests

`rustak-server/tests/sidecar_enrolment.rs` — real CA, real `POST /Marti/api/tls/signClient/v2`,
real mTLS `:8089`, driven through `serve`:

- `a_sidecar_enrols_on_its_first_start_and_uses_what_it_wrote_on_the_next_one` — `--enroll`
  writes the three files and does **not** start the plugin (the `Connected` channel is
  empty); the certificate's subject names the account (`svc.enrolling`), not the service
  (`enrolling`); the key is `0600` and holds a private key; then a second `serve` with the
  same (now spent) token still in the environment connects to the stream and registers, and
  the certificate file is byte-identical — it cannot have re-enrolled, because the server
  would have refused the spent token.
- `a_token_the_server_refuses_stops_start_up_rather_than_running_half_identified` — a
  `Kind::User` error naming the account, carrying "Could not enrol" and the refusal's own
  "one-time" advice, and no key left behind.

Unit tests: 12 in `sidecar::enrolment` (path resolution incl. `pki_dir` and a relative
`--config`, marti-over-control precedence, account default, the ignored leftover token, the
unresolved token, the wiremock enrolment round trip asserting Basic auth is the account, the
refusal, the `pki_dir` round trip, the four `check` cases, the subject/expiry summary), 2 new
in `enroll` (the `0600` mode over a `0644` file, writing to configured paths), 2 new in
`sidecar::run` (`--check` on a pre-enrolment file; `--check` on an unresolved token), 1 new
in `sidecar::config` (the token is redacted and reads back).

## Exit checks

```
$ cargo fmt --all --check
ok

$ cargo clippy --workspace --all-targets -- -D warnings
    Checking rustak-server v0.1.0 (/Users/bpannell/dev/gh/SierraSoftworks/rustak/rustak-server)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.17s

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.56s
   Generated /Users/bpannell/dev/gh/SierraSoftworks/rustak/target/doc/rustak_api/index.html and 8 other files

$ ./scripts/check-file-length.sh
no file over the limit
  (enrolment.rs 190, config.rs 190, run.rs 151, enroll.rs 183 functional lines)

$ cargo test -p rustak-client -p rustak-plugin-ais -p rustak-plugin-adsb -p rustak-plugin-example
test result: ok. 190 passed; 0 failed   (rustak-client lib)
test result: ok. 11 passed; 0 failed    (sidecar_harness)
test result: ok. 2 passed; 0 failed     (stream_tls)
test result: ok. 97 passed; 0 failed    (rustak-plugin-ais)
test result: ok. 64 passed; 0 failed    (rustak-plugin-adsb)
test result: ok. 10 / 6 / 14 passed; 0 failed  (plugin suites, example, doc-tests)

$ cargo test -p rustak-server --test services_flow --test feed_sidecars --test sidecar_enrolment
     Running tests/feed_sidecars.rs
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.44s
     Running tests/services_flow.rs
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.93s
     Running tests/sidecar_enrolment.rs
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.93s
```

## What I could not verify

- **Against a real deployment.** Everything is exercised against the in-process harness and
  `wiremock`; nothing was run against the maintainer's Nomad cluster or a public listener
  behind a Let's Encrypt certificate. The platform-roots path (enrolling against a publicly
  issued certificate with no truststore) is therefore untested in anger — the suite's server
  is plain HTTP, as `services_flow`'s is.
- **Non-Unix permissions.** `write_pem` sets `0600` only on Unix; the `#[cfg(unix)]` tests do
  not run elsewhere, and there is no Windows equivalent implemented.
- **Cross-test env-var isolation.** `RUSTAK_ENROLLMENT_TOKEN` is process-wide and
  `std::env::set_var` is unsafe under edition 2024 (and `unsafe_code = "forbid"`), so the
  integration suite delivers it through an `--env` file and relies on
  `[service] enrollment_token` taking precedence for the refusal case. That ordering is what
  makes the two tests in that binary independent; it is asserted only indirectly.

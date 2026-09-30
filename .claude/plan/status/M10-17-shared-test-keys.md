# M10-17 — Generate the test keys once per run, not once per test

**Done.** Every RSA key the test helpers use is generated once per build and
read from a file by every other test process. The stream harness adopts the
shared signing key instead of generating one per harness, and the rate-limit
flood test takes 0.2 s instead of 13–14 s (42 s in CI). Locally, instrumented,
`cargo nextest run --workspace --profile ci -j 4` went from a median of
**383 s to 120 s** (−69%), and CPU from 1,284–1,552 s to 224–230 s.

## What changed and why

### Keys shared across processes (`testing/keys.rs`, new `testing/key_file.rs`)

- `testing::keys::rsa(name) -> RsaPrivateKey`: the first process to ask for a
  name generates an RSA-2048 key and leaves it at
  `<target>/<profile>/rustak-test-keys/<name>.pk8` (PKCS#8 DER); every later
  process reads and parses it. The `LazyLock`s in front stay, so a process
  parses each key once.
- **Where**: beside the test binaries. `CARGO_TARGET_TMPDIR` is set by Cargo
  only when compiling integration tests and benches, not the library the keys
  are made in (unit tests and the `testing` feature both compile `src/testing`
  as library code). So the directory is derived from `current_exe()`: a binary
  in `<…>/deps/` uses the `rustak-test-keys` directory next to `deps/`. Anything
  else (doctests) generates in memory and writes nothing. Instrumented builds
  under `--target-dir target/cov` get their own directory.
- **Writes are atomic**: a temporary name unique to the process and write, then
  `rename` in the same directory. Directory `0700`, files `0600` on Unix.
- **A file that does not parse** as a 2048-bit RSA PKCS#8 key is treated as
  missing and replaced.
- **Decision beyond the brief — a maker lock.** The first "after" run (no lock)
  showed a cold start's cost: nextest started four `workload_identity` tests at
  once, each generated the same four keys, and those tests took 14–30 s each
  instrumented (binary 109 s summed). So the first process to find no file
  creates `<name>.lock` (`create_new`), rechecks, makes and stores the key, and
  removes the lock; the others poll for the file every 25 ms. A lock older than
  60 s (`STALE`) belongs to a killed process and is taken over; a waiter that
  has waited 60 s makes its own key. Neither case fails anything — the worst is
  one extra generation — so `STALE` bounds wasted time, not a test. Result:
  `workload_identity` 109 s → 51–67 s summed. `File::lock` would be neater but
  is Rust 1.89 and the workspace MSRV is 1.88.
- **Keys are distinct by name**: `jwt-signing`, `oidc-provider`,
  `oidc-unadvertised`, `workload-primary`, `workload-rotated`,
  `workload-unadvertised`, `webauthn-rs256`. The forgery keys must differ from
  the advertised ones or the forgery tests would pass for the wrong reason.
- Threat model stated in the `keys.rs` module doc: the keys protect nothing,
  live and die with `target/`, are never checked in, are compiled only under
  `cfg(test)`/`testing`, and nothing a deployment runs reads them; anyone who
  can write `target/` can already replace the test binaries.
- Persistence: the files are per target directory, not per nextest run, so
  local runs after the first pay no generation at all. "Once per run" is the
  upper bound, which is what CI should see (see open questions).

### Consumers

- `keys::JWT_SIGNING_KEY` reads `rsa("jwt-signing")`.
- `oidc.rs` `PROVIDER_KEY`/`UNADVERTISED_KEY`, `workload.rs`
  `PRIMARY`/`ROTATED`/`UNADVERTISED`, `authenticator/keys.rs` `RSA_KEY` all read
  through `rsa(name)`. `workload.rs`'s `warm()` is unchanged in behaviour (its
  doc updated). The P-256 and Ed25519 authenticator keys are seeded randomness,
  not a search, and stay per process.
- Tests about generation or rotation still generate: `auth::jwt` tests that
  call `load_or_create`, `a_first_start_creates_everything_it_needs`, and
  `bootstrap` (which runs the real binary) are untouched.

### Harnesses (`tests/stream_support/mod.rs`, `testing::keys::adopt_signing_key`)

`build_context` is start-up's own path and calls `JwtIssuer::load_or_create`,
which generated an RSA key per `Harness` (M10-07's open item). Rather than a
testing variant of `build_context` in `lib.rs` (outside my files),
`testing::keys::adopt_signing_key(&config)` opens the database and secret store
the context will open — same paths, same `SecretStore::load` — and calls
`JwtIssuer::load_or_adopt` with the shared key, then closes the database.
`build_context` then *loads* the stored key: a second start instead of a first.
`Harness::build` calls it just before `build_context`; `feed_support` and
`workload_support` build on `Harness` and need no change. No harness test is
about the signing key. `tokens.rs`/`jwt/mod.rs` needed no change.

### The flood test (`auth/ratelimit.rs`, tests only)

Why it cost 13 s uninstrumented, 14 s instrumented locally and 42 s in CI: the
flood inserted `MAX_BUCKETS + 5_000` lockouts; every insert past the ceiling
calls `make_room`, which **prunes the whole 100,000-entry map** before evicting
— 5,000 × 100,000 = 500 million bucket visits. Now it overruns the ceiling by
5 (500,000 visits) and asserts the map is at exactly `MAX_BUCKETS` (was `<=`).
The part that matters is unchanged: 10,000 checks against a full map of live
lockouts, counting full scans, `<= 1`. Verified it still catches the bug by
mutation: with `check_at` pruning on every call, it fails with "scanned the
full map 10000 times". 0.17–0.20 s after, either way.

### Docs

- `docs/ci.md`: lever 1 rewritten (keys once per build, harness adoption);
  M10-16's projection replaced by the two measured `main` runs (step times,
  per-binary table); new "Test keys shared across processes" section with the
  mechanism and the local before/after figures.
- `CONTRIBUTING.md`: the nextest "a test has its process to itself" bullet now
  points at `testing::keys::rsa` and where the files live. The local commands
  did not change.

## Measurements

Local: 10-core Apple silicon, 32 GB, nextest 0.9.146 installed with
`cargo install --locked --root` into the worktree. "Before" = `main` at
`2eeb9915` exported with `git archive` into a scratch directory with its own
target; "after" = this worktree. Every run: `cargo nextest run --workspace
--profile ci --no-fail-fast -j 4`, tests built beforehand and outside the
timing, the key directory deleted first so every run starts cold as CI does.
Instrumented runs: `RUSTFLAGS=-Cinstrument-coverage`,
`LLVM_PROFILE_FILE=<dir>/%m.profraw`, `--target-dir target/cov`. Order:
plainB1, plainA1, covB1, covA1, covB2, covA2, plainB2, plainA2, covA3, covB3,
covA4, plainA3, plainB3. A1 (both modes) was measured before the maker lock
existed; A2–A4 are the final code (source reformatted by rustfmt afterwards
only). 1-minute load 3.5–9.5 throughout; no other agent's build was visible.

| Wall, `-j 4` | before | after (final) | after, no lock (A1) |
|---|---|---|---|
| **Instrumented** | 411.9, 383.4, 342.4 s — **median 383.4** | 123.7, 119.6, 112.6 s — **median 119.6** | 137.7 s |
| CPU (user), instrumented | 1,552 / 1,476 / 1,284 s | 229 / 230 / 224 s | 403 s |
| Uninstrumented | 128.9, 124.3, 120.8 s — median 124.3 | 92.8, 87.8 s — median 90.3 | 94.5 s |
| CPU (user), uninstrumented | 286–294 s | 153–154 s | 156 s |

Per-binary summed test time, instrumented (before B1/B2/B3 → after A2/A3/A4):

| Binary | Tests | before | after |
|---|---|---|---|
| `rustak-server` (lib) | 2,056 → 2,065 | 617 / 635 / 613 s | 209 / 212 / 209 s |
| `oidc_provider` | 24 | 50 / 49 / 49 s | 4.2 / 4.4 / 4.2 s |
| `workload_identity` | 7 | 236 / 248 / 157 s | 61 / 67 / 51 s |
| `cloudtak_onboarding` | 20 | 36 / 35 / 40 s | 22 / 17 / 20 s |
| `oauth_flows` | 31 | 53 / 47 / 44 s | 4.2 / 4.4 / 4.2 s |
| `api_v1_map` | 16 | 20 / 24 / 21 s | 3.0 / 3.2 / 2.9 s |
| all | 3,866 → 3,875 | 1,429 / 1,450 / 1,347 s | 437 / 450 / 425 s |

Uninstrumented summed: before 456–465 s, after 322–327 s. Tests with ≥ 5 s:
instrumented 16–19 → 13; what is left is the eight 10 s refused-handshake waits
in `pki::tls` (M10-16's open item) and the cold-start wait in the first
`workload_identity` tests (10–15 s each; later ones take ~1 s).

CI (read with `gh`, not changed): run 36648789737 on `03b09f0e` — job 20m56s,
`Run tests` 18m17s (3m30s compiling, 881 s running, 3,464 s summed), doctests
55 s, grcov 69 s; run 36648792932 on `2eeb9915` (plan docs only) — `Run tests`
45m05s (3m40s compiling, 2,479 s running, 9,796 s summed), doctests 52 s,
grcov 67 s: a slow host, about 2.8× per test, same per-binary shape.
Projection for the next `main` run: the local after/before ratio of summed
test time (~0.3) applied to run 1 puts the running part near 4–5 minutes on
four slots instead of 15. Not a measurement; it assumes the runner's ratio
matches this machine's, which M10-16 showed it need not.

## Isolation and correctness

- What is shared is key material only. Databases, data directories, secret
  stores, content stores and rate limiters stay per test; the signing key is
  sealed under each server's own secret store as before.
- Ten uninstrumented nextest runs in a row at default parallelism (the first
  cold): 10 × 3,875 passed, 2 skipped, 0 failed, 39.7–40.1 s each.
- Cold `rustak-test-keys` with 256 slots (every test starting at once, load
  average reaching 67 and 104): 2 × 3,875 passed; afterwards exactly the seven
  `.pk8` files, no `.lock` or `.tmp` left.
- Every one of the 13 measurement runs was cold: all passed (3,866 before,
  3,875 after), instrumented and not.
- `cargo test --workspace --no-fail-fast` (the other runner), cold: every
  binary `ok`, exit 0.

## New tests, and a host ten times slower

`testing::key_file` (fake 32-byte keys, no RSA): made once and read back; two
names are two keys; an unparseable file is replaced; 16 threads released by a
barrier on a cold directory make exactly one key and all use it; a lock older
than `STALE` is taken over; directory `0700` and file `0600`.
`testing::keys`: the shared key parses as 2048-bit; the key a process uses is
the one the file holds; the directory is `<profile>/rustak-test-keys`; a
512-bit key or garbage is not accepted.

On a slower host: nothing is timed. The only duration is `STALE` (60 s); a
generation that outlasts it makes waiters generate their own key — one more
generation, never a failure. The 16-thread test counts makers, and on any
interleaving the lock makes it one (the lock is taken before making and the
file rechecked under it). The stale-lock test backdates the lock's mtime by
2 × `STALE`, so it does not wait. The flood test counts scans, not time.

## Files

- `rustak-server/src/testing/key_file.rs` — **new**: directory, lock, atomic
  store, and their tests.
- `rustak-server/src/testing/keys.rs` — `rsa(name)`, `JWT_SIGNING_KEY` through
  it, `adopt_signing_key`, threat model, tests.
- `rustak-server/src/testing/mod.rs` — `mod key_file`.
- `rustak-server/src/testing/oidc.rs`, `workload.rs`,
  `authenticator/keys.rs` — keys read through `rsa(name)`; docs.
- `rustak-server/src/testing/context.rs` — module doc only.
- `rustak-server/tests/stream_support/mod.rs` — `adopt_signing_key` before
  `build_context`; module doc.
- `rustak-server/src/auth/ratelimit.rs` — the flood test only.
- `docs/ci.md`, `CONTRIBUTING.md`.
- `.claude/plan/status/M10-17-shared-test-keys.md` — this note.

No file outside the brief's list was touched. `.config/nextest.toml` needed no
setting.

## Exit checks (this worktree, after the last source change)

1. `cargo fmt --check` — pass.
2. `cargo clippy --workspace --all-targets -- -D warnings` — pass.
3. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` — pass.
4. `./scripts/check-file-length.sh` — pass.
5. `cargo test -p rustak-server` — pass: 39 result lines, 2,429 passed,
   0 failed (lib 2,065 incl. the new tests). The workspace ran under both
   runners as above.

## Open questions

1. **Does rust-cache keep `target/debug/rustak-test-keys`?** I expect not (it
   prunes a profile directory to `build`, `deps`, `.fingerprint`), so each CI
   run generates seven keys once. If it does keep them, runs pay nothing — also
   fine; the keys are no more sensitive in a cache than in `target/`.
2. **Production finding, not fixed (outside my files):** `RateLimiter::make_room`
   prunes the whole map on *every* new-key insert once the map is at
   `MAX_BUCKETS`, regardless of `next_sweep`. Under a flood of distinct
   subjects past 100,000 lockouts, each further failure is a full scan under
   the mutex — the same linear-per-request cost the sweep clock fixed for
   checks, now on the failure path. That is what made the test cost 42 s.
   Worth a small brief (e.g. prune in `make_room` only when the sweep is due).
3. The eight 10 s refused-handshake waits in `pki::tls` (M10-16) are now the
   largest single items in the library binary.
4. The first `workload_identity` tests of a cold run still wait ~10 s
   instrumented for the orchestrator's keys; unavoidable once per run unless
   CI keeps the key files.
5. `docs/ci.md`'s history paragraphs (lines ~63, ~98) still say "two-vCPU";
   left as history.

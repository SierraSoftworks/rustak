# M7-04 — `rustak-ui`'s unit tests run in CI; demo mode shows every TLS source — complete

Brief: `.claude/plan/briefs/M7-04-ui-tests-and-demo-tls.md`
Read first: `conventions.md`; `.github/workflows/rust.yml` (`ui` job) and `docs/ci.md`;
`rustak-ui/Cargo.toml`; status `M2-14-loose-ends.md` (the TLS card), `M2-07-admin-ui-identity.md`
and `M3-04-admin-ui-operations.md` (UI patterns, demo fixtures, the `?demo` macro);
`rustak-ui/src/pages/settings_tls.rs`; `e2e/`.

Only `rustak-ui/**`, the `ui` job in `.github/workflows/rust.yml`, `docs/ci.md`, the README's
Development block, `e2e/tests/settings.spec.ts` and this status file were touched. **Nothing under
`rustak-server/**`, `rustak-core/**` or `rustak-api/**` was changed**, and no `git`/`but` command
that writes was run.

## 1. Tests that run

**Mechanism: a host-target `cargo test` — option (a), unchanged.** No feature gate, no extra tool,
no browser, nothing installed. `rustak-ui` is a binary crate, so `cargo test` builds `src/main.rs`
as a test harness for the host triple; `wasm-bindgen`'s generated bindings compile there and panic
only if something *calls* one, and nothing in the suite does.

```
cd rustak-ui && cargo test
test result: ok. 51 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

| | |
|---|---:|
| Tests now running | **51** (46 before this brief, + 5 new below) |
| Tests excluded (`#[cfg(target_arch = "wasm32")]`) | **0** |
| Test files | 19 |

Nothing needed gating: every test in the crate is about a pure function — a label, a filter, a
diff, a tokeniser — which is what M2-14 guessed and is now measured. A test that one day does need
a DOM gates itself behind `#[cfg(target_arch = "wasm32")]`; the clippy step still type-checks it
and this step skips it. That convention is written into the workflow step's comment and into
`docs/ci.md` so the next person does not have to rediscover it.

**Option (b) was not needed and would have cost more.** `wasm-bindgen-test` means installing
`wasm-pack` (or `wasm-bindgen-cli` pinned to the exact `wasm-bindgen` version, which is a second
pin to keep in step with `Cargo.lock`) plus a headless Chrome or a Node shim, then running the
wasm binary through it — against a `ui` job that takes ~1.5 minutes today. It buys the DOM, and
nothing in the suite wants the DOM. When something does, (b) is the upgrade path and (a) keeps
working beside it.

**Proof that a failing assertion now fails the job.** `renew_label(TlsSource::Files).0` in
`rustak-ui/src/pages/settings_tls.rs` was changed from `"Re-read the files"` to `"Renew now"`:

```
thread 'pages::settings_tls::tests::the_button_is_offered_for_the_sources_that_fetch_a_certificate'
panicked at src/pages/settings_tls.rs:376:9:
assertion `left == right` failed
  left: "Re-read the files"
 right: "Renew now"
test result: FAILED. 50 passed; 1 failed
$ cargo test >/dev/null 2>&1; echo $?
101
```

The file was then restored from a copy taken before the edit, `cargo test` re-run (`51 passed`,
exit `0`), and `git diff --stat rustak-ui/src/pages/settings_tls.rs` is empty — the file is
byte-identical to `HEAD`.

**The `ui` job's added time: about half a minute, cold; about two seconds, warm.** Measured here
with an empty `CARGO_TARGET_DIR` (a cold `Swatinem/rust-cache`) and a warm cargo registry:
**21.6 s wall, 60.5 s CPU**; a touch-one-file rebuild is **2.3 s**. A GitHub runner has two vCPUs
against this machine's many, so the cold case there is CPU-bound at roughly 30–60 s and every run
after the first is seconds, because `rust-cache` keeps the host dependency artefacts exactly as it
already keeps the wasm32 ones. The step is placed after clippy and before the two `trunk build`s,
so a broken assertion fails before two bundles are built and uploaded.

**Not a finding, but worth recording:** `cargo fmt --all --check` run from `rustak-ui/` reports
diffs in *workspace* crates as well as in `rustak-ui` (today: several in `rustak-server`, from the
other agents' in-flight work). It does still check `rustak-ui` — proved by adding a deliberately
mis-formatted function to `rustak-ui/src/util.rs` and watching both `cargo fmt --check` and
`cargo fmt --all --check` name it, then restoring the file — so the `ui` job's fmt step is not
lying and was left alone. `cargo fmt --check` (no `--all`) is the narrower command if that
spill-over ever becomes annoying.

## 2. Demo TLS sources

`?demo&tls=<source>` now selects which status the fixtures hand the Transport security card:

| URL | Card |
|---|---|
| `?demo` (or any unrecognised value) | ACME, three failed orders — exactly as before |
| `?demo&tls=files` | `Missing`, with `cert_file`, `key_file`, "Last read", the note banner and "Re-read the files" |
| `?demo&tls=internal` | `Valid` from this installation's own authority: no ACME rows, no button |
| `?demo&tls=none` | The "This server is not serving TLS." warning, which is the whole card |

`tls=none` is beyond the brief's three by one line of `match`; it is the fourth branch
`settings_tls.rs` renders and nothing else in demo mode could reach it.

- `rustak-ui/src/fixtures/mod.rs` — new `demo_flag(name)`: the value of a query parameter beside
  `?demo`, read through `UrlSearchParams` (already a `web-sys` feature, used by `auth::oidc`).
  Returns `None` unless `is_demo()`, so a flag can never change what a real server said, and it is
  `#[cfg(debug_assertions)]` like the rest of the fixtures — a release bundle has no path to it.
- `rustak-ui/src/fixtures/certificates.rs` — `status_for(flag)` chooses between the existing
  `acme_status()` and the new `files_status()`, `internal_status()` and `TlsStatus::fixed(None)`;
  the `TLS` thread-local is seeded with it instead of with `acme_status()` directly.
- The same file's `renew_tls` now sets `note = None` and, for `files`, `loaded_at` rather than
  `last_attempt_at`/`renews_at`. Without that, pressing "Re-read the files" in the demo left the
  card saying it was still waiting for files it had just read, and filled in ACME fields a files
  listener does not have. The transition moved into `fetched(&mut status)` so it can be tested —
  reaching the thread-local reads `window.location`, and a host test has no window (this was
  found by the new tests: the first version of one panicked with "cannot access imported statics
  on non-wasm targets").

`certificates.rs`'s module header says what the flag is for. `backlog.md` and M2-14's own
status note were left alone — they are not this brief's to edit; the two lines this closes
are listed at the end of this note instead.

## 3. Tests that were silently wrong

**None.** All 46 pre-existing tests passed the first time they were ever run, and none was
deleted or rewritten. The 51 are the 46 plus five new ones in
`rustak-ui/src/fixtures/certificates.rs`:

- every source the card renders can be asked for by name, and an unknown flag falls back to ACME
  rather than to an empty card;
- a `files` listener shows what only a files listener has (both file rows, no `loaded_at`, the
  note, `needs_attention()`);
- re-reading the files clears the note and sets `loaded_at`;
- an order that succeeds clears the attempt count and schedules the next one, and sets no
  `loaded_at`;
- the internal certificate is `Valid`, needs no attention and has no ACME rows to render.

## Files changed

| File | What |
|---|---|
| `.github/workflows/rust.yml` | `ui` job only: new `cargo test (rustak-ui)` step after clippy, before `trunk build` |
| `docs/ci.md` | job graph line, the `lint`/`test` bullet, the `ui` bullet (what the step is, why the host target, what it costs), and `cargo test` in "Running the checks locally" |
| `README.md` | one line in the Development block: `cd rustak-ui && cargo test && cd ..` |
| `rustak-ui/src/fixtures/mod.rs` | `demo_flag` |
| `rustak-ui/src/fixtures/certificates.rs` | `status_for`, `files_status`, `internal_status`, `fetched`, five tests, header |
| `e2e/tests/settings.spec.ts` | new: the `files` card and the `acme` card, each asserted through `?demo` |

`rustak-ui/src/pages/settings_tls.rs` was edited and restored for the failing-assertion proof; it
is unchanged. No other file was touched.

## Exit checks

Run on 2026-09-19.

```
$ cd rustak-ui && cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.14s

$ cargo test
test result: ok. 51 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

$ trunk build
2026-09-19T16:35:04.341762Z  INFO ✅ success

$ cargo fmt --all --check      # rustak-ui is clean; the diffs it prints are other agents' crates
(no rustak-ui/src file named; verified with `rustfmt --check --edition 2024` on the three files)

$ ./scripts/check-file-length.sh
(clean)

$ cd e2e && npm run typecheck
> tsc --noEmit
(clean)
```

### Playwright

The suite needs a `rustak` binary with the UI **embedded**, and `rustak-server` did not compile
for about an hour of this session — another agent's in-flight `pki::acme` work
(`error[E0061]`/`E0631` in `runtime.rs`'s `http01_routes` call). Nothing here touched it; the
build was retried until it went green at 17:40 and the suite then ran against a binary carrying
this brief's bundle.

The first attempt, against the binary that was on disk at the time, failed `tls=files` for a
reason worth writing down: **the server embeds `rustak-ui/dist` at compile time**
(`include_dir!` in `rustak-server/src/web/ui.rs`), so a UI change is invisible to the e2e suite
until `rustak-server` is rebuilt, and an older binary quietly serves the older bundle. The failure
looked like a broken fixture and was not one — `grep -a "Waiting for the certificate files" target/debug/rustak`
was empty while the same grep over `rustak-ui/dist/*.wasm` matched. That ordering is already in
`e2e/scripts/start-server.mjs`'s header; this is the failure it prevents.

```
$ cd e2e && RUSTAK_E2E_CHROMIUM="…/chromium-1234/…/Google Chrome for Testing" npx playwright test
  ✓  31 [chromium] › tests/settings.spec.ts:25:1 › a listener waiting for its certificate files says so, and says which files (384ms)
  ✓  32 [chromium] › tests/settings.spec.ts:53:1 › an ACME listener whose orders keep failing shows the authority's own reason (369ms)
  35 passed (24.2s)
```

All four `?demo&tls=…` cards were additionally read out of the freshly built bundle directly
(a throwaway static server over `rustak-ui/dist` plus a headless page, no `rustak-server`
involved), which is how the stale-binary diagnosis above was confirmed: `files` renders the
subtitle, the "What the listener is waiting for." banner, the "Waiting for the files" pill and
both file rows; `internal` renders the dates and no button; `none` renders the plaintext warning
and nothing else.

## Backlog items this closes

- **`rustak-ui`'s unit tests are compiled but never run** (M2-14) — closed by the `cargo test`
  step in the `ui` job. 51 run, 0 excluded.
- **Demo mode can only show one TLS source** (M2-14) — closed by `?demo&tls=…`.

## Backlog items this leaves

- **Nothing in `rustak-ui` is tested through a DOM.** The 51 tests are all pure functions, which
  is why the host target runs them all; what a component *renders* is covered only by the
  Playwright suite, and only for the pages that suite visits. `wasm-bindgen-test` with
  `wasm-pack test --headless` is the mechanism when that becomes worth its install cost, and it
  can be added beside this step rather than instead of it. Found by M7-04.

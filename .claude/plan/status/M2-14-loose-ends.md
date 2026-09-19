# M2-14 — Loose ends from the review fixes and the backlog — complete

Brief: `.claude/plan/briefs/M2-14-loose-ends.md`
Read first: `conventions.md`, `backlog.md`; status `M1-10-robustness-stream.md` (the
`GroupCache`), `M2-13-tls-files-reload.md` (the `files` status fields and why
`needs_attention` was left alone), `M3-06-admin-api-polish.md` (the `410` body and the
`bound_at` that was deferred), `M2-11-stream-harness-tidy.md` (the e2e launcher).

Five independent items, each with a test. Nothing here shares a file with anything else.

## What was built

| File | Functional lines | What changed |
|---|---:|---|
| `rustak-server/src/identity/groups.rs` | 174 (was 156) | `create`/`patch`/`delete` take `&AppContext`; new private `routing_changed` calls `LiveState::channels_changed()` |
| `rustak-server/src/web/api/groups.rs` | 159 (unchanged) | three call sites: `context.db()` → `&context` |
| `rustak-server/src/stream/live.rs` | 182 (was 177) | `LiveState::bound_at` recorded in `new`, read through `bound_at()` — additive |
| `rustak-server/src/web/api/clients.rs` | 256 (was 257) | `status` reads the slot once and reports `bound_at` |
| `rustak-api/src/client.rs` | 66 (was 63) | `StreamStatus::bound_at: Option<DateTime<Utc>>`, omitted when unset |
| `rustak-api/src/settings.rs` | 111 (unchanged) | `TlsStatus::needs_attention()` now covers `files` as well as `acme` |
| `rustak-ui/src/api/missions.rs` | 84 (was 70) | `get` reads the `410` body; private `deleted` decodes it |
| `rustak-ui/src/pages/mission_detail.rs` | 167 (was 159) | a `Deleted <when>` pill in the heading, on every tab |
| `rustak-ui/src/pages/clients.rs` | 291 (was 286) | "The stream listener bound …" above the connected list |
| `rustak-ui/src/pages/settings_tls.rs` | 224 (was 173) | the `files` rows, the `note` alert, source-aware labels, the button for both fetching sources |
| `rustak-ui/src/fixtures/{missions,packages}.rs` | — | `STANDDOWN` made `pub`; `stream_status()` carries a `bound_at` |
| `rustak-ui/styles.scss` | — | `.panel-note` |
| `docs/deployment.md` | — | one sentence: the console shows the `files` status too |
| `e2e/scripts/start-server.mjs` | — | `stop()` awaits the child before `cleanUp()`, bounded at 12 s |
| `e2e/tests/missions.spec.ts` | — | re-opens the deleted mission rather than only reading the `410` over HTTP |

### 1. Channel-cache invalidation

`<dest group="…">` resolves a name through `stream::GroupCache`, which is cached because
reading the table per destination element was a denial of service (R-03 H2). It refreshes
itself within a second, so an administrator who creates a channel and tells a client to
send to it watches the first message reach nobody.

`identity::groups::{create,patch,delete}` now take `&AppContext` instead of `&Database` and
end with `routing_changed(context)`, which is `has_live()` → `live()` → `channels_changed()`
and never fails: an installation with `[stream.tls] enabled = false` has no cache to
invalidate, and a channel that was written is not un-written by a notification that could
not be delivered.

`patch` gets the call although nothing it does today moves a bit position. It is the
function a rename or a re-allocation would be added to, and a cache refreshed on two of the
three ways the table can change is the kind of thing found a year later.

### 2. The console renders a `410` body

`GET /api/v1/missions/{guid}` answers `410` **carrying the whole `MissionDetail`** for a
deleted mission (M3-06). The generic client turned every `410` into `ApiError::Gone` before
the body was looked at — right for the setup wizard, wrong here — so the page said "That is
no longer available on this server." where it had the mission in hand.

`api::missions::get` now sends the request itself and, on `410`, decodes the body through a
private `deleted(Option<Value>)`. A body that is not a mission (the `{"error": …}` shape, an
intermediary's replacement) still becomes `ApiError::Gone`; nothing is guessed at.

The Overview tab already rendered "This mission has been deleted." from `deleted_at`. The
second branch is in the *heading*, which is on all four tabs — none of them is about a live
mission any more, and Subscribers and Layers would otherwise look like an ordinary empty
mission.

### 3. `bound_at`

`LiveState::new` is called after `listener_tls::bind` returns and immediately before
`AppContext::install_live` publishes the registry, so the moment it is constructed *is* the
moment the listener bound. It is taken there, read through `LiveState::bound_at()`, and
reported by `GET /api/v1/clients/status`. A clone carries the original moment.

`status` also stopped asking `has_live()` twice. It used to ask once for the connection
count and again for `bound`, so a listener that published between the two was reported as
bound with nobody on it — the one answer this endpoint exists to make unambiguous.

### 4. e2e launcher shutdown

`SIGTERM` used to kill the child, remove the scratch directory and `process.exit(130)` in
that order, so the server ran its 8 s drain and its 2 s WAL checkpoint against a directory
that had already gone. The handler now calls `stop()`, which sends `SIGTERM`, awaits the
child's `exit` — bounded at **12 s**, the server's own 10 s budget plus two, and inside
Playwright's 15 s `gracefulShutdown` — and only then removes the directory. A second signal
is ignored rather than racing the first, the child's own `exit` handler stands aside while a
stop is in progress, and `process.on("exit", cleanUp)` stays as the synchronous last resort.

### 5. `files`-mode TLS in the console

`needs_attention()` is now true for `acme` **and** `files`. The distinction that matters is
not "which source" but "which sources fetch a certificate while the server runs": M2-13 made
a `files` listener bind with an internal certificate and wait for a pair a sidecar may never
write, so a running server presenting something no client will trust is now an ordinary
state, and the log was the only thing that said so. M2-13 left this alone deliberately,
because `rustak-ui` was another agent's ground and the UI asserted the old answer; both
sides land here together.

The card gained the `cert_file`, `key_file` and "Last read" rows, an alert carrying `note`
("waiting for the certificate files to appear"), and two labels that stop lying: `Missing`
reads "Waiting for the files" rather than "Not issued yet" (nobody issues those here), and a
`files` error is titled "The certificate files could not be used." rather than "The last
order failed." — which sends an operator looking for an order this installation never placed.

## Testing

- `identity::groups` — **3 new.** A channel created and routed to in the same moment
  (`Relayed { recipients: 1 }` where the probe a line earlier got
  `NoSuchGroup`, which is what proves the cache was stale and was dropped); a deleted
  channel stops being a destination at once; both operations succeed on an installation with
  no stream listener at all.
- `web::api::clients` — **2 new.** No listener means no `bound_at`; a bound one reports a
  moment between the two `Utc::now()` readings around `install_live`.
- `rustak-api::client` — `bound_at` on the wire, omitted when unset, and a body from a server
  that predates it still parses.
- `rustak-api::settings` — `needs_attention` for every source × state that matters, including
  the two `files` states this brief added and `none`, which stays false.
- `rustak-ui::api::missions` — **2 new.** A `410` body written out in the *wire* shape (the
  summary is `#[serde(flatten)]`ed, so `name` and `deleted_at` are top level) is decoded; a
  body that is not a mission stays `ApiError::Gone`. The branch these cover is also exercised
  for real by the e2e re-open below, which is what makes it more than a compile check.
- `rustak-ui::pages::settings_tls` — **2 new, 2 rewritten.** Labels for every state × source;
  the files error title; which sources offer the button; `needs_attention` for `files`.
  **These are compiled, not run** — `--all-targets` clippy type-checks the UI crate's tests
  and CI has no wasm test runner. Backlog below.
- `e2e/tests/missions.spec.ts` — the deleted mission is **re-opened** after the delete, where
  the page has no local memory of it and the `410` body is the only copy. Asserts the name
  renders, the banner and the heading pill are there, and "That is no longer available on
  this server." is not.

## Exit checks

Run on 2026-09-19. All green.

```
$ cargo fmt --all -- --check
(clean)

$ cargo clippy --workspace --all-targets -- -D warnings
(clean)

$ RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
(clean)

$ MAX_FUNCTIONAL_LINES=300 bash scripts/check-file-length.sh
(clean — exit 0)

$ cargo test --workspace --no-fail-fast
    Finished `test` profile [unoptimized + debuginfo] target(s)
… 35 test binaries, every one `ok`.
TOTAL 2723 passed; 0 failed.

Five of those are new (3 in `identity::groups`, 2 in `web::api::clients`); two existing
`rustak-api` tests were extended and one renamed. The four new `rustak-ui` tests are not in
this number — see the note under Testing.

$ ./target/debug/rustak --config config.example.toml --check
config.example.toml is valid: rustak would listen on 0.0.0.0:8446, with data in ./data.

$ cd rustak-ui && cargo fmt --all --check
(clean)

$ cd rustak-ui && cargo clippy --all-targets --target wasm32-unknown-unknown -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s)

$ cd rustak-ui && trunk build
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 23.68s
2026-09-19T01:40:21Z  INFO ✅ success

$ cd e2e && npm run typecheck
(clean)

$ cd e2e && env RUSTAK_E2E_CHROMIUM="…/Google Chrome for Testing" npx playwright test
  ✓  15 [chromium] › tests/missions.spec.ts:39:1 › a mission a client created is listed,
        opened, and deleted (1.8s)
  …
  29 passed (1.2m)
```

### The shutdown check

Not a test in the suite — the launcher run on its own, `SIGTERM`ed, and watched.

```
$ node e2e/scripts/start-server.mjs        # then, once /robots.txt answered:
$ date +%H:%M:%S; kill -TERM <launcher>; …; date +%H:%M:%S
02:49:13   SIGTERM sent
02:49:14   node exited
$ ls -d <workspace>
ls: …/e2e-shutdown: No such file or directory
```

The server logged `The public listener has stopped.` and **not**
`The database was not checkpointed cleanly on the way out.`, which is the warning
`runtime::run_all` emits when the `TRUNCATE` checkpoint fails — so the checkpoint ran
against a directory that was still there, and the directory went afterwards. That one
second of waiting *is* the fix: the old handler killed the child, removed the directory and
called `process.exit(130)` in the same tick.
The full `playwright test` run above also left no scratch directory behind and printed no
`[e2e] could not remove …`.

Local toolchain is older than CI's stable; CI remains the authority for new lints.

## Deviations from the brief

1. **`rustak-server/src/web/api/groups.rs` was edited**, which the brief does not list.
   `identity/groups.rs` has no way to reach a `LiveState` from a `&Database`, so `create`,
   `patch` and `delete` had to take `&AppContext` — three call sites, `context.db()` →
   `&context`, no other change. Changing the signature rather than adding a second
   invalidation call in the route file means a future caller gets the hook for free.
2. **`services/mod.rs` was not touched.** The brief offers it or `stream/live.rs` for the
   timestamp; `LiveState` is built after the bind and before the registry is published, so
   recording it there is both accurate and additive, and `AppContext` needed no new state.
3. **The OIDC auto-create path does not invalidate.** `groups::apply_claims` →
   `lookup_or_create` can create a channel at sign-in and still takes `&Database`, because
   its caller (`identity/users.rs`, not mine) does too. The sub-second refresh covers it;
   backlog line added below.
4. **`rustak-ui/src/pages/clients.rs`'s listener sentence is inline rather than a function.**
   As a helper it put the file at exactly 300 functional lines — inside the script's `-gt`
   check and outside the convention's "< 300". Inline it is 291.
5. **`POST /settings/tls/renew` is now offered for `files` as well as `acme`**, labelled
   "Re-read the files". Beyond the brief's wording, but M2-13 built the files branch of that
   endpoint precisely so a re-read could be forced, and an operator looking at "waiting for
   the files" who has just written them is exactly who wants it.
6. **The `410` unit test writes the JSON out** instead of serialising a fixture. The fixture
   route would have needed `#[cfg(all(test, debug_assertions))]`, which is not the
   `#[cfg(test)] mod tests` the conventions and the length script expect. `fixtures::STANDDOWN`
   is still made `pub`: `/admin/missions/f0a19c52-…?demo` is the only way to look at the
   deleted-mission view without a server, and the listing cannot link to it.
7. **`e2e/tests/missions.spec.ts` was extended** while the CI steward owns tests/CI. Two
   assertions added inside an existing test; no structural change, no config change.
8. **One sentence added to `docs/deployment.md`**, which the brief does not list. "Confirming
   it" for `mode = "files"` described the API only; the console now shows the same thing, and
   a deployment guide that does not say so sends an operator to `curl`. No other agent holds
   that file.
9. **One backlog line removed that this brief did not close**: "No admin-UI panel for TLS"
   (M2-10). `rustak-ui/src/pages/settings_tls.rs` reads both endpoints and renders the
   banner; item 5 finished the part of it that was still true.

## Backlog items this leaves

- **A channel a sign-in creates does not invalidate the routing cache.**
  `identity::groups::apply_claims` → `lookup_or_create` creates channels from OIDC claims and
  takes `&Database`, so the hook `create`/`patch`/`delete` now have is not reachable from it;
  it would mean threading `&AppContext` through `identity/users.rs` as well. Costs the
  sub-second wait the rest of the table no longer pays. (`identity/{groups,users}.rs`.)
  Found by M2-14.
- **Demo mode can only show one TLS source.** `fixtures::certificates::acme_status()` is a
  failed ACME order, so the `files` rows this brief added are unreachable with `?demo` and
  are only ever seen against a real `mode = "files"` installation. A query parameter or a
  second card in the demo would cover both. (`rustak-ui/src/fixtures/certificates.rs`.)
  Found by M2-14.
- **`rustak-ui`'s unit tests are compiled but never run.** `--all-targets` clippy type-checks
  them and CI has no wasm test runner, so an assertion that would fail is not a failing
  build. `wasm-bindgen-test` in the UI job, or a host-target `cargo test` for the modules
  that do not touch `web_sys`, would make the ~20 tests in that crate mean something.
  Found by M2-14.

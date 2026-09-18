# CI-01 — CI steward: running log

Append newest first. One entry per run I acted on: run id, what failed, what I did,
what remains. Brief: `.claude/plan/briefs/CI-01-ci-steward.md`.

---

## 2026-09-18 — nightly 35394055984 (`c61b592`): CloudTAK's **first run** — 7 passed, 0 skipped, 2 failed

Dispatched so `interop-cloudtak` would get its first real run. **The whole stack
came up**: Postgres, `cloudtak-api:v13.89.0` and a rustak built from the
checkout, on one compose network, in **10m52s** — inside M4-03 §4's 10–15 minute
estimate and well inside the 60-minute bound. `[cloudtak] surfaces missing:
(none)`, so nothing skipped, and 40/40 of the suite's own unit tests passed.

Six of the eight API steps plus `package` passed: `configure-server`, `login`,
**`channels`**, `data-sync`, `marker`, `changes`, `package`.

**M4-03 §4's most likely failure did not fire.** `channels` — the per-device
active-channel state against CloudTAK's missing `clientUid`, prediction #1 and
the one flagged as "the assertion I would relax first" — **passed**. So did the
bind-mount uid (#4). Two others did fail, one of them predicted.

### F6 — `ui-smoke`: the login card has two "sign in" buttons (landed-ready)

M4-03 §4 prediction #3, near enough:

```
locator.click: Error: strict mode violation:
  getByRole('button', { name: /sign in/i }) resolved to 2 elements:
  1) <button type="submit" class="btn btn-primary w-100"> Sign In </button>
  2) <button type="button" class="btn btn-secondary …"> Sign in with … </button>
```

Not a missed selector — a loose one. The card carries the submit button and an
SSO button, and both accessible names match `/sign in/i`. **Fixed** in
`interop/cloudtak/src/smoke.ts`:
`getByRole("button", { name: "Sign In", exact: true })`.

### F7 — `file`: CloudTAK's router ate the body before its own handler could forward it

This one was **not** predicted, and the failure pointed at the wrong component:

```
POST /api/marti/missions/{guid}/upload?name=interop-notes.txt answered 400:
{"status":"BAD_REQUEST","code":2,"message":"Invalid Request: HTTP request body has no content."}
```

That message is **rustak's own** — `rustak-server/src/marti/sync.rs`, a faithful
port of TAK Server's `UploadServlet` — so it reads as a rustak rejection. It is
not. rustak was told the truth: nothing arrived.

Reading CloudTAK's source settles it. Its handler streams the *request itself*
onward:

```ts
// api/stateless/routes/marti-mission.ts, POST /marti/missions/:guid/upload
const content = await api.Files.upload({
    name, contentLength: Number(req.headers['content-length']), …
}, req);                                    // ← the live stream
```

and its router is `@openaddresses/batch-schema`, which installs four body
parsers before any route runs (`lib/schema.ts:219`):

```js
bodyparser.urlencoded(…)
bodyparser.json(…)
bodyparser.text({ type: ['text/*', 'application/xml', 'application/*+xml'] })
bodyparser.raw({  type: ['application/octet-stream'] })      // ← ours
```

`bodyparser.raw` drains the stream into `req.body`, so the handler pipes an
**exhausted** `req` and forwards zero bytes. Our `Content-Type:
application/octet-stream` chose the one type guaranteed to be swallowed.

**Fixed:** `uploadFile` now sends **no `Content-Type`**, which matches none of
the four parsers, so the stream survives to the handler; `fetch` still sets
`Content-Length` from the buffer, which is the only header the handler reads.
`interop/cloudtak/src/client.ts` no longer defaults a raw body to
`application/octet-stream` either — that default was the trap — and sets the
header only when a call asks for one. (CloudTAK's own UI avoids this the other
way: its `Upload` component posts the browser `File`'s type, `application/zip`
for a data package, equally unparsed. Either works; ours does not have to claim
a text note is a zip.)

Files: `interop/cloudtak/src/missions.ts`, `src/client.ts`, `src/smoke.ts`,
`tests/missions.test.ts`. Verified: typecheck clean, **40/40**.

**Also in this run:** the EUD job was 8 passed / 1 failed — only `mp-download`,
whose fix was not yet landed. Everything else held.

---

## 2026-09-18 — `main` green on `0198065`; nightly 35392357088: **8 passed, 0 skipped, 1 failed**

**`rust.yml` 35390806681 (`0198065`) — green, all 24 jobs.** `1569 passed; 0
failed`, including `a_certificate_yields_the_identity_the_handshake_proved ...
ok` — the 1-in-128 serial defect simply did not roll this time, which is what the
write-up predicts. `enroll_flows` 10/10 in 3.48 s, so §F4 holds. `Test` back to
11m52s from 13m13s.

**The EUD suite is one scenario away from complete.** `enroll-revoked` **✔** —
§F3's post-revocation snapshot works, and it is now asserting the thing the
server was already doing right. `chat-direct`, `disconnect`, `enroll-basic`,
`mp-upload`, `negotiate-refused`, `negotiate-silent`, `two-eud-routing` all ✔.

### mp-download: the server is fixed, the scenario was wrong

**M3-05's HTTP/2 `:authority` fix works.** The URL rustak now hands the peer is

```
completed upload to server! 1282 bytes uploaded,
URL is https://127.0.0.1:8443/Marti/sync/content?hash=e661814544d5…
```

against `https://rustak-interop-eud-mp-download/…` before it. That closes
`CI-01-2026-09-18-mission-package-url.md`.

The remaining failure is mine, and the artefacts settle it in one line. From
`artifacts/mp-download/bravo/commo-log.txt`:

```
Receive of MP /work/payload.dat from MPDL-ALPHA requested
  - assigned output file /work/mprx-0-/work/payload.dat
Download … failed; response code = 23 (Failure writing output to destination,
                                        passed 1052 returned 0)
```

`23` is `CURLE_WRITE_ERROR`, and "passed 1052 returned 0" means **curl was handed
all 1052 bytes and the write callback refused them** — the transfer succeeded and
the *file* could not be opened. The reason is the path: the name ALPHA sends is
the name BRAVO receives, and BRAVO builds its output as
`mprx-<id>-<that name>` inside its mounted directory, so the scenario's
`mpsend:/work/payload.dat:…` produced `/work/mprx-0-/work/payload.dat`, whose
`/work/mprx-0-/` directory does not exist.

(The 1282/1052 difference is not a discrepancy: 1282 is what ALPHA uploaded
including the multipart envelope, 1052 is the stored resource. rustak served
exactly what it holds.)

**Fixed:** `interop/eud/scenarios/mp-download.toml` now sends
`mpsend:payload.dat:EUD-MPDL-BRAVO`. The container's `WORKDIR` is `/work`
(`interop/eud/Dockerfile:110`), so a bare name resolves for the sender and stays
writable for the receiver. `mp-upload`'s `smpsend:/work/payload.dat:0` is
untouched and still correct — nothing receives that one, so no output path is
derived from it.

Verified: typecheck clean, 43/43. **This should be the ninth green scenario.**

### Note: the server's request log is not a witness for these routes

Worth recording, because it cost time here. Every `request{…}` line in the job
output is a *handler event* rendered inside the request span — so a route whose
handler emits nothing at INFO leaves no trace at all. `/Marti/sync/content` and
`/Marti/sync/missionupload` are both such routes, which is why the log showed
zero sync requests during a scenario that uploaded and served a file
successfully. For these scenarios the EUD-side artefacts are the evidence, not
the server log.

---

## 2026-09-18 — wave A, run 35390132588 (`36c1411`): red on `Lint` and `Test`

Seven commits: ACME, the admin API, the operations UI, the HTTP/2 `:authority`
fix plus p256 0.14, OAuth2/OIDC federation and the services API. **Everything
expensive passed** — `End-to-End Tests` (29 new Playwright specs),
`Interop: node-tak`, `Build UI`, all ten `build` matrix jobs, all four
`docker-build` and both `docker-publish`. Two jobs failed, for unrelated reasons.

**`Lint` — two unused imports.** §F5 in the previous entry; fix ready, two line
deletions.

**`Test` — one library test out of 1569**, and it is **not a wave A regression**:

```
---- pki::tls::peer::tests::a_certificate_yields_the_identity_the_handshake_proved ----
assertion `left == right` failed: a 128-bit serial
  left: 30    right: 32
test result: FAILED. 1568 passed; 1 failed; 2 ignored
```

`pki::issue::random_serial` clears the serial's top bit, so its leading byte is
uniform over `0x00..=0x7f` and is **zero once in 128**. DER integers are minimal,
so such a serial is encoded in 15 bytes — and `pki/tls/peer.rs:62` hex-encodes
`raw_serial()` (15 bytes → 30 chars) while `pki/issue.rs:207` hex-encodes the
array it generated (16 bytes → 32 chars). The two spellings of the same
certificate's serial disagree, and the audit trail records the short one
(`auth/cert.rs:161`). Authentication and revocation are unaffected — both key off
the fingerprint.

**Written up in `CI-01-2026-09-18-serial-hex-der-minimal.md`**, with the padding
fix and the deterministic test that would pin it. **The failing test has been
left red and not relaxed**: it is asserting a true invariant that the product
violates, which is exactly the case the brief says to write up rather than edit.
Expect this to reappear about one run in 128 until the fix lands; a re-run is a
fair response to seeing it, but it is a defect and not a flake.

---

## 2026-09-18 — nightly 35388998399 (`8b5a31c`): **7 passed, 0 skipped, 2 failed**, in 15m08s

No hang, no skips, all nine scenarios ran. **15m08s** end to end, which both
validates the three fixes from the previous entry and confirms the new
`timeout-minutes: 30` is the right bound.

| Scenario | Result | Note |
|---|---|---|
| `chat-direct` | **✔ — first pass ever** | M1-08's `b-t-f-s` bounce landed and ATAK reads it. This was M2-09 §4's one predicted standing failure, and it is closed. |
| `disconnect` | ✔ | holding since F1 |
| `enroll-basic` | ✔ | |
| `mp-upload` | ✔ | |
| `negotiate-refused` | ✔ | |
| `negotiate-silent` | ✔ | |
| `two-eud-routing` | ✔ | |
| `enroll-revoked` | ✖ | the *harness* — §F3 below. The server did everything right. |
| `mp-download` | ✖ | **expected**: the HTTP/2 `:authority` fix is `e2e77a9`, which is newer than the `8b5a31c` this ran on. |

### F3 — `client_endpoints_absent` was asked of the wrong data (landed-ready)

`enroll-revoked` ran to completion for the first time — `[alpha] revoked at
T+25s`, so the switch to `POST /api/v1/certificates/{id}/revoke` works — and
then failed on one assertion: `/Marti/api/clientEndPoints lists 'REVOKED-ALPHA',
which it should not.`

**The server is not at fault; it is the best result in this run.** Its own log:

```
20:08:34.913  POST /api/v1/certificates/{id}/revoke
20:08:34.915  pki.revoke{fingerprint=7405…}: stream::notify: Closed stream connection
20:08:34.913  stream.conn{user=eud-revoked-alpha}: A client left the stream. rx=23 tx=3
20:08:50.118  pki::tls::client_verifier: Refused a client certificate at the handshake. fingerprint=7405…
20:09:05.659  … refused again
20:09:21.200  … and again
```

Both halves of what M2-09 §4 called "uncertain" hold: revocation drops the live
session by fingerprint, **and** the stream listener re-checks revocation on every
new handshake rather than only against the live set. Three reconnection attempts,
three refusals.

The failure is mine. `sample()` accumulates every callsign ever seen into one
`Set`, and `checkClientEndpoints` was given that union for both directions.
That is right for `client_endpoints_present` — "did it get as far as connecting"
— and cannot ever be right for `client_endpoints_absent`, because an EUD that is
revoked mid-scenario *has* to connect first and is therefore in the union by
construction. The assertion could only ever fail.

**Fixed:** the revocation task now takes a reading of `/Marti/api/clientEndPoints`
five seconds after the revoke returns — after it, and while the EUD is still
running, which is the only moment at which "no longer connected" means anything —
and `checkClientEndpoints` judges `absent` against that snapshot while `present`
still reads the union. A scenario with no revocation falls back to the union,
where the two are the same question. The five seconds are because the revoke
response returns once the hook has closed the connection, but the listener's view
of who is connected updates on the connection task.

Files: `interop/eud/src/execute.ts`, `interop/eud/src/expect.ts`,
`interop/eud/tests/expect.test.ts` (new case: *a revoked EUD is judged on the
reading taken after it was revoked*, plus the reworded message in the existing
one). Verified: typecheck clean, **43/43**.

### F4 — `rustak-server/tests/enroll_flows.rs`: the `reserve_port` TOCTOU (landed-ready)

M6-01 traced a flake under `cargo test --workspace` to `reserve_port()`, which
bound `:0`, read the port and **closed the socket immediately** — leaving the
port unclaimed for the whole of `TestServer::start_with` plus `Pki::load`,
hundreds of milliseconds under load, during which any other test binding `:0`
could take it.

`build_marti` binds from the configuration and returns an already-`run()`
`Server`, so a test cannot hand it a pre-bound listener or read the port back
without changing `src/web/server.rs`. Closed from the test side instead, in two
parts: `reserve_port` now **returns the listener still bound**, and the caller
drops it in the instruction immediately before `build_marti` binds — reducing
the window from hundreds of milliseconds to two instructions — and `harness()`
retries on a fresh port up to `BIND_ATTEMPTS` (5) if even that is lost, so the
suite cannot flake at all. `try_harness` returns `Result` so a bind failure is a
retry rather than a panic; every other `expect` is unchanged.

Verified: `cargo test -p rustak-server --features testing --test enroll_flows`
→ **10 passed**; rustfmt clean.

### F5 — two unused imports failing `Lint` on `main` (landed-ready, `src/`)

Wave A (`36c1411`) went in with `Lint` red:

```
error: unused import: `rustak_core::prelude::*`
  --> rustak-server/src/web/api/events.rs:40:5
  --> rustak-server/src/web/api/services.rs:25:5
  = note: `-D unused-imports` implied by `-D warnings`
```

Both files import `rustak_core::prelude::*` **and** `crate::prelude::*`, and
`rustak-server/src/prelude.rs:27` is `pub use rustak_core::prelude::*;` — so the
first is redundant. (The other three files in `web/api` that import the core
prelude — `subject.rs`, `error.rs`, `extract.rs` — do not also import the crate
prelude, which is why only these two errored.)

These are non-test `src/` files, which I do not normally edit. Applied under the
brief's one-line-lint exception and flagged here because it is two line
deletions with no behavioural change and `main` is red without them: if the
owning agent is mid-edit and about to *use* the core prelude in either file, take
their version over mine.

---

## 2026-09-18 — nightly 35379867680: **cancelled at the 90-minute timeout**, and why

Dispatched at 18:23 on `1e5e1c3`; killed by its own `timeout-minutes: 90` at
19:53. It was **not** a slow run — the runner *died* at 18:31:44, nine minutes
in, and the job then sat for 82 minutes with nothing happening.

**What the run proved before it died:**

- `[eud] surfaces missing: (none)` — `certificateRevocation` now answers, so
  `enroll-revoked` ran for the very first time.
- **`disconnect` ✔ — F1 works.** The scenario passes.
- **`chat-direct` ✖ with one failure instead of two — F1 works.** It now reports
  only `[alpha] commo-xml.txt has no event with type=b-t-f-s`, the real standing
  gap; the spurious `the EUDs ran for 37s, short of the 60s` is gone.
- `enroll-basic` ✔.
- `mp-upload`, `mp-download`, `negotiate-refused`, `negotiate-silent` and
  `two-eud-routing` never ran — the process was gone by then.

### The failure chain

```
Error: DELETE /api/v1/credentials/1 answered 404: {"error":"That credential has already gone."}
    at revokeEud (interop/eud/src/session.ts:221:11)
    at async <anonymous> (interop/eud/src/execute.ts:77:9)
Node.js v24.20.0
…
2026-09-18T19:53:40Z Terminate orphan process: pid (13061) (rustak)
```

Three defects, one behind the other. All three are in `interop/**` and mine.

**1. `enroll-revoked` revokes the wrong thing.** M2-09 §5.2 recorded the
deviation — the runner revokes the *credential* rather than the certificate,
"switching to a per-certificate revoke is one function in `src/session.ts`" — and
the first real run shows the deviation does not work. The credential is a
**one-time enrolment token**: by the time the EUD is connected and there is
something worth revoking, the token has been spent, so
`DELETE /api/v1/credentials/{id}` finds nothing to revoke and answers `404`.
(The server is behaving correctly. `credentials::remove` returns `404` when
`credentials::revoke` reports `false`, which is the right answer to "revoke this
already-spent token".) **Fixed:** `revokeEud` now lists
`GET /api/v1/certificates?username=…&state=active&kind=client` and calls
`POST /api/v1/certificates/{id}/revoke` with `reason: "device_lost"` — the
endpoint the scenario's `certificateRevocation` surface is *named* for, which
M2-08 landed.

**2. A rejected promise nobody was watching killed the runner.** `execute.ts`
builds the revocation promises at line 73 — where an `async` body starts running
at once — and does not `await Promise.all(revocations)` until line 106, after
every container has exited. So `revokeEud`'s rejection spent ~50 s as an
unhandled rejection, which node treats as fatal. That skipped the `finally` that
calls `session.stop()`. **Fixed:** a failed revocation is recorded in `failures`
— it *is* a scenario failure — so the promise never rejects and the scenario
reports it properly.

**3. The orphaned server held the job's stdout open for 82 minutes.** This is
the one that turned a nine-minute failure into a ninety-minute one.
`interop/shared/src/launch.ts` spawns rustak with
`stdio: ["ignore", "inherit", "inherit"]`, which is what makes the server log
readable in the job output — and means a runner that dies without calling
`stop()` leaves the child holding that pipe. The CI step pipes through `tee`,
`tee` never sees EOF, the step never returns. The runner's cleanup even names it:
`Terminate orphan process: pid (13061) (rustak)`. **Fixed:** the launcher now
registers a `process.on("exit")` handler that `SIGKILL`s the child, removed again
by `stop()` on the normal path. `exit` fires for a clean exit, an uncaught
exception and an unhandled rejection alike, and `kill` is synchronous, which is
all such a handler may be. This also protects `interop/node-tak`, which uses the
same launcher.

### Files changed (landed-ready)

- `interop/eud/src/session.ts` — `revokeEud` switched to the per-certificate
  endpoint.
- `interop/eud/src/execute.ts` — revocation failures recorded, not thrown.
- `interop/shared/src/launch.ts` — the child dies with the runner.

Verified: `interop/eud` typecheck clean, **42/42** unit tests; `interop/node-tak`
typecheck clean, **25/25** (and note those are 25 *passing* now — the three
`TODO(M4)` mission-API skips M2-09 §5.7 recorded have started running and pass,
since M4 landed).

**I have not re-dispatched the nightly.** These fixes are not on `main` yet, and
a dispatch before they land would reproduce the identical 90-minute hang and burn
a runner for nothing. Ready to dispatch the moment they are in.

**Note on the timeout.** `timeout-minutes: 90` is correct as an upper bound and I
am not proposing to change it — but this job's real envelope is ~15 minutes
(13m20s for all nine scenarios on the first run), so a tighter bound would have
surfaced this in a quarter of the time. Worth considering once the suite's
runtime is settled.

---

## 2026-09-18 — run 35379830824 (`1e5e1c3`): green, second in a row

`docs: Add the HTTP/2 authority and p256 bump brief`. Every job succeeded; the
only non-success in the run is `Update Homebrew Tap`, correctly skipped because
this is not a release. So `main` has now been green twice consecutively, which
is what makes 35379781633 a fix rather than a lucky run.

No pushes to `main` since. Nothing further queued at 18:46 UTC.

---

## 2026-09-18 — run 35379781633 (`973117a`): **`main` is green, every job**

The first fully green `rust.yml` run. All 25 jobs succeeded — `Lint`, `Test`,
`Build UI`, `e2e`, `interop-node-tak`, all ten `build` matrix jobs, all four
`docker-build`, both `docker-publish`, and the `ci` aggregator; `tap` correctly
skipped (not a release). Coverage uploaded through `codecov-action@v7.0.0`.

`tests/bootstrap.rs`: **3 passed, 0 failed, finished in 30.81 s.**

That number is the whole story of F2. The old `STARTUP_TIMEOUT` was 30 s and the
suite needs **30.81 s** on this runner even after the P-256 change removed two of
the three RSA-2048 generations — so it was failing by under a second of margin,
and neither half of the fix would have been enough alone. The suite now has the
120 s budget against ~31 s of real work, and a start-up that *fails* no longer
burns any of it.

`Test` took **13m13s** against its 30-minute timeout (was 12m06s before these
three tests started passing rather than timing out at 30 s each — the increase is
the work they now actually do).

**Slow spots:** nothing near 15 minutes. `Test` 13m13s, then
`windows-amd64-rustak` 7m37s, `linux-arm64-rustak` 6m33s, `darwin-amd64-rustak`
6m09s, `darwin-arm64-rustak` 6m05s, `linux-amd64-rustak` 5m51s. No cuts proposed.

**The two runs this closes out.** 35377781744 (`5429eeb`) and 35377904509
(`ce0998d`) both finished red while F2 was being written, and both failed on
`bootstrap` and nothing else — `error: 1 target failed: -p rustak-server --test
bootstrap`, `1 passed; 2 failed` in each. So every red `main` run today traced to
that one file, with no second cause behind it. They predate the fix and are not
being re-run.

---

## 2026-09-18 — F1 and F2 landed; nightly re-dispatched

The coordinator landed all six files as `973117a` ("test: Report server start-up
errors in the bootstrap suite and fix two EUD scenario expectations"). Verified
byte-identical to what I wrote — `rustak-server/tests/bootstrap.rs`,
`interop/eud/{src/expect.ts,tests/expect.test.ts,scenarios/disconnect.toml,README.md}`
and `docs/ci.md`.

**Watching two runs:**

- `rust.yml` **35379781633** (`973117a`) — the landing itself, and the first run
  that should get a green `Test` job. **35379830824** (`1e5e1c3`, the M3-05 brief)
  is right behind it.
- `nightly.yml` **35379867680** — dispatched by hand with
  `gh workflow run nightly.yml --ref main` at 18:23 on `1e5e1c3`, so that the
  `disconnect` fix and the `chat-direct` runtime fix get exercised now rather than
  at 04:00, and so that `enroll-revoked` runs for the first time (the
  `certificateRevocation` surface it probes, `/api/v1/certificates`, landed with
  M2-08 *after* the sha the first nightly used). Expected steady state after
  these fixes: **7 passed, 2 failed** — `chat-direct`'s `b-t-f-s` bounce half
  (waiting on `M1-08-chat-bounce`) and `mp-download`'s URL (now M3-05, below) —
  or 8 passed if `enroll-revoked` is clean.

**Handed off, no longer mine:**

- The HTTP/2 `:authority` finding in
  `CI-01-2026-09-18-mission-package-url.md` is now brief **M3-05**, with an agent
  on it. That brief also carries the `p256` 0.14 bump, i.e. the
  `to_encoded_point` → `to_sec1_point` one-liner at
  `rustak-server/src/testing/authenticator/keys.rs:97` that Dependabot #3 needs.
  I will re-check `mp-download` and PR #3 once it lands.

**With the user, and not to be acted on by me:**

- The `security_audit.yml` ignore list (§S3). `docs/ci.md` now states the
  position factually; no workflow or `.cargo/audit.toml` change has been made.
- Posting the §S4 review findings as comments on Dependabot PRs #1–#3. The
  findings stay in this file until the user says otherwise. Nothing is merged or
  pushed.

---

## Standing state of CI (2026-09-18, 19:15 UTC+?)

| Workflow | On `main` | Verdict |
|---|---|---|
| `rust.yml` | **green** on `0198065`, all 24 jobs | the 1-in-128 serial defect remains latent and written up |
| `nightly.yml` `interop-eud` | **8 passed, 0 skipped, 1 failed** | only `mp-download`; fix ready (the scenario sent an absolute path the receiver could not write) |
| `nightly.yml` `interop-cloudtak` | **first run: 7 passed, 0 skipped, 2 failed** | stack came up in 10m52s, no skips; both failures harness-side and fixed (§F6, §F7) |
| `security_audit.yml` | **red, and has never been green** (8 of 8 recorded runs failed) | two advisories, neither fixable from this repository today — needs a decision, §S3 |
| `changelog.yml` | green | — |

---

## 2026-09-18 — run 35376447445 (`main`, `b802803`, "fix(test): Cache test key material…")

**The M0-21 landing did what it set out to do.** The `Test` job now **finishes**,
in **12m 06s** (17:48:10 → 18:00:16) against its 30-minute timeout, where it
previously had to be cancelled by hand. The library target ran **1215 tests in
140 s**. `Lint` passed, which confirms M0-21's blind `result_large_err` fix in
`marti/tls.rs` against CI's newer stable clippy. `Build UI`, `Interop: node-tak`,
`End-to-End Tests`, all ten `build` matrix jobs and all four `docker-build` jobs
passed; `docker-publish` ran; `tap` correctly skipped (not a release).

**What failed:** `rustak-server/tests/bootstrap.rs`, 2 of 3.

```
---- a_first_start_serves_its_own_tls_and_walks_an_operator_all_the_way_in ----
panicked at rustak-server/tests/bootstrap.rs:222:9:
/tmp/.tmpg7u7fV/pki/ca.crt was never written

---- the_insecure_development_listener_serves_plaintext_and_still_stops_cleanly ----
panicked at rustak-server/tests/bootstrap.rs:243:9:
the listener never came up: error sending request for url (http://localhost:45039/api/v1/health)
test result: FAILED. 1 passed; 2 failed … finished in 60.30s
```

Both are the file's own `STARTUP_TIMEOUT` (30 s) expiring. This suite is the one
that does **not** go through `TestServer`, so none of M0-21's three levers reach
it: it drives `rustak_server::run` directly, and a first start mints its own
RSA-2048 token-signing key (`auth::jwt::create_key`) and its own RSA-2048
certificate authority (`pki::ca`), plus an RSA server certificate in the TLS
case — none of them the cached `testing::keys::JWT_SIGNING_KEY`. The third test,
`a_listener_that_cannot_bind_reports_it_rather_than_running_without_one`, passes
because it has no deadline of its own: it just awaits `run` to its bind failure.

**Same two tests fail on the newest `main` too** — run 35377904509 (`ce0998d`),
`finished in 34.60s`, `error: 1 target failed: -p rustak-server --test bootstrap`
and nothing else in the job. So it is the file, not the commit.

### F2 — `rustak-server/tests/bootstrap.rs` (landed-ready, verified)

Three changes, all in that one file, which the brief gives me:

1. **`start()` now races the readiness wait against the server task.** This is
   the fix that mattered most, and it is a defect independent of the timeout:
   `start()` spawned `run` on a `JoinHandle` and then polled a file and a socket
   for 30 s *without ever looking at the handle*. A `run` that returned an error
   in the first second therefore produced "`…/pki/ca.crt` was never written" or
   "the listener never came up" — identical text for every possible start-up
   failure. My first local reproduction proved the point: it failed with those
   same two messages, and the real cause was a migration-numbering error from
   another agent's half-landed `0012`, which the third test happened to surface
   and the other two hid. A new `ready()` helper `tokio::select!`s (biased) on
   `&mut handle`, so a server that stopped, returned or panicked is reported with
   *its* error.
2. **`config.pki.key_type = KeyType::EcdsaP256` in `start()`.** `[pki] key_type`
   governs both the authority and the server certificate, so this removes two of
   the three RSA-2048 generations a first start does in the TLS test and one of
   two in the plaintext test. This suite is about the wiring — an authority that
   did not exist a second ago, a certificate issued from it, a client that trusts
   it — and not the key algorithm; `pki::ca` still asserts `Rsa2048` is the
   shipped default and that a default CA gets one (`ca.rs:675`, `:681`), and
   `pki::keys` exercises all three types, so nothing is left uncovered.
   `pki::testing::TestAuthority` has been P-256 for this reason since M0-21 §5.1.
3. **`STARTUP_TIMEOUT` 30 s → 120 s**, for the one RSA-2048 token-signing key a
   test file cannot avoid: `auth::jwt` is RS256 with `KEY_BITS = 2048` hard-coded
   and `load_or_create` has no seam a test can reach without editing `src/`. With
   (1) in place a *failed* start-up no longer waits this out, so the number now
   bounds only the honest slow case.

**Verified** with the CI configuration on a filtered target:

```
$ RUSTFLAGS=-Cinstrument-coverage cargo test -p rustak-server --features testing     --test bootstrap -- --test-threads=2
running 3 tests
test a_listener_that_cannot_bind_reports_it_rather_than_running_without_one ... ok
test a_first_start_serves_its_own_tls_and_walks_an_operator_all_the_way_in ... ok
test the_insecure_development_listener_serves_plaintext_and_still_stops_cleanly ... ok
test result: ok. 3 passed; 0 failed … finished in 26.49s

$ rustfmt --edition 2024 --check rustak-server/tests/bootstrap.rs      → clean
$ cargo clippy -p rustak-server --features testing --test bootstrap -- -D warnings
                                                                       → clean
```

(That 26.49 s is on this machine with `--test-threads=2`; CI's two vCPUs are
slower, which is what the 120 s headroom is for. The runner had not finished
start-up inside 30 s even with the RSA cost it no longer pays.)

**Later runs on `main`:** 35377781744 (`5429eeb`) and 35377904509 (`ce0998d`) were
still in `Test`/`build` at the time of writing, with `Lint`, `Build UI`,
`Interop: node-tak` and `End-to-End Tests` already green on `ce0998d`.

---

## 2026-09-18 — nightly 35376450618 (`main`, `b802803`) — first EUD scenario run

**`5 passed, 1 skipped, 3 failed`**, against M2-09 §4's predictions:

| Scenario | M2-09 expected | Actual | Verdict |
|---|---|---|---|
| `enroll-basic` | runs; pass plausible | **pass** | ATAK's own enrollment code works against rustak first time — the PKCS#12 truststore shape, the subject order and the Basic-auth path all held |
| `two-eud-routing` | pass | **pass** | as predicted |
| `negotiate-refused` | pass | **pass** | denial wording held |
| `negotiate-silent` | pass | **pass** | 60 s timeout line held |
| `mp-upload` | uncertain — the `!ECDH` gate | **pass** | the `[ports]` pinning was right |
| `enroll-revoked` | runs | **skipped** | *correct*: `certificateRevocation` probes `/api/v1/certificates`, which M2-08 landed **after** `b802803`. Not a regression — the probe doing its job. It will run on the next nightly. |
| `chat-direct` | **fail — expected, and the point** | **fail** | exactly as written up: `[alpha] commo-xml.txt has no event with type=b-t-f-s`. The delivery half passed (31 SA events + a `t-x-d-d` received). Standing failure until `M1-08-chat-bounce` lands. |
| `disconnect` | pass | **fail** | **the server half passed** — bravo's `t-x-d-d` with `link_uid = EUD-GONE-ALPHA` was asserted and found. Both failures were client-log expectations the harness had wrong. Fixed, §F1. |
| `mp-download` | uncertain | **fail** | **a real server bug.** Written up in `CI-01-2026-09-18-mission-package-url.md`. |

M2-09 §4's stated risk for `disconnect` — "`remiface:0` … if it is wrong ALPHA
never disconnects and both halves fail together" — did **not** happen: ALPHA
disconnected, BRAVO was told, and the `<link>` named the right device.

### F1 — harness fixes for `disconnect` and `chat-direct` (landed-ready)

Files changed, all mine under the brief:

- **`interop/eud/scenarios/disconnect.toml`** — ALPHA asserted `Interface Down`
  after `remiface:0`. `remiface` takes the interface *out of the stack* rather
  than bringing it down, and by `quit` there is no interface left to take down
  either, so that line is never logged. ALPHA does log
  `Contact Removed: EUD-GONE-BRAVO`, which is the client-side evidence its stream
  went away, so that is what it asserts now. BRAVO asserted
  `log_any = ["Contact Added", "Contact Removed"]`; `log_any` means "in any
  order", not "any one of", so this asked BRAVO to age ALPHA out of its contact
  list within the 30 s it stays up — `commotest`'s own staleness timer, which
  this scenario does not control. Now `["Contact Added"]`. **What the scenario
  proves is unchanged**: BRAVO's `xml_present = [{ type = "t-x-d-d", link_uid =
  "EUD-GONE-ALPHA" }]` is the assertion this scenario exists for, and it passed.
- **`interop/eud/src/expect.ts`** — `checkRuntime` measured `min_runtime_seconds`
  against the **shortest**-running EUD. `chat-direct` scripts BRAVO to quit at
  T+35 *on purpose*, so that ALPHA can speak into the gap at T+50 — and the guard
  read that deliberate departure as a run cut short
  (`the EUDs ran for 37s, short of the 60s…`) on top of the real `b-t-f-s`
  failure. Now measured against the longest, which is the scenario's own clock.
  Nothing is lost: an EUD that died before its script finished is caught by the
  timeout kill (`docker.ts` → `timedOut`) and by its own per-EUD expectations.
- **`interop/eud/tests/expect.test.ts`** — a new case,
  `an EUD scripted to leave early does not make the scenario a short run`.
- **`interop/eud/README.md`** — the two semantics now documented in the schema
  section: `log_any` relaxes order, not count; `min_runtime_seconds` is the
  scenario's clock.

Verified: `npm run typecheck` clean, `npm run test:unit` **42 pass, 0 fail**.

After these, the nightly's expected steady state is **7 passed, 2 failed**
(`chat-direct`'s bounce half, `mp-download`'s URL) until `M1-08-chat-bounce` and
the write-up's fix land, and 8 once `enroll-revoked` starts running.

---

## S3 — `security_audit.yml` has never been green, and needs a decision

Every recorded run has failed (35337851102 through 35377904545). Two advisories,
and **neither is fixable from this repository today**:

- **`RUSTSEC-2023-0071` — `rsa 0.9.10`, the Marvin timing attack.** The advisory
  itself records `patched = []` and says so explicitly: "Still affected as of
  2026-09-12: rsa 0.9.10 (latest stable) and rsa 0.10.0-rc.18 (latest).
  `patched = []` is intentional." rustak signs tokens and issues certificates with
  it; there is no version to move to.
- **`RUSTSEC-2026-0258` — `h2 0.3.27`, unbounded empty DATA frames.** Patched in
  `0.4.16`, which is a different major line. `cargo tree -i h2@0.3.27` gives one
  path and only one: `actix-http 3.13.6` → `actix-web 4.15.0` (and
  `actix-multipart`, `actix-ws`). actix-web 4 pins `h2 ^0.3`; moving off it is an
  actix major upgrade, not a dependency bump.

So the workflow as configured can only ever be red, which means it reports
nothing: a genuinely new advisory would land in a job that was already failing.
**I have not changed it** — suppressing a security gate is the user's call, not
mine. The two shapes of fix, for whoever decides:

1. `rustsec/audit-check@v2.0.0` takes an `ignore:` input; or `.cargo/audit.toml`
   with `[advisories] ignore = ["RUSTSEC-2023-0071", "RUSTSEC-2026-0258"]`, which
   `cargo audit` reads and which keeps the rationale in the repository next to
   the list. Either ignores **those two ids only** — any new advisory still fails
   the job.
2. Leave it red and treat the job as a report rather than a gate — in which case
   `docs/ci.md` should say so, because right now it reads as a gate.

My recommendation is (1) with a dated comment per advisory and a re-check note,
plus a line in `docs/ci.md`. Say the word and I will write it.

---

## S4 — Dependabot PRs #1–#3 (evaluated; **not merged**, per the brief)

**#1 — `codecov/codecov-action` 7.0.0 → 7.1.0. Recommend: merge after a re-run.**
The diff is `dist/codecov.sh` (+41/-15), `action.yml` (+5), `src/version`,
`README.md` — the uploader wrapper, no input removed or renamed, so `rust.yml`'s
`with:` block is unaffected. Its checks show `End-to-End Tests` and the two
`darwin-arm64-*` jobs failing, and **both are stale-base artefacts, not this
bump**: the run is 35329781507 from 09:32, and the `darwin-arm64-rustak` job log
shows `sudo apt update && sudo apt install -y musl-tools` → `sudo: apt: command
not found` on a **Linux** runner under a job named `darwin-arm64-rustak` — an old
`build` matrix where `run_on`/`setup` merged onto the wrong entries. The matrix on
`main` today is correct and every darwin job passes there. Re-run against current
`main` and it should be green.

**#2 — `typescript` 5.9.3 → 7.0.2 in `e2e`. Recommend: rebase, then merge.**
No code change needed: `End-to-End Tests` and `Interop: node-tak` both **passed**,
so TypeScript 7 compiles and runs the suite. The only red check is `Lint`, and it
is not about TypeScript — `error: the Err-variant returned from this function is
very large`, i.e. the newer stable's `clippy::result_large_err` that M0-21 fixed
in `marti/tls.rs`. The PR branched at 17:34, that fix landed at 17:47. A rebase
clears it.

**#3 — `p256` 0.13.2 → 0.14.0. Needs a one-line code change first.** `Lint` and
`Test` both fail to compile, in one place:

```
error[E0599]: no method named `to_encoded_point` found for reference
              `&ecdsa::verifying::VerifyingKey<NistP256>`
  --> rustak-server/src/testing/authenticator/keys.rs:97:54
help: there is a method `to_sec1_point` with a similar name
```

`to_encoded_point` → `to_sec1_point` at
`rustak-server/src/testing/authenticator/keys.rs:97`. The `.x()` / `.y()` calls on
the result are unchanged. Everything else builds: all ten `build` matrix jobs,
`e2e` and `interop-node-tak` are green on the PR. I have **not pushed to the
Dependabot branch**; the change belongs on `main` (or in a follow-up commit on the
PR by whoever merges it).

---

## S5 — Slow spots

Nothing over 15 minutes on `main` today. The longest jobs on 35376447445:
`Test` 12m 06s, `windows-amd64-rustak` 10m 38s, `darwin-amd64-rustak` 7m 32s,
`linux-arm64-rustak` 6m 30s. The nightly's `Interop: EUD harness` ran **13m 20s**
for nine scenarios — under M2-09 §4's own 20–25 minute estimate and well inside
its 90-minute timeout. No cuts proposed.

## S6 — Concurrency note

`interop/shared/src/probe.ts` was edited by another agent while I was reading it
(a retry window on the stream probe, so a listener still binding is not read as
absent). Left alone — it is a real improvement and does not touch anything I
changed.

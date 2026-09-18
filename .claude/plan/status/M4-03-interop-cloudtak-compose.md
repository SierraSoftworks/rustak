# M4-03 — `interop/cloudtak`: the CloudTAK full-stack compose suite

**Status:** delivered, unproven end to end. The compose stack, the runner, 40 unit tests and a real
`interop-cloudtak` nightly job in place of the `if: false` placeholder. **Docker is not available on
this machine**, so no container in this suite has ever been started: the request builders and the
response parsers are tested against fixtures, the compose file and the generated rustak
configuration are asserted against each other from both sides, and §4 records exactly what the first
CI run is expected to do — including the four places it is most likely to stop.

**Nothing was copied from CloudTAK.** The compose file and the runner were written from CloudTAK's
documented configuration read in the reference checkout (`docker-compose.yml`, `.env.example`,
`api/common/config.ts`, `api/index.ts`, `api/nginx.conf.js`, `api/stateless/routes/{server,login,
marti,marti-mission,marti-package}.ts`) — facts only: env var names, ports, route paths, body and
response shapes. The fixtures under `interop/cloudtak/fixtures/` are hand-written from those shapes,
not captures.

---

## 1. What landed

### New — `interop/cloudtak/**`

| File | What it is |
|---|---|
| `docker-compose.yml` | Three services: `postgis` (tmpfs, health-gated), `cloudtak` (the pinned published image, with the test CA mounted and named in `NODE_EXTRA_CA_CERTS`), `rustak` (the image the job builds, `/data` bind-mounted, running as the invoking user). Every port published on the loopback and every one overridable. |
| `src/settings.ts` | Paths, ports, the image pins, the three container URLs and the names a run creates — the single source of truth the compose file is asserted against. |
| `src/pki.ts` | `openssl`: the test CA, a server certificate with `DNS:rustak, DNS:localhost, IP:127.0.0.1`, the full chain staged into `/data/tls/`, and the client key + CSR for CloudTAK's admin certificate. |
| `src/rustak.ts` | The configuration the container is given (`[web.public.tls] mode = "files"`, all three listeners, `client_passwords_enabled`), rendered by `interop/shared/src/config.ts`, plus the `ServerInfo` the shared bootstrap needs. |
| `src/compose.ts` | Is there a runtime, `up`, `down`, `ps`, logs, and the loud failure when there is none. |
| `src/http.ts` | One `node:https` client that can send Basic auth and a text body while still verifying the chain against the test CA. |
| `src/enroll.ts` | `GET /Marti/api/tls/config` → CSR → `POST /Marti/api/tls/signClient/v2`, as builders and parsers: the name entries, the bare-base64 body, the bare-base64 answer re-armoured. |
| `src/client.ts` | Talking to CloudTAK, unwrapping its error sentences (a rustak failure arrives as a CloudTAK `400` whose `message` is the useful part). |
| `src/api.ts`, `src/missions.ts` | Every CloudTAK call as a pure builder and parser: server configuration, login, channels, missions, contents, changes, packages. `expectObject` is where the `Content-Type` bug is named. |
| `src/step-kit.ts`, `src/steps-setup.ts`, `src/steps-datasync.ts`, `src/steps.ts` | The eight steps and their assertions, in order. |
| `src/smoke.ts` | The Playwright smoke: login page → map canvas → Data Sync menu, three screenshots kept either way. |
| `src/session.ts` | The cold start: shared bootstrap → channel → operator account → client password → surface probe → admin certificate. |
| `src/surfaces.ts` | The six rustak surfaces a step may need, each with the brief that will flip its skip. |
| `src/run.ts` | The three modes, the stack lifecycle, the report and the exit code. |
| `tests/{api,missions,enroll,stack}.test.ts`, `tests/fixtures.ts`, `fixtures/**` | 40 unit tests, no Docker, no server. |
| `README.md`, `package.json`, `package-lock.json`, `tsconfig.json`, `.gitignore` | |

### Modified

| File | Change |
|---|---|
| `.github/workflows/nightly.yml` | The `interop-cloudtak` job, replacing the `if: false` placeholder, **and** a `pull_request` trigger on the workflow so the `run-cloudtak` label can select that job. No other job was touched; every other job's `if:` already tests `github.event_name`, so they skip on a pull request. |
| `docs/interop.md` | The suite table row, a new "The CloudTAK stack" section, the nightly table row, and a paragraph on the `run-cloudtak` label. |
| `.claude/plan/compat/cloudtak.md` | §2 gains what the *first* `PATCH /api/server` actually does (below). |

Nothing under `rustak-server/**`, `rustak-api/**`, `interop/shared/**`, `interop/node-tak/**` or
`interop/eud/**` was touched, and no `git`/`but` command was run.

---

## 2. Exit checks

```
$ cd interop/cloudtak && npm run typecheck
> tsc --noEmit
(no output)
typecheck exit=0

$ cd interop/cloudtak && npm run test:unit
> node --import tsx --test tests/*.test.ts
✔ the server certificate names both the service and the loopback (0.103ms)
ℹ tests 40
ℹ suites 0
ℹ pass 40
ℹ fail 0
ℹ cancelled 0
ℹ skipped 0
ℹ todo 0

$ cd interop/cloudtak && npm test        # the default: Docker is required
Error:
'docker compose' is not usable here, and this suite is nothing without it.

Every assertion in it is made against CloudTAK's own container talking to
a rustak built from this checkout, so a skipped run would prove nothing.

  * In CI: the job must have Docker and must have built the rustak image
    (rustak-interop-cloudtak:local) from rustak-server/Dockerfile.
  * Locally: start Docker, or set RUSTAK_INTEROP_REQUIRE_DOCKER=0 to get
    the unit tests and a run in which every scenario skips.
(exit 1 — this is the required behaviour, not a failure of the check)

$ cd interop/cloudtak && RUSTAK_INTEROP_REQUIRE_DOCKER=0 npm test
… 40 unit tests pass …
[cloudtak] no container runtime, and RUSTAK_INTEROP_REQUIRE_DOCKER=0 — nothing to drive.
﹣ configure-server / login / channels / data-sync / marker / file / changes / package / ui-smoke
    no container runtime: this suite drives CloudTAK's own container.
[cloudtak] 0 passed, 9 skipped, 0 failed.
(exit 0)

$ actionlint .github/workflows/nightly.yml
(no output, exit 0)

$ ./scripts/check-file-length.sh
(no output, exit 0)

$ # every TypeScript file, functional lines (the script only walks *.rs):
175 src/api.ts · 170 src/missions.ts · 163 src/run.ts · 153 src/steps-datasync.ts ·
114 src/enroll.ts · 105 src/smoke.ts · 97 src/pki.ts · 95 src/compose.ts · 90 src/steps-setup.ts ·
74 src/session.ts · 66 src/rustak.ts · 63 src/http.ts · 59 src/client.ts · 48 src/step-kit.ts ·
48 src/settings.ts · 38 src/surfaces.ts · 5 src/steps.ts   (largest test file: 123)
```

---

## 3. The decisions worth knowing

1. **`[web.public.tls] mode = "files"`, and the CA is mounted into CloudTAK.** This is the whole
   point of the suite rather than a convenience: CloudTAK's `webtak` calls verify the chain through
   `undici` with no override, so an internal CA breaks login outright unless the operator mounts the
   root and names it in `NODE_EXTRA_CA_CERTS` (`compat/cloudtak.md` §3, CloudTAK issue #983). A run
   that signs in has proved the documented workaround works. rustak's internal CA still issues every
   *client* certificate in the same container; the generated pair lives in `/data/tls/`, not
   `/data/pki/`, so the two cannot collide.
2. **`[server] base_url` names `localhost`, not `rustak`.** The passkey the shared bootstrap
   registers is registered from the host, and `rustak-server/src/auth/passkeys/mod.rs` relaxes the
   port to `Port::Any` only for loopback relying parties — so `localhost` is the one name that works
   whether or not the published port matches the bound one. CloudTAK is unaffected: it reaches the
   same listener at `https://rustak:8446`, which is what its three URLs record, and the server
   certificate carries both names.
3. **The operator is an ordinary account, not rustak's administrator.** CloudTAK makes the first
   account that configures it its own system administrator; handing it rustak's administrator would
   hide an authorisation bug that bites everybody else. The account gets one channel (`Interop`) and
   one client password, expiring in a day.
4. **The admin certificate is minted by the runner, not by CloudTAK.** `PATCH /api/server` takes
   `auth: {cert, key}` and CloudTAK has no endpoint that mints one, so `src/enroll.ts` performs the
   same `tls/config` → CSR → `signClient/v2` flow node-tak's `Credentials.generate()` performs. It
   uses `node:https` with the CA named explicitly rather than `NODE_EXTRA_CA_CERTS`, which Node
   reads once at process start and which the run only generates *after* start.
5. **Three services, not seven.** MinIO, the pmtiles tiler, the events and retention workers and the
   media server are left out; `README.md` → What is and is not in the stack says why for each. The
   short version: with `StackName` unset CloudTAK does not require `ASSET_BUCKET`, and nothing this
   suite asserts reads CloudTAK's object store.
6. **`PUT /api/marti/package`, not the multipart `POST`.** The `public: true` branch uploads through
   `/Marti/sync/missionupload` and publishes to the package list, which is the rustak surface worth
   asserting; the multipart branch would only add a busboy round-trip inside CloudTAK.
7. **CloudTAK is pinned at `v13.89.0`**, the newest tag actually published to
   `ghcr.io/dfpc-coe/cloudtak-api` (checked against the registry; the reference checkout is
   `13.90.0`, whose tag is not built yet). The image is `linux/amd64` only, which is what
   `ubuntu-latest` is.
8. **`compat/cloudtak.md` §2 gained a paragraph**: the *first* `PATCH /api/server` is
   unauthenticated, **requires** a username and password as well as the certificate, runs the
   password grant and `Credentials.generate()` with them, and makes that account CloudTAK's system
   administrator. §1.2 of report 03 says the call validates with `/files/api/config` "and nothing
   else", which is true of the certificate check and not of the call — so a bring-up test that gets
   past it has already proved three Tier 1 surfaces, and the first step of this suite is worth far
   more than it looks.

---

## 4. What the first CI run is expected to do

The job is `interop-cloudtak` in `.github/workflows/nightly.yml`: nightly at 04:00 UTC,
`workflow_dispatch`, or a pull request labelled `run-cloudtak`. Dispatching it is how this gets its
first real run.

**The steps, in order, and what each should print.**

1. Checkout, toolchain, cache, `cargo binstall trunk@0.21.14`, `trunk build --release`,
   `cargo build --release -p rustak-server`. On a cold cache this is the long pole — 20-35 minutes;
   with the shared `Swatinem/rust-cache` key it should be a few.
2. `Build the rustak image`: `dist/rustak` staged from `target/release/rustak`, then
   `docker build -f rustak-server/Dockerfile -t rustak-interop-cloudtak:local .`. Expect ~30s: the
   Dockerfile packages a binary, it does not compile one.
3. `npm ci`, `npm run typecheck`, `npx playwright install --with-deps chromium` (~1 minute).
4. `npm test` →
   - the 40 unit tests pass, exactly as above;
   - `docker compose down` (no-op), the test CA is generated, `.run/rustak/config.toml` is written;
   - `docker compose up --detach --quiet-pull` pulls `postgis/postgis:17-3.4-alpine` and
     `ghcr.io/dfpc-coe/cloudtak-api:v13.89.0` (~400MB, 1-2 minutes) and starts all three;
   - the runner waits for `https://localhost:8446/api/v1/health` and for `GET http://localhost:5000/api`.
     **CloudTAK's first start runs its migrations and seeds its iconsets, which is the slowest part
     of the wait** — the bound is four minutes and I expect 60-120 seconds;
   - the shared bootstrap walks rustak's `/api/v1` (setup token read out of the bind mount →
     administrator → passkey → wizard), creates the `Interop` channel and the `cloudtak-operator`
     account, mints a client password, probes the six surfaces and enrols the admin certificate;
   - the eight steps run, then the UI smoke. Expect `[cloudtak] 9 passed, 0 skipped, 0 failed.` and
     three screenshots in `interop/cloudtak/artifacts/`, which are only uploaded on failure.
5. On failure: `docker compose logs` is written to `artifacts/compose.log`, the stack comes down, and
   the `cloudtak-artefacts` artifact carries the log, the screenshots and `rustak-cloudtak.log`.

Total, warm cache: **10-15 minutes**. The 60-minute bound is for a cold one.

**Where I expect it to stop first, in order of likelihood.** None of these can be settled without a
container runtime, and each is a one-line fix in a named place:

1. **The `channels` step.** It toggles the `Interop` channel's `active` flag through
   `PUT /api/marti/group` and asserts the flip in the list that comes back. rustak's active state is
   per *device* (`device_group_state`, driving `PUT /groups/active?clientUid=`), and CloudTAK sends
   no `clientUid` here — so the flip may not be visible to the next `GET` at all, or may apply to
   every device of that user. If it reports `toggling 'Interop' to false was not reflected`, the
   question is which behaviour is right (`compat/groups.md`), not whether the suite is broken; the
   assertion is in `src/steps-setup.ts` and is the one I would relax first.
2. **The `file` step's hash.** The runner computes SHA-256 over the bytes it uploads and looks for
   that hash in the mission's contents. `compat/files.md` §"missionquery" confirms SHA-256 is the
   convention, but if rustak hashes something else the failure names every hash the Data Sync does
   hold, which should settle it in one read.
3. **The UI smoke's selectors.** `input[type="password"]`, the first text input, and a
   `Sign In` button by role, then any `canvas`, then the mission name as text. CloudTAK's login form
   is a Vue component whose fields carry placeholders rather than names; if a selector misses, the
   `99-failure.png` screenshot is in the artifact and the fix is in `src/smoke.ts` alone. The smoke
   failing does not invalidate any API-level assertion.
4. **The bind-mount ownership.** `rustak` runs as `${RUSTAK_INTEROP_UID}:${RUSTAK_INTEROP_GID}`,
   taken from `process.getuid()`. On the GitHub runner that is uid 1001 (`runner`), and the
   directory is created by the same process, so it should be right; if it is not, rustak exits
   immediately with a permission error on `/data` and the step fails at
   `rustak … was not ready within 240s` with the container log attached.

A fifth possibility is simply that a rustak surface is not served yet, in which case the step
**skips** with the brief that owns it rather than failing — the same convention the other two
suites use. Because M4-01/M4-02 and M3-01 have landed, I expect every surface to probe as present;
if `missions` or `files` probes absent, the run will be green with five skips, which is a signal to
read rather than a pass to celebrate.

---

## 5. Deviations from the brief

1. **The workflow's `on:` block was touched, not only the job.** The brief asks for a `run-cloudtak`
   PR label, which cannot work without `pull_request` in the triggers. The addition is four lines
   and selects nothing else: every other job in the file already tests `github.event_name` and skips
   on a pull request. The CI steward should know the workflow now fires (and immediately skips
   everything) on PR events.
2. **`interop/shared` was reused but not extended.** The launcher (`startServer`) is the one piece
   that does not fit — this suite's server is a container, not a child process — so `src/rustak.ts`
   builds the same `ServerInfo` by hand and the bootstrap, the config writer, the HTTP client and
   the probe are shared unchanged. If a fourth suite ever needs the same thing, the right move is a
   `ServerInfo`-producing "attach" helper in `interop/shared/src/launch.ts`; one caller did not
   justify it.
3. **The suite depends on `playwright`.** `npm ci` pulls the package; the browser is installed
   separately (`npx playwright install chromium`), and the smoke skips with that instruction when it
   is not there. `PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1` keeps a developer's install light.
4. **`RUSTAK_INTEROP_REQUIRE_DOCKER` defaults to requiring Docker**, the opposite of
   `interop/eud`'s `RUSTAK_EUD_REQUIRE_DOCKER`, exactly as the brief asks. The two suites now
   disagree about their defaults on purpose: `interop/eud` has a useful probe-only mode and this one
   does not. Both are documented where a reader meets them.
5. **`compat/cloudtak.md` was edited** (§2), which the brief permits for corrections. The change is
   an addition, not a contradiction, and it names where it was read from.

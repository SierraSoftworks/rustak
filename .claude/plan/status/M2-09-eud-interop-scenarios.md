# M2-09 — EUD interop scenarios: driving `commotest` against rustak in CI

**Status:** delivered. Nine declarative scenarios, a TypeScript runner, a shared launcher/bootstrap extracted from
`interop/node-tak`, the `[stream] negotiation` compatibility switch, and a real `interop-eud` nightly job in place
of the `if: false` placeholder. Docker is not available on this machine, so the end-to-end path is verified by
dispatching the nightly workflow; §4 records exactly what each scenario is expected to do on that first run.

**Licence posture held.** Nothing from `atak-civ` entered this repository. The runner shells out to
`docker run` and parses two text files; it does not link, include, vendor or generate from `commoncommo`, and the
fixtures under `interop/eud/fixtures/` are hand-written from the format strings recorded in M1-00 §3.2, not
captures.

---

## 1. What landed

### New — `interop/shared/**` (the half both suites need)

| File | What it is |
|---|---|
| `interop/shared/src/launch.ts` | The throwaway-server launcher, lifted out of `interop/node-tak/src/rustak.ts`: scratch directory, internal CA, three listeners on the loopback, stale-workspace sweep, `waitForServer`, and a new `waitForPort` (the stream listener binds *after* the public one — see §5). Takes per-suite `config` overrides and optional fixed `ports`. |
| `interop/shared/src/config.ts` | Assembles the generated configuration as tables and renders it once. Text concatenation cannot work: TOML forbids defining a table twice, so a scenario could not add `[auth] anon_group_default = false` on top of the launcher's `[auth]`. |
| `interop/shared/src/http.ts` | One client, two flavours: the global `fetch` (node-tak, `NODE_EXTRA_CA_CERTS`) and `node:https` with the authority named explicitly (the EUD runner, which therefore needs no second process). |
| `interop/shared/src/bootstrap.ts` | The cold-start ceremony (setup token → administrator → passkey → wizard) plus `createUser`, `createGroup`, `setGroups`, `mintCredential`. |
| `interop/shared/src/webauthn.ts` | Moved verbatim from `interop/node-tak/src/webauthn.ts`. |
| `interop/shared/src/probe.ts` | The surface probe, generic over a suite's surface map. |
| `interop/shared/package.json`, `README.md` | `{"type": "module"}` so `tsc` resolves these files as ESM from either suite, and a note on what belongs here. Source only: no dependencies, no build. |

### New — `interop/eud/**` (the suite)

- `package.json`, `package-lock.json`, `tsconfig.json`, `.gitignore` — `npm test` = unit tests, then scenarios.
  One dependency: `smol-toml`.
- `scenarios/*.toml` — the nine night-one scenarios (§4).
- `src/surfaces.ts` — the six surfaces the scenarios need, each with the brief that will flip its skip.
- `src/scenario.ts` — loader and validator (keys, types, placeholders, regular expressions, `<wait> <command>`
  pairs ending in `quit`, channel directions, `[ports]`).
- `src/template.ts` — the closed placeholder set and the argv builder.
- `src/artefacts.ts` — the `commo-log.txt` and `commo-xml.txt` parsers, deliberately prefix-agnostic.
- `src/expect.ts` — the assertion engine (ordered log, unordered log, forbidden log, `commo-xml.txt` by
  uid/type/link, the issued certificate's subject, `clientEndPoints`, the audit trail, minimum runtime).
- `src/openssl.ts` — PKCS#12: exporting the CA as the truststore `estream:` verifies against, and reading back
  what rustak issued. Legacy algorithms first, modern as the fallback.
- `src/docker.ts` — `docker run --rm --network host -v <out>:/work <image> <argv>`, the timeout kill, the image
  pull, and the loud failure when `RUSTAK_EUD_REQUIRE_DOCKER=1` and there is no runtime.
- `src/session.ts` — one server per scenario: bootstrap, channels, an account and a one-time token per EUD, the
  truststore and `payload.dat` in each EUD's mounted directory, revocation, audit and `clientEndPoints` reads.
- `src/execute.ts` — runs one scenario: staggered containers, server-side sampling *while* they run, assertions
  *after* they exit, artefacts kept on failure.
- `src/run.ts` — the three modes (§3), the probe pass, the report, the exit code.
- `tests/{scenario,template,artefacts,expect}.test.ts` and `fixtures/**` — 41 unit tests, no Docker, no server.

### Modified

| File | Change |
|---|---|
| `interop/node-tak/src/rustak.ts` | Now the suite's own config (`client_passwords_enabled`, its names) over the shared launcher. |
| `interop/node-tak/src/bootstrap.ts` | Now the suite's two accounts over the shared ceremony. |
| `interop/node-tak/src/probe.ts` | Now the binding of its `SURFACES` to the shared probe. |
| `interop/node-tak/src/surfaces.ts` | Imports and re-exports `SurfaceProbe` from `interop/shared`. |
| `interop/node-tak/src/webauthn.ts` | Deleted; moved to `interop/shared/src/webauthn.ts`. |
| `rustak-server/src/config/stream.rs` | `NegotiationMode`, the `[stream] negotiation` key, and the selection the state machine reads (§5 deviation 1). |
| `rustak-server/src/stream/negotiation.rs` | `Negotiation::with_mode`, `refuse` answering `status="false"`, `silent` never offering. `Negotiation::new`'s signature is unchanged, so `stream/connection.rs` still compiles untouched. |
| `config.example.toml` | The `[stream]` block, documenting `negotiation` as a compatibility-testing switch and pointing at `[stream.limits] negotiate_protobuf` as the supported way to turn protobuf off. |
| `.github/workflows/nightly.yml` | The `interop-eud` job, replacing the `if: false` placeholder. |
| `interop/eud/README.md` | The Scenarios section: how a run works, the modes, the environment variables, the night-one table, the schema, and the traps. |
| `docs/interop.md` | The scenarios, `interop/shared`, the negotiation switch, and the nightly table row. |

`interop/node-tak/tests/**`, the image scripts (`Dockerfile`, `fetch-*.sh`, `build-takthirdparty.sh`,
`stage-runtime.sh`) and every other `rustak-server` file were not touched.

---

## 2. Exit checks

```
$ cd interop/eud && npm run typecheck
> tsc --noEmit
(no output)

$ cd interop/eud && npm test
ℹ tests 41
ℹ pass 41
ℹ fail 0
[eud] 9 scenario(s) loaded and validated.
[eud] no container runtime — probe-only run. The image would be ghcr.io/sierrasoftworks/rustak-interop-commoncommo:latest.
[eud] surfaces served:  enrollment, stream, clientEndPoints, channels, certificateRevocation, missionPackages
[eud] surfaces missing: (none)
﹣ chat-direct … ﹣ two-eud-routing      (nine skips: "no container runtime")
[eud] 0 passed, 9 skipped, 0 failed.        exit 0

$ cd interop/eud && RUSTAK_EUD_REQUIRE_DOCKER=1 npm run test:scenarios
Error:
RUSTAK_EUD_REQUIRE_DOCKER=1 is set, but 'docker' is not usable here.
… (names the image, and what to do in CI and locally)     exit 1

$ cd interop/node-tak && npm run typecheck && npm test
ℹ tests 25
ℹ pass 22
ℹ fail 0
ℹ skipped 3      (all three are TODO(M4): the mission API)

$ cargo test -p rustak-server --features testing -- stream::negotiation config::stream
test result: ok. 19 passed; 0 failed; 0 ignored; 1251 filtered out

$ ./scripts/check-file-length.sh
(no output, exit 0)

$ rustfmt --edition 2024 --check rustak-server/src/config/stream.rs rustak-server/src/stream/negotiation.rs
(no output, exit 0)

$ cargo fmt --all --check
Diff in rustak-server/src/auth/mission_token.rs:137, :348, :427     ← another agent's file, not this brief's

$ cargo clippy --workspace --all-targets -- -D warnings
error: unnecessary closure used with `bool::then`          --> rustak-server/src/auth/mission_token.rs:235
error: enclosing `Ok` and `?` operator are unneeded        --> rustak-server/src/db/repos/missions/changes.rs:155
error: methods with the following characteristics …        --> rustak-server/src/stream/mission_notify.rs:321
   ← all three are in in-flight M4 files; nothing in config/stream.rs or stream/negotiation.rs

$ actionlint .github/workflows/nightly.yml
nightly.yml:27:9: constant expression "false" in condition …   ← the interop-cloudtak placeholder (pre-existing;
                                                                 this brief removed the other one). actionlint
                                                                 also reports findings in rust.yml, so it is not
                                                                 a clean gate in this repository today.
```

`cargo test` needed two retries: the lib did not compile for several minutes because of another agent's
in-progress edits (`users::get`, `groups::members`, `devices::active_groups`, `Utc`). It compiled and passed on
the third attempt with no change to this brief's files.

---

## 3. How the suite runs

`npm test` = `npm run test:unit` (node:test over `tests/*.test.ts`) then `npm run test:scenarios`
(`tsx src/run.ts`). The scenario phase picks its mode from the machine:

| Docker | `RUSTAK_EUD_REQUIRE_DOCKER` | Result |
|---|---|---|
| yes | anything | scenarios run for real |
| no | `1` | the run fails loudly, naming the image — what CI sets |
| no | unset | probe-only: every scenario file is validated, the surfaces are probed, everything skips |

For each scenario: start rustak with the scenario's configuration overrides → wait for the API *and* the stream
port → bootstrap → create channels, accounts, grants and one-time tokens → export the CA as a PKCS#12 truststore
into each EUD's mounted directory → run the containers, staggered → sample `/Marti/api/clientEndPoints` while they
run → read the two text files and the issued keystore **after** every container has exited → assert. A scenario
whose surfaces are missing skips with the brief that will serve them.

---

## 4. Expected outcome of the first nightly run

Surfaces, probed against this tree on the day of writing: `enrollment`, `stream`, `clientEndPoints`, `channels`,
`certificateRevocation` and `missionPackages` are **all served**, so **every scenario runs** — none skips. (The
brief expected `enroll-revoked`, `mp-upload` and `mp-download` to skip; M2-08 and M3-01 landed while this was
being written, `/api/v1/certificates` appearing between two probe runs an hour apart.)

| Scenario | Expected | Why |
|---|---|---|
| `enroll-basic` | **runs; pass plausible, failure informative** | The first time ATAK's own enrollment code touches rustak. The most likely first-run failures, in order: the PKCS#12 truststore shape `commoncommo` will accept (we write SHA-1/3DES with a SHA-1 MAC, falling back to OpenSSL 3 defaults); the subject order in the certificate rustak signs (`CN` must come first); the `201`-is-a-failure rule; and the Basic-auth enrollment path on the `[web.public]` listener with a `Host` of `127.0.0.1` rather than the configured `localhost`. |
| `two-eud-routing` | **pass** | Routing by channel with disjoint IN/OUT grants is already covered by rustak's own `stream_routing` tests; this proves the same thing through ATAK's pipeline. The one new risk is `anon_group_default = false` interacting with enrollment. |
| `chat-direct` | **fail — expected, and the point** | The delivery half should pass. The bounce half cannot: `rustak-cot::types::cot_type::CHAT_FAILED` (`b-t-f-s`) is defined and **nothing in `rustak-server/src/stream` ever sends one**, so an undeliverable unicast chat is dropped silently. This scenario is what will notice when that is implemented; until then it is a standing, accurate failure. Worth a brief. |
| `disconnect` | **pass** | `stream::notify::on_disconnect` sends `t-x-d-d` and `rustak-cot::msgs` puts the departing uid in a `<link>`. Risk: `remiface:0` — the interface index an enrollment-created stream gets is an assumption, and if it is wrong ALPHA never disconnects and both halves fail together. |
| `negotiate-refused` | **pass** | The knob is implemented and unit-tested; the client side is ATAK's own state machine. Risk: the exact wording of the denial line, which is asserted as `negotiation request denied, using xml only`. |
| `negotiate-silent` | **pass** | Same, with the 60-second timeout line. This scenario runs 75 s per EUD, so it is the slowest. |
| `enroll-revoked` | **runs; uncertain** | Revocation drops the live session (the `on_revoked` hook closes by fingerprint), which the first assertion catches. The second — that presenting the same keystore again is refused **at the handshake** — depends on the stream listener checking revocation on every new connection, not only on the live set. If it does not, this fails and names a real gap. |
| `mp-upload` | **runs; uncertain — the `!ECDH` gate** | `result = SUCCESS` requires all three HTTP steps to have succeeded over the `DEFAULT:!ECDH` curl context. The likeliest first-run failure is not the cipher list but the **URL** `commoncommo` builds: `smpsend:` takes no port, so the scenario pins ATAK's own ports (8446/8443/8089) via the new `[ports]` key. If the log shows the upload going elsewhere, that block and `{marti_port}` are the two things to change. |
| `mp-download` | **runs; uncertain** | Needs the peer-addressed `mpsend:` to be relayed as a `b-f-t-r` the other EUD can fetch. Depends on the same URL convention plus rustak's own hosting of the uploaded package. |

Runtime: nine scenarios, sequential, each 50–120 s of EUD time plus a server start, so **≈20–25 minutes** after
the UI and server builds. The job's timeout is 90 minutes.

If the very first run fails inside `enroll-basic`, read `interop/eud/artifacts/enroll-basic/alpha/commo-log.txt`
from the job's artefacts before changing anything: the enrollment steps and the `Interface …` lines say which of
the four failure points above it was.

---

## 5. Deviations

1. **The negotiation knob reaches the connection through a process-wide selection, not through `ConnLimits`.**
   `Negotiation` is built in `rustak-server/src/stream/connection.rs` from a `bool` that
   `rustak-server/src/stream/mod.rs` fills in, and both files belong to other agents working in this tree — the
   brief allowed this brief exactly two server files. So `config/stream.rs` publishes the parsed mode into an
   `AtomicU8` as it deserialises (once per process) and `Negotiation::new` reads it. `Negotiation::new`'s
   signature is unchanged, so nothing else needed editing. Consequences: two servers in one process cannot have
   different modes (only the interop runner ever sets one, and it starts one server per scenario), and the unit
   tests are written so that only one of them touches the selection, since `config::stream` and
   `stream::negotiation` share a test binary.

   **The patch that replaces it**, whenever those two files are free:

   ```rust
   // stream/connection.rs — ConnLimits
   -    pub negotiate: bool,
   +    pub negotiate: crate::config::stream::NegotiationMode,
   // stream/connection.rs — line 133
   -    let mut negotiation = Negotiation::new(deps.limits.negotiate, deps.server_version.clone());
   +    let mut negotiation = Negotiation::with_mode(deps.limits.negotiate, deps.server_version.clone());
   // stream/mod.rs — line 181
   -                negotiate: limits.negotiate_protobuf,
   +                negotiate: if limits.negotiate_protobuf { config.stream.negotiation } else { NegotiationMode::Silent },
   ```

   then delete `SELECTED`, `select`, `selected` and `select_negotiation` from `config/stream.rs` and make
   `Negotiation::new` private or drop it.

2. **`enroll-revoked` revokes the credential, not the certificate.** The brief said to skip it until
   `POST /api/v1/certificates/{id}/revoke` exists. `requires = ["certificateRevocation"]` still gates it on that
   surface — which now answers, so it runs — but the revocation the runner performs is
   `DELETE /api/v1/credentials/{id}`, which rustak documents as cascading to the certificates issued with that
   credential. Switching to a per-certificate revoke is one function in `src/session.ts`.

3. **`mp-upload`'s server-side assertion is `uploaded`, not a three-step sequence.** rustak audits the stored
   resource (`marti/sync.rs` → `upload::audit`), not each of `missionquery`/`missionupload`/`metadata`. The
   three-step sequence is what `result = SUCCESS` on the client already proves — `commoncommo` does not report
   success unless all three answered as it expects — so the audit check is the server-side witness that the bytes
   landed rather than a restatement of it.

4. **One `missionPackages` surface instead of `missionUpload` + `missionQuery`.** A *served*
   `/Marti/sync/missionquery` answers `404` for a hash it does not hold, which is exactly what the probe reads as
   "no such route". `missionupload` is POST-only and answers `405` when the scope is mounted, so it is the
   unambiguous probe for the whole enterprise-sync surface. (Found by probing: the first run reported
   `missionQuery` missing while `missionUpload` was served.)

5. **A `[ports]` key was added to the scenario schema.** `smpsend:`/`mpsend:` build their own HTTPS URL rather
   than being handed one, so the mission-package scenarios pin ATAK's own ports (8446/8443/8089) instead of taking
   reserved ones. Nothing else uses it, and the loader rejects a port below 1024.

6. **No scenario forbids `Interface Down`.** It is what `quit` produces on a clean shutdown; forbidding it would
   fail every passing run. Only `Interface Error` is forbidden, and `disconnect` asserts the `Down` it wants. The
   fixture caught this before a container could.

7. **node-tak is 22/25, not the 16/25 the brief expected.** Nothing regressed — M2-04, M2-06 and M3's surfaces
   landed since the brief was written, so six scenarios that used to skip now pass. The three remaining skips are
   all `TODO(M4)` on the mission API. Verified before and after the shared extraction.

8. **`interop/shared` is source-only and imported by relative path** (`../../shared/src/…`), not as an npm
   workspace package. A `file:` dependency would have meant regenerating `interop/node-tak/package-lock.json`,
   which is the one thing making that suite's CI run reproducible. The only cost is a `package.json` in
   `interop/shared` carrying `{"type": "module"}` so `tsc` resolves those files as ESM.

9. **node-tak's `src/*.ts` became thin wrappers rather than having their imports rewritten.** `rustak.ts`,
   `bootstrap.ts` and `probe.ts` keep their existing export shape over the shared implementations, so
   `prepare.ts`, `run.ts`, `session.ts` and every file under `tests/` needed no change at all — which is what
   keeps the "the suite must stay green" requirement verifiable rather than hopeful.

---

## 6. Notes for whoever reads the first run

- **The stream listener binds after the public one.** An in-process runner that probed the moment
  `/api/v1/health` answered was told there was nothing on the stream port; `waitForPort` in
  `interop/shared/src/launch.ts` is the fix, and `interop/node-tak` only ever avoided it by accident (it spawns a
  child process in between).
- **The EUD reaches rustak at `127.0.0.1` while the runner uses `localhost`.** The server's certificate names
  `localhost` only, which is fine because the scenarios use normal-enrollment mode (`-` on the host), where ATAK
  does not verify the host name. A scenario that wanted quick-connect semantics would need `127.0.0.1` in
  `[server] domains`, and the passkey the bootstrap registers means the *first* domain must stay a name.
- **`commotest` has no exit-code contract** — every path returns 0, including an argument error — so the runner
  asserts only on file contents, and the loader validates everything it can before a container starts.
- **Artefacts.** A failed scenario's `commo-log.txt`, `commo-xml.txt` and `commo-enroll-cert.p12` are copied to
  `interop/eud/artifacts/<scenario>/<eud>/` before the scratch directory is removed, and the nightly job uploads
  them with the run log. `RUSTAK_EUD_KEEP=1` keeps them for passing scenarios too.
- **The `interop-eud` job does not `needs:` `interop-eud-image`.** It pulls `atak-civ-<short sha>` derived from
  the `ARG ATAK_CIV_SHA` line, falling back to `:latest`, so a night on which the weekly image build did not run
  is still a night the scenarios have an image. It needs `packages: read` because the package is private to the
  organisation on purpose (GPLv3 §6).

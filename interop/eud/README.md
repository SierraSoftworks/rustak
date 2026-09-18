# `interop/eud` — the ATAK-side (EUD) interop harness

This directory builds a CI image containing **`commotest`**, the console client
that ships with **`commoncommo`** — the C++ library ATAK itself uses for
enrollment, the TLS stream, TAK Protocol v1 negotiation, ping/pong, contacts,
GeoChat and mission-package transfer. Driving that binary against a running
rustak is the closest thing to testing against a real ATAK device that fits in
CI: it is not an ATAK-*like* client, it is the code path ATAK runs.

The choice, the alternatives that were rejected, and the evidence behind every
claim here live in
[`.claude/plan/status/M1-00-eud-interop-harness-exploration.md`](../../.claude/plan/status/M1-00-eud-interop-harness-exploration.md).

```
interop/eud/
  Dockerfile               multi-stage: builder (clones + compiles) → runtime (commotest)
  fetch-source.sh          blobless, shallow, cone-sparse checkout of the pinned commit
  fetch-distfiles.sh       resolves the Git-LFS distfiles over plain HTTPS, sha256-verified
  build-takthirdparty.sh   ordered, per-package build of the commoncommo dependency chain
  stage-runtime.sh         assembles the runtime payload and its licence paperwork
  scenarios/*.toml         declarative scenarios: config, argv, expectations
  src/*.ts                 the runner — starts rustak, runs containers, asserts
  tests/*.test.ts          unit tests for the loader, parsers and assertions
  fixtures/                hand-written commo-log.txt / commo-xml.txt for those
  README.md                this file
```

The launcher and the `/api/v1` bootstrap live one directory up, in
`interop/shared/`, because `interop/node-tak` performs the same walk.

The scenarios and the runner that drives them are described under
[Scenarios](#scenarios) below; `npm test` is the whole entry point.

## Licence posture

> **rustak is MIT. `commoncommo` is GPL-3.0. Nothing from atak-civ is in this
> repository, and nothing from atak-civ may be added to it.**

- `atak-civ`'s `LICENSE.md` is GPL-3.0 with no linking exception.
  `commoncommo/README-DISTRIBUTION.txt` is a third-party notice for
  ngtcp2/libcurl/OpenSSL, **not** a grant covering `commoncommo`.
- What lives here is a `Dockerfile` that *clones* a pinned upstream commit,
  shell scripts that drive upstream's own makefiles, declarative scenario data,
  and (from M2) a runner that starts a container and parses text files. **No
  commoncommo source, no headers, no `.proto`, no generated code, and above all
  no linking.** The runner talks to the tool across a process boundary, exactly
  the way CI talks to `openssl` or `curl`. rustak never becomes a derivative
  work.
- **Do not "simplify" the runner by linking `libcommoncommo.a` or vendoring a
  header.** That single change would relicense rustak. If a future scenario
  needs an API `commotest` does not expose (`sendCoTToServerMissionDest` and
  `sendCoTServerControl` are the known gaps), the driver that links the GPL
  library belongs *inside the image*, never in this tree.
- **Publishing.** Baking GPL binaries into a container and pushing it to a
  registry is *conveying* under GPLv3 §6, which obliges us to offer the
  corresponding source. The preferred posture is to keep the GHCR package
  **private to the organisation** — internal use, not conveying — and let CI
  pull it with `GITHUB_TOKEN`. If it is ever made public, the §6 offer is
  satisfied by what the image already carries: `LICENSE.md`, `THIRDPARTY.md`
  and `README-DISTRIBUTION.txt` under `/usr/share/doc/commoncommo/`, a
  `PROVENANCE.txt` naming the exact upstream revision, and the
  `dev.rustak.upstream.corresponding-source` label pointing at that revision's
  tree. The corresponding source is *that commit plus this directory*.
- **Test vectors.** `commotest`'s `proto:` command turns CoT XML into TAK
  Protocol v1 bytes. Wire bytes are facts and using the tool as a live oracle
  in CI is fine; **do not** commit its output as golden fixtures under
  `rustak-cot/tests/` — `conventions.md` requires our fixtures to be our own.
  Compare live, never vendor.
- Upstream marks `commotest` as an internal developer tool. That is a
  developer's note inside a GPL-3.0 work rather than an additional restriction,
  but treat it as a reason to be conservative about republishing binaries.

## The image

`ghcr.io/sierrasoftworks/rustak-interop-commoncommo`

| Tag | Meaning |
|---|---|
| `atak-civ-<40-char sha>` | the exact upstream revision — what scenario jobs should pin |
| `atak-civ-<7-char sha>` | the same image, short form |
| `latest` | most recent successful build |
| `buildcache` | buildx registry layer cache, not a runnable image |

**Contents of the runtime stage** (`debian:bookworm-slim`):

- `/usr/local/bin/commotest` — the entrypoint
- `/opt/commoncommo/lib` — any shared objects the chain produced, on
  `LD_LIBRARY_PATH`. takthirdparty configures every library in this chain with
  `--disable-shared` and archives `commoncommo` itself into
  `libcommoncommo.a`, so this directory is expected to be nearly empty; it
  exists so an upstream change to that policy does not silently break the image.
- `/usr/share/doc/commoncommo/` — licence, third-party notices, upstream
  changelog and `PROVENANCE.txt` (upstream revision, build time, `ldd` output)

**Upstream pin.** `ARG ATAK_CIV_SHA` at the top of the `Dockerfile` is the
single source of truth; the CI job derives the image tags from that line by
`sed`, so a bump cannot leave a tag pointing at the wrong revision. Today:

```
9f6893dd657feacc35ec5de03dad721c2e44170e   # tag 5.5.1.10
```

Pin the **commit**, not the tag: `5.5.1.10` is not a direct commit tag, so
`git clone --branch 5.5.1.10` warns while still landing on this revision.

**When you bump the pin**, re-read `commotest -h` — it is an internal test tool
with no CLI stability promise — and re-read
`takthirdparty/target-config/linux-amd64.mk` to confirm `commoncommo_BUILDTEST`
is still `yes`. The image's smoke test asserts the CLI still advertises
`estream:`, `sstream:`, `chatsend:`, `smpsend:` and `proto:`, so a rename fails
the build rather than a scenario.

### What gets built, and why not distro packages

`takthirdparty` builds the whole dependency chain from pinned distfiles, in
this order, and so does this image:

| Package | Version (verified from the pinned distfile) |
|---|---|
| zlib | 1.3.1 |
| libiconv | 1.15 |
| libxml2 | 2.13.3 |
| openssl | **openssl-3.0.14-quic1 (the quictls fork)** |
| nghttp2 | 1.62.1 |
| curl | 8.9.1 |
| ngtcp2 | 1.6.0 |
| protobuf | 3.21.0 |
| libmicrohttpd | 1.0.1 |

Using `libssl-dev`/`libxml2-dev`/`libprotobuf-dev` instead would be faster and
wrong, twice over:

1. `quicconnection.cpp` and `quicmanagement.cpp` are unconditionally in
   commoncommo's object list and link `-lngtcp2_crypto_quictls`, which needs
   the quictls QUIC-TLS API that stock OpenSSL does not provide. A system build
   means patching upstream's object list — ongoing maintenance on GPL sources.
2. More importantly, **the TLS stack is part of what we are testing.** ATAK's
   manual certificate verification, its `DEFAULT:!ECDH` cipher list on package
   transfers, and its PKCS#12 legacy algorithms all behave according to the
   linked OpenSSL. Swapping it changes the thing under test.

Three deliberate quirks in `build-takthirdparty.sh`:

- **The per-package loop is not an optimisation target.** takthirdparty's
  top-level Makefile lists the packages as sibling prerequisites of one goal
  with no ordering edges between them, so `make -j build_commoncommo` is free
  to start curl before openssl has installed. The loop keeps inter-package
  order serial and lets each package parallelise internally.
- **The host-protoc patch.** `mk/protobuf-common.mk` builds protoc for the
  build host with a bare `make` rather than `$(MAKE)`, so GNU make cannot hand
  it the jobserver and it falls back to `-j1` — the single dominant term in the
  build. The script rewrites that one recipe and then greps to prove the
  rewrite applied, so an upstream rewording fails loudly instead of silently
  costing a quarter of an hour.
- **The OpenSSL `no-tests` patch.** `mk/openssl.mk` invokes OpenSSL's
  `build_apps` target, but OpenSSL's own unified-build Makefile keeps
  `build_apps` only for backward compatibility: it and `build_tests` are both
  aliases for the shared `build_programs` target, which links *every*
  `PROGRAMS` entry Configure found — the `apps/openssl` CLI **and** the whole
  `test/` tree. commoncommo never touches those test binaries, and under `-j`
  they were racing a still-settling `libcrypto.a` and failing to link
  (`undefined reference to ossl_set_error_state`). The script patches
  `target-config/${TARGET}.mk`'s `openssl_CONFIG` line to add `no-tests`,
  OpenSSL's own Configure flag for dropping `test/` from the build outright,
  and greps to prove the patch applied. See
  [`M1-04b-openssl-build-fix.md`](../../.claude/plan/status/M1-04b-openssl-build-fix.md)
  for the full analysis.

Java/JNI (`libcommoncommojni.so`, `jcommoncommo.jar`) is switched off with
`commoncommo_BUILDJAVA=`, which keeps a JDK and Ant out of the builder. See the
comment at the top of `build-takthirdparty.sh` before re-enabling it.

### Building it locally

```bash
docker build --progress=plain -t rustak-interop-commoncommo interop/eud
docker run --rm rustak-interop-commoncommo -h     # full command listing
```

Expect **~25-45 minutes cold** on four cores; the source fetch itself is about
10 seconds (measured: ~2 s for the trees, ~8 s for the distfiles).

If a bump breaks the build, the levers — in the order worth trying — are:

- `--build-arg TTP_JOBS=1`, if a package in the chain has started misbehaving
  under `-j`.
- `-Werror`. commoncommo compiles with `-Wall -Werror` and is validated against
  older toolchains than bookworm's GCC 12. Passing `CXXFLAGS=` through to
  `commoncommo/core/impl` **replaces** the makefile's `+=`, so it must restate
  every include path the build needs; the exact incantation is recorded in
  M1-00 §2.4. Pinning the builder stage to `debian:bullseye-slim` (GCC 10) is
  the blunter alternative.
- The host-protoc build. It is the slowest step and the only one this recipe
  patches; M1-00 §2.4 also records how to pre-seed it from Debian's own
  `protobuf-compiler` (3.21.12 against the source's 3.21.0 — same minor,
  generated-code compatible) if it ever becomes the blocker.

CI builds it weekly (03:00 UTC Mondays) and on `workflow_dispatch`, not
nightly: the result only changes when the pin or the recipe does. See
`.github/workflows/nightly.yml`, job `interop-eud-image`.

## Running `commotest`

```
commotest <uid> <callsign> <output-dir> { <wait-seconds> <command> } ...
```

The image's `WORKDIR` is `/work`; mount a directory there and pass `/work` as
the output directory:

```bash
mkdir -p /tmp/eud-out
docker run --rm --network host -v /tmp/eud-out:/work \
  ghcr.io/sierrasoftworks/rustak-interop-commoncommo:latest \
  EUD-UID-1 ALPHA /work \
  0 'sstream:/work/client.p12:/work/truststore.p12:atakatak:127.0.0.1:8089' \
  30 quit
```

Notes that will save an afternoon:

- `commotest` broadcasts its own SA/PLI every 1000 ms by default, so simply
  connecting produces a steady, assertable stream. `safreq:<ms>` changes the
  cadence.
- **Always end a script with `quit`** — otherwise it runs forever — and wrap
  the run in an outer timeout anyway.
- Upstream's own documentation warns that the argument parser is not error
  tolerant and does not always catch bad arguments. A malformed scenario can
  look like a rustak failure; schema-validate scenario files before building
  argv, and always assert on a positive marker rather than only on the absence
  of an error.
- **There is no exit-code contract**: `commotest` returns 0 from every path,
  including argument errors. Assert on file contents.

### Assertion surface

`commotest` writes into its output directory as it goes:

| File | Contents |
|---|---|
| `commo-xml.txt` | every CoT message received *and accepted* by commoncommo, re-serialised to XML with a timestamp and the receiving endpoint id. Protobuf frames are decoded before this callback, so this file proves protobuf round-tripping end to end without the runner understanding protobuf. |
| `commo-log.txt` | the library log plus commotest's own events: protocol-negotiation outcomes, enrollment steps, interface up/down/error, contact add/remove, mission-package results |
| `commo-enroll-cert.p12` | the client keystore issued by `estream:`, so the runner can inspect the certificate rustak issued with ordinary tooling |

**Assert after the process exits.** The interface and mission-package callbacks
do not flush, so a mid-run read can miss lines that have already happened.

### Enrollment (`estream:`) trust semantics

`estream:<truststore>:<trustpass>:<keypass>:<clientpass>:<capass>:<user>:<pass>:<host>:<eport>:<port>:<versioninfo>`

| ATAK behaviour to reproduce | How to encode it |
|---|---|
| normal enrollment — hostname verification **off**, keeps the pre-existing truststore | prefix `<host>` with `-`, leave `<capass>` empty |
| quick connect — hostname verification **on**, adopts the CA chain the server returned | no `-` on `<host>`, `<capass>` non-empty |
| no server-certificate checking at all | `<truststore>` = `-` (requires a non-empty `<capass>`) |
| QR/token enrollment | **leave the `-` off `<user>`.** The `-` prefix selects `Authorization: Bearer`, but ATAK-CIV always sends the token as the **Basic password**. Use `<user>=<username>`, `<pass>=<token>` to match ATAK; use the `-` form only to test rustak's Bearer path deliberately. |

`<eport>` is the enrollment port, **8446** in ATAK — distinct from the mTLS
Marti listener on 8443. rustak must expose a Basic-auth, no-client-cert
enrollment listener there or `estream:` cannot run at all.

## Coverage matrix

**F** = faithful (the real ATAK code path) · **P** = partial · **—** = not
reachable from this harness.

| Area | | Notes |
|---|---|---|
| TLS handshake: client cert required, server verified against the enrollment truststore only, no hostname check, no ALPN | **F** | commoncommo's own manual-verify path |
| `DEFAULT:!ECDH` cipher list on mission-package transfers over HTTPS | **F** | **unique to this harness** — no other candidate reproduces it. Note it applies to the Marti/HTTPS listener (8443), *not* to the 8089 stream, which sets no cipher list at all. |
| Certificate enrollment: `/Marti/api/tls/config` → `signClient/v2`, CSR shape, banner stripping, PKCS#12 legacy algorithms, 201-is-a-failure | **F** | `estream:` |
| Bearer-token vs Basic enrollment auth | **F** | both reachable; default to Basic to match ATAK |
| Quick-connect vs normal trust semantics | **F** | see the table above |
| TAK Protocol v1 negotiation (`t-x-takp-v`/`-q`/`-r`), including refusal and both timeouts | **F** | the only candidate implementing ATAK's client state machine |
| Protobuf framing: `0xBF` + varint, 64 KiB cap, resync | **F** | resync/skip counters are logged |
| XML framing: `</event>` split, trailing-newline strip | **F** | |
| Ping/pong `t-x-c-t` at 15 s / 4.5 s / 25 s | **F** | absence of `Interface Error` over a long idle is the assertion |
| SA/PLI emission, server SA replay, contact add/remove | **F** | |
| GeoChat `b-t-f` send/receive, `<marti><dest>` unicast | **F** | `chatsend:` |
| Mission-package upload (3-step) and download (`b-f-t-r` → `b-f-t-a`) | **F** | `smpsend:` / server-pushed |
| Peer-to-peer MP via the server, local HTTPS MP server | **F** | `mpsend:` |
| `t-x-d-d` disconnect, `t-x-g-c` group-change **receipt** | **P** | commoncommo receives them; the *application* reaction (re-fetch groups, clear items) is ATAK-Java and absent here |
| Arbitrary authenticated Marti GET/PUT through ATAK's curl stack | **P** | `sfilesend:`/`sfilerecv:` reach any URL, but not the exact ATAK request shapes |
| Mission-dest CoT / server-control CoT **send** direction | **P** | present in the Commo API but not exposed by the CLI → cover from `interop/node-tak` or the Rust fake EUD; the receive direction is covered here |
| Device profiles (`/Marti/api/device/profile/*`, `.pref` import) | **—** | ATAK-Java. `interop/node-tak` + a rustak contract test |
| Channels / groups (`/Marti/api/groups/all`, `sendLatestSA`) | **—** | ATAK-Java. `interop/node-tak` |
| Data Sync / Mission API | **—** | ATAK-Java. `interop/node-tak` + `interop/cloudtak` |
| Contacts API, server version probe, OAuth / `/login/*` | **—** | `interop/node-tak` |
| QR-code / `.pref` import, UI wizard, map behaviour | **—** | manual checklist in `docs/compat/` |
| QUIC (8090, ALPN `takstream`) | — | `qstream:` exists, but QUIC is deferred in `plan.md` |

**Net:** this harness owns the *transport, crypto and CoT* half of the
compatibility contract. `interop/node-tak` owns the *HTTP/JSON* half. Neither
replaces the other; together they cover everything except the ATAK UI.

## Scenarios

`npm test` is the whole entry point. It runs the unit tests (the loader, the two
parsers and the assertion engine, against the fixtures in `fixtures/`) and then
the scenarios:

```bash
cd interop/eud
npm ci
npm test                          # unit tests, then the scenarios
npm run test:scenarios -- enroll   # one scenario, by name fragment
```

What a scenario run does, for each `scenarios/*.toml` in turn:

1. Starts a throwaway rustak in a scratch directory with that scenario's
   configuration overrides merged over the shared defaults
   (`interop/shared/src/launch.ts`), and waits for both the API and the stream
   listener — they do not bind at the same moment.
2. Bootstraps it through `/api/v1` — setup token, first administrator, a passkey
   registered with a software authenticator, the wizard — then creates the
   channels the scenario names, an account per EUD with its channel grants, and
   a **one-time enrolment token** for each.
3. Exports rustak's own CA as a PKCS#12 truststore, which is what `estream:`
   verifies the server against. `openssl` does that; nothing in Node writes
   PKCS#12.
4. Runs each EUD as `docker run --rm --network host -v <out>:/work <image>
   <argv>`, staggered by its `start_delay_seconds`, sampling
   `/Marti/api/clientEndPoints` while they run — by the time a container has
   exited, nobody is connected.
5. **After every container has exited**, reads `commo-log.txt`, `commo-xml.txt`
   and `commo-enroll-cert.p12` and applies the scenario's expectations. After,
   because the interface and mission-package callbacks do not flush.

A scenario whose server surfaces are not served yet **skips**, naming the brief
that will flip it (`src/surfaces.ts`), exactly as `interop/node-tak` does.

### Modes, and what happens without Docker

| Docker | `RUSTAK_EUD_REQUIRE_DOCKER` | Result |
|---|---|---|
| yes | anything | scenarios run for real |
| no | `1` | **the run fails loudly**, naming the image it needed |
| no | unset | probe-only: every scenario file is validated and the surfaces are probed, then everything skips |

The nightly job sets `RUSTAK_EUD_REQUIRE_DOCKER=1`, so a runner that lost its
container runtime fails rather than reporting a green run in which nothing ran.
Probe-only is what a developer gets, and it is enough to work on the loader, the
parsers and the assertions — which is what the unit tests cover.

Other environment variables: `RUSTAK_EUD_IMAGE` (default
`ghcr.io/sierrasoftworks/rustak-interop-commoncommo:latest`), `RUSTAK_EUD_DOCKER`
(for podman), `RUSTAK_EUD_OPENSSL`, `RUSTAK_EUD_ARTIFACTS` (where a failed
scenario's files are kept; default `interop/eud/artifacts/`), `RUSTAK_EUD_KEEP=1`
(keep them for passing scenarios too) and `RUSTAK_INTEROP_BINARY`.

### The night-one set

| Scenario | What it proves | Needs |
|---|---|---|
| `enroll-basic` | a one-time token enrols through `/Marti/api/tls/config` → `signClient/v2`, the stream comes up, protobuf is negotiated, the SA reaches `/Marti/api/clientEndPoints`, no `Interface Error` across 95 s (past the 25 s receive timeout, so it is the ping/pong assertion), and the issued keystore carries `CN=<username>` first | enrollment, stream, clientEndPoints |
| `two-eud-routing` | disjoint IN/OUT grants on one channel route one way and only one way, with `anon_group_default = false` so `__ANON__` cannot mask it | + channels |
| `chat-direct` | a unicast `b-t-f` reaches a connected peer, and bounces back as `b-t-f-s` when the peer has gone | enrollment, stream |
| `disconnect` | `remiface:` produces a `t-x-d-d` at the *other* EUD with a `<link>` naming the one that left | enrollment, stream |
| `negotiate-refused` | `[stream] negotiation = "refuse"` → ATAK's `using xml only` fallback, and XML still flowing between two EUDs | enrollment, stream |
| `negotiate-silent` | `[stream] negotiation = "silent"` → the 60 s `Timed out waiting for protocol version support message`, the connection kept, and no reconnect | enrollment, stream |
| `enroll-revoked` | revoking what was issued drops the live stream and makes the certificate unusable for a new one | + certificateRevocation (M2-08) |
| `mp-upload` | `smpsend:` succeeds over the `DEFAULT:!ECDH` curl context and rustak records the upload. **This is the `!ECDH` gate** | + missionPackages (M3-01) |
| `mp-download` | a peer-addressed `mpsend:` is relayed as `b-f-t-r` and fetched by the other EUD | + missionPackages (M3-01) |

### Writing one

A scenario is TOML, validated before anything starts — because `commotest`
returns 0 from every path, including an argument error, so a malformed script is
otherwise indistinguishable from a server that refused the connection. The
loader checks the keys and types, that `script` is `<wait-seconds> <command>`
pairs ending in `quit`, that every `{placeholder}` is one the runner fills, that
every expectation is a valid regular expression, and that every `requires` names
a surface the probe knows.

```toml
name = "enroll-basic"
summary = "..."
requires = ["enrollment", "stream", "clientEndPoints"]
timeout_seconds = 210          # the outer limit on a container
min_runtime_seconds = 90       # below this, the assertions prove nothing

[config.stream]                # merged over the launcher's defaults
negotiation = "accept"

[[euds]]
id = "alpha"                   # also its output directory
uid = "EUD-ENROLL-ALPHA"
callsign = "ALPHA"
username = "eud-alpha"         # the account the runner creates
channels = [{ group = "__ANON__", direction = "BOTH" }]
start_delay_seconds = 0
script = ["0", "estream:{truststore}:atakatak:atakatak:atakatak::{username}:{token}:-{host}:{enroll_port}:{stream_port}:rustak-interop", "95", "quit"]

[euds.expect]
log = ["...", "..."]           # regexes, matched IN ORDER
log_any = ["Interface Up"]     # in any order
log_absent = ["Interface Error"]
xml_present = [{ uid = "...", type = "a-f-G-U-C" }]
xml_absent = [{ uid = "..." }]           # uid, type and link_uid; any subset
enroll_cert_cn = "{username}"

[expect]                       # the server's own half
client_endpoints_present = ["ALPHA"]
audit_matches = ["uploaded"]
```

Placeholders: `{host}`, `{stream_port}`, `{enroll_port}`, `{marti_port}`,
`{username}`, `{token}`, `{truststore}`, `{work}`, `{uid}`, `{callsign}`. Every
EUD's mounted directory also holds `payload.dat`, for the mission-package
scenarios to send.

Two rules the scenarios follow:

- **Assert on a positive marker, not only on the absence of an error.** A run
  that never connected has no `Interface Error` either.
- **Ordered means ordered.** `log` is a sequence; use `log_any` when it is not.

Known traps, and what was done about them:

- `Interface Up` can log an unknown-interface description for
  enrollment-created interfaces, because the description map is populated after
  the callback can fire. The scenarios assert on the `Interface Up` prefix, not
  the description.
- `Interface Down` is *normal* at `quit`, so no scenario forbids it; only
  `Interface Error` is forbidden, and `disconnect` asserts the `Down` it wants.
- A served `/Marti/sync/missionquery` answers `404` for a hash it does not hold,
  which is exactly what a probe reads as "no such route" — so the enterprise-sync
  surface is probed at `missionupload`, which is POST-only and answers `405`.
- `stream:<host>:<port>` (plaintext TCP) is useful as a **negative** test —
  rustak has no plaintext listener by design.

One independent-witness enrollment smoke (pytak or goatak) is still worth having
in the nightly suite: it catches "we accidentally depend on libcurl quirks".
Neither should ever become the primary EUD — pytak has no `t-x-takp-*` handling
at all and sends protobuf unconditionally, which is the opposite of ATAK.

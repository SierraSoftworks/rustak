# M1-00 — Exploration result: the ATAK-side (EUD) interop harness

**Status:** decided. **Recommendation: Candidate 1 — build ATAK's own `commoncommo` from `atak-civ` at image-build
time and drive the stock `commotest` console client from a rustak-side scenario runner.** Candidate 2 (Android
emulator) is rejected on APK availability, not on runtime cost. Candidate 3 (pytak / takproto / goatak) is kept as a
*complement* for the HTTP surfaces commoncommo does not implement, never as the primary EUD.

Everything below is verified against the `atak-civ` tree at commit `9f6893dd657feacc35ec5de03dad721c2e44170e`
(tag `5.5.1.10`, the same checkout the `research/07` report used). File references are to that tree. GPL sources
were read for facts only; nothing is copied into rustak.

---

## 1. Decision in one paragraph

`commoncommo` is not merely "ATAK-like" — it *is* the code path ATAK uses for enrollment, the TLS stream, TAK
Protocol v1 negotiation, ping/pong, contacts, GeoChat and mission-package transfer (`research/07` §1.1, §3, §6).
It builds on Linux as a first-class upstream target, and upstream's own build system already produces a scripted
console client for that target: `takthirdparty/target-config/linux-amd64.mk` sets `commoncommo_BUILDTEST=yes` and
`commoncommo_BUILDJAVA=yes`, and `takthirdparty/ChangeLog.txt` release 4.2.3 (2025‑05‑15) records *"commoncommo
builds for Linux now build the `commotest` console application"*. `commotest` takes a timed command script on
argv and writes two machine-readable files (`commo-log.txt`, `commo-xml.txt`) that are a ready-made assertion
surface. So the harness needs **no GPL-derived code in the rustak tree at all** — only a `Dockerfile` that clones
a pinned upstream commit, plus scenario scripts (data) and a runner that shells out and parses text.

Decisive extra argument: **`commoncommo` is the only candidate that reproduces the `DEFAULT:!ECDH` cipher-list
constraint.** `SSL_CTX_set_cipher_list(sslCtx, "DEFAULT:!ECDH")` lives in
`commoncommo/core/impl/streamingsocketmanagement.cpp:421`, inside `configSSLForConnection()`, whose only callers
are `missionpackagemanager.cpp:2354` and `:2457` — i.e. the curl contexts for **mission-package upload to and
download from a TAK server**. `research/02` §A.9 flagged the `!ECDH` consequence as an inference "worth an
empirical test against a real device". This harness *is* that test, and it is the only one of the three candidates
that can run it. (Note for the design: it is **not** on the 8089 stream CTX — the stream sets no cipher list at
all — so the `openssl s_client -cipher 'DEFAULT:!ECDH'` gate in `plan.md` → Verification should point at the
**Marti/HTTPS listener (8443)**, not only at 8089.)

---

## 2. Build recipe

### 2.1 What has to be built, and why not from distro packages

`commoncommo/core/impl/Makefile` and `commoncommo/test/console/Makefile` both resolve their headers and libraries
from a single overridable variable `TAKTHIRDPARTYDIR`, so a "just use `libssl-dev`/`libxml2-dev`/`libprotobuf-dev`"
build looks attractive. **Do not do it.** The decisive fact: the OpenSSL that `takthirdparty` builds is
`openssl-openssl-3.0.14-quic1` — the **quictls fork**, not stock OpenSSL (verified by decoding the tar header of
`takthirdparty/distfiles/openssl.tar.gz`, whose sha256 `75b0d942…` matches the LFS pointer in the repo). Two
consequences:

1. `commoncommo/core/impl/quicconnection.cpp` and `quicmanagement.cpp` are unconditionally in `OBJS` and link
   `-lngtcp2_crypto_quictls`, which needs the quictls QUIC-TLS API. Stock distro OpenSSL 3.0–3.4 does not provide
   it, so a system build means patching the upstream `OBJS` list — ongoing maintenance on GPL sources.
2. More importantly, **the TLS stack is part of what we are testing.** `SSLv23_client_method()` +
   `SSL_VERIFY_NONE` + manual `X509_verify_cert` (`streamingsocketmanagement.cpp:80-85`, `:1971-1984`), the
   `DEFAULT:!ECDH` list, and the PKCS#12 legacy algorithms (`cryptoutil.cpp:260-265`, `:503-505`) all behave
   according to the linked OpenSSL. Swapping it for a different OpenSSL silently changes the thing under test.

Everything else in the chain is stock-but-pinned, so building it costs time once and buys exactness. The package
set is fixed by `takthirdparty/Makefile`:

```
packages_commoncommo-linux-amd64 = zlib libiconv libxml2 openssl nghttp2 curl ngtcp2 protobuf libmicrohttpd commoncommo
```

Verified versions (tar headers decoded from the pinned distfiles): zlib 1.3.1, libiconv 1.15, libxml2 2.13.3,
openssl 3.0.14‑quic1, protobuf **3.21.0**, libmicrohttpd 1.0.1 (nghttp2/curl/ngtcp2 are bz2, versions not probed).
Protobuf 3.21.0 matters: it is the runtime the generated `*.pb.cc` must match, and it is contemporaneous with
Debian bookworm's 3.21.12 — useful if the host-protoc shortcut in §2.4 is ever taken.

### 2.2 Source acquisition — measured, 8 s and ~55 MB

`atak-civ` is a ~2 GB repo with Git-LFS distfiles, but only `commoncommo/` and `takthirdparty/` are needed. A
blobless, shallow, cone-sparse clone was **measured in this session** at **2 s / 18 MB**, and the nine LFS
distfiles the commoncommo chain needs fetch in **6 s / ~37 MB** over plain HTTPS with hashes matching the LFS
pointers exactly.

Two traps found by doing it:

- The repo's `.gitattributes` registers LFS filters for `*.tar.gz`, `*.tar.bz2`, `*.zip`. If `git-lfs` is not
  installed the checkout **hard-fails** on `pluginsdk.zip` (`fatal: … smudge filter lfs failed`), even with
  `GIT_LFS_SKIP_SMUDGE=1`. Either install `git-lfs`, or disable the filter explicitly:
  `git -c filter.lfs.smudge=cat -c filter.lfs.process= -c filter.lfs.required=false …`.
- `git clone --branch 5.5.1.10` emits `warning: refs/tags/5.5.1.10… is not a commit!` (the tag object is not a
  direct commit tag) yet still lands on the right revision. **Pin the commit SHA, not the tag**:
  `9f6893dd657feacc35ec5de03dad721c2e44170e`.
- `libxml2.tar.xz` is **not** LFS-tracked (`.xz` is absent from `.gitattributes`), so it arrives with the clone.

### 2.3 Dockerfile sketch

```dockerfile
# syntax=docker/dockerfile:1
# interop/eud/Dockerfile — GPL test tool, built from upstream source at image-build time.
# Nothing from atak-civ is vendored into this repository.

ARG ATAK_CIV_SHA=9f6893dd657feacc35ec5de03dad721c2e44170e   # tag 5.5.1.10

FROM debian:bookworm-slim AS build
ARG ATAK_CIV_SHA
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
# Dependency list distilled from takthirdparty/ci-support/linux_cihost_prep.sh, minus the
# packages only gdal/spatialite need (swig, tcl, bison/flex, p7zip, jq, patchelf).
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git git-lfs \
      build-essential autoconf automake libtool patch make pkg-config \
      perl dos2unix bzip2 xz-utils zip \
      openjdk-17-jdk-headless ant \
 && rm -rf /var/lib/apt/lists/*
# ^ openjdk + ant are only needed for the optional JNI artefacts (libcommoncommojni.so +
#   jcommoncommo.jar), which expose the whole Commo API to a custom Java driver — useful only
#   if a scenario ever needs an API commotest does not expose (see the "Mission dest CoT" row
#   in §4). For the recommended CLI-only harness, drop them and pass commoncommo_BUILDJAVA=
#   to save ~250 MB and a couple of minutes. Note a Java driver WOULD link the GPL library,
#   so it must live in the image, never in this repository (§7).

WORKDIR /src
RUN git clone --filter=blob:none --no-checkout https://github.com/TAK-Product-Center/atak-civ.git . \
 && git sparse-checkout set --cone commoncommo takthirdparty \
 && git checkout "${ATAK_CIV_SHA}" \
 && git lfs pull --include "takthirdparty/distfiles/*" \
 && rm -rf .git/lfs

# Optional, ~10-20 min saving: let the host-protobuf sub-build use all cores.
# takthirdparty/mk/protobuf-common.mk invokes bare `make` (not `$(MAKE)`), so it runs -j1.
RUN sed -i 's|&& make && make install|\&\& make -j"$(nproc)" \&\& make install|' \
      takthirdparty/mk/protobuf-common.mk

# Build package-by-package, IN ORDER, each with -j. Do NOT run `make -j build_commoncommo`:
# the top-level lists the packages as sibling prerequisites of one target with no ordering
# edges between them, so a parallel top-level make would start curl before openssl installs.
WORKDIR /src/takthirdparty
RUN make TARGET=linux-amd64 prebuild \
 && for p in zlib libiconv libxml2 openssl nghttp2 curl ngtcp2 protobuf libmicrohttpd commoncommo; do \
        echo "=== $p ===" && make TARGET=linux-amd64 -j"$(nproc)" "$p" || exit 1; \
    done

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/takthirdparty/builds/linux-amd64-release/bin/commotest /usr/local/bin/commotest
COPY --from=build /src/takthirdparty/builds/linux-amd64-release/lib/ /opt/commoncommo/lib/
COPY --from=build /src/commoncommo/core/ChangeLog.txt /src/LICENSE.md /opt/commoncommo/
ENV LD_LIBRARY_PATH=/opt/commoncommo/lib
# GPLv3 §6 corresponding-source pointer (see §7).
LABEL org.opencontainers.image.source="https://github.com/TAK-Product-Center/atak-civ" \
      org.opencontainers.image.revision="${ATAK_CIV_SHA}" \
      org.opencontainers.image.licenses="GPL-3.0-only"
WORKDIR /work
ENTRYPOINT ["/usr/local/bin/commotest"]
```

Why the runtime stage is small: `takthirdparty` tracks every library in the commoncommo chain as a **static**
archive except `libiconv` (`zlib`, `libxml2`, `openssl`, `nghttp2`, `curl`, `ngtcp2`, `protobuf-lite` and
`libmicrohttpd` all use `$(LIB_STATICSUFFIX)`; `libiconv_libfile` uses `$(LIB_SHAREDSUFFIX)`), and `commoncommo`
itself is archived as `libcommoncommo.a` and linked into `commotest`. Some autotools packages may still drop a
`.so` alongside the `.a`, and the linker would prefer it — which is why the runtime stage copies the whole `lib/`
directory and sets `LD_LIBRARY_PATH` rather than trying to prove the binary is fully static. Expect
**~100–150 MB** for the `commotest`-only image, **~350–400 MB** if the JDK/jar variant is kept.

### 2.4 Verified escape hatches if the build misbehaves

| Symptom | Lever | Evidence |
|---|---|---|
| `-Werror` fails on a newer GCC | `make … CXXFLAGS="-I../include -I<ttp>/include -I<ttp>/include/libxml2 -D__STDC_FORMAT_MACROS -DCURL_STATICLIB -std=c++11 -Wall"` — a command-line `CXXFLAGS` **replaces** the makefile's `+=` (so you must restate the includes), which is how `-Werror` gets dropped | `commoncommo/core/impl/Makefile` uses `CXXFLAGS += … -Wall -Werror` |
| Host-protobuf build dominates the time | Pre-seed `builds/linux-amd64-release/host-protobuf/bin/protoc` from Debian's `protobuf-compiler` (3.21.12 vs source 3.21.0 — same minor, generated-code compatible) and `touch host-protobuf/.built` | `mk/protobuf-common.mk` gates on `$(protobuf_hosttools_dir)/.built`; `PROTOC` is also overridable in `core/impl/Makefile` |
| `git-lfs` unavailable / flaky | Fetch the 9 distfiles straight from `https://github.com/TAK-Product-Center/atak-civ/raw/<sha>/takthirdparty/distfiles/<f>` — **measured working**, and the sha256 matches the in-repo LFS pointer so the integrity check is free | measured this session |
| `javah` missing on JDK ≥ 10 | Already handled upstream: `commoncommo/jni/build.xml` target `jversion` selects `usejavacheaders` for any non-`1.x` `ant.java.version`, and `javac`'s `nativeheaderdir="native/headers"` supplies the headers | `jni/build.xml:86`, `:140-151` |

Note `commoncommo/core/impl/Makefile`'s `buildstampgen` shells out to `git rev-parse`; in a non-repo it fails
harmlessly because the pipeline's exit status comes from `tr`, and `versionimpl.cpp` falls back to `"undefined"`.

---

## 3. How CI drives it

### 3.1 `commotest`'s interface

```
commotest <uid> <callsign> <output-dir> { <wait-seconds> <command> } ...
```

It sends its own SA/PLI (`a-f-G-U-C`, callsign, `<__group name="Cyan" role="Team Member"/>`, `<takv>`-less,
`<track>`, `<status battery>`) via `broadcastCoT()` every 1000 ms by default — so simply connecting produces a
steady, assertable SA stream — and runs until the script reaches `quit` (otherwise forever; always end scripts
with `quit` and wrap in a CI timeout).

Commands that matter to us (`commoncommo/test/console/commotest.cpp:146-262`):

| Command | Exercises |
|---|---|
| `estream:<truststore>:<trustpass>:<keypass>:<clientpass>:<capass>:<user>:<pass>:<host>:<eport>:<port>:<versioninfo>` | full enrollment (`/Marti/api/tls/config` → `signClient/v2`) then auto-connect the TLS stream with the issued cert |
| `sstream:<p12>:<truststore>:<pass>[:<user>:<pass>]:<host>:<port>` | TLS stream with a pre-issued client cert; the 8-field form also sends the `<auth>` document |
| `stream:<host>:<port>` | plaintext TCP stream — useful as a **negative** test (rustak has no plaintext listener by design) |
| `safreq:<ms>` | SA cadence / load |
| `chatsend:<uid>` | GeoChat `b-t-f` with `__chat`/`chatgrp`/`link`, unicast via `<marti><dest>` |
| `smpsend:<file>:<ifaceNum>` | mission-package **upload** to the server over the `!ECDH` curl context |
| `mpsend:<file>:<uid>` | peer MP send, which the server relays / hosts |
| `remiface:<n>` | clean disconnect (drives the server's `t-x-d-d` to peers) |
| `sfilesend`/`sfilerecv:<file>^<p12>^<pass>[^<user>^<pass>]^<url>` | arbitrary authenticated file GET/PUT through ATAK's own curl stack |
| `proto:<cotxml>:<takprotofile>` | offline XML→TAK-Protocol-v1 encoder, for cross-checking `rustak-cot` framing |

**Enrollment-mode mapping** (this is the bit worth getting right — from `commotest.cpp:830-912` cross-checked with
`research/07` §1.2/§1.4):

| ATAK behaviour | `estream` encoding |
|---|---|
| normal enrollment (hostname verification **off**, keeps the pre-existing truststore) | prefix host with `-`, leave `<capass>` **empty** |
| quick connect (hostname verification **on**, adopts the CA chain the server returned) | no `-` on host, `<capass>` non-empty |
| no server-cert checking at all | `<truststore>` = `-` (requires a non-empty `<capass>`) |
| QR/token enrollment | **leave the `-` off `<user>`.** The `-` prefix selects `Authorization: Bearer`, but ATAK-CIV always passes `passIsToken=false` and sends the token as the **Basic password** (`research/07` §1.2). Use `<user>=<username>`, `<pass>=<token>` to match ATAK; use the `-` form only to test rustak's Bearer path deliberately. |

The enrollment port is a separate argument (`<eport>`), defaulting in ATAK to **8446**
(`SslNetCotPort.java:18-20`). rustak must therefore expose a Basic-auth, no-client-cert enrollment listener on
8446 distinct from the mTLS Marti listener on 8443 — the harness cannot enroll otherwise.

### 3.2 Assertion surface — no parsing of network traffic required

`commotest` writes two files into `<output-dir>` and flushes as it goes:

- **`commo-xml.txt`** — every CoT message *received and accepted by commoncommo*, re-serialised to XML with a
  timestamp and the receiving endpoint id (`cotMessageReceived`). Because protobuf frames are decoded before this
  callback, **this file proves protobuf round-tripping end to end** without the runner understanding protobuf.
- **`commo-log.txt`** — the library log plus commotest's own events. The lines an interop suite asserts on
  (exact format strings, `streamingsocketmanagement.cpp:1159-1255` and `commotest.cpp`):
  - `TakServer <ep> Proto Negotiate: Server supports protocol versions: <list>`
  - `TakServer <ep> Proto Negotiate: Requesting transition to protocol version 1`
  - `TakServer <ep> Proto Negotiate: Protocol negotiation request accepted, swapping proto version`
  - `… protocol negotiation request denied, using xml only` / `… Timed out waiting for protocol version support message - continuing as XML only` / `… Timed out waiting for protocol negotiation response, reconnecting...` (the three negative outcomes)
  - `EnrollUpdate: step <0|1|2> id <n> status <n> (<info>) …` — one line per `ENROLL_STEP_KEYGEN|CSR|SIGN`
  - `Completed status for enrollment <n>, adding stream connection`
  - `Interface Up: …` / `Interface Down: …` / `Interface Error: <code>`
  - `Contact Added: <uid>` / `Contact Removed: <uid>` — proves the server's SA replay and `t-x-d-d` handling
  - `Mission package <n> sent to TAK Server, result = SUCCESS|FAILED, reason: …`
  - `Receive of MP <file> result OK|FAIL …` — proves `b-f-t-r` → download
  - the issued client keystore is dropped at `<output-dir>/commo-enroll-cert.p12`, so the runner can inspect the
    certificate rustak issued (subject order, validity, chain) with ordinary tooling.

Caveat: `interfaceUp`/`interfaceDown`/`missionPackage*` do not `fflush`, so **assert after the process exits**
(script ends in `quit`), not while it runs.

### 3.3 Suggested shape of `interop/eud/`

```
interop/eud/
  Dockerfile                     # §2.3 — the only GPL-touching file, and it only clones
  scenarios/*.toml               # declarative: rustak config, commotest argv, expected log/xml assertions
  README.md                      # licensing note, how to rebuild/pin the image
  (runner)                       # a #[test] harness in rustak-server/tests/ or a small TS runner
```

The runner starts rustak with a test CA, `docker run --rm --network <net> -v <out>:/work ghcr.io/…:<tag> <argv>`,
waits for exit, then asserts against the two files. **The runner never links, includes or vendors commoncommo** —
process boundary + text files only. That is deliberate (see §7).

Scenarios worth having on night one, in dependency order:

1. `enroll-basic` — one-time enrollment token as Basic password → `signClient/v2` → stream up → negotiation
   accepted → SA appears in rustak → `t-x-c-t` ping answered (no `Interface Error` for 90 s > the 25 s rx timeout).
2. `enroll-revoked` — enroll, revoke the cert, `remiface` + re-add → handshake failure → `Interface Error`.
3. `negotiate-refused` — rustak answers `t-x-takp-r status="false"` → expect `using xml only`, and XML still flows.
4. `negotiate-silent` — rustak sends no `t-x-takp-v` → expect the 60 s timeout line and XML forever, connection kept.
5. `two-eud-routing` — two `commotest` containers in different groups; assert reachability (IN/OUT) in `commo-xml.txt`.
6. `chat-direct` — `chatsend` from A to B, assert delivery and the `b-t-f-s` bounce when B is absent.
7. `mp-upload` — `smpsend`, assert the three-step `missionquery`/`missionupload`/`metadata/<hash>/tool` sequence
   server-side **and** `result = SUCCESS` client-side. *This is the `DEFAULT:!ECDH` gate.*
8. `mp-download` — rustak pushes `b-f-t-r`; assert `Receive of MP … result OK` and byte-identical content.
9. `disconnect` — `remiface`, assert peers get `t-x-d-d` with the right `<link>`.

---

## 4. Scenario coverage matrix

Legend: **F** = faithful (real ATAK code path), **P** = partial/with caveats, **—** = not reachable.

| Scenario | commoncommo `commotest` | Notes / who covers the gap |
|---|---|---|
| TLS 1.2/1.3 handshake, client cert required, server verified against enrollment truststore only, no hostname check, no ALPN | **F** | the exact `SSLv23_client_method` + manual-verify path |
| `DEFAULT:!ECDH` cipher list (MP transfers over HTTPS) | **F** — *unique to this candidate* | `streamingsocketmanagement.cpp:421` via `missionpackagemanager.cpp:2354/:2457` |
| Certificate enrollment `/Marti/api/tls/config` → `signClient/v2` (CSR subject order, SHA-256, banner stripping, `<enrollment>`/`signedCert` parsing, PKCS#12 legacy algs, 201-is-a-failure) | **F** | `estream:` |
| Bearer-token vs Basic enrollment auth | **F** | both reachable; default to Basic to match ATAK |
| Quick-connect vs normal trust semantics | **F** | see the mapping table in §3.1 |
| TAK Protocol v1 negotiation (`t-x-takp-v/q/r`), incl. refusal and both timeouts | **F** | the only candidate that implements the client state machine as ATAK does |
| Protobuf framing `0xBF`+varint, 64 KiB cap, `0xBF` resync | **F** | resync/skip counters are logged |
| XML framing (`</event>` split, trailing-newline strip) | **F** | |
| Ping/pong `t-x-c-t` at 15 s / 4.5 s / 25 s | **F** | absence of `Interface Error` over a long idle is the assertion |
| SA/PLI emission and server SA replay → `Contact Added` | **F** | |
| GeoChat `b-t-f` send/receive, `<marti><dest>` unicast | **F** | `chatsend:` |
| Mission-package upload (3-step) and download (`b-f-t-r`→`b-f-t-a`) | **F** | `smpsend:` / server-pushed |
| Peer-to-peer MP via the server, local HTTPS MP server | **F** | `mpsend:`, `setMissionPackageLocalHttpsParams(8443,…)` is on by default |
| `t-x-d-d` disconnect, `t-x-g-c` group-change **receipt** | **P** | commoncommo receives them; the *application* reaction (re-fetch groups, clear items) is ATAK-Java, absent here |
| Arbitrary authenticated Marti GET/PUT through ATAK's curl stack | **P** | `sfilesend`/`sfilerecv` reach any URL, but not the exact ATAK request shapes |
| Mission dest CoT (`sendCoTToServerMissionDest`) / server control (`sendCoTServerControl`) | **P** | present in `commo.h:1130/:1150` but **not exposed by the `commotest` CLI** → cover the send direction from `interop/node-tak` or the Rust fake-EUD; the receive direction is covered here |
| **Device profiles** (`/Marti/api/device/profile/*`, `.pref` import, relative `cert/*.p12` paths) | **—** | ATAK-Java (`DeviceProfileOperation.java`), not commoncommo. Cover with `interop/node-tak` + a rustak contract test; goatak also hits `/Marti/api/tls/profile/enrollment` |
| **Channels / groups** (`/Marti/api/groups/all`, `sendLatestSA`) | **—** | ATAK-Java (`ServerGroup.java`). Covered by `interop/node-tak` |
| **Data Sync / Mission API** (create, token, subscribe, contents, changes, layers, logs) | **—** | ATAK-Java. Covered by `interop/node-tak` + `interop/cloudtak` |
| **Contacts API**, server version probe, OAuth/`/login/*` | **—** | `interop/node-tak` |
| QR-code / `.pref` import, UI wizard, map behaviour | **—** | manual checklist in `docs/compat/` |
| QUIC (8090, ALPN `takstream`) | available (`qstream:`) but out of scope per `plan.md` → Deferred |

**Net:** the EUD harness owns the *transport, crypto and CoT* half of Appendix A (A.1 streaming, the enrollment
half of A.3, the package half of A.3/A.4). `interop/node-tak` keeps owning the *HTTP/JSON* half. Neither replaces
the other, and together they cover everything in Appendix A except the ATAK UI.

---

## 5. Cost estimate

| Item | Estimate | Confidence |
|---|---|---|
| Source acquisition (clone + LFS) | **8 s, ~55 MB** | **measured** this session |
| Image build: zlib/libiconv/libxml2/nghttp2/ngtcp2/libmicrohttpd | ~5–8 min total | modelled (autotools, 4 vCPU, `-j4`) |
| Image build: openssl 3.0.14 `no-asm` (`build_libs build_apps` + `install_sw`) | ~5–9 min | modelled |
| Image build: protobuf 3.21 **twice** (host tools serial unless patched, then target) | ~10–14 min patched, **~20–30 min unpatched** | modelled — the dominant term |
| Image build: commoncommo itself (29 TUs / 27 229 lines + 10 generated `*.pb.cc` + `commotest.cpp`) | ~3–5 min | modelled from LOC |
| **Total image build (cold)** | **~25–45 min on a 4-vCPU `ubuntu-latest`** | **modelled — measure on the first real build** |
| Frequency of that cost | only when `ATAK_CIV_SHA` or the Dockerfile changes — upstream tags roughly quarterly | high |
| Nightly job: pull image + start rustak + 9 scenarios | **~6–12 min** (scenarios are wall-clock bound by `commotest`'s `wait-seconds` scripting and the 15/25 s ping windows, not by CPU) | modelled |
| PR cost | **zero** — this suite is nightly + `workflow_dispatch` only, per `plan.md` | — |
| Storage | ~100–150 MB per image tag in GHCR | modelled |

Caching: tag the image `ghcr.io/sierrasoftworks/rustak-interop-commoncommo:atak-civ-9f6893d` (short SHA) plus a
moving `:latest`. The nightly job consumes the pinned tag; a separate `workflow_dispatch`-only workflow builds and
pushes when the pin changes. Do **not** rebuild it in the nightly job.

**Rejected-candidate costs, for the record.** The Android emulator is *not* prohibitively slow any more — GitHub
enabled nested virtualisation on Linux runners in April 2024 and `reactivecircus/android-emulator-runner` boots in
~15 s with KVM (vs ~2 min 23 s without). It fails on **supply**: the `atak-civ` GitHub releases API returns exactly
one release (`5.5.1.8`) whose only asset is `ATAK-CIV-5.5.1.8-SDK.zip` — **no APK**. The APK is distributed via
Google Play, a tak.gov login, or the community mirror `files.civtak.org` (which itself just links to Play). So CI
would depend on a manually-uploaded binary in a secret store, kept in sync by hand, plus UI automation through
ATAK's EULA → permissions → device-setup wizard before any test can start. Cost is unbounded and the fidelity gain
over commoncommo is limited to the Java layer we can cover with `interop/node-tak` anyway.

---

## 6. Candidate 3 assessed (kept as a complement, not the EUD)

| | enrollment | v1 negotiation | ping/pong | `!ECDH` | verdict |
|---|---|---|---|---|---|
| **pytak 7.6.1** (Apache-2.0) | **yes** — `crypto_classes.py:CertificateEnrollment` does `/Marti/api/tls/config` and `signClient/v2`, ATAK-compatible PKCS#12 rewrite, and even builds `.pref`/data-package onboarding zips | **no** — `classes.py` has no `t-x-takp-*` handling at all; with `TAK_PROTO=1` it sends protobuf **unconditionally**, which is the opposite of ATAK | no | no | useful as a *second* enrollment client to prove rustak isn't accidentally curl-specific; useless for the stream state machine |
| **takproto 3.0.1** (MIT) | n/a | n/a — codec only | n/a | n/a | a codec library; possibly a cross-check oracle for `rustak-cot`, but see the provenance caveat in `research/02` → Licensing |
| **goatak** (kdudkov) | **yes** — `internal/client/enroll.go` does `tls/config`, `signClient/v2` **and `/Marti/api/tls/profile/enrollment`** | **yes** — `internal/client/client_handler.go:327/341/357` handles `t-x-takp-q/v/r` on both sides | partial | no | the best third-party client; worth a smoke job for the device-profile endpoint, but it is a reimplementation, not ATAK |

Recommendation: add **one** pytak-or-goatak enrollment smoke to the nightly suite as an independent witness
(it catches "we accidentally depend on libcurl quirks"), and leave everything protocol-shaped to commoncommo.
Do not let either become the primary EUD.

---

## 7. Licensing note

- `atak-civ/LICENSE.md` is **GPL-3.0**, unmodified body, no linking exception. `commoncommo/` has no separate
  licence file; `commoncommo/README-DISTRIBUTION.txt` is a *third-party notice* for ngtcp2/libcurl/OpenSSL, **not**
  a grant covering commoncommo (this matches `research/02` → Licensing).
- `commoncommo/test/console/README.txt` says *"This test application is for internal testing use only. Do not
  distribute."* That sentence is a developer note inside a GPL-3.0 work, not an additional restriction — but it is
  a clear signal to be conservative about republishing binaries.
- **What goes in the rustak tree:** a `Dockerfile` that clones a pinned upstream commit, declarative scenario
  files, a README, and a runner that invokes a container and parses text files. **No commoncommo source, no
  headers, no `.proto`, no generated code, no linking** — the runner talks to the tool across a process boundary,
  the same way CI talks to `openssl` or `curl`. rustak stays MIT and never becomes a GPL derivative.
- **Publishing the image.** Baking GPL binaries into a container and pushing it to a registry is *conveying* under
  GPLv3 §6, which requires corresponding source. Two acceptable options:
  1. **Preferred:** keep the GHCR package **private to the org** (internal use, not conveying) and let CI pull it
     with `GITHUB_TOKEN`.
  2. If it must be public: keep the `org.opencontainers.image.source` / `.revision` labels above, ship
     `LICENSE.md` inside the image, and state in `interop/eud/README.md` that the corresponding source is the
     named upstream commit plus the Dockerfile in this repository (a §6(b)/(d) style written offer). Also ship the
     third-party notices (`README-DISTRIBUTION.txt`) for the statically linked OpenSSL/curl/ngtcp2/protobuf.
- **Test vectors.** `commotest`'s `proto:` command can turn CoT XML into TAK-Protocol-v1 bytes. Wire bytes are
  facts, and using the tool as an *oracle* in CI is fine; **do not** commit its output as golden fixtures in
  `rustak-cot/tests/golden/` — `conventions.md` requires our fixtures to be our own. Compare live, don't vendor.
- Keep the existing rule: `atak-civ` is read for facts only; nothing from it — including comment text — is copied.

---

## 8. Open risks

| # | Risk | Severity | Mitigation / next action |
|---|---|---|---|
| 1 | **No build was actually executed.** This machine has no container runtime (`docker` absent; `podman` installed but has no VM — `podman machine list` is empty) and no usable C++ toolchain (Xcode licence not accepted, no `sudo`). Every build claim is inferred from the upstream makefiles, not observed. | **high** | first action of the implementation brief: run the §2.3 Dockerfile once, in CI or locally, and record the real wall-clock per package. Budget a day for `-Werror`/autotools fallout on GCC 12. |
| 2 | `-Werror` on GCC 12 (bookworm) against code last validated on older toolchains | medium | escape hatch in §2.4; if it bites hard, pin the builder to `debian:bullseye` (GCC 10) |
| 3 | `protobuf 3.21` autotools build is deprecated upstream and slow | medium | the host-protoc shortcut in §2.4; or accept the one-off cost |
| 4 | `takthirdparty` package targets have no inter-package ordering edges, so `make -j` at the top level is a latent, non-deterministic breakage | medium | the per-package loop in §2.3 — do not "optimise" it back into one parallel invocation |
| 5 | `commotest`'s parser is explicitly fragile (`README.txt`: "bad arguments are not always caught") — a malformed scenario can look like a rustak failure | medium | scenario files are schema-validated by the runner before it builds argv; always assert on a positive marker, never only on the absence of an error |
| 6 | `commotest` has no exit code contract (it `return`s from `run()` regardless) | medium | assert on file contents + an outer CI timeout; treat a missing `Interface Up` as failure |
| 7 | `Interface Up` can log `Unknown interface?!?` for enrollment-created interfaces (the description map is populated after the callback can fire) | low | assert on the `Interface Up:` prefix, not the description |
| 8 | A rustak `t-x-takp-v` sent before the connection settles could race the client's 60 s timer | low | scenario 4 exists specifically to pin this behaviour |
| 9 | Upstream may change `commotest`'s CLI between tags | low | the SHA pin makes bumps explicit; re-read `do_help()` on every bump |
| 10 | rustak must expose an enrollment listener on **8446** (Basic auth, no client cert) distinct from Marti on 8443, or `estream:` cannot run | medium | confirm against `design/03-identity-pki-acme-auth.md` before building the harness |
| 11 | The `openssl s_client -cipher 'DEFAULT:!ECDH'` gate in `plan.md` currently names :8089; the constraint actually applies to the **HTTPS/Marti** listener | low | correct the plan line when this brief is implemented |
| 12 | GPL hygiene drift — someone later "simplifies" the runner by linking `libcommoncommo.a` or vendoring a header | medium | state the process-boundary rule in `interop/eud/README.md` and in `conventions.md` → Licensing |

---

## 9. What was verified vs. inferred

**Verified by direct inspection of the pinned tree or by running a command in this session:**
`commotest` exists, is built for `linux-amd64` by upstream, and its full command set; the two output files and
their exact log format strings; the `estream`/`sstream` argument layouts and their trust semantics; the enrollment
callback flow and the `commo-enroll-cert.p12` artefact; the MP upload URL sequence and the `!ECDH` call sites; the
package list and versions for `build_commoncommo`; that the OpenSSL is the **quictls** fork; that the `.proto`
files compile cleanly (ran `protoc` over all ten — 0 errors); clone + LFS timings and sizes; the git-lfs smudge
trap; the absence of an APK in `atak-civ` releases; pytak's enrollment support and its **lack** of `t-x-takp-*`;
goatak's handling of both.

**Inferred, not measured:** all build durations; image sizes; the claim that GCC 12 compiles the tree without
`-Werror` trouble; nightly suite duration.

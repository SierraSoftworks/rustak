# M1-04 — `interop/eud`: the commoncommo/`commotest` CI image

**Status:** delivered, **unbuilt**. Every path, make target, distfile URL and sha256 below was verified against
the pinned upstream tree or by running a command in this session. **No container build was executed** — this
machine has no container runtime — so every wall-clock and image-size number is modelled, and §5 lists exactly
what the first real CI run has to confirm.

Productises `.claude/plan/status/M1-00-eud-interop-harness-exploration.md` (Candidate 1).

---

## 1. The pin

```
TAK-Product-Center/atak-civ @ 9f6893dd657feacc35ec5de03dad721c2e44170e     (tag 5.5.1.10)
```

Single source of truth: the `ARG ATAK_CIV_SHA=` line at the top of `interop/eud/Dockerfile`. The workflow
derives the published image tags from that line with

```
sed -n 's/^ARG ATAK_CIV_SHA=\([0-9a-f]\{40\}\)[[:space:]]*$/\1/p' interop/eud/Dockerfile | head -n 1
```

(verified to return the SHA against the file as written, and to fail the job with a legible message if the line
is ever reworded), so a bump cannot leave a tag pointing at the wrong revision. The commit is pinned rather than
the tag because `5.5.1.10` is not a direct commit tag.

## 2. What landed

| File | Role |
|---|---|
| `interop/eud/Dockerfile` | multi-stage: `builder` (debian:bookworm-slim + toolchain) → `runtime` (debian:bookworm-slim + `commotest` + any shared libs + licence paperwork) |
| `interop/eud/fetch-source.sh` | blobless, shallow, cone-sparse checkout of the pinned commit |
| `interop/eud/fetch-distfiles.sh` | resolves the Git-LFS distfiles over plain HTTPS, sha256- and size-verified against the in-tree pointers |
| `interop/eud/build-takthirdparty.sh` | ordered per-package build of the commoncommo chain, plus the host-protoc parallelism patch |
| `interop/eud/stage-runtime.sh` | assembles `/rootfs`, licence/notice/provenance files, and smoke-tests `commotest -h` inside the builder |
| `interop/eud/README.md` | licence posture, the image, how to run `commotest`, the assertion surface, the coverage matrix, the M2 scenario set |
| `.github/workflows/nightly.yml` | new job `interop-eud-image`; both existing `if: false` placeholders kept (comment/echo text refreshed now the M1-00 decision has landed) |
| `docs/interop.md` | docs-level map of the four interop suites and the nightly job graph |

Not created: scenario files and the runner. Those need rustak to issue certificates and to expose the 8446
enrollment listener, so they belong to M2 — `interop-eud` stays an `if: false` placeholder, as the brief asked.

## 3. Verified in this session

**Upstream facts** (`git ls-tree` / `git show` against the pinned tree):

- `takthirdparty/Makefile` — `packages_commoncommo-$(TARGET) = zlib libiconv libxml2 openssl nghttp2 curl
  ngtcp2 protobuf libmicrohttpd commoncommo`, in that order; `prebuild` creates `bin/ include/ lib/ java/`;
  every package is its own phony goal, so the per-package loop is a supported invocation.
- `target-config/linux-amd64.mk` — `commoncommo_BUILDTEST=yes` (so `commotest` is built) and
  `commoncommo_BUILDJAVA=yes` (overridden to empty by the build script to keep a JDK and Ant out of the image);
  `openssl_CONFIG` uses `./config … no-asm no-module`.
- `mk/commoncommo.mk` — `commotest` is installed to `$(OUTDIR)/bin/commotest`, i.e.
  `takthirdparty/builds/linux-amd64-release/bin/commotest`; `commoncommo_libfile` resolves to
  `libcommoncommo.a` unconditionally (`$(if commoncommo_BUILDSTATIC,…)` tests a literal, not a variable);
  `commoncommo_src=../commoncommo`, so the sparse checkout must contain both trees, and it aborts if
  `commoncommo/core/impl` already holds object files (`fetch-source.sh` checks this too, with a better message).
- `commoncommo/test/console/Makefile` — links `-lcommoncommo -lprotobuf-lite -lxml2 -lssl -lcrypto
  -lmicrohttpd -liconv -lngtcp2 -lngtcp2_crypto_quictls` plus `curl-config --libs`, and compiles with
  `-Wall -Werror`. `TAKTHIRDPARTYDIR` is `:=` but is overridden from the command line by `commoncommo.mk`.
- `commoncommo/core/impl/Makefile` — `PROTOC := $(TAKTHIRDPARTYDIR)/host-protobuf/bin/protoc`, so the host
  protobuf build is not optional; `buildstampgen` shells out to `git` and degrades harmlessly to an empty
  build stamp.
- `mk/protobuf-common.mk:64` — the host protoc recipe really is `… && make && make install` (bare `make`, so
  no jobserver, so `-j1`). The `sed` rewrite and its `grep` assertion were run against the real file and
  produce `… && make -j$$(nproc) && make install`; a scratch makefile confirmed make expands `$$(…)` through
  to the shell.
- `ci-support/linux_cihost_prep.sh` — the authoritative dependency list the Dockerfile's `apt-get` line is
  distilled from. Dropped: cmake, swig, tcl, bison, byacc, flex, p7zip, jq, patchelf (gdal/spatialite only),
  git-lfs (replaced), ant/JDK (Java build disabled), apg, zip. Kept and explicitly listed: `perl` (OpenSSL's
  Configure needs IPC::Cmd, which `perl-base` does not carry) and `dos2unix` (invoked directly by the curl,
  libxml2, protobuf, ngtcp2 and libmicrohttpd install recipes).
- `commoncommo/test/console/commotest.cpp` — argv contract, the full command list, `commo-log.txt` /
  `commo-xml.txt` / `commo-enroll-cert.p12` output paths, no terminal or stdin use (safe non-interactive), and
  `main()` returning 0 unconditionally (hence no exit-code contract, hence the output-based smoke test).
- Licence/notice paths that `stage-runtime.sh` copies all exist and all land in a cone-sparse checkout of
  `commoncommo takthirdparty`: `LICENSE.md`, `THIRDPARTY.md`, `commoncommo/README-DISTRIBUTION.txt`,
  `commoncommo/core/ChangeLog.txt` (cone mode always materialises the repository root).

**Measured by running it:**

- The exact source-acquisition recipe in `fetch-source.sh` (`git init` + `sparse-checkout set --cone` +
  `fetch --depth 1 --filter=blob:none origin <sha>` + `checkout FETCH_HEAD`, with the LFS filters disabled
  inline): **1.6 s, 18 MB on disk, 7.3 MB of it `.git`.** GitHub serves the bare SHA to `git fetch`, so no
  branch or tag is involved.
- `fetch-distfiles.sh` against that checkout: **8.3 s, 37 MB**, all nine files present, eight LFS blobs
  downloaded and sha256/size-matched against their in-tree pointers, `libxml2.tar.xz` correctly recognised as
  already-in-tree (`.xz` is absent from `.gitattributes`).
- All nine distfile URLs return 200 in the form
  `https://github.com/TAK-Product-Center/atak-civ/raw/<sha>/takthirdparty/distfiles/<name>` — the LFS ones
  redirect to `media.githubusercontent.com` and serve real content (verified end-to-end: `ngtcp2.tar.bz2`
  downloaded whole, sha256 `3e40cfb9…`, matching its pointer exactly), `libxml2.tar.xz` redirects to
  `raw.githubusercontent.com` and matches the local blob.
- Upstream versions, read from the downloaded tar headers — this **extends** M1-00, which had not probed the
  bz2 files: zlib 1.3.1, libiconv 1.15, libxml2 2.13.3, **openssl-openssl-3.0.14-quic1 (quictls confirmed)**,
  nghttp2 **1.62.1**, curl **8.9.1**, ngtcp2 **1.6.0**, protobuf 3.21.0, libmicrohttpd 1.0.1.
- `shellcheck -s bash interop/eud/*.sh` — clean (two intentional `SC2016` hits on the literal `$$` carry
  inline disables).
- `~/go/bin/actionlint .github/workflows/nightly.yml` — the **only** findings are the two pre-existing
  `if-cond` warnings on the `if: false` placeholders, which predate this brief and which the brief told me to
  keep. With those ignored the file is clean; the new job adds no findings. (The repo has no
  `.github/actionlint.yaml` and adding one was outside this brief's file scope.)

## 4. Expected build time and size

| Item | Estimate | Basis |
|---|---|---|
| Source acquisition (both layers) | **~10 s, ~55 MB** | **measured** |
| zlib, libiconv, libxml2, nghttp2, ngtcp2, libmicrohttpd | ~5-8 min | modelled (autotools, 4 vCPU, `-j4`) |
| openssl 3.0.14-quic1 `no-asm` | ~5-9 min | modelled |
| protobuf 3.21, host tools **and** target | ~10-14 min with the jobserver patch; ~20-30 min without | modelled — the dominant term, and the reason the patch exists |
| commoncommo + `commotest` | ~3-5 min | modelled from LOC |
| **Cold image build** | **~25-45 min on a 4-vCPU `ubuntu-latest`** | modelled; `timeout-minutes: 150` gives ~3× headroom |
| Warm build (ghcr layer cache hit, pin and scripts unchanged) | **~1-2 min** | modelled — pull + re-push of tags |
| Runtime image | ~100-150 MB | modelled; the whole chain is `--disable-shared`, so the payload should be `commotest` plus glibc/libstdc++ |
| Frequency of the cold cost | only when the pin or a script changes; upstream tags roughly quarterly | high confidence |
| PR cost | **zero** — weekly schedule + `workflow_dispatch` only | — |

## 5. What only a real CI run can confirm

Ordered by how likely each is to be the thing that breaks the first build.

1. **That it compiles at all.** commoncommo builds with `-Wall -Werror` and was last validated against older
   toolchains than bookworm's GCC 12. This is the single biggest unknown. Escape hatches (restated `CXXFLAGS`,
   or pinning the builder to `debian:bullseye-slim`/GCC 10) are in the README and M1-00 §2.4.
2. **Every wall-clock number in §4.** Nothing here has been observed. Record the real per-package times from
   the first `--progress=plain` build and replace the table.
3. **Whether the host-protoc patch actually pays.** The `-j1` fallback is inferred from GNU make's jobserver
   rules, not observed; the saving may be larger or smaller than modelled.
4. **Per-package `-j` safety.** OpenSSL's step passes five goals to one recursive make
   (`Makefile build_libs build_apps openssl.pc libssl.pc libcrypto.pc`); under `-j` that is the most plausible
   place for a parallelism race. `TTP_JOBS=1` is the lever, exposed as a build arg.
5. **Whether `/opt/commoncommo/lib` ends up empty.** Every library in the chain is configured
   `--disable-shared` (libiconv included, despite `libiconv.mk` naming its output `.so`), so `commotest`
   *should* need nothing from it. `stage-runtime.sh` copies whatever `.so*` exists rather than asserting;
   `PROVENANCE.txt` carries the `ldd` output so the first build answers this definitively.
6. **The runtime image's true size**, and whether `libstdc++6` was the only missing runtime package.
7. **Two upstream rules that produce a differently-named file than the makefile expects** —
   `libxml2_out_lib` resolves to `liblibxml2.a` and `libiconv_out_lib` to `libiconv.so`, neither of which
   autotools will install under those names. Make does not error when a recipe fails to create its target, so
   this is expected to be benign (the real `libxml2.a` is installed and `commotest` links `-lxml2`), but it
   means those two goals re-run on every invocation and it is worth eyeballing the first build log.
8. **GHCR behaviour:** that `GITHUB_TOKEN` can create the package on the first push, that
   `type=registry` `cache-to,mode=max` round-trips, and that the package's visibility ends up **private to the
   organisation** — which is the licence posture this brief assumes (see §6).
9. **The `git status` call inside `buildstampgen`** running against a partial clone inside the image. Expected
   to be fast and offline (the only dirty tracked file is the one `build-takthirdparty.sh` patches, whose blob
   is local), but it is the one place the build could try to reach the network mid-compile.
10. **`commotest -h` under the runtime image's `LD_LIBRARY_PATH`** — asserted twice (in the builder by
    `stage-runtime.sh`, and against the pushed image by the workflow's smoke step), but never actually run.

## 6. Licensing posture recorded

Unchanged from M1-00 §7, now written down where the code is (`interop/eud/README.md` → Licence posture) and
summarised in `docs/interop.md`:

- Nothing from atak-civ is vendored. The repository contains a Dockerfile that clones a pinned commit, scripts
  that drive upstream's own makefiles, and (from M2) a runner that talks to `commotest` across a **process
  boundary**. The README states the "do not link `libcommoncommo.a`, do not vendor a header" rule explicitly,
  because that is the drift this posture is exposed to (M1-00 risk 12).
- The image ships `LICENSE.md`, `THIRDPARTY.md`, `README-DISTRIBUTION.txt` and a `PROVENANCE.txt` naming the
  upstream revision under `/usr/share/doc/commoncommo/`, plus
  `dev.rustak.upstream.corresponding-source` pointing at that revision's tree — so the GPLv3 §6 offer is
  satisfied *if* the package is ever made public. **The intended posture remains option 1: keep the GHCR
  package private to the organisation**, which is internal use rather than conveying.
- `org.opencontainers.image.source` deliberately points at **rustak**, not at atak-civ, so GHCR links the
  package to this repository and `GITHUB_TOKEN` keeps write access; upstream provenance lives in the
  `dev.rustak.upstream.*` labels instead. This is a considered departure from M1-00 §2.3's sketch.
- `docker/metadata-action` is deliberately **not** used for this image: it would stamp
  `org.opencontainers.image.licenses` from the repository (MIT) onto a GPL-3.0 image.

## 7. Follow-ups for other briefs

- **M2 owns the scenarios and the runner** (`interop/eud/scenarios/*`, plus the runner), and with them the
  `interop-eud` job's `if: false`. The night-one set, the assertion markers and the known traps are written up
  in `interop/eud/README.md` → Scenarios.
- **rustak must expose a Basic-auth, no-client-cert enrollment listener on 8446**, distinct from the mTLS
  Marti listener on 8443, or `estream:` cannot run at all (M1-00 risk 10). Still to be confirmed against
  `design/03-identity-pki-acme-auth.md`.
- **`plan.md` → Verification** still points the `openssl s_client -cipher 'DEFAULT:!ECDH'` gate at :8089. The
  constraint applies to the **Marti/HTTPS** listener (8443); the stream sets no cipher list at all. M1-00 risk
  11 asked for this line to be corrected when this brief landed — it was **not** corrected here, because
  `plan.md` is outside this brief's file scope. Someone with that scope should fix it.
- **`docs/ci.md` → Nightly interop** now understates the workflow: it describes two `if: false` placeholders,
  where there are now three jobs, one of them active, and a second cron. `docs/ci.md` was outside this brief's
  file scope; `docs/interop.md` carries the accurate description in the meantime and `docs/ci.md` should link
  to it.
- Consider adding `.github/actionlint.yaml` with a `paths:` ignore for the placeholder `if-cond` warnings, so
  `actionlint .github/workflows/*.yml` can be a clean gate. Outside this brief's file scope.

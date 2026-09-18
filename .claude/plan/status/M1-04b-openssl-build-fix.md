# M1-04b — fix `interop-eud-image`'s OpenSSL link race

**Status:** patched, **unbuilt** (same constraint as M1-04: this machine has no container runtime, so the fix
below could not be exercised end to end — the next real signal is the nightly `interop-eud-image` job).

Follow-up to [`M1-04-interop-eud-image.md`](M1-04-interop-eud-image.md), which already flagged this exact
failure mode as the top entry in its "what only a real CI run can confirm" list (§5.4, "Per-package `-j`
safety").

## 1. The failure

GitHub Actions run `35342687191`, job **"Interop: EUD image (commoncommo)"** (`interop-eud-image`), failed
inside `interop/eud/build-takthirdparty.sh` while building the `openssl` package (openssl-3.0.14-quic1, the
quictls fork — see `interop/eud/README.md`'s package table):

```
make[2]: *** [Makefile:22164: test/asn1_dsa_internal_test] Error 1
mk/openssl.mk:18: openssl_build
```

with `undefined reference to ossl_set_error_state` from `libcrypto.a` at the link step. The library itself
built; only a test binary failed to link, under parallel make (`TTP_JOBS` defaults to `nproc`).

## 2. Root cause

`mk/openssl.mk`'s `openssl_build` recipe (line 18, confirmed against the pinned upstream tree — see M1-04 §1
for how to re-derive it, and the command at the end of this section for how it was actually re-checked here):

```makefile
openssl_build: $(openssl_configtouchfile)
	$(MAKE) -C $(OUTDIR)/$(openssl_srcdir) Makefile build_libs build_apps openssl.pc libssl.pc libcrypto.pc
```

This asks OpenSSL's own build for `build_apps`. In OpenSSL 3.0's unified build system
(`Configurations/unix-Makefile.tmpl` in the quictls tree, tag `openssl-3.0.14-quic1`), `build_apps` is **not**
what it sounds like:

```make
build_programs_nodep: $(PROGRAMS) $(SCRIPTS)

# Kept around for backward compatibility
build_apps build_tests: build_programs
```

`build_apps` and `build_tests` are both plain aliases for the shared `build_programs` target, which links
**every** entry in `$(PROGRAMS)` — and `$(PROGRAMS)` is populated from `$unified_info{programs}`, which
Configure assembles from every `build.info` file under every `SUBDIRS` it processes. The top-level
`build.info` in that same tree:

```make
SUBDIRS=crypto ssl apps util tools fuzz providers doc
IF[{- !$disabled{tests} -}]
  SUBDIRS=test
ENDIF
```

`test/` is only excluded from `SUBDIRS` when the `tests` feature is disabled. So asking for `build_apps`
— as `mk/openssl.mk` does — actually compiles and links **the whole `test/` tree**, including
`test/asn1_dsa_internal_test`, as a side effect. commoncommo never runs or needs any of these binaries; they
exist purely because OpenSSL's own default build graph doesn't let `build_apps` opt out of them.

Under `-j` (inherited via GNU make's jobserver from `build-takthirdparty.sh`'s outer
`make TARGET="$TARGET" -j"$JOBS" … "$pkg"` — see the per-package loop), one of these internal test binaries
linked against `libcrypto.a` while the archive was not yet in the state the linker needed, producing the
`undefined reference to ossl_set_error_state` failure. `ossl_set_error_state` is an ordinary internal
`libcrypto` symbol (present in the fully-built archive); nothing about this failure implicates commoncommo,
curl, ngtcp2 or any other package in the chain, and the library and its public API surface are unaffected —
only an unused-by-us test binary failed.

(`fuzz/`'s test programs are not part of this failure: they're gated behind
`IF[!$disabled{"fuzz-afl"} || !$disabled{"fuzz-libfuzzer"}]`, and both `fuzz-afl` and `fuzz-libfuzzer` are
disabled by default in `Configure`'s `%disabled` table, so they were never built regardless.)

Verified directly against the quictls source for this exact pinned version (`openssl-3.0.14-quic1`):

```
curl -sS https://raw.githubusercontent.com/quictls/openssl/openssl-3.0.14-quic1/Configurations/unix-Makefile.tmpl
curl -sS https://raw.githubusercontent.com/quictls/openssl/openssl-3.0.14-quic1/build.info
curl -sS https://raw.githubusercontent.com/quictls/openssl/openssl-3.0.14-quic1/Configure   # confirms `tests`
                                                                                              # is a real disablable,
                                                                                              # and that disabling
                                                                                              # it does not cascade
                                                                                              # to `apps`
```

`Configure`'s disable-cascade table has `"apps" => ["tests"]` (disabling `apps` cascades to disable `tests`)
but nothing the other way around, so disabling only `tests` leaves `apps` — and therefore `build_libs`,
`install_sw`, and everything commoncommo actually links against — untouched.

## 3. The fix

`interop/eud/build-takthirdparty.sh` now patches `target-config/${TARGET}.mk`'s `openssl_CONFIG` line (the
file that assembles the `./config …` invocation `mk/openssl.mk` runs) to append OpenSSL's own `no-tests`
Configure flag, using the same sed-then-grep pattern already established for the host-protoc patch:

```bash
sed -i 's|^openssl_CONFIG=\(.*\) no-asm no-module$|openssl_CONFIG=\1 no-asm no-module no-tests|' "target-config/${TARGET}.mk"
grep -q '^openssl_CONFIG=.* no-asm no-module no-tests$' "target-config/${TARGET}.mk" || {
    echo "build-takthirdparty: target-config/${TARGET}.mk no longer contains the openssl_CONFIG line this patches" >&2
    exit 1
}
```

Before this patch, `target-config/linux-amd64.mk` (the only target this image builds, `TARGET` defaults to
`linux-amd64`) reads:

```
openssl_CONFIG=./config --prefix=$(OUTDIR_CYGSAFE) --libdir=lib $(openssl_CFLAGS) -DPURIFY no-asm no-module
```

After the patch:

```
openssl_CONFIG=./config --prefix=$(OUTDIR_CYGSAFE) --libdir=lib $(openssl_CFLAGS) -DPURIFY no-asm no-module no-tests
```

**Why this fixes the failure, structurally rather than by luck:** with `tests` disabled, Configure never adds
`test/` to `SUBDIRS`, so `test/build.info` is never processed and none of its `PROGRAMS` (including
`asn1_dsa_internal_test`) are ever added to `$unified_info{programs}`. `build_programs_nodep: $(PROGRAMS)`
therefore has nothing from `test/` to build or link — the race is not "fixed" by making it less likely, the
racing target no longer exists. `build_apps` (still invoked exactly as before by `mk/openssl.mk`) now does
what its name says: it builds `apps/openssl` and nothing else. `build_libs` and the three `.pc` targets in the
same recipe are unaffected by `no-tests` — they don't depend on `SUBDIRS=test` at all — so `libssl.a`,
`libcrypto.a` and the `.pc` files commoncommo's own build reads are byte-for-byte what they would have been
before. `install_sw` (the next step, in `mk/openssl.mk`'s `$(openssl_out_libs)` rule) only ever installed
libraries, headers and `apps/openssl`; it never touched `test/`, so it is unaffected too.

This matches the two options the brief suggested, converging on the same outcome: `no-tests` is the "hook the
makefile offers" for excluding OpenSSL's own test/fuzz programs from `Configure`, and its effect —
`build_apps` only building `build_libs` + the real apps — is exactly the "build only `build_libs` + `install_sw`
if the install target allows it" alternative, achieved without having to fork the `mk/openssl.mk` recipe itself.

`README.md`'s "Two deliberate quirks" section (now three) gained a matching entry describing this patch, next
to the existing host-protoc one, so the pattern of "sed a specific line, then grep to prove it landed" stays
documented in one place for the next person who bumps the pin.

## 4. On capping OpenSSL's own parallelism

The brief also asked to cap OpenSSL's `-j` if the makefile passes it in explicitly and the race persists.
Checked directly: neither `mk/openssl.mk`, `mk/openssl-common.mk`, `target-config/linux-amd64.mk` nor the
top-level `takthirdparty/Makefile` contains a literal `-j` anywhere (`git grep -n -- "-j" -- 'takthirdparty/*'`
against the pinned tree returns nothing). OpenSSL's own submake only ever runs in parallel because
`openssl_build`'s recipe calls `$(MAKE)` (not a bare `make`), which lets it inherit GNU make's jobserver from
`build-takthirdparty.sh`'s outer `make TARGET="$TARGET" -j"$JOBS" … "$pkg"` — the exact same mechanism every
other package in the loop (zlib, curl, ngtcp2, …) already relies on for its own internal parallelism. There is
no OpenSSL-specific `-j` hook to patch that wouldn't also be a generic "cap every package's parallelism" change.

Given that, and given `no-tests` removes the specific racing target rather than just making the race less
likely, a separate parallelism cap was **not** added here — it would cost real wall-clock time (OpenSSL is one
of the two dominant packages per M1-04 §4) for a risk this change should have already eliminated. The existing,
already-documented and already-wired lever remains the right fallback if the nightly job still fails after this
patch: `--build-arg TTP_JOBS=1` (see `interop/eud/README.md` → "Building it locally", first item in the levers
list, and the `ARG TTP_JOBS=` / `ENV TTP_JOBS=` plumbing already in `interop/eud/Dockerfile`). If the next run
still shows `ossl_set_error_state` or a similar OpenSSL-internal link failure with `no-tests` in place, that
would mean the race lives in `build_libs` itself (the object-compile → `ar`/`ranlib` step for `libcrypto.a`),
not in a test binary racing a finished library — a materially different bug from the one this run's log shows,
and `TTP_JOBS=1` isolates whether OpenSSL specifically is still the culprit before spending more time on it.

## 5. Verification performed

- **`shellcheck -s bash interop/eud/*.sh`** — clean (`/opt/homebrew/bin/shellcheck`), no new findings; the
  pre-existing `SC2016` disables for the protobuf patch are untouched and still the only inline suppressions in
  the file.
- **The sed transform itself**, run against the real, pinned upstream file (not a hand-written approximation):
  fetched `takthirdparty/target-config/linux-amd64.mk` via
  `git -C <scratch>/refs/atak-civ show HEAD:takthirdparty/target-config/linux-amd64.mk` (that scratch checkout
  is pinned to `9f6893dd657feacc35ec5de03dad721c2e44170e`, the same `ATAK_CIV_SHA` this image builds), applied
  the exact `sed` command from the script to a scratch copy, and confirmed both the resulting line
  (`openssl_CONFIG=./config --prefix=$(OUTDIR_CYGSAFE) --libdir=lib $(openssl_CFLAGS) -DPURIFY no-asm no-module
  no-tests`) and the follow-up `grep` assertion pass.
- **`~/go/bin/actionlint .github/workflows/nightly.yml`** — unchanged from M1-04 §3: the only findings are the
  two pre-existing `if: false` placeholder warnings on `interop-eud` and `interop-cloudtak-suite`, which this
  brief didn't touch (only `interop/eud/**` changed) and which M1-04 already recorded as expected and kept.
- **Not done, because it can't be:** an actual container build. This machine has no container runtime, matching
  M1-04's own stated limitation. The next real signal is the first `interop-eud-image` run after this patch
  merges.

## 6. File scope

Only `interop/eud/build-takthirdparty.sh` and `interop/eud/README.md` were changed, plus this status file.
No other file under `interop/eud/` needed changes: `Dockerfile` already exposes `TTP_JOBS` as a build-arg
fallback, and `fetch-source.sh` / `fetch-distfiles.sh` / `stage-runtime.sh` are untouched by this failure.
No `git`/`but` operations were run — the working tree changes are unstaged, for the session that requested
this fix to review and commit.

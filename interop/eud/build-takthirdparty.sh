#!/usr/bin/env bash
# Build takthirdparty's commoncommo chain for linux-amd64, package by package,
# then commoncommo itself — which also produces the `commotest` console client,
# because target-config/linux-amd64.mk sets commoncommo_BUILDTEST=yes.
#
# Why a loop rather than `make -j build_commoncommo`: the top-level Makefile
# lists the packages as sibling prerequisites of a single goal with no ordering
# edges between them, so a parallel top-level make is free to start curl before
# openssl has installed. The loop keeps the inter-package order serial and lets
# each package parallelise internally. Do not "optimise" it back.
#
# Java/JNI (libcommoncommojni.so + jcommoncommo.jar) is switched off: the
# harness drives the CLI across a process boundary, and dropping it saves a JDK
# and Ant in the builder. Re-enable by dropping the commoncommo_BUILDJAVA
# override below and adding openjdk-17-jdk-headless + ant to the Dockerfile —
# but note a Java driver would *link* the GPL library, so it must never live in
# this repository (see README.md).
set -euo pipefail

SRC="${1:?usage: build-takthirdparty.sh <atak-civ-checkout>}"
TARGET="${TTP_TARGET:-linux-amd64}"
JOBS="${TTP_JOBS:-$(nproc)}"

cd "${SRC}/takthirdparty"

# mk/protobuf-common.mk builds protoc for the build host with a bare `make`
# rather than `$(MAKE)`, so GNU make cannot hand it the jobserver and it falls
# back to -j1 with a warning. That serial sub-build is the single dominant term
# in this image's wall clock (~20-30 min of a ~25-45 min build), so give it the
# cores back. `$$(nproc)` survives make's expansion and reaches the shell.
#
# The grep is the point of the exercise: if upstream rewords that recipe the
# build fails here, loudly, instead of silently costing a quarter of an hour.
# shellcheck disable=SC2016  # the literal $$ is the point: make expands it, the shell runs it
sed -i 's|&& make && make install|\&\& make -j$$(nproc) \&\& make install|' mk/protobuf-common.mk
# shellcheck disable=SC2016
grep -q 'make -j\$\$(nproc) && make install' mk/protobuf-common.mk || {
    echo "build-takthirdparty: mk/protobuf-common.mk no longer contains the host-protoc recipe this patches" >&2
    exit 1
}

# mk/openssl.mk's `openssl_build` recipe invokes `$(MAKE) -C … build_libs
# build_apps …`, but OpenSSL's own (Configure-generated) unified-build
# Makefile keeps `build_apps` only "for backward compatibility": both
# `build_apps` and `build_tests` are aliases for the shared `build_programs`
# target, which links *every* PROGRAMS entry Configure discovered — the
# `apps/openssl` CLI *and* the whole `test/` tree (asn1_dsa_internal_test and
# friends), since `build.info` only drops `SUBDIRS=test` when the `tests`
# feature is disabled. commoncommo never runs or needs those test binaries,
# and under -j they raced a still-settling libcrypto.a and failed to link
# (`undefined reference to ossl_set_error_state`). `no-tests` is OpenSSL's own
# Configure flag for exactly this: it removes `test/` from the build
# entirely, so `build_apps` goes back to building only the CLI — the libs and
# headers commoncommo actually links against are untouched.
#
# The grep is the point of the exercise: if upstream rewords the
# openssl_CONFIG line this patches, the build fails here, loudly, instead of
# racing intermittently in CI.
sed -i 's|^openssl_CONFIG=\(.*\) no-asm no-module$|openssl_CONFIG=\1 no-asm no-module no-tests|' "target-config/${TARGET}.mk"
grep -q '^openssl_CONFIG=.* no-asm no-module no-tests$' "target-config/${TARGET}.mk" || {
    echo "build-takthirdparty: target-config/${TARGET}.mk no longer contains the openssl_CONFIG line this patches" >&2
    exit 1
}

make TARGET="$TARGET" prebuild

OUT="${PWD}/builds/${TARGET}-release"

for pkg in zlib libiconv libxml2 openssl nghttp2 curl ngtcp2 protobuf libmicrohttpd commoncommo; do
    echo "=== takthirdparty: ${pkg} (-j${JOBS}) ==="
    if [ "$pkg" = openssl ]; then
        # OpenSSL is built serially, and it is the only package that is.
        # Two races have been seen under -j here: mk/openssl.mk installs with an
        # ungrouped multi-target rule (`$(openssl_out_libs): …` names both
        # lib/libssl.a and lib/libcrypto.a, so `install_sw` ran twice at once
        # and `install_dev` failed), and OpenSSL's own unified Makefile, handed
        # `build_libs build_apps …` as sibling goals, produced a truncated
        # object while `ar` was already reading it (`libcrypto.a: error reading
        # libcrypto-lib-ct_b64.o: file truncated`). -j1 removes both by
        # construction; the cost is a few minutes on an image that is rebuilt
        # only when its inputs change.
        make TARGET="$TARGET" -j1 commoncommo_BUILDJAVA= "$pkg"
        continue
    fi
    make TARGET="$TARGET" -j"$JOBS" commoncommo_BUILDJAVA= "$pkg"
done

test -x "${OUT}/bin/commotest" || {
    echo "build-takthirdparty: ${OUT}/bin/commotest was not produced" >&2
    exit 1
}
echo "build-takthirdparty: ${OUT}/bin/commotest built"

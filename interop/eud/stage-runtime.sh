#!/usr/bin/env bash
# Assemble the runtime image's payload: commotest, anything it needs to load at
# runtime, and the licence paperwork that has to travel with a GPL binary.
set -euo pipefail

SRC="${1:?usage: stage-runtime.sh <atak-civ-checkout> <rootfs-dir>}"
ROOT="${2:?usage: stage-runtime.sh <atak-civ-checkout> <rootfs-dir>}"
TARGET="${TTP_TARGET:-linux-amd64}"
OUT="${SRC}/takthirdparty/builds/${TARGET}-release"

install -Dm0755 "${OUT}/bin/commotest" "${ROOT}/usr/local/bin/commotest"

# takthirdparty configures every library in this chain with --disable-shared
# (libiconv included, despite libiconv.mk naming its output .so), and
# commoncommo itself is archived into libcommoncommo.a and linked in — so
# commotest is expected to need nothing from OUT/lib. Copy whatever shared
# objects were produced anyway instead of asserting a fully static link that
# would have to be re-proven on every upstream bump; LD_LIBRARY_PATH in the
# Dockerfile points at the result. The .a archives are deliberately left behind.
mkdir -p "${ROOT}/opt/commoncommo/lib"
find "${OUT}/lib" -maxdepth 1 -name '*.so*' -exec cp -a {} "${ROOT}/opt/commoncommo/lib/" \;

# GPLv3 §6 paperwork: the upstream licence, the third-party notices covering
# the statically linked ngtcp2/curl/OpenSSL, and the revision this binary was
# built from. See README.md → Licence posture.
install -Dm0644 "${SRC}/LICENSE.md"      "${ROOT}/usr/share/doc/commoncommo/LICENSE.md"
install -Dm0644 "${SRC}/THIRDPARTY.md"   "${ROOT}/usr/share/doc/commoncommo/THIRDPARTY.md"
install -Dm0644 "${SRC}/commoncommo/README-DISTRIBUTION.txt" \
                                         "${ROOT}/usr/share/doc/commoncommo/README-DISTRIBUTION.txt"
install -Dm0644 "${SRC}/commoncommo/core/ChangeLog.txt" \
                                         "${ROOT}/usr/share/doc/commoncommo/ChangeLog.txt"

{
    echo "upstream:  https://github.com/TAK-Product-Center/atak-civ"
    echo "revision:  ${ATAK_CIV_SHA:-unknown}"
    echo "built:     $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo
    echo "commotest dynamic dependencies:"
    ldd "${ROOT}/usr/local/bin/commotest"
} > "${ROOT}/usr/share/doc/commoncommo/PROVENANCE.txt"

# Smoke test. commotest has no exit-code contract — every path returns 0 — so
# assert on the output, and capture it first rather than piping into grep
# (pipefail plus grep -q would fail the build on commotest's own SIGPIPE).
help="$(LD_LIBRARY_PATH="${ROOT}/opt/commoncommo/lib" "${ROOT}/usr/local/bin/commotest" -h 2>&1 || true)"
for marker in 'estream:' 'sstream:' 'chatsend:' 'smpsend:' 'proto:'; do
    grep -qF "$marker" <<<"$help" || {
        echo "stage-runtime: commotest -h did not list '${marker}' — upstream CLI changed" >&2
        exit 1
    }
done

echo "stage-runtime: staged $(du -sh "$ROOT" | cut -f1) into ${ROOT}"

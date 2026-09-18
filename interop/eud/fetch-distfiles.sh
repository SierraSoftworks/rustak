#!/usr/bin/env bash
# Resolve the Git-LFS distfiles takthirdparty's commoncommo chain needs,
# without git-lfs.
#
# Each LFS-tracked distfile is checked out as a ~130-byte pointer carrying the
# blob's sha256 and size. This reads that pointer, pulls the real blob over
# plain HTTPS (github.com/<org>/<repo>/raw/<sha>/... redirects to
# media.githubusercontent.com and serves LFS content), and verifies size and
# digest before the bytes replace the pointer. The integrity check is therefore
# free and self-updating: bumping the pin brings new hashes with it, so there
# is nothing to keep in sync by hand.
set -euo pipefail

SRC="${1:?usage: fetch-distfiles.sh <atak-civ-checkout> <atak-civ-sha>}"
SHA="${2:?usage: fetch-distfiles.sh <atak-civ-checkout> <atak-civ-sha>}"
BASE="${ATAK_CIV_RAW_BASE:-https://github.com/TAK-Product-Center/atak-civ/raw}"

DIST="${SRC}/takthirdparty/distfiles"
POINTER_MAX=1024   # an LFS pointer is ~130 bytes; a real distfile is megabytes

# takthirdparty/Makefile:
#   packages_commoncommo-linux-amd64 =
#       zlib libiconv libxml2 openssl nghttp2 curl ngtcp2 protobuf
#       libmicrohttpd commoncommo
# The per-package .patch files are not LFS-tracked and arrive with the
# checkout, as does libxml2.tar.xz (.gitattributes covers .tar.gz, .tar.bz2 and
# .zip only). Those are size-checked here rather than downloaded.
FILES=(
    zlib.tar.gz
    libiconv.tar.gz
    libxml2.tar.xz
    openssl.tar.gz
    nghttp2.tar.bz2
    curl.tar.bz2
    ngtcp2.tar.bz2
    protobuf.tar.gz
    libmicrohttpd.tar.gz
)

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1   # BSD/macOS, for running this outside the image
    fi
}

size_of() { wc -c < "$1" | tr -d '[:space:]'; }

for name in "${FILES[@]}"; do
    path="${DIST}/${name}"
    test -f "$path" || { echo "fetch-distfiles: ${path} is missing from the checkout" >&2; exit 1; }

    if (( $(size_of "$path") > POINTER_MAX )); then
        printf 'fetch-distfiles: %-22s in tree already (%s bytes)\n' "$name" "$(size_of "$path")"
        continue
    fi

    want_oid="$(awk '$1 == "oid" { sub(/^sha256:/, "", $2); print $2 }' "$path")"
    want_size="$(awk '$1 == "size" { print $2 }' "$path")"
    if [[ ! "$want_oid" =~ ^[0-9a-f]{64}$ || ! "$want_size" =~ ^[0-9]+$ ]]; then
        echo "fetch-distfiles: ${path} is neither a distfile nor a readable LFS pointer" >&2
        exit 1
    fi

    curl --fail --silent --show-error --location \
         --retry 5 --retry-delay 2 --retry-all-errors \
         --output "${path}.tmp" "${BASE}/${SHA}/takthirdparty/distfiles/${name}"

    got_size="$(size_of "${path}.tmp")"
    got_oid="$(sha256_of "${path}.tmp")"
    if [[ "$got_size" != "$want_size" || "$got_oid" != "$want_oid" ]]; then
        echo "fetch-distfiles: ${name} does not match its LFS pointer" >&2
        echo "  want ${want_oid} (${want_size} bytes)" >&2
        echo "  got  ${got_oid} (${got_size} bytes)" >&2
        rm -f "${path}.tmp"
        exit 1
    fi

    mv "${path}.tmp" "$path"
    printf 'fetch-distfiles: %-22s %s bytes, sha256 ok\n' "$name" "$got_size"
done

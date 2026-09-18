#!/usr/bin/env bash
# Check out the two atak-civ trees the commoncommo chain needs — blobless,
# shallow and cone-sparse. Measured at ~2 s / 18 MB against github.com.
#
# Two upstream traps this works around:
#
#   * atak-civ's .gitattributes registers Git-LFS filters for *.tar.gz,
#     *.tar.bz2 and *.zip. Cone mode always materialises the repository root,
#     so without git-lfs installed the checkout hard-fails on the top-level
#     pluginsdk.zip even with GIT_LFS_SKIP_SMUDGE=1. The filters are therefore
#     disabled outright and fetch-distfiles.sh resolves the handful of pointers
#     the build actually consumes.
#   * The 5.5.1.10 tag is not a direct commit tag, so `--branch 5.5.1.10` warns
#     while still landing on the right revision. Fetch the commit instead —
#     GitHub serves arbitrary reachable SHAs to `git fetch`.
set -euo pipefail

DEST="${1:?usage: fetch-source.sh <dest-dir> <atak-civ-sha>}"
SHA="${2:?usage: fetch-source.sh <dest-dir> <atak-civ-sha>}"
REPO="${ATAK_CIV_REPO:-https://github.com/TAK-Product-Center/atak-civ.git}"

if [[ ! "$SHA" =~ ^[0-9a-f]{40}$ ]]; then
    echo "fetch-source: expected a full 40-character commit SHA, got '${SHA}'" >&2
    exit 1
fi

nolfs=(-c filter.lfs.smudge=cat -c filter.lfs.process= -c filter.lfs.required=false)

mkdir -p "$DEST"
cd "$DEST"
git init -q .
git remote add origin "$REPO"
git sparse-checkout init --cone
git sparse-checkout set commoncommo takthirdparty
git "${nolfs[@]}" fetch --depth 1 --filter=blob:none origin "$SHA"
git "${nolfs[@]}" checkout -q FETCH_HEAD

# takthirdparty/mk/commoncommo.mk aborts the build if the tree it copies from
# already contains object files. A fresh checkout never does; check anyway so a
# mounted or reused source directory fails here with a legible message.
shopt -s nullglob
stale=(commoncommo/core/impl/*.o)
shopt -u nullglob
if (( ${#stale[@]} )); then
    echo "fetch-source: ${DEST}/commoncommo/core/impl is not clean (${#stale[@]} object files)" >&2
    exit 1
fi

for required in takthirdparty/Makefile \
                takthirdparty/target-config/linux-amd64.mk \
                commoncommo/core/impl/Makefile \
                commoncommo/test/console/Makefile; do
    test -f "$required" || { echo "fetch-source: missing ${required}" >&2; exit 1; }
done

echo "fetch-source: atak-civ ${SHA} -> ${DEST}"

#!/usr/bin/env bash
# Fails when any Rust source file has more than MAX functional lines.
# Functional = not blank, not a comment line, and before the top-level `#[cfg(test)]`
# that introduces the trailing `mod tests` block. Test-only trees are exempt.
set -euo pipefail
MAX="${MAX_FUNCTIONAL_LINES:-300}"; status=0
while IFS= read -r file; do
  case "$file" in */tests/*|*/testing/*|*/fixtures/*|*_tests.rs) continue;; esac
  count=$(awk '
    /^#\[cfg\(test\)\]/ { exit }            # column-0 attribute: everything after is the test module
    /^[[:space:]]*$/       { next }          # blank
    /^[[:space:]]*\/\//    { next }          # // and /// and //! comments
    { n++ } END { print n+0 }' "$file")
  if [ "$count" -gt "$MAX" ]; then printf '%s: %d functional lines (limit %d)\n' "$file" "$count" "$MAX"; status=1; fi
done < <(git ls-files '*.rs' ':!:rustak-ui/dist')
exit $status

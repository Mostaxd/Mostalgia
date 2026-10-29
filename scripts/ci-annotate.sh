#!/usr/bin/env bash
# Runs a cargo command, keeps its output and turns compiler errors and
# failing tests into GitHub annotations, which can be read through the
# check-runs API without downloading the log. Exits with cargo's status.
#
#   scripts/ci-annotate.sh cargo build --workspace --message-format=short
set -o pipefail
log=$(mktemp)
"$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
if [ "$status" -ne 0 ]; then
  # `path:line:col: error: message` (from --message-format=short).
  grep -E '^[^ :]+:[0-9]+:[0-9]+: error' "$log" | head -50 | while IFS= read -r line; do
    file=${line%%:*}; rest=${line#*:}
    ln=${rest%%:*}; rest=${rest#*:}; rest=${rest#*:}
    echo "::error file=$file,line=$ln::${rest# }"
  done
  # One block with the first lines after each error and of the failing
  # tests, newlines encoded as %0A.
  block=$( { grep -E -A12 '^error(\[|:)' "$log" | head -150
             grep -E -A30 '^---- .* stdout ----|^failures:' "$log" | head -150; } |
           sed -e 's/%/%25/g' | awk '{printf "%s%%0A", $0}' | cut -c1-60000)
  if [ -n "$block" ]; then echo "::error::$block"; fi
fi
exit "$status"

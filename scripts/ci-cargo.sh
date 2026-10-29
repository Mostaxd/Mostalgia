#!/usr/bin/env bash
# Runs a cargo command in CI with its output in the log as usual, then turns
# compiler errors, warnings and failed tests into GitHub annotations: they
# show on the run's summary page and can be read through the checks API
# without downloading the log. Exits with cargo's status.
set -o pipefail
log="$(mktemp)"
CARGO_TERM_COLOR=never "$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
py="$(command -v python3 || command -v python)"
if [ -n "$py" ]; then
  "$py" "$(dirname "$0")/ci_annotate.py" "$log" "$status" || true
fi
exit "$status"

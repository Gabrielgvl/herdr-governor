#!/usr/bin/env bash
# check-herdr-schema.sh — Herdr schema-fixture gate (spec §10 P1).
# The fixture `herdr-api-schema.json` lives under a member's
# `tests/fixtures/` dir (Phase 0 hands it) and is pinned byte-for-byte by a
# `<fixture>.sha256` sidecar in the same directory. Modes:
#   (default)  the `just schema-check` ci gate: the fixture exists, parses
#     as JSON, has top-level .protocol == 22, and its sha256 matches the
#     sidecar pin. Missing fixture, missing pin, or any deviation is a
#     FAIL — a gate that cannot run is red, never skipped, so `just ci`
#     reports the same verdict locally and in CI.
#   --live     the `just schema-live` recipe (kept out of ci): the same
#     static checks plus a diff against `herdr api schema --json`. A
#     missing herdr binary is a FAIL, not a skip.
# More than one fixture copy is a FAIL — the fixture has exactly one home.
# For the selftest, HERDR_SCHEMA_LIVE_FILE=<path> feeds canned live output
# instead of the binary (only meaningful with --live).
# Exit 0 pass, 1 on FAIL, 2 on usage/environment error.
set -euo pipefail
export LC_ALL=C
shopt -s nullglob

live=0

usage() {
  printf 'usage: %s [--live]   (runs from anywhere inside the repo)\n' "$0" >&2
}

case "${1:-}" in
  -h | --help)
    usage
    exit 0
    ;;
  --live)
    live=1
    ;;
  "") ;;
  *)
    usage
    exit 2
    ;;
esac

if ! root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  usage
  printf 'check-herdr-schema: not a git repository\n' >&2
  exit 2
fi
cd "$root"

fixture_name="herdr-api-schema.json"
found=()
for f in "tests/fixtures/$fixture_name" */tests/fixtures/"$fixture_name"; do
  [ -e "$f" ] && found+=("$f")
done

if [ "${#found[@]}" -gt 1 ]; then
  printf 'FAIL SCHEMA fixture committed in more than one place: %s\n' "${found[*]}"
  exit 1
fi

if [ "${#found[@]}" -eq 0 ]; then
  printf 'FAIL SCHEMA fixture %s missing — the gate cannot run without it\n' "$fixture_name"
  exit 1
fi

fixture="${found[0]}"
printf 'SCHEMA fixture: %s\n' "$fixture"

# Static checks: parses as JSON, top-level .protocol == 22, and the bytes
# match the sha256 pinned in `<fixture>.sha256` (sha256sum output format:
# "<hex>  <repo-relative path>"; regenerating the fixture means
# regenerating the pin).
fixture_rc=0
FIXTURE="$fixture" python3 - <<'PYEOF' || fixture_rc=$?
import hashlib
import json
import os
import re
import sys

fixture = os.environ["FIXTURE"]

try:
    with open(fixture, encoding="utf-8") as fh:
        doc = json.load(fh)
except Exception as exc:
    print("FAIL SCHEMA fixture does not parse: %s" % exc)
    sys.exit(1)
if not isinstance(doc, dict) or "protocol" not in doc:
    print("FAIL SCHEMA fixture has no top-level 'protocol' field")
    sys.exit(1)
if doc["protocol"] != 22:
    print("FAIL SCHEMA fixture protocol %r != 22" % doc["protocol"])
    sys.exit(1)

pin_path = fixture + ".sha256"
try:
    with open(pin_path, encoding="utf-8") as fh:
        pin = fh.read().split()
except OSError as exc:
    print("FAIL SCHEMA sha256 pin unreadable: %s" % exc)
    sys.exit(1)
if len(pin) != 2 or not re.fullmatch(r"[0-9a-f]{64}", pin[0]):
    print("FAIL SCHEMA sha256 pin malformed (want '<hex>  <path>'): %s" % pin_path)
    sys.exit(1)
if pin[1] != fixture:
    print("FAIL SCHEMA pin path %r does not match fixture %r" % (pin[1], fixture))
    sys.exit(1)
with open(fixture, "rb") as fh:
    actual = hashlib.sha256(fh.read()).hexdigest()
if actual != pin[0]:
    print("FAIL SCHEMA fixture sha256 %s != pinned %s" % (actual, pin[0]))
    sys.exit(1)
print("SCHEMA fixture parses, protocol == 22, sha256 matches pin")
PYEOF
if [ "$fixture_rc" -ne 0 ]; then
  exit 1
fi

if [ "$live" -eq 0 ]; then
  exit 0
fi

# --live: diff the fixture against real `herdr api schema --json` output.
# The recipe exists to run this check, so no binary on PATH is a FAIL.
live_file=""
cleanup=""
if [ -n "${HERDR_SCHEMA_LIVE_FILE:-}" ]; then
  if [ ! -f "$HERDR_SCHEMA_LIVE_FILE" ]; then
    printf 'FAIL SCHEMA HERDR_SCHEMA_LIVE_FILE unreadable: %s\n' "$HERDR_SCHEMA_LIVE_FILE"
    exit 1
  fi
  live_file="$HERDR_SCHEMA_LIVE_FILE"
elif command -v herdr >/dev/null 2>&1; then
  live_file="$(mktemp)"
  cleanup="$live_file"
  trap 'rm -f "$cleanup"' EXIT
  if ! herdr api schema --json >"$live_file" 2>/dev/null; then
    printf 'FAIL SCHEMA herdr api schema --json failed\n'
    exit 1
  fi
else
  printf 'FAIL SCHEMA herdr not on PATH — live drift check cannot run\n'
  exit 1
fi

live_rc=0
FIXTURE="$fixture" LIVE_FILE="$live_file" python3 - <<'PYEOF' || live_rc=$?
import json
import os
import sys

with open(os.environ["FIXTURE"], encoding="utf-8") as fh:
    fixture = json.load(fh)
try:
    with open(os.environ["LIVE_FILE"], encoding="utf-8") as fh:
        live = json.load(fh)
except Exception as exc:
    print("FAIL SCHEMA live herdr output did not parse: %s" % exc)
    sys.exit(1)
if fixture == live:
    print("SCHEMA fixture matches live herdr api schema")
    sys.exit(0)
print("FAIL SCHEMA fixture drifted from live `herdr api schema --json` output")
sys.exit(1)
PYEOF
exit "$live_rc"

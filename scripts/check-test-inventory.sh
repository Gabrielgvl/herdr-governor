#!/usr/bin/env bash
# check-test-inventory.sh [--allow-empty-base] BASE_FILE HEAD_FILE —
# test-inventory ratchet (spec §10 P1). Both files are the output of
# `cargo nextest list --message-format json` (the justfile's test-inventory
# recipe produces them from a BASE worktree and the HEAD tree). A test that
# ran at BASE must still be present and scheduled at HEAD: missing -> FAIL,
# present but ignored -> FAIL, present but filter-mismatched -> FAIL. New
# tests are always fine.
# An empty or unparsable BASE inventory means the ratchet has nothing to
# check against — that is a FAIL, not a skip. The single exception is
# --allow-empty-base: the caller asserts the BASE commit has no Cargo.toml
# (a pre-workspace bootstrap seed), so the inventory is legitimately empty.
# The file must still be a well-formed nextest document with zero tests —
# an unreadable or unparsable file FAILs even under the flag.
# GOV_PROTECTED_OK=1 downgrades every FAIL to a loud OVERRIDE report —
# intentional test removals go through the owner label.
# Exit 0 clean, 1 on FAIL, 2 on usage error.
set -euo pipefail
export LC_ALL=C

usage() {
  printf 'usage: %s [--allow-empty-base] BASE_FILE HEAD_FILE\n' "$0" >&2
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi
allow_empty_base=0
if [ "${1:-}" = "--allow-empty-base" ]; then
  allow_empty_base=1
  shift
fi
if [ $# -ne 2 ]; then
  usage
  exit 2
fi
base_file="$1"
head_file="$2"
for f in "$base_file" "$head_file"; do
  if [ ! -f "$f" ]; then
    usage
    printf 'check-test-inventory: unreadable inventory file: %s\n' "$f" >&2
    exit 2
  fi
done

GOV_PROTECTED_OK="${GOV_PROTECTED_OK:-0}" python3 - "$base_file" "$head_file" "$allow_empty_base" <<'PYEOF'
import json
import os
import sys

base_path, head_path, allow_empty_flag = sys.argv[1:4]
override = os.environ.get("GOV_PROTECTED_OK") == "1"
allow_empty_base = allow_empty_flag == "1"

fails = []
reports = []


def fail(msg):
    if override:
        reports.append("REPORT INV OVERRIDE GOV_PROTECTED_OK=1 — %s" % msg)
    else:
        fails.append("FAIL INV %s" % msg)


def load_inventory(path):
    """Return ({key: running}, doc_ok) where key is package::binary::test and
    running means the test is neither ignored nor filter-mismatched, and
    doc_ok is True when the file is a `cargo nextest list
    --message-format json` document (a `rust-suites` map of suites ->
    testcases). The defensive JSON-lines fallback (records with
    type == "test") sets doc_ok False. A well-formed document listing zero
    tests — what the recipe writes for a manifest-less BASE — yields
    ({}, True); an empty or unparsable file yields ({}, False)."""
    with open(path, encoding="utf-8") as fh:
        raw = fh.read()
    inv = {}
    try:
        doc = json.loads(raw)
    except json.JSONDecodeError:
        doc = None
    if isinstance(doc, dict) and isinstance(doc.get("rust-suites"), dict):
        for suite in doc["rust-suites"].values():
            if not isinstance(suite, dict):
                continue
            pkg = suite.get("package-name", "")
            binary = suite.get("binary-name", "")
            if suite.get("status") != "listed":
                # An unlisted/unbuilt suite contributes no testcases; its
                # absence from the inventory is itself the signal.
                continue
            cases = suite.get("testcases")
            if not isinstance(cases, dict):
                continue
            for name, tc in cases.items():
                if not isinstance(tc, dict) or tc.get("kind") != "test":
                    continue
                running = (
                    not tc.get("ignored", False)
                    and tc.get("filter-match", {}).get("status") == "matches"
                )
                inv["%s::%s::%s" % (pkg, binary, name)] = running
        return inv, True
    # Fallback: JSON-lines with {"type": "test", ...} records.
    for line in raw.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rec = json.loads(line)
        except json.JSONDecodeError:
            continue
        if not isinstance(rec, dict) or rec.get("type") != "test":
            continue
        pkg = rec.get("package", "")
        binary = rec.get("binary", rec.get("binary_id", ""))
        name = rec.get("name", "")
        fm = rec.get("filter_match", rec.get("filter-match", {}))
        status = fm.get("status", "matches") if isinstance(fm, dict) else "matches"
        running = not rec.get("ignored", False) and status in ("matches", "run")
        inv["%s::%s::%s" % (pkg, binary, name)] = running
    return inv, False


try:
    base_inv, base_doc_ok = load_inventory(base_path)
except OSError as exc:
    print("check-test-inventory: cannot read %s: %s" % (base_path, exc))
    sys.exit(2)
try:
    head_inv, _ = load_inventory(head_path)
except OSError as exc:
    print("check-test-inventory: cannot read %s: %s" % (head_path, exc))
    sys.exit(2)

if not base_inv:
    if allow_empty_base and base_doc_ok:
        print(
            "check-test-inventory: BASE inventory legitimately empty "
            "(--allow-empty-base: BASE has no Cargo.toml)"
        )
    else:
        fail("BASE inventory has no tests — the ratchet has nothing to check")

for key in sorted(base_inv):
    if not base_inv[key]:
        continue  # already not running at BASE: pre-existing, grandfathered
    if key not in head_inv:
        fail("required test missing at HEAD: %s" % key)
    elif not head_inv[key]:
        fail("required test filtered or ignored at HEAD: %s" % key)

required = sum(1 for v in base_inv.values() if v)
added = sum(1 for k in head_inv if k not in base_inv)
for r in reports:
    print(r)
for f in fails:
    print(f)
print(
    "check-test-inventory: %d fail(s), %d report(s) "
    "(%d required at BASE, %d at HEAD, %d added)"
    % (len(fails), len(reports), required, len(head_inv), added)
)
sys.exit(1 if fails else 0)
PYEOF

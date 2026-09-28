#!/usr/bin/env bash
# check-owner-approval.sh REPO PR HEAD_SHA LABEL — head-bound approval
# verdict (spec §10 P1). Prints APPROVED / STALE / ABSENT on stdout:
#   APPROVED  LABEL is on the PR and the latest `owner-approval` check run
#             on HEAD_SHA concluded `success`, counting only runs the
#             `github-actions` app wrote — a same-named check from any
#             other app is ignored. The stamp is made by the base-defined
#             pull_request_target workflow
#             (.github/workflows/owner-approval.yml) only for an
#             owner-approved labeled event at that head while the label
#             remains present. Unrelated label events cannot mint a stamp
#             or cancel revocation, so APPROVED means the current
#             head is the reviewed commit — not merely a commit whose
#             (forgeable) committer date predates the label. Provenance is
#             app-level, not workflow-level: a PR-added workflow could
#             post a same-named check as github-actions — it would have to
#             survive owner review of a hard-protected .github/workflows
#             edit (docs/guardrails.md).
#   STALE     LABEL is on the PR but HEAD_SHA carries no successful
#             binding: the label predates the latest push (the workflow
#             revokes it on synchronize and records a `failure` check on
#             the moved head), or the bind run has not landed yet
#   ABSENT    LABEL is not on the PR
# Live mode shells out to `gh api` (PR label list + the HEAD_SHA check
# runs). For tests, set GOV_APPROVAL_LABELS_FILE and
# GOV_APPROVAL_CHECKS_FILE to fixture paths holding the same JSON shapes
# (a flat or --slurp page-array of label objects; a check-runs response
# object, bare array, or --slurp array of page objects) — the verdict
# logic is identical and never touches the network.
# Exit 0 APPROVED, 1 STALE/ABSENT, 2 usage/transport error (fail closed:
# an error is never read as approval).
set -euo pipefail
export LC_ALL=C

usage() {
  printf 'usage: %s REPO PR HEAD_SHA LABEL\n' "$0" >&2
}

if [ "${1:-}" = "-h" ] || [ "${1:-}" = "--help" ]; then
  usage
  exit 0
fi
if [ $# -ne 4 ]; then
  usage
  exit 2
fi
repo="$1"
pr="$2"
head_sha="$3"
label="$4"
check_name="owner-approval"

command -v python3 >/dev/null 2>&1 || {
  printf 'check-owner-approval: python3 missing\n' >&2
  exit 2
}

labels_file="${GOV_APPROVAL_LABELS_FILE:-}"
checks_file="${GOV_APPROVAL_CHECKS_FILE:-}"
tmpdir=""

if [ -n "$labels_file" ] || [ -n "$checks_file" ]; then
  # Fixture mode: both files are required together so a half-wired override
  # can never masquerade as a live verdict.
  for f in "$labels_file" "$checks_file"; do
    if [ -z "$f" ] || [ ! -f "$f" ]; then
      usage
      printf 'check-owner-approval: unreadable fixture file: %s\n' "${f:-<unset>}" >&2
      exit 2
    fi
  done
else
  command -v gh >/dev/null 2>&1 || {
    printf 'check-owner-approval: gh not on PATH\n' >&2
    exit 2
  }
  tmpdir="$(mktemp -d)"
  trap 'rm -rf "$tmpdir"' EXIT
  labels_file="$tmpdir/labels.json"
  checks_file="$tmpdir/checks.json"
  if ! gh api --paginate --slurp "repos/$repo/issues/$pr/labels" >"$labels_file" 2>/dev/null; then
    printf 'check-owner-approval: gh api labels failed\n' >&2
    exit 2
  fi
  # `--method GET` is load-bearing: `gh api` switches to POST whenever
  # -f/-F fields are present without an explicit method, which turns this
  # read into a rejected write — every PR would fail closed. `--paginate
  # --slurp` yields an array of check-runs page objects.
  if ! gh api --method GET --paginate --slurp "repos/$repo/commits/$head_sha/check-runs" -f check_name="$check_name" -f filter=latest >"$checks_file" 2>/dev/null; then
    printf 'check-owner-approval: gh api check-runs failed\n' >&2
    exit 2
  fi
fi

LABELS_FILE="$labels_file" CHECKS_FILE="$checks_file" LABEL="$label" CHECK_NAME="$check_name" python3 - <<'PYEOF'
import json
import os
import sys


def die(msg):
    sys.stderr.write("check-owner-approval: %s\n" % msg)
    sys.exit(2)


def load_list(env_name):
    """A flat JSON array, or a --paginate --slurp array of page arrays."""
    try:
        with open(os.environ[env_name], encoding="utf-8") as fh:
            doc = json.load(fh)
    except Exception as exc:
        die("%s JSON did not parse: %s" % (env_name, exc))
    if not isinstance(doc, list):
        die("%s is not an array" % env_name)
    items = []
    for item in doc:
        if isinstance(item, list):
            items.extend(item)
        else:
            items.append(item)
    return items


def load_check_runs(env_name):
    """The check-runs response object ({"check_runs": [...]}), a bare
    array of check-run objects, or a --paginate --slurp array of page
    objects."""
    try:
        with open(os.environ[env_name], encoding="utf-8") as fh:
            doc = json.load(fh)
    except Exception as exc:
        die("%s JSON did not parse: %s" % (env_name, exc))
    if isinstance(doc, dict):
        doc = [doc]
    if not isinstance(doc, list):
        die("%s has no check_runs array" % env_name)
    runs = []
    for item in doc:
        if isinstance(item, list):
            runs.extend(item)
        elif isinstance(item, dict):
            page = item.get("check_runs")
            if isinstance(page, list):
                runs.extend(page)
            elif "check_runs" in item:
                die("%s check_runs is not an array" % env_name)
            else:
                runs.append(item)
        else:
            die("%s has a non-object element" % env_name)
    return runs


labels = load_list("LABELS_FILE")
check_runs = load_check_runs("CHECKS_FILE")
label = os.environ["LABEL"]
name = os.environ["CHECK_NAME"]

if not any(isinstance(l, dict) and l.get("name") == label for l in labels):
    print("ABSENT")
    sys.exit(1)

# The check runs were fetched at HEAD_SHA (check_name + filter=latest), so
# a verdict here is bound to exactly that commit — a success recorded on
# an older head never enters this view. Only runs the `github-actions`
# app wrote count: a same-named check from any other app is not the bind
# workflow's stamp.
bound = [
    r
    for r in check_runs
    if isinstance(r, dict)
    and r.get("name") == name
    and (r.get("app") or {}).get("slug") == "github-actions"
]
if not bound:
    sys.stderr.write(
        "check-owner-approval: %s label present but no %s check on this head\n"
        % (label, name)
    )
    print("STALE")
    sys.exit(1)
bound.sort(key=lambda r: r.get("completed_at") or r.get("started_at") or "",
           reverse=True)
latest = bound[0]
if latest.get("status") != "completed" or latest.get("conclusion") != "success":
    sys.stderr.write(
        "check-owner-approval: %s check on this head is %r/%r\n"
        % (name, latest.get("status"), latest.get("conclusion"))
    )
    print("STALE")
    sys.exit(1)
print("APPROVED")
sys.exit(0)
PYEOF

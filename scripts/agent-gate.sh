#!/usr/bin/env bash
# agent-gate.sh — shared PreToolUse gate (plan §3 command-level rules).
# stdin: hook event JSON: {"tool_input":{"file_path"|"path": ..., "command": ...}}
# Verdict: deny -> exit 2 + Claude/Devin deny JSON on stdout; allow -> exit 0, silent.
# Fail-open on unparsable stdin or a broken interpreter (hooks are advisory —
# check-protected-diff.sh/check-lint-integrity.sh are the enforcement layer).
# Used by: .claude/settings.json, .codex/hooks.json, .devin/hooks.v1.json,
#          .pi/extensions/agent-gate.ts (B5 wires all four to this contract).
# This repo is public with CI from day one: gh credentials are in scope, so
# forge self-approval (the owner-approved label) and merges are denied here
# too — they are owner-only actions, same class as git push.
set -euo pipefail
export LC_ALL=C

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
policy="$script_dir/protected-paths.txt"

input="$(cat)"

verdict=""
if out="$(
  python3 - "$policy" "$input" 2>/dev/null <<'PYEOF'
import json
import os
import re
import sys


def emit(msg):
    sys.stdout.write(msg + "\n")
    sys.exit(0)


policy_path = sys.argv[1]
raw = sys.argv[2] if len(sys.argv) > 2 else ""

try:
    event = json.loads(raw)
except Exception:
    sys.exit(0)
if not isinstance(event, dict):
    sys.exit(0)
ti = event.get("tool_input")
if not isinstance(ti, dict):
    ti = {}
path = ti.get("file_path") or ti.get("path") or ""
cmd = ti.get("command") or ""
if not isinstance(path, str):
    path = ""
if not isinstance(cmd, str):
    cmd = ""

hard, cond = [], []
try:
    with open(policy_path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            if parts[0] == "hard":
                hard.append(parts[1].strip())
            elif parts[0] == "conditional":
                cond.append(parts[1].strip())
except OSError:
    sys.exit(0)


def norm(p):
    return os.path.normpath(p.strip().strip('"').strip("'").strip())


def pat_re(pat):
    out, i = [], 0
    while i < len(pat):
        c = pat[i]
        if c == "*":
            if pat[i:i + 2] == "**":
                out.append(".*")
                i += 2
            else:
                out.append("[^/]*")
                i += 1
        elif c == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(c))
            i += 1
    return "".join(out)


def path_matches(p, pat):
    if not p:
        return False
    return re.search("(?:^|/)" + pat_re(pat) + "$", p) is not None


def mentions(p, pat):
    # a bare mention of a "dir/**" pattern's directory counts too
    if path_matches(p, pat):
        return True
    if "**" in pat:
        base = pat.split("**", 1)[0].rstrip("/")
        if base:
            return re.search("(?:^|/)" + pat_re(base) + "(?:/|$)", p) is not None
    return False


p = norm(path) if path else ""

for pat in hard:
    if path_matches(p, pat):
        emit("protected path: %s requires owner review" % pat)
for pat in cond:
    if path_matches(p, pat) and os.path.exists(p):
        emit("protected path (exists): %s requires owner review" % pat)

# `cargo insta` tolerates cargo's global flags (`+<toolchain>`, `--config`,
# `-Z`, `--verbose`, ...) between `cargo` and `insta` — same command.
CARGO_INSTA = (
    r"(?:cargo-insta|\bcargo\s+"
    r"(?:(?:[+@][^\s|;&]+|-{1,2}\w[\w-]*(?:[=\s][^\s|;&]*)?)\s+)*insta)\b"
)
FORBIDDEN = [
    (CARGO_INSTA + r"\s+accept\b", "insta accept is snapshot self-acceptance"),
    (CARGO_INSTA + r"[^|;&]*--accept", "insta --accept is snapshot self-acceptance"),
    (CARGO_INSTA + r"\s+review\b", "insta review is interactive snapshot acceptance"),
    (r"insta_update\s*=\s*[\"']?(always|force|unseen|new)\b",
     "INSTA_UPDATE=always/force/unseen/new (quoted or not) is snapshot self-acceptance"),
    (r"\bgit\s+push\b", "git push: deliverable is an uncommitted diff for owner review"),
    (r"--no-verify\b", "--no-verify bypasses hooks"),
    (r"\bgit\s+rm\b[^|;&]*\btest", "git rm on tests is test deletion"),
    (r"core\.hookspath", "mentioning core.hooksPath repoints git hooks"),
    (r"\bgit_config_(count|key_\d+|value_\d+|parameters|global|system)",
     "GIT_CONFIG_* env vars inject git config outside the gated flags"),
    (r"\bgit\s+update-index\b[^|;&]*(--skip-worktree|--assume-unchanged)",
     "git update-index hides changes from review"),
    (r"gov_protected_ok", "GOV_PROTECTED_OK is the owner-approval signal — agents must never set it"),
    (r"\bgh\b[^|;&]*\bowner-approved\b",
     "the owner-approved label is the owner's approval signal — agents never set or remove it"),
    (r"\bgh\s+pr\b[^|;&]*\bmerge\b|\bgh\b[^|;&]*(/merg|mergePullRequest)",
     "merging a PR is the owner's action"),
]
for rx, msg in FORBIDDEN:
    if re.search(rx, cmd, re.IGNORECASE):
        emit(msg)


def protected_token(tok):
    t = norm(tok.rsplit("=", 1)[-1])
    if not t or t.startswith("-"):
        return None
    for pat in hard:
        if mentions(t, pat):
            return pat
    for pat in cond:
        if mentions(t, pat) and os.path.exists(t):
            return pat
    return None


VERBS = [
    r"\bsed\b[^|;&]*\s-[a-zA-Z]*i",
    r"\bperl\b[^|;&]*\s-[a-zA-Z]*i",
    r"\btee\b",
    r"\bdd\b",
    r"\bcp\b",
    r"\bmv\b",
    r"\binstall\b",
    r"\brm\b",
    r"\bchmod\b[^|;&]*(\s[a-zA-Z]*-x|\s-?[0-7]{3,4}\b)",
    r"\bgit\s+clean\b[^|;&]*\s-[a-zA-Z]*f",
    r"\bgit\s+(checkout|restore|apply|stash)\b",
    r"\bpython[0-9.]*\b[^|]*\bopen\s*\([^)]*['\"]\s*[wax+]",
    r"\bpython[0-9.]*\b[^|]*\bwrite_(text|bytes)\s*\(",
]

mention = None
for tok in re.findall(r"[^\s|;&<>'\"(){}\[\]]+", cmd):
    mention = protected_token(tok)
    if mention:
        break

if mention:
    for rx in VERBS:
        if re.search(rx, cmd, re.IGNORECASE):
            emit("shell write/remove on protected path (%s)" % mention)

for m in re.finditer(r"(?<![-=<>|])(?:&|[0-9])?>>?\s*([^\s|;&<>]+)", cmd):
    tgt = protected_token(m.group(1).strip().strip('"').strip("'"))
    if tgt:
        emit("redirect writes to protected path (%s)" % tgt)

sys.exit(0)
PYEOF
)"; then
  verdict="$out"
fi

if [ -n "$verdict" ]; then
  printf '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"%s"}}\n' "$verdict"
  exit 2
fi
exit 0

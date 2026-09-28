#!/usr/bin/env bash
# fmt-on-edit.sh — PostToolUse companion to agent-gate.sh (plan §3).
# stdin: hook event JSON. On *.rs writes runs `rustfmt --check` (rustfmt
# discovers rustfmt.toml upward from the file's directory; edition comes from
# that config, so no --edition flag). Failure -> exit 2 + {"decision":"block"}
# JSON fed back to the model. Fail-open on unparsable input/missing rustfmt.
set -euo pipefail
export LC_ALL=C

input="$(cat)"

path="$(printf '%s' "$input" | python3 -c 'import json, sys
try:
    event = json.load(sys.stdin)
    ti = event.get("tool_input") or {}
    print(ti.get("file_path") or ti.get("path") or "")
except Exception:
    pass' 2>/dev/null || true)"

case "$path" in
  *.rs) ;;
  *) exit 0 ;;
esac

[ -f "$path" ] || exit 0
command -v rustfmt >/dev/null 2>&1 || exit 0

if ! rustfmt --check "$path" >/dev/null 2>&1; then
  safe="${path//\\/}"
  safe="${safe//\"/}"
  printf '{"decision":"block","reason":"%s fails rustfmt --check — run cargo fmt before continuing"}\n' "$safe"
  exit 2
fi
exit 0

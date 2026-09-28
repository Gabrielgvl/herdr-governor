#!/usr/bin/env bash
# check-core-purity.sh — governor-core purity gate (spec §9/§10).
#   (a) dependency-graph check via `cargo metadata --no-deps`:
#       governor-core normal deps ⊆ {serde, thiserror, sha2}, dev-deps ⊆
#       {proptest, tempfile}, no build deps; member edges are forbidden
#       except herdr-governor -> governor-core, which is allowed but
#       never required — dependency rules forbid edges, never create them.
#   (b) crate-root check: governor-core/src/lib.rs must open with the
#       #![no_std] inner attribute, and a lexical scan of
#       governor-core/src/** fails on I/O or clock tokens — std::fs,
#       std::net, std::io, std::process, std::env, std::time::,
#       Instant::now, SystemTime, tokio, reqwest, rusqlite, unsafe — plus
#       the escape forms a flat token scan cannot see: grouped/aliased
#       `use` trees that reach those modules, glob imports of std/core/
#       alloc (`use std::*` names no banned module but reaches all of
#       them), whitespace-split paths, include/include_str/include_bytes
#       identifiers (invocations, imports/aliases, raw identifiers),
#       env!/option_env!, #[path], and every `extern crate
#       std` spelling (r#std, `as` aliases, #[macro_use]) — all pull
#       unscanned code or host state into the crate. The scan runs on the
#       comment-stripped, literal-blanked view (scripts/
#       strip_rust_comments.py), so comments and strings can neither hide
#       a token nor false-fire one. governor-core/clippy.toml carries the
#       compiler-enforced half of this boundary (disallowed
#       methods/types/macros — the macro entries name their core:: paths,
#       which is what they resolve to under no_std) — this script stays
#       the pre-compile tripwire. Core inputs are values, including time,
#       so none of these may appear — not even in #[cfg(test)] modules
#       inline in src (test code that needs the filesystem lives under
#       governor-core/tests/).
# Fails closed: an unresolvable workspace or a missing member is a FAIL,
# never a skip. Exit 0 clean, 1 on FAIL, 2 on usage/environment error.
set -euo pipefail
export LC_ALL=C

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"

usage() {
  printf 'usage: %s [--help]   (runs from anywhere inside the repo)\n' "$0" >&2
}

case "${1:-}" in
  -h | --help)
    usage
    exit 0
    ;;
  "") ;;
  *)
    usage
    exit 2
    ;;
esac

if ! root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  usage
  printf 'check-core-purity: not a git repository\n' >&2
  exit 2
fi
cd "$root"

command -v cargo >/dev/null 2>&1 || {
  printf 'FAIL PURITY cargo not on PATH — cannot resolve workspace\n'
  exit 1
}
command -v rg >/dev/null 2>&1 || {
  printf 'FAIL PURITY rg not on PATH — cannot scan governor-core/src\n'
  exit 1
}

if ! meta="$(cargo metadata --no-deps --format-version 1 2>/dev/null)"; then
  printf 'FAIL PURITY cargo metadata failed — workspace unresolvable\n'
  exit 1
fi

meta_rc=0
GOV_META="$meta" python3 - <<'PYEOF' || meta_rc=$?
import json
import os
import sys

fails = []


def fail(msg):
    fails.append(msg)


try:
    doc = json.loads(os.environ["GOV_META"])
except Exception:
    print("FAIL PURITY cargo metadata output did not parse")
    sys.exit(1)

pkgs = {p["name"]: p for p in doc.get("packages", [])}
by_id = {p["id"]: p for p in doc.get("packages", [])}
members = set()
for mid in doc.get("workspace_members", []):
    p = by_id.get(mid)
    if p is not None:
        members.add(p["name"])

CORE = "governor-core"
BIN = "herdr-governor"
ALLOWED_DEPS = {"serde", "thiserror", "sha2"}
ALLOWED_DEV = {"proptest", "tempfile"}

core = pkgs.get(CORE)
if core is None:
    fail("workspace has no member package 'governor-core'")
if BIN not in pkgs:
    fail("workspace has no member package 'herdr-governor'")

if core is not None:
    normal, dev, build = set(), set(), set()
    for d in core.get("dependencies", []):
        kind = d.get("kind")
        name = d.get("name", "")
        if kind == "dev":
            dev.add(name)
        elif kind == "build":
            build.add(name)
        else:
            normal.add(name)
    for name in sorted(normal - ALLOWED_DEPS):
        fail("governor-core normal dependency not allowed: %s" % name)
    for name in sorted(dev - ALLOWED_DEV):
        fail("governor-core dev-dependency not allowed: %s" % name)
    for name in sorted(build):
        fail("governor-core build dependency forbidden: %s" % name)

# Member edges: a non-dev dependency whose name is another workspace member.
# Every member edge is forbidden except herdr-governor -> governor-core;
# that edge may exist but is never required.
member_edges = []
for name in sorted(members):
    pkg = pkgs.get(name)
    if pkg is None:
        continue
    for d in pkg.get("dependencies", []):
        if d.get("kind") == "dev":
            continue
        dep_name = d.get("name", "")
        if dep_name in members and dep_name != name:
            member_edges.append((name, dep_name))
for src, dst in member_edges:
    if (src, dst) != (BIN, CORE):
        fail("forbidden member edge: %s -> %s" % (src, dst))

for f in fails:
    print("FAIL PURITY %s" % f)
print("check-core-purity(metadata): %d fail(s)" % len(fails))
sys.exit(1 if fails else 0)
PYEOF

# Crate-root check: lexical scan of governor-core/src/** on the
# comment-stripped, literal-blanked view — `std::/**/fs` unwraps to the
# banned token, and a token inside a doc comment or string is never a
# false hit. Runs even when metadata failed, so both failure sets surface
# in one invocation. The shared stripper is imported under -B so it never
# writes a __pycache__ next to the gate scripts.
scan_rc=0
if [ -d governor-core/src ]; then
  python3 -B - "$script_dir" "governor-core/src" <<'PYEOF' || scan_rc=$?
import os
import re
import sys

sys.path.insert(0, sys.argv[1])
import strip_rust_comments as S

SRC = sys.argv[2]
BANNED = ("fs", "net", "io", "process", "env", "time", "os", "backtrace")
GLOB_ROOTS = ("std", "core", "alloc")

TOKEN_RX = re.compile(
    r"std\s*:\s*:\s*(?:fs|net|io|process|env|os|backtrace)\b"
    r"|std\s*:\s*:\s*time\s*:\s*:|Instant\s*:\s*:\s*now|SystemTime"
    r"|tokio|reqwest|rusqlite|\bunsafe\b"
    # Include macros expand before Clippy: reject the identifiers themselves,
    # including raw spellings, so imports cannot rename unscanned ingress.
    r"|\b(?:r#)?include(?:_str|_bytes)?\b|\benv\s*!|\boption_env\s*!"
    r"|\bextern\s+crate\s+(?:r#)?(?:std|core|alloc)\b")


def split_top(s):
    parts, depth, cur = [], 0, ""
    for c in s:
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
        if c == "," and depth == 0:
            parts.append(cur)
            cur = ""
        else:
            cur += c
    if cur.strip():
        parts.append(cur)
    return parts


def expand(prefix, tree):
    out = []
    for part in split_top(tree):
        part = part.strip()
        if not part:
            continue
        m = re.match(r"^(.*?)\s*::\s*\{(.*)\}\s*$", part, re.S)
        if m:
            out += expand(prefix + m.group(1) + "::", m.group(2))
            continue
        m = re.match(r"^\{(.*)\}$", part, re.S)
        if m:
            out += expand(prefix, m.group(1))
            continue
        seg = re.split(r"\s+as\s+", part, maxsplit=1)
        leaf = seg[0].strip()
        alias = seg[1].strip() if len(seg) > 1 else ""
        if leaf == "self":
            out.append((prefix[:-2] if prefix.endswith("::") else prefix, alias))
        elif leaf == "*":
            out.append((prefix + "*", alias))
        else:
            out.append((prefix + leaf, alias))
    return out


use_re = re.compile(r"\b(?:pub(?:\s*\([^)]*\))?\s+)?use\s+([^;]+);", re.S)
fails = []

# governor-core is #![no_std] — the attribute must be the plain first
# inner attribute of lib.rs (stripped view: a commented-out copy or one
# hidden after a doc comment cannot fake it).
lib = os.path.join(SRC, "lib.rs")
try:
    with open(lib, encoding="utf-8", errors="replace") as fh:
        lib_code = S.strip(fh.read(), blank_literals=True)
except OSError:
    lib_code = ""
if not re.match(r"\s*#!\s*\[\s*no_std\s*\]", lib_code):
    fails.append("%s:1: first inner attribute must be #![no_std]" % lib)

for root_, _dirs, files in os.walk(SRC):
    for fn in sorted(files):
        if not fn.endswith(".rs"):
            continue
        fp = os.path.join(root_, fn)
        try:
            with open(fp, encoding="utf-8", errors="replace") as fh:
                code = S.strip(fh.read(), blank_literals=True)
        except OSError:
            continue
        for m in TOKEN_RX.finditer(code):
            fails.append("token %s:%d: %r" % (fp, S.line_of(code, m.start()),
                                            S.squash(m.group(0))))
        for ln, meta in S.iter_metas(code):
            if re.search(r"\bpath\s*=", meta):
                fails.append("token %s:%d: #[path] source inclusion" % (fp, ln))
        # Expand grouped/aliased use trees to full paths — `use std::{fs,
        # io}` never writes the banned token — and reject `*` leaves on
        # std/core/alloc roots outright: a glob names no module, so the
        # path check alone cannot see `use std::*`.
        for m in use_re.finditer(code):
            ln = S.line_of(code, m.start())
            body = re.sub(r"\s*:\s*:\s*", "::", m.group(1))
            for path, alias in expand("", body):
                path = path.lstrip(":")
                segs = [re.sub(r"^r#", "", s) for s in path.split("::")]
                if not segs or segs[0] not in GLOB_ROOTS:
                    continue
                if segs[-1] == "*":
                    fails.append(
                        "%s:%d: glob import `%s` reaches the whole %s:: namespace"
                        % (fp, ln, path, segs[0]))
                elif segs[0] == "std":
                    if len(segs) == 1:
                        if alias:
                            fails.append(
                                "%s:%d: `use std as %s` aliases the std root past the token scan"
                                % (fp, ln, alias))
                    elif segs[1] in BANNED:
                        fails.append(
                            "%s:%d: `use` reaches banned module: %s"
                            % (fp, ln, path))

for f in fails:
    print("FAIL PURITY governor-core %s" % f)
sys.exit(1 if fails else 0)
PYEOF
elif [ "$meta_rc" -eq 0 ]; then
  # Metadata resolved but the core crate dir is absent: inconsistent tree.
  printf 'FAIL PURITY governor-core/src missing while workspace resolves\n'
  scan_rc=1
fi

if [ "$meta_rc" -eq 0 ] && [ "$scan_rc" -eq 0 ]; then
  printf 'check-core-purity: clean\n'
  exit 0
fi
exit 1

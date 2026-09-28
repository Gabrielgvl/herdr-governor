#!/usr/bin/env bash
# mutants-diff.sh BASE_SHA [--classify-only] — diff-scoped mutation gate
# (spec §10 P1). Classifies `git diff BASE...HEAD`:
#   (i)   a governor-core/src/** PRODUCTION change -> `cargo mutants
#         -p governor-core --in-diff` on the merge-base diff; FAIL on missed
#         mutants (cargo-mutants exit code) and when `--list` itself errors.
#         A zero-entry `--list` is legitimate — const/type declarations,
#         deletions and comment-only diffs carry no mutation candidates
#         — so it reports and passes; attribute/cfg-only behaviour changes are
#         policed by R8/I3/lints, not by mutant count.
#   (ii)  every changed path is test-only -> full `cargo mutants
#         -p governor-core`; a weakened helper or same-name no-op test shows
#         up here. Test-only means: */tests/** and test-named */src/**.rs by
#         path, plus governor-core/src/*.rs files whose changed lines all sit
#         inside test regions (cfg(test)-gated items incl. `mod x;` decls and
#         #[test] fns) or whose only module decls are test-gated.
#   (iii) otherwise -> REPORT "no core-relevant change", exit 0 (present,
#         not skipped: the classification itself is evidence).
# --classify-only prints the branch decision and exits 0 without running
# cargo-mutants (selftest hook).
# Fails closed: missing/invalid BASE, a missing cargo-mutants, or an
# unresolvable diff are all errors. Exit 0 pass, 1 on FAIL, 2 on usage error.
set -euo pipefail
export LC_ALL=C

MUTANTS_TIMEOUT="${GOV_MUTANTS_TIMEOUT:-300}"

usage() {
  printf 'usage: %s BASE_SHA [--classify-only]\n' "$0" >&2
}

classify_only=0
base=""
for arg in "$@"; do
  case "$arg" in
    -h | --help)
      usage
      exit 0
      ;;
    --classify-only)
      classify_only=1
      ;;
    -*)
      usage
      exit 2
      ;;
    *)
      if [ -z "$base" ]; then
        base="$arg"
      else
        usage
        exit 2
      fi
      ;;
  esac
done

if [ -z "$base" ]; then
  usage
  exit 2
fi

if ! root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  usage
  printf 'mutants-diff: not a git repository\n' >&2
  exit 2
fi
cd "$root"

if ! git rev-parse --verify "$base^{commit}" >/dev/null 2>&1; then
  usage
  printf 'mutants-diff: not a commit: %s\n' "$base" >&2
  exit 2
fi

if ! git diff --name-status "$base...HEAD" >/dev/null; then
  printf 'mutants-diff: git diff %s...HEAD failed\n' "$base" >&2
  exit 2
fi

# Classification needs the full diff text (test-region membership is decided
# per changed line), so materialize it once and share it with the runner below.
diff_file="$(mktemp)"
trap 'rm -f "$diff_file"' EXIT
git diff "$base...HEAD" >"$diff_file"

# Suffix-anchored test paths (*/tests/**, test-named .rs under */src/**) are
# test-only by name. Other governor-core/src/*.rs files are test-only only
# when every changed line sits inside a test region (a cfg(test)-gated item or
# a #[test] fn) — or when the file's only `mod <stem>;` declarations are
# themselves test-gated (a dedicated test module the name doesn't reveal).
class_detail="$(
  python3 - "$base" "$diff_file" <<'PYEOF'
import os
import re
import subprocess
import sys

base, diff_path = sys.argv[1:3]


def git(*args):
    return subprocess.run(
        ["git", *args],
        capture_output=True, text=True, encoding="utf-8", errors="replace",
    ).stdout


mb = git("merge-base", base, "HEAD").strip() or base
ns = git("diff", "--name-status", "%s...HEAD" % base)
with open(diff_path, encoding="utf-8", errors="replace") as fh:
    diff_text = fh.read()


def is_test_path(p):
    if p.startswith("tests/") or "/tests/" in p:
        return True
    if p.endswith(".rs") and (p.startswith("src/") or "/src/" in p):
        return "test" in p.rsplit("/", 1)[-1]
    return False


ATTR_CFG = re.compile(r"#\s*!?\[\s*cfg\s*\(([^)]*)\)")
ATTR_TEST = re.compile(r"#\s*\[\s*(?:[A-Za-z_][\w]*::)*test\b")


def cfg_is_test(body):
    # cfg(test)/cfg(all(test, ...)) gate test-only code; a not() operand does not
    return bool(re.search(r"\btest\b", body)) and not re.search(r"\bnot\s*\(", body)


def scrub(lines):
    # braces inside strings/chars/comments must not move the depth counter;
    # ceiling: multiline raw strings can still confuse it (lexical, not parsed)
    out = []
    in_block = False
    for line in lines:
        i, buf, in_str = 0, [], False
        while i < len(line):
            c = line[i]
            if in_block:
                if line.startswith("*/", i):
                    in_block = False
                    i += 2
                else:
                    i += 1
                continue
            if in_str:
                if c == "\\":
                    i += 2
                    continue
                if c == '"':
                    in_str = False
                i += 1
                continue
            if line.startswith("//", i):
                break
            if line.startswith("/*", i):
                in_block = True
                i += 2
                continue
            if c == '"':
                in_str = True
                i += 1
                continue
            if c == "'":
                m = re.match(r"'(\\.|[^\\'])'", line[i:])
                if m:
                    i += m.end()
                    continue
            buf.append(c)
            i += 1
        out.append("".join(buf))
    return out


def test_regions(text):
    """1-based line numbers governed by a test-only attribute."""
    lines = scrub(text.split("\n"))
    marks = set()
    i, n = 0, len(lines)
    while i < n:
        s = lines[i].strip()
        m = ATTR_CFG.match(s)
        if not ((m and cfg_is_test(m.group(1))) or ATTR_TEST.match(s)):
            i += 1
            continue
        start = i
        j = i + 1
        while j < n and (lines[j].strip().startswith("#") or not lines[j].strip()):
            j += 1
        if j >= n:
            marks.update(range(start + 1, n + 1))
            break
        depth, opened, end, done = 0, False, j, False
        for k in range(j, n):
            for c in lines[k]:
                if c == "{":
                    depth += 1
                    opened = True
                elif c == "}":
                    depth -= 1
                elif c == ";" and not opened:
                    done = True  # `;`-terminated item (use/mod/static decl)
                    break
            end = k
            if done or (opened and depth <= 0):
                break
        marks.update(range(start + 1, end + 2))
        i = end + 1
    return marks


def module_stem(p):
    name = p.rsplit("/", 1)[-1][:-3]
    if name == "mod":
        name = p.rsplit("/", 2)[-2]
    return name


def src_text(ref, p):
    return git("show", "%s:%s" % (ref, p))


def file_declared_test_only(stem):
    # every `mod <stem>;` decl under governor-core/src sits inside a test
    # region -> the file compiles only into the test harness
    found = False
    pat = re.compile(r"\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+" + re.escape(stem) + r"\s*;")
    for root_, _dirs, files in os.walk("governor-core/src"):
        for fn in files:
            if not fn.endswith(".rs"):
                continue
            fp = os.path.join(root_, fn)
            try:
                with open(fp, encoding="utf-8", errors="replace") as fh:
                    text = fh.read()
            except OSError:
                continue
            reg = test_regions(text)
            for idx, l in enumerate(scrub(text.split("\n")), 1):
                if pat.match(l):
                    found = True
                    if idx not in reg:
                        return False
    return found


# per-file changed line numbers: minus -> old-side line, plus -> new-side
hunks = {}  # b-path -> {"a": a-path, "minus": set, "plus": set}
cur_a = cur_b = None
old_ln = new_ln = 0
for line in diff_text.split("\n"):
    if line.startswith("diff --git "):
        m = re.match(r'diff --git "?a/(.+?)"? "?b/(.+?)"?$', line)
        cur_a, cur_b = (m.group(1), m.group(2)) if m else (None, None)
        if cur_b:
            hunks.setdefault(cur_b, {"a": cur_a, "minus": set(), "plus": set()})
        continue
    if line.startswith("@@ "):
        m = re.match(r"@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@", line)
        if m:
            old_ln, new_ln = int(m.group(1)), int(m.group(2))
        continue
    if not cur_b or line[:1] not in ("+", "-", " "):
        continue
    if line.startswith("+++") or line.startswith("---"):
        continue
    h = hunks[cur_b]
    if line[:1] == "-":
        h["minus"].add(old_ln)
        old_ln += 1
    elif line[:1] == "+":
        h["plus"].add(new_ln)
        new_ln += 1
    else:
        old_ln += 1
        new_ln += 1


def is_core_src(p):
    return p.startswith("governor-core/src/")


def core_file_is_test_only(b):
    """Non-test-named governor-core/src/*.rs: test-only iff every changed
    line is inside a test region, or the whole file is a gated test module."""
    h = hunks.get(b, {"a": b, "minus": set(), "plus": set()})
    if file_declared_test_only(module_stem(b)):
        return True
    old_marks = test_regions(src_text(mb, h["a"]))
    new_marks = test_regions(src_text("HEAD", b))
    return all(l in old_marks for l in h["minus"]) and all(
        l in new_marks for l in h["plus"])


paths = []
for entry in ns.split("\n"):
    if not entry.strip():
        continue
    parts = entry.split("\t")
    for p in parts[1:] or parts[:1]:
        p = p.strip().strip('"')
        if p:
            paths.append(p)

if not paths:
    print("no-core-change\tempty diff")
    sys.exit(0)

prod = False
all_test = True
for p in paths:
    if is_test_path(p):
        continue
    if is_core_src(p) and p.endswith(".rs") and core_file_is_test_only(p):
        continue
    all_test = False
    if is_core_src(p):
        prod = True

if prod:
    print("core-src-diff\tgovernor-core/src/** production change -> in-diff mutants")
elif all_test:
    print("test-only\tall changed paths are test paths -> full governor-core mutation run")
else:
    print("no-core-change\tno governor-core/src/** change")
PYEOF
)"
class="${class_detail%%$'\t'*}"
detail="${class_detail#*$'\t'}"
printf 'mutants-diff: class=%s (%s)\n' "$class" "$detail"

if [ "$classify_only" -eq 1 ]; then
  exit 0
fi

if [ "$class" = "no-core-change" ]; then
  printf 'REPORT MUTANTS no core-relevant change under %s...HEAD\n' "$base"
  exit 0
fi

if ! command -v cargo-mutants >/dev/null 2>&1; then
  printf 'FAIL MUTANTS cargo-mutants not on PATH — cannot run mutation gate\n'
  exit 1
fi
if [ ! -f governor-core/Cargo.toml ]; then
  printf 'FAIL MUTANTS governor-core/Cargo.toml missing — cannot scope the run\n'
  exit 1
fi

if [ "$class" = "core-src-diff" ]; then
  if ! list_out="$(cargo mutants -p governor-core --in-diff "$diff_file" --list 2>&1)"; then
    printf 'FAIL MUTANTS cargo-mutants --list failed:\n%s\n' "$list_out"
    exit 1
  fi
  n_mutants="$(printf '%s\n' "$list_out" | grep -c '^[^[:space:]]' || true)"
  if [ "$n_mutants" -eq 0 ]; then
    # a clean --list with zero entries is not a dodged run: cargo-mutants
    # cannot express mutants for declarations, deletions or comments.
    printf 'REPORT MUTANTS 0 mutants — the diff has no mutation candidates\n'
    exit 0
  fi
  printf 'mutants-diff: %s in-diff mutants to run\n' "$n_mutants"
  if ! cargo mutants -p governor-core --in-diff "$diff_file" --timeout "$MUTANTS_TIMEOUT"; then
    printf 'FAIL MUTANTS in-diff mutation run failed (missed/timeout/unviable mutants)\n'
    exit 1
  fi
  exit 0
fi

# class == test-only
printf 'mutants-diff: test-only diff — running full cargo mutants -p governor-core\n'
if ! cargo mutants -p governor-core --timeout "$MUTANTS_TIMEOUT"; then
  printf 'FAIL MUTANTS full mutation run failed (missed/timeout/unviable mutants)\n'
  exit 1
fi
exit 0

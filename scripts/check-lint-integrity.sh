#!/usr/bin/env bash
# check-lint-integrity.sh — content invariants (plan §3 I1–I10). Catches
# weakening that a diff-vs-HEAD cannot see: missing/empty required files,
# weakened lint levels, suppression attributes in source, release-only cfg
# gates, lost exec bits, deny-list drift, unpinned CI actions, justfile
# references to missing files, members that dropped the workspace-lint
# opt-in, harness-kind literals outside the transcript adapter, and
# lifecycle writes outside store::transitions. Exit 0 on a clean tree;
# non-zero with a named rule on violations.
# Workspace layout: members are top-level dirs, so source scans cover
# src tests */src */tests — the dirs that exist at the time of the scan.
set -euo pipefail
export LC_ALL=C
shopt -s nullglob

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
paths_file="$script_dir/protected-paths.txt"
policy_file="$script_dir/lint-policy.txt"

fails=0
fail() {
  printf 'FAIL %s\n' "$*"
  fails=$((fails + 1))
}
note() {
  printf 'REPORT %s\n' "$*"
}

# run_scan <rule> <max-ok-rc> <cmd...> — run a scan producer with stdout
# captured into $scan_out. A producer run inside `< <(cmd)` can fail
# without the parent ever noticing, so output goes through a checked
# command substitution instead: a scanner that crashes, is killed or
# cannot read its inputs must fail the gate, not look like an empty
# scan. rg/grep exit 1 for "no lines selected" — a clean empty scan —
# so their callers pass max-ok-rc 1; python producers pass 0.
scan_out=""
run_scan() {
  local rule="$1" max_rc="$2"
  shift 2
  local rc=0
  scan_out="$("$@")" || rc=$?
  if [ "$rc" -gt "$max_rc" ]; then
    fail "$rule scanner exited $rc — a failed scan is a failure, not an empty result"
    scan_out=""
  fi
  return 0
}

ci_mode=0
[ "${CI:-}" = "true" ] && ci_mode=1

in_repo=0
if root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  cd "$root"
  in_repo=1
fi

if [ ! -f "$paths_file" ]; then
  printf 'FAIL I1 protected-paths.txt missing or unreadable\n' >&2
  exit 1
fi
[ -f "$policy_file" ] || fail "I2 lint-policy.txt missing or unreadable"

# I1: every required (`!`) entry exists and is non-empty. Absent-but-tracked or
# CI-mode absence is a FAIL; absent-and-never-tracked is a REPORT (sibling
# scaffold files land on other branches — §2 bootstrap ordering).
while IFS=$'\t' read -r mode pat req _rest; do
  case "$mode" in '' | '#'*) continue ;; esac
  [ "$req" = "!" ] || continue
  existing=()
  for m in $pat; do # unquoted: word-split + glob expansion is intended
    [ -e "$m" ] && existing+=("$m")
  done
  if [ "${#existing[@]}" -eq 0 ]; then
    if [ "$ci_mode" -eq 1 ] || { [ "$in_repo" -eq 1 ] && [ -n "$(git ls-files -- "$pat")" ]; }; then
      fail "I1 required path missing: $pat"
    else
      note "I1 required path not yet present: $pat"
    fi
    continue
  fi
  for m in "${existing[@]}"; do
    [ -s "$m" ] || fail "I1 required path empty: $m"
  done
done <"$paths_file"

# I2: every <file><TAB><regex> pair in lint-policy.txt matches.
if [ -f "$policy_file" ]; then
  while IFS=$'\t' read -r f rx _rest; do
    case "$f" in '' | '#'*) continue ;; esac
    [ -f "$f" ] || {
      fail "I2 policy target missing: $f"
      continue
    }
    grep -Eq -- "$rx" "$f" || fail "I2 $f missing required pattern: $rx"
  done <"$policy_file"
fi

# I3: source scans on the comment-stripped, literal-blanked view (comments
# and strings can neither hide a banned attribute nor fake one): no
# allow/deny/warn/forbid attributes — top-level or nested inside cfg_attr —
# every #[expect] carries a reason and, inside member src/ trees, may not
# name the purity lints (disallowed_methods/types/macros in any spelling)
# or a group that silences them (clippy::all, clippy::style, warnings); no
# #[ignore]; no #[mutants::skip] in any spelling; and no release-only cfg —
# cfg, cfg_attr or cfg! forms of not(test) — the committed-tree mirror of
# R8. Catches intent in committed files the diff gate misses. Non-.rs
# files under the scanned dirs keep the raw line scan (fixture text is not
# Rust). The shared engine lives in scripts/strip_rust_comments.py so I3
# and R2/R8 cannot drift apart. The reasoned-#[expect] escape is scoped
# positively: only member tests/ trees claim it — everything else scanned
# keeps the banned-target rule. The producer's exit status is checked via
# run_scan: a crashed or killed scanner is a FAIL, never a clean scan.
scan_dirs=()
for d in src tests */src */tests; do
  [ -d "$d" ] && scan_dirs+=("$d")
done
if [ "${#scan_dirs[@]}" -gt 0 ]; then
  run_scan I3 0 python3 -B - "$script_dir" "${scan_dirs[@]}" <<'PYEOF'
import os
import re
import sys
import tomllib

sys.path.insert(0, sys.argv[1])
import strip_rust_comments as S

KIND_MSG = {
    "suppress": "suppression attribute",
    "expect-reason": "expect without reason",
    "expect-target": "expect names a banned purity lint/group",
    "ignore": "ignored test",
    "mutants-skip": "mutants::skip attribute",
    "not-test": "release-only cfg (R8)",
}

# Raw-view rules for non-.rs files — the pre-strip behavior (fixture text
# is not Rust and must not lose the scan).
RAW_RX = (
    ("suppression attribute",
     re.compile(r"#!?\[(?:r#)?(?:allow|deny|warn)\(|(?:r#)?forbid\(")),
    ("ignored test", re.compile(r"#!?\[(?:r#)?ignore")),
    ("release-only cfg (R8)",
     re.compile(
         r"#!?\[(?:r#)?cfg\([^)]*not\s*\(\s*test\s*,?"
         r"|(?:r#)?cfg!\s*[(\[{][^)\]}]*not\s*\(\s*test\s*,?")),
)


def _walk_err(exc):
    # an unreadable in-scope dir is a failed scan, not an empty one
    print("unreadable in-scope dir: %s (%s)"
          % (exc.filename, exc.strerror or exc))


def _root_has_package():
    try:
        with open("Cargo.toml", "rb") as fh:
            return "package" in tomllib.load(fh)
    except Exception:
        return False  # undecidable -> keep the strict scope


ROOT_PKG = _root_has_package()


def in_member_tests(p):
    """The sanctioned reasoned-expect escape exists only inside member
    tests/ trees: <member>/tests where <member>/Cargo.toml exists, or the
    root tests/ when the root manifest is itself a package. Every other
    scanned path keeps the strict banned-target rule — the exemption is
    claimed positively, never inferred from 'not under src/'."""
    parts = p.split(os.sep)
    if len(parts) >= 3 and parts[1] == "tests":
        return os.path.isfile(os.path.join(parts[0], "Cargo.toml"))
    return len(parts) >= 2 and parts[0] == "tests" and ROOT_PKG


for d in sys.argv[2:]:
    for root_, _dirs, files in os.walk(d, onerror=_walk_err):
        for fn in sorted(files):
            p = os.path.join(root_, fn)
            try:
                with open(p, encoding="utf-8", errors="replace") as fh:
                    raw = fh.read()
            except OSError as exc:
                print("unreadable in-scope source: %s (%s)"
                      % (p, exc.strerror or exc))
                continue
            if fn.endswith(".rs"):
                code = S.strip(raw, blank_literals=True)
                for kind, ln, detail in S.lint_findings(
                        code, src_scope=not in_member_tests(p)):
                    print("%s: %s:%d: %s" % (KIND_MSG[kind], p, ln, detail))
            else:
                for ln, line in enumerate(raw.splitlines(), 1):
                    for label, rx in RAW_RX:
                        if rx.search(line):
                            print("%s: %s:%d: %s" % (label, p, ln, line.strip()[:80]))
                    for m in re.finditer(r"#!?\[(?:r#)?expect\(", line):
                        if not re.search(r"(?:r#)?reason\s*=", line):
                            print("expect without reason: %s:%d: %s"
                                  % (p, ln, line.strip()[:80]))
PYEOF
  while IFS= read -r l; do
    [ -n "$l" ] && fail "I3 $l"
  done <<<"$scan_out"
fi

# I4: hooks and gate scripts keep their exec bit.
for f in .githooks/* scripts/*.sh; do
  [ -e "$f" ] || continue
  [ -x "$f" ] || fail "I4 not executable: $f"
done

# I5: every hard pattern appears in both harness deny lists. A pattern is
# covered when another hard pattern's dir/** base contains it (e.g.
# scripts/agent-gate.sh is covered by scripts/**). Absent configs are tolerated
# locally pre-merge, strict in CI.
hard_pats=()
while IFS=$'\t' read -r mode pat _rest; do
  case "$mode" in '' | '#'*) continue ;; esac
  [ "$mode" = "hard" ] && hard_pats+=("$pat")
done <"$paths_file"

covered() {
  local p="$1" q base
  for q in "${hard_pats[@]}"; do
    [ "$q" = "$p" ] && continue
    case "$q" in
      *'**')
        base="${q%%\**}"
        base="${base%/}"
        case "$p" in
          "$base" | "$base"/*) return 0 ;;
        esac
        ;;
    esac
  done
  return 1
}

for cfg in .claude/settings.json .devin/config.json; do
  if [ ! -f "$cfg" ]; then
    if [ "$ci_mode" -eq 1 ]; then
      fail "I5 $cfg missing"
    else
      note "I5 $cfg absent (tolerated pre-merge)"
    fi
    continue
  fi
  for p in "${hard_pats[@]}"; do
    covered "$p" && continue
    grep -Fq -- "$p" "$cfg" || fail "I5 $cfg lacks deny rule for: $p"
  done
done

# I6: every uses: in workflows pinned to a full 40-hex SHA.
if [ -d .github/workflows ]; then
  run_scan I6 1 rg -n 'uses:' .github/workflows/
  if [ -n "$scan_out" ]; then
    i6_uses="$scan_out"
    run_scan I6 1 rg -v 'uses:[[:space:]]*[^[:space:]@]+@[0-9a-f]{40}\b' <<<"$i6_uses"
    while IFS= read -r l; do
      [ -n "$l" ] && fail "I6 unpinned uses: $l"
    done <<<"$scan_out"
  fi
fi

# I7: justfile references resolve (post-merge check; no justfile -> skip).
if [ -f justfile ]; then
  run_scan I7 1 grep -v '^[[:space:]]*#' justfile
  if [ -n "$scan_out" ]; then
    i7_body="$scan_out"
    run_scan I7 1 grep -oE '([[:alnum:]_.*-]+\.toml|\.githooks/[[:alnum:]_.*-]+|scripts/[[:alnum:]_.*-]+|\.config/[[:alnum:]_.*-]+|\.cargo/[[:alnum:]_.*-]+)' <<<"$i7_body"
    if [ -n "$scan_out" ]; then
      i7_refs="$scan_out"
      run_scan I7 0 sort -u <<<"$i7_refs"
    fi
    while IFS= read -r t; do
      [ -z "$t" ] && continue
      if [ ! -e "$t" ] && ! compgen -G "$t" >/dev/null; then
        fail "I7 justfile references missing path: $t"
      fi
    done <<<"$scan_out"
  fi
else
  note "I7 skipped (no justfile)"
fi

# I8: every member manifest opts into the workspace lints. Members are the
# top-level */Cargo.toml files; the virtual root has [workspace.lints.*] and
# no [lints] table of its own, so it is not scanned here.
member_manifests=(*/Cargo.toml)
if [ "${#member_manifests[@]}" -eq 0 ]; then
  note "I8 no member manifests found (pre-scaffold tree)"
else
  run_scan I8 0 python3 - "${member_manifests[@]}" <<'PYEOF'
import sys
import tomllib

for path in sys.argv[1:]:
    try:
        with open(path, "rb") as fh:
            doc = tomllib.load(fh)
    except Exception as exc:
        print(f"{path}: unreadable manifest ({exc})")
        continue
    if doc.get("lints", {}).get("workspace") is not True:
        print(path)
PYEOF
  while IFS= read -r l; do
    [ -n "$l" ] && fail "I8 member manifest lacks [lints] workspace = true: $l"
  done <<<"$scan_out"
fi

# I9: harness-literal tripwire — harness names (claude, devin, pi, agy, codex,
# gemini) live only in the transcript adapter. Lexical scan over every member
# src/** — filename alone is NOT an exemption: a `test_*.rs`/`tests.rs` file or
# a src/**/tests/ dir still compiles into the crate, so only the transcript
# adapter is excluded (member tests/ trees sit outside the scanned src dirs).
# `pi` matches case-sensitively — lowercase `pi` is the harness literal; `PI`
# (e.g. the math constant) is benign. The other names stay case-insensitive.
# Known ceiling: inline #[cfg(test)] modules and comments can still trip it —
# reword or move.
i9_dirs=()
for d in src */src; do
  [ -d "$d" ] && i9_dirs+=("$d")
done
if [ "${#i9_dirs[@]}" -gt 0 ]; then
  run_scan I9 1 rg -n --no-heading '\b(?i:claude|devin|agy|codex|gemini)\b|\bpi\b' "${i9_dirs[@]}" \
    -g '*.rs' \
    -g '!herdr-governor/src/adapters/transcript' \
    -g '!herdr-governor/src/adapters/transcript/**'
  while IFS= read -r l; do
    [ -n "$l" ] && fail "I9 harness literal outside adapters/transcript: $l"
  done <<<"$scan_out"
fi

# I10: lifecycle writes live only in store::transitions (spec §9: no lifecycle
# setter exists outside it). Scans every member src for SQL writes against the
# Appendix-B lifecycle tables, and for direct `store::transitions::` path
# references (the public API is `store::apply`). -U/--multiline-dotall so a
# line break between the verb/table or inside the module path does not hide a
# write, and `\s*::\s*` covers spaced path segments. Fail-closed: scans
# whatever exists — the transitions dir being absent does not silence hits.
i10_dirs=()
for d in src */src; do
  [ -d "$d" ] && i10_dirs+=("$d")
done
if [ "${#i10_dirs[@]}" -gt 0 ]; then
  run_scan I10 1 rg -n --no-heading -U --multiline-dotall -i \
    '\b(INSERT|REPLACE|UPDATE|DELETE|DROP[[:space:]]+TABLE|ALTER[[:space:]]+TABLE)\b.{0,80}\b(callers|launches|runs|effects|outbox|mailbox|handoffs|judgment_sets|judgments|recoveries|cooldowns|qualifications|outcomes)\b' \
    "${i10_dirs[@]}" -g '*.rs' \
    -g '!herdr-governor/src/store/transitions' \
    -g '!herdr-governor/src/store/transitions/**' \
    -g '!herdr-governor/src/store/transitions.rs'
  while IFS= read -r l; do
    [ -n "$l" ] && fail "I10 lifecycle write outside store/transitions: $l"
  done <<<"$scan_out"
  run_scan I10 1 rg -n --no-heading -U '\bstore\s*:\s*:\s*transitions\s*:\s*:' "${i10_dirs[@]}" -g '*.rs' \
    -g '!herdr-governor/src/store/transitions' \
    -g '!herdr-governor/src/store/transitions/**' \
    -g '!herdr-governor/src/store/transitions.rs'
  while IFS= read -r l; do
    [ -n "$l" ] && fail "I10 direct store::transitions reference outside the module: $l"
  done <<<"$scan_out"
fi

printf 'check-lint-integrity: %d failure(s)\n' "$fails"
[ "$fails" -eq 0 ]

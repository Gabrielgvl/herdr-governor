#!/usr/bin/env bash
# check-protected-diff.sh [BASE] — protected-path diff gate (plan §3 R1–R8).
# Local mode (no arg): deliverable = `git diff HEAD` + untracked files from
#   `git ls-files --others`; staged/tracked changes on protected paths ->
#   FAIL R1; still-untracked -> REPORT R1 and join the R2/R6/R7/R8 content scans.
#   An untracked in-scope file the scan cannot read — over the 8 MiB scan
#   limit, failing to open/stat, or not a regular file at all (a dangling
#   symlink is the known cheat: isfile follows the link and misses it) —
#   is FAIL R0 naming the file (and the limit or the reason): a skipped
#   scan is not a clean one.
# CI mode (BASE arg): `git diff BASE...HEAD`; any protected path -> FAIL R1.
# GOV_PROTECTED_OK=1 is the owner override: every failure that concerns a
# protected path or a protected Cargo.toml section (R1, R4, R5, R6)
# becomes a loud OVERRIDE report instead of a FAIL — the approved path for
# dependency additions (Cargo.lock is hard) and other owner-applied
# protected edits. Agents must never set it. R2/R3/R7/R8 are source-integrity
# rules, not protected-path rules: they FAIL regardless — R7 fires on every
# path except hard policy files (conditional paths included), and the owner
# override never downgrades it.
# R7 skips only files classified hard in protected-paths.txt: the configs,
# gate scripts, and docs that define the policy necessarily quote the
# patterns, and agents cannot write them. Conditional paths are
# agent-writable since 2026-10-02 (test helpers and generators define no
# policy), so they are scanned like report-mode paths — R7 is a content
# rule the owner label cannot muffle.
# R6 mirrors I4: exec-bit policy covers scripts/*.sh and .githooks/* only,
# not every file under scripts/.
# R2/R8 run the comment-stripped, literal-blanked engine
# (scripts/strip_rust_comments.py — the same engine I3 uses) on the
# COMPLETE new-side file (HEAD blob in CI mode, the worktree file locally),
# never on a document stitched from '+' lines: lexical context decides
# whether a token is code — a banned spelling inside a multiline string
# stays inert, while an edit inside an existing attribute is judged by the
# whole attribute. Raw-identifier spellings (r#expect, clippy::r#all,
# r#not(test)) are normalized before comparison. A finding reports only
# when the diff introduces it — findings already on the old side are
# subtracted as a (kind, detail) multiset, so a pre-existing violation is
# not re-blamed on every touch (I3 owns the whole-tree scan). Findings
# keep their real file line numbers. R8 is the release-only-cfg rule:
# added cfg/cfg_attr/cfg! forms of not(test) — including the trailing
# comma and all cfg! delimiter pairs — hide behaviour from the test
# build; banned outright, never downgradable.
# Workspace layout: members are top-level dirs, so every tracked */Cargo.toml
# gets its own section map, and the compiled test surface is <member>/src/**
# plus <member>/tests/** for every member (root joins only if it declares
# [package]). Cargo section rules are evaluated per manifest path.
# Exit 0 clean, 1 on FAIL.
set -euo pipefail
export LC_ALL=C

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
policy="$script_dir/protected-paths.txt"

if ! root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  printf 'check-protected-diff: not a git repository\n' >&2
  exit 2
fi
cd "$root"

if [ ! -f "$policy" ]; then
  printf 'FAIL R0 protected-paths.txt missing or unreadable\n'
  exit 1
fi

mode="local"
base=""
if [ $# -ge 1 ]; then
  mode="ci"
  base="$1"
fi

python3 -B - "$policy" "$mode" "$base" "${GOV_PROTECTED_OK:-0}" <<'PYEOF'
import os
import re
import subprocess
import sys
from collections import Counter

policy_path, mode, base, ok_env = sys.argv[1:5]
sys.path.insert(0, os.path.dirname(os.path.abspath(policy_path)))
import protected_paths as PP
import strip_rust_comments as S
override = ok_env == "1"
ci = mode == "ci"

EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"


def git(*args, check=False):
    r = subprocess.run(
        ["git", *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env={**os.environ, "LC_ALL": "C"},
    )
    if check and r.returncode != 0:
        sys.stderr.write(r.stderr)
        sys.exit(2)
    return r.stdout


def git_or_none(*args):
    """git stdout on success, None on any failure (e.g. absent blob)."""
    r = subprocess.run(
        ["git", *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        env={**os.environ, "LC_ALL": "C"},
    )
    return r.stdout if r.returncode == 0 else None


pol = PP.load_policy(policy_path)
hard, cond, sect, rep = pol["hard"], pol["conditional"], pol["section"], pol["report"]


def classify_path(p):
    for pat in hard:
        if PP.path_matches(p, pat):
            return "hard"
    for pat in cond:
        if PP.path_matches(p, pat):
            return "cond"
    for pat in sect:
        if PP.path_matches(p, pat):
            return "section"
    for pat in rep:
        if PP.path_matches(p, pat):
            return "report"
    return None


fails = []
reports = []

# Rules whose failure concerns a protected path or a protected Cargo.toml
# section; the owner override degrades them to loud OVERRIDE reports. R7 and
# R8 are excluded on purpose: R7 fires only on non-protected paths, and
# release-only cfg is a content cheat — owner approval for protected edits
# must never muffle either.
DOWNGRADABLE = {"R1", "R4", "R5", "R6"}


def fail(rule, msg):
    if override and rule in DOWNGRADABLE:
        reports.append("REPORT %s OVERRIDE GOV_PROTECTED_OK=1 — %s" % (rule, msg))
    else:
        fails.append("FAIL %s %s" % (rule, msg))


def report(rule, msg):
    reports.append("REPORT %s %s" % (rule, msg))


if ci:
    mb = git("merge-base", base, "HEAD").strip()
    old_ref = mb or base
    rng = "%s...HEAD" % base
else:
    have_head = subprocess.run(
        ["git", "rev-parse", "--verify", "HEAD"], capture_output=True
    ).returncode == 0
    old_ref = "HEAD" if have_head else EMPTY_TREE
    rng = old_ref

# Force textual, attribute-independent diff output: --text defeats
# `-diff`/binary attribute masking, --no-textconv/--no-ext-diff block custom
# diff drivers and GIT_EXTERNAL_DIFF, and the pinned render config keeps the
# a//b prefixes stable so `diff --git` parsing cannot lose `cur` to
# diff.noprefix/prefix/relative overrides.
RENDER_PIN = (
    "-c", "diff.noprefix=false", "-c", "diff.mnemonicprefix=false",
    "-c", "diff.srcprefix=a/", "-c", "diff.dstprefix=b/", "-c", "diff.relative=",
)
DIFF_FLAGS = ("--text", "--no-textconv", "--no-ext-diff")

ns_raw = git(*RENDER_PIN, "diff", "--name-status", "-z", rng, check=True)
diff_text = git(*RENDER_PIN, "diff", *DIFF_FLAGS, rng, check=True)

changes = []  # (status, path); R/C emit both sides
renames = []  # (status, src, dst) for R/C records
toks = ns_raw.split("\0")
i = 0
while i < len(toks):
    r = toks[i]
    i += 1
    if not r:
        continue
    parts = r.split("\t")
    if len(parts) >= 2:
        st, pths = parts[0], parts[1:]
    else:
        st = r
        n = 2 if st[:1] in ("R", "C") else 1
        pths = [t for t in toks[i:i + n] if t]
        i += n
    for p in pths:
        changes.append((st[:1] if not st.startswith("??") else "??", p))
    if st[:1] in ("R", "C") and len(pths) == 2:
        renames.append((st[:1], pths[0], pths[1]))

untracked = []
if not ci:
    for p in git("ls-files", "--others", "--exclude-standard", "-z").split("\0"):
        if p:
            untracked.append(p)
            changes.append(("??", p))

for st, p in changes:
    cls = classify_path(p)
    if cls in ("hard", "cond"):
        if ci or st in ("A", "M", "D", "T", "R", "C", "U"):
            fail("R1", "protected path changed: %s %s" % (st, p))
        else:
            report("R1", "untracked protected path: %s" % p)
    elif cls == "report":
        report("R1", "report-mode path changed: %s %s" % (st, p))
    elif cls == "section" and st != "M":
        fail("R4", "Cargo.toml status %s — only content modification is allowed" % st)

# Workspace layout: members are top-level dirs. Package roots are the parents
# of every tracked */Cargo.toml; the root joins only when its own manifest
# declares [package] (a virtual root compiles nothing). A pre-scaffold tree
# with no manifests keeps the kit's single-crate root surface.
def root_manifest_has_package():
    if ci:
        body = git("show", "HEAD:Cargo.toml")
    else:
        try:
            with open("Cargo.toml", encoding="utf-8") as fh:
                body = fh.read()
        except OSError:
            body = git("show", "%s:Cargo.toml" % old_ref)
    return "package" in set(build_map(body).values())


def discover_pkg_roots():
    manis = [
        p for p in git("ls-files", "-z").split("\0")
        if p == "Cargo.toml" or p.endswith("/Cargo.toml")
    ]
    roots = set()
    for m in manis:
        if "/" in m:
            roots.add(m.rsplit("/", 1)[0])
        elif root_manifest_has_package():
            roots.add(".")
    if not roots and not manis:
        roots.add(".")
    return sorted(roots)


def build_map(text):
    m, sec = {}, None
    for n, line in enumerate(text.splitlines(), 1):
        s = line.strip()
        mm = re.match(r"^\[+\s*([^\]\[]+?)\s*\]+", s)
        if mm and not s.startswith("#"):
            sec = mm.group(1).strip().strip("'\"")
        m[n] = sec
    return m


PKG_ROOTS = discover_pkg_roots()

# R3 file side: deleted or typechanged files on the test surface, and renames
# that move a test path (or a file carrying test markers) off the compiled
# surface — a rename is a deletion of the old path in disguise. The compiled
# surface is each member's tests/** plus src/**; a 'tests/' dir anywhere else
# (docs/tests/, vendor/tests/) compiles nothing. One deletion is exempt: a
# .rs under a member's tests/support/ whose old blob carried no test marker —
# a helper module retiring with its tests is not a deleted test, and R1 still
# judges the conditional path. Hunk side, a removed #[test]/#[tokio::test]
# line is forgiven when every function name marked in the file's old blob
# still carries a marker on the new side (unmarked_tests below).
def test_path(p):
    for r in PKG_ROOTS:
        pre = "" if r == "." else r + "/"
        if p.startswith(pre + "tests/"):
            return True
        if p.startswith(pre + "src/") and "test" in p.rsplit("/", 1)[-1]:
            return True
        if re.match(re.escape(pre) + r"src/.+/tests/.+\.rs$", p):
            return True
    return False


def compiled_rs(p):
    if not p.endswith(".rs"):
        return False
    for r in PKG_ROOTS:
        pre = "" if r == "." else r + "/"
        if p.startswith(pre + "src/") or p.startswith(pre + "tests/"):
            return True
    return False


def support_rs(p):
    """A .rs file under a member's tests/support/ — the one test-surface
    directory whose marker-free helpers may retire without an R3 fail."""
    if not p.endswith(".rs"):
        return False
    for r in PKG_ROOTS:
        pre = "" if r == "." else r + "/"
        if p.startswith(pre + "tests/support/"):
            return True
    return False


test_mark = re.compile(
    r"#!?\[(?:tokio::)?test\b|#!?\[\s*cfg\s*\(\s*test\s*\)\s*\]|\bfn\s+(?:test_|should_)")

for st, p in changes:
    if st in ("D", "T") and test_path(p):
        if st == "D" and support_rs(p):
            old_blob = git_or_none("show", "%s:%s" % (old_ref, p))
            if old_blob is not None and not test_mark.search(old_blob):
                report("R3", "marker-free tests/support module deleted: %s" % p)
                continue
        fail("R3", "test file %s: %s" % ("deleted" if st == "D" else "typechanged", p))

for st, src, dst in renames:
    if st != "R" or test_path(dst):
        continue
    if test_path(src):
        fail("R3", "test file renamed out of test paths: %s -> %s" % (src, dst))
    elif compiled_rs(src) and not compiled_rs(dst) and test_mark.search(
            git("show", "%s:%s" % (old_ref, src))):
        fail("R3", "test content renamed off the compiled surface: %s -> %s" % (src, dst))


def classify_section(sec):
    if not sec:
        return "unknown"
    first = sec.split(".", 1)[0]
    if sec.startswith("package.metadata."):
        return "report"
    if (
        first in ("package", "workspace", "replace", "bin", "lib", "test", "bench", "example")
        or sec.startswith("lints")
        or sec.startswith("profile.") or sec == "profile"
        or sec.startswith("patch.") or sec == "patch"
        or sec == "features"
    ):
        return "fail"
    if (
        sec == "dependencies"
        or sec.endswith("-dependencies")
        or sec.endswith(".dependencies")
        or first == "target"
        or sec == "badges"
    ):
        return "report"
    return "unknown"


def is_dep_section(sec):
    return sec == "dependencies" or sec.endswith("-dependencies") or sec.endswith(".dependencies")


cargo_fail_secs = {}  # (path, section) -> True when introduced (absent at BASE)
cargo_report_secs, cargo_unknown_secs = set(), set()
dep_changed = False
lock_changed = any(p == "Cargo.lock" or p.endswith("/Cargo.lock") for _, p in changes)
cargo_maps = {}  # manifest path -> {"old": map, "new": map, "old_secs": set}


def cargo_maps_for(p):
    """Per-manifest section maps: each */Cargo.toml in the diff gets its own
    old/new line->section maps, so member manifests are classified with the
    same rules as the root one."""
    if p in cargo_maps:
        return cargo_maps[p]
    old_m = build_map(git("show", "%s:%s" % (old_ref, p)))
    if ci:
        new_m = build_map(git("show", "HEAD:%s" % p))
    else:
        try:
            with open(p, encoding="utf-8") as fh:
                new_m = build_map(fh.read())
        except OSError:
            new_m = {}
    entry = {"old": old_m, "new": new_m, "old_secs": set(old_m.values())}
    cargo_maps[p] = entry
    return entry


def cargo_classify(p, sec, introduced=False):
    global dep_changed
    cls = classify_section(sec)
    if cls == "fail":
        key = (p, sec)
        cargo_fail_secs[key] = cargo_fail_secs.get(key, False) or introduced
    elif cls == "report":
        cargo_report_secs.add((p, sec))
        if is_dep_section(sec):
            dep_changed = True
    else:
        cargo_unknown_secs.add((p, sec or "<no section>"))


def r6_scoped(p):
    # I4 parity: exec-bit policy covers scripts/*.sh and .githooks/* only.
    return (
        re.match(r"scripts/[^/]+\.sh$", p) is not None
        or re.match(r"\.githooks/[^/]+$", p) is not None
    )


# `cargo insta` with cargo's global flags (`+<toolchain>`, `--config`, `-Z`,
# ...) inserted between `cargo` and `insta` is the same command.
CARGO_INSTA = (
    r"(?:cargo-insta|\bcargo\s+"
    r"(?:(?:[+@][^\s|;&]+|-{1,2}\w[\w-]*(?:[=\s][^\s|;&]*)?)\s+)*insta)\b"
)


def r7_hit(text):
    return (
        re.search(r"INSTA_UPDATE\s*=\s*[\"']?(always|force|unseen|new)\b", text)
        or re.search(CARGO_INSTA + r"\s+accept\b", text)
        or re.search(CARGO_INSTA + r"\s+review\b", text)
        or re.search(r"\binsta\b[^|;&]*--accept", text)
    )


def src_scope(p):
    """True for every path that is NOT a member tests/ tree — the
    sanctioned reasoned-expect escape is claimed positively: only
    <pkg-root>/tests/** escapes the banned-#[expect]-target rule; src/,
    fixtures, docs and stray dirs do not."""
    for r in PKG_ROOTS:
        pre = "" if r == "." else r + "/"
        if p.startswith(pre + "tests/"):
            return False
    return True


def new_side_text(p):
    """Complete new-side content of a changed path: the HEAD blob in CI
    mode, the worktree file in local mode. None when the path has no
    readable new side (deleted file, rename source)."""
    if ci:
        return git_or_none("show", "HEAD:%s" % p)
    if not os.path.isfile(p):
        return None
    try:
        with open(p, encoding="utf-8", errors="replace") as fh:
            return fh.read()
    except OSError:
        fail("R2", "%s: unreadable new-side source — cannot scan" % p)
        return None


def old_side_text(p):
    """Content at the diff's old side (merge-base/HEAD); None when the
    path did not exist there (added file) — then every finding is new."""
    return git_or_none("show", "%s:%s" % (old_ref, old_of.get(p, p)))


def lint_scan(path, new_text, old_text=None):
    """R2 (suppression/expect/ignore/mutants-skip) and R8 (release-only
    cfg) on the complete new-side file — the same findings engine I3
    runs, on the whole document so lexical context is real. A finding is
    reported only when the diff introduces it: (kind, detail) findings
    already present on the old side are subtracted as a multiset, so a
    pre-existing violation that merely moves lines is not re-reported
    here (I3 scans the whole tree and owns that verdict)."""
    scope = src_scope(path)
    prior = Counter()
    if old_text is not None:
        old_code = S.strip(old_text, blank_literals=True)
        for kind, _ln, detail in S.lint_findings(old_code, src_scope=scope):
            prior[(kind, detail)] += 1
    new_code = S.strip(new_text, blank_literals=True)
    for kind, ln, detail in S.lint_findings(new_code, src_scope=scope):
        key = (kind, detail)
        if prior[key] > 0:
            prior[key] -= 1
            continue
        rule = "R8" if kind == "not-test" else "R2"
        fail(rule, "%s:%d: %s" % (path, ln, detail))


old_of = {dst: src for _st, src, dst in renames}  # R/C: new path -> old path


def marked_fns(text):
    """Names of functions carrying a #[test]/#[tokio::test] marker, on the
    comment-stripped literal-blanked view: the marker attaches to the next
    fn item, so a marker plus the fn line that follows it names a test."""
    names = set()
    pending = False
    for ln in S.strip(text, blank_literals=True).splitlines():
        if re.search(r"#!?\[(?:tokio::)?test\b", ln):
            pending = True
        m = re.search(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)", ln)
        if pending and m:
            names.add(m.group(1))
            pending = False
    return names


def unmarked_tests(path):
    """Names marked with #[test]/#[tokio::test] in the file's old-side blob
    that carry neither marker on the new side — an empty set means every
    marked test kept a marker and a removed marker line is a conversion."""
    old_names = marked_fns(old_side_text(path) or "")
    new_text = new_side_text(path)
    new_names = marked_fns(new_text) if new_text is not None else set()
    return old_names - new_names


cur, cur_cls, old_ln, new_ln = None, None, 0, 0
for line in diff_text.split("\n"):
    if line.startswith("diff --git "):
        m = re.match(r'diff --git "?a/(.+?)"? "?b/(.+?)"?$', line)
        cur = m.group(2) if m else None
        cur_cls = classify_path(cur) if cur else None
        continue
    if line.startswith("@@ "):
        m = re.match(r"@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@", line)
        if m:
            old_ln, new_ln = int(m.group(1)), int(m.group(2))
        continue
    mm = re.match(r"(?:new file mode|new mode)\s+(\d+)", line)
    if mm:
        if cur and r6_scoped(cur) and mm.group(1) != "100755":
            fail("R6", "%s mode %s — hook/gate scripts must stay 100755" % (cur, mm.group(1)))
        continue
    sign = line[:1]
    if sign == "+" and line.startswith("+++"):
        continue
    if sign == "-" and line.startswith("---"):
        continue
    if sign not in ("+", "-", " "):
        continue
    is_rs = bool(cur) and cur.endswith(".rs")
    is_cargo = bool(cur) and cur.rsplit("/", 1)[-1] == "Cargo.toml"
    text = line[1:]
    if sign == "+":
        # R7: snapshot self-acceptance machinery — on every path except the
        # hard policy files that must quote the patterns (conditional paths
        # are agent-writable and scanned).
        if cur_cls != "hard" and r7_hit(text):
            fail("R7", "%s: snapshot self-acceptance added: %s" % (cur, text.strip()[:100]))
        if is_cargo:
            cm = cargo_maps_for(cur)
            sec = cm["new"].get(new_ln)
            cargo_classify(cur, sec, introduced=sec not in cm["old_secs"])
        new_ln += 1
    elif sign == "-":
        if is_rs:
            # removed #[cfg(test)] and fn test_*/should_* lines are still
            # deletions; a removed test-attribute line is forgiven only when
            # every name marked in the old blob keeps a marker in the new blob
            if (re.search(r"#!?\[\s*cfg\s*\(\s*test\s*\)\s*\]", text)
                    or re.search(r"\bfn\s+(test_|should_)", text)):
                fail("R3", "%s: test code removed: %s" % (cur, text.strip()[:100]))
            elif re.search(r"#!?\[(tokio::)?test\b", text):
                lost = sorted(unmarked_tests(cur))
                if lost:
                    fail("R3", "%s: test marker removed: %s (%s now unmarked)"
                         % (cur, text.strip()[:100], ", ".join(lost)))
        if is_cargo:
            cargo_classify(cur, cargo_maps_for(cur)["old"].get(old_ln))
        old_ln += 1
    else:
        old_ln += 1
        new_ln += 1

# R2/R8: every .rs file in the diff is scanned as the complete new-side
# document, so a joined '+'-line view can neither fake a violation inside
# a string literal nor miss one whose attribute head is an unchanged
# context line. Deleted files and rename sources have no new side.
rename_srcs = {src for _st, src, _dst in renames}
for st, p in changes:
    if st in ("D", "??") or not p.endswith(".rs"):
        continue
    if p in rename_srcs:
        continue
    new_text = new_side_text(p)
    if new_text is None:
        if ci and st not in ("D",):
            fail("R2", "%s: new-side blob unreadable at HEAD — cannot scan" % p)
        continue
    lint_scan(p, new_text, old_side_text(p))

# Local mode: untracked files are the deliverable — their content and exec
# bits join the R2/R6/R7/R8 scans (a tracked-only diff cannot see them).
if not ci:
    for p in untracked:
        if not os.path.isfile(p):
            # git lists whatever occupies the path, not just regular
            # files — a dangling symlink is the known cheat. A path
            # that vanished between ls-files and this scan hides
            # nothing; anything still present but not a readable
            # regular file is an unscannable in-scope source.
            if os.path.lexists(p):
                if os.path.islink(p):
                    why = ("dangling symlink" if not os.path.exists(p)
                           else "symlink to a non-regular file")
                else:
                    why = "not a regular file"
                fail("R0", "%s: unscannable untracked in-scope source — %s"
                     % (p, why))
            continue
        if r6_scoped(p) and not os.access(p, os.X_OK):
            fail("R6", "%s new file not executable — hook/gate scripts must stay 100755" % p)
        cls = classify_path(p)
        is_rs = p.endswith(".rs")
        if cls == "hard" and not is_rs:
            continue
        try:
            size = os.path.getsize(p)
            if size > 8 * 1024 * 1024:
                fail("R0", "%s: %d bytes exceeds the 8 MiB untracked-file "
                     "scan limit — refusing to skip" % (p, size))
                continue
            with open(p, encoding="utf-8", errors="replace") as fh:
                body = fh.read()
        except OSError:
            fail("R0", "%s: unreadable untracked in-scope source — "
                 "cannot scan" % p)
            continue
        for line in body.splitlines():
            if cls != "hard" and r7_hit(line):
                fail("R7", "%s: snapshot self-acceptance added (untracked): %s"
                     % (p, line.strip()[:100]))
        if is_rs:
            lint_scan(p, body, None)

for (p, s) in sorted(cargo_fail_secs):
    verb = "introduces" if cargo_fail_secs[(p, s)] else "changes"
    fail("R4", "%s %s protected section [%s]" % (p, verb, s))
for (p, s) in sorted(cargo_report_secs):
    report("R4b", "%s changed in reviewable section [%s]" % (p, s))
for (p, s) in sorted(cargo_unknown_secs):
    report("R4b", "%s changed in unclassified section [%s]" % (p, s))

if lock_changed != dep_changed:
    fail(
        "R5",
        "Cargo.lock %s while Cargo.toml dependency sections %s — the pair must move together"
        % ("changed" if lock_changed else "unchanged", "changed" if dep_changed else "untouched"),
    )

for r in reports:
    print(r)
for f in fails:
    print(f)
print("check-protected-diff: %d fail(s), %d report(s)" % (len(fails), len(reports)))
sys.exit(1 if fails else 0)
PYEOF

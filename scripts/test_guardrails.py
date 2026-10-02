from __future__ import annotations

"""test_guardrails.py — adversarial self-test for the gate scripts.

Every gate ships with seeded cheats (cases that MUST make a gate FAIL) and
paired benign controls (cases that MUST pass). Run via `just guard-selftest`
or bare `python3 scripts/test_guardrails.py`; the report printed after the
suite maps each cheat case to its tests and their outcomes. Fixture repos are
mini two-crate workspaces built under /tmp — root virtual manifest plus
governor-core/ and herdr-governor/ members — while the repo's own scripts/
are the gates under test.
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "scripts"

# Importing the gate helper module under test must not leave a __pycache__
# in scripts/ — fixture repos iterate that directory wholesale.
sys.dont_write_bytecode = True
sys.path.insert(0, str(SCRIPTS))
import strip_rust_comments as stripper  # noqa: E402

CORE = "governor-core"
BIN = "herdr-governor"
CORE_MANI = CORE + "/Cargo.toml"
BIN_MANI = BIN + "/Cargo.toml"
CORE_LIB = CORE + "/src/lib.rs"
BIN_MAIN = BIN + "/src/main.rs"
CORE_TEST = CORE + "/tests/it.rs"
BIN_TEST = BIN + "/tests/it.rs"
SCHEMA_FIXTURE = "tests/fixtures/herdr-api-schema.json"
SCHEMA_BODY = '{"protocol": 22, "methods": []}\n'

ENV_SCRUB = (
    "CI",
    "GOV_PROTECTED_OK",
    "GOV_APPROVAL_LABELS_FILE",
    "GOV_APPROVAL_CHECKS_FILE",
    "HERDR_SCHEMA_LIVE_FILE",
    "GOV_MUTANTS_TIMEOUT",
)


def run(script: str, args=(), stdin="", cwd=None, env=None, timeout=60):
    e = dict(os.environ)
    for k in ENV_SCRUB:
        e.pop(k, None)
    if env:
        e.update(env)
    return subprocess.run(
        [str(SCRIPTS / script), *args],
        input=stdin,
        text=True,
        capture_output=True,
        cwd=str(cwd) if cwd else str(ROOT),
        env=e,
        timeout=timeout,
    )


def gate(payload) -> subprocess.CompletedProcess:
    return run("agent-gate.sh", stdin=json.dumps(payload), cwd=tempfile.gettempdir())


def gate_cmd(command: str) -> subprocess.CompletedProcess:
    return gate({"tool_input": {"command": command}})


def gate_path(path: str, key="file_path") -> subprocess.CompletedProcess:
    return gate({"tool_input": {key: path}})


class Repo:
    def __init__(self):
        self.dir = Path(tempfile.mkdtemp(prefix="guardrails-test-"))
        self.git("init", "-q")

    def git(self, *args, check=True):
        return subprocess.run(
            ["git", "-C", str(self.dir), *args],
            check=check,
            capture_output=True,
            text=True,
        )

    def write(self, rel, content, mode=None):
        p = self.dir / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
        if mode is not None:
            os.chmod(p, mode)

    def remove(self, rel):
        (self.dir / rel).unlink()

    def commit_all(self, msg="c"):
        self.git("add", "-A")
        self.git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", msg)

    def head(self):
        return self.git("rev-parse", "HEAD").stdout.strip()

    def install_scripts(self):
        dst = self.dir / "scripts"
        dst.mkdir(exist_ok=True)
        for f in SCRIPTS.iterdir():
            if f.name.startswith("test_") or not f.is_file():
                # not a file: skip __pycache__ and any other directory
                continue
            shutil.copy2(f, dst / f.name)
            os.chmod(dst / f.name, 0o755)

    def cleanup(self):
        shutil.rmtree(self.dir, ignore_errors=True)


def real_file(name):
    return (ROOT / name).read_text()


def schema_pin(body, path):
    return hashlib.sha256(body.encode()).hexdigest() + "  " + path + "\n"


def harness_dir(name):
    p = ROOT / name
    if not p.is_dir():
        raise RuntimeError(
            f"selftest requires a complete repo: missing harness directory {p}"
        )
    return p


def base_repo():
    """Mini two-crate workspace mirroring the real one: virtual root
    manifest, the real Cargo.lock (cargo's --locked recipes need a lockfile
    consistent with the real manifests and their dependencies), two members
    with [lints] workspace = true, every real file under each member's src/
    tree, member tests/ files, the schema fixture, and the gate scripts
    installed."""
    r = Repo()
    r.write("Cargo.toml", real_file("Cargo.toml"))
    r.write("Cargo.lock", real_file("Cargo.lock"))
    r.write("clippy.toml", 'allow-unwrap-in-tests = true\n')
    r.write(CORE_MANI, real_file(CORE_MANI))
    r.write(BIN_MANI, real_file(BIN_MANI))
    for member in (CORE, BIN):
        for src in sorted((ROOT / member / "src").rglob("*")):
            if src.is_file():
                r.write(str(src.relative_to(ROOT)), src.read_text())
    r.write(
        CORE_TEST,
        '#[test]\nfn test_it() {\n    assert_eq!(1 + 1, 2, "math works");\n}\n',
    )
    r.write(BIN_TEST, '#[test]\nfn test_bin() {\n    assert!(true, "smoke");\n}\n')
    r.write(SCHEMA_FIXTURE, SCHEMA_BODY)
    r.write(SCHEMA_FIXTURE + ".sha256", schema_pin(SCHEMA_BODY, SCHEMA_FIXTURE))
    r.write("docs/plan/p.md", "plan\n")
    r.write("notes.txt", "notes\n")
    r.write("scripts/x.sh", "#!/bin/sh\nexit 0\n", mode=0o755)
    r.install_scripts()
    r.commit_all()
    return r


def full_repo():
    """Workspace satisfying every required-file + lint-policy invariant."""
    r = base_repo()
    for name in ("clippy.toml", "rust-toolchain.toml", "rustfmt.toml", "deny.toml"):
        r.write(name, real_file(name))
    r.write(CORE + "/clippy.toml", real_file(CORE + "/clippy.toml"))
    r.write(".config/nextest.toml", real_file(".config/nextest.toml"))
    r.write("justfile", "ci:\n\tscripts/check-lint-integrity.sh\n")
    r.write("AGENTS.md", "# agents\n")
    r.git("add", "-A")
    r.git("-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "full")
    return r


def deny_reason(proc):
    return json.loads(proc.stdout)["hookSpecificOutput"]


class BaseRepoFixtureTests(unittest.TestCase):
    """pi-review F1: pin the fixture builder itself. base_repo() must mirror
    the real tree it stands in for — every file under each member's src/, at
    any depth, plus the real Cargo.lock — or the gates get exercised against
    a fixture that can drift from the repo it claims to represent."""

    def test_mirrors_member_src_trees_and_real_lockfile(self):
        global ROOT
        seeds = {
            "Cargo.toml": "[workspace]\nmembers = []\n",
            "Cargo.lock": "# distinctive lockfile sentinel 0xbeef\n",
            CORE_MANI: '[package]\nname = "core"\n',
            BIN_MANI: '[package]\nname = "bin"\n',
            CORE_LIB: "//! lib\n",
            CORE + "/src/a/b.rs": "// nested core module\n",
            BIN_MAIN: "// main\n",
            BIN + "/src/deep/leaf.rs": "// nested bin module\n",
        }
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            for rel, body in seeds.items():
                p = root / rel
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(body)
            saved, ROOT = ROOT, root
            try:
                repo = base_repo()
            finally:
                ROOT = saved
            self.addCleanup(repo.cleanup)
            for rel in ("Cargo.lock", CORE + "/src/a/b.rs",
                        BIN + "/src/deep/leaf.rs"):
                with self.subTest(path=rel):
                    self.assertEqual(
                        (repo.dir / rel).read_text(), seeds[rel])


# --------------------------------------------------------------------------
# Seeded-cheat registry. Every assignment cheat maps to >=1 "cheat" test that
# must make its gate FAIL and >=1 "control" test that must let it PASS. The
# CheatCoverageTests meta-tests enforce both sides; report_cases() prints the
# case -> test -> outcome table after the run.
# --------------------------------------------------------------------------

CASES = {}
EXPECTED_CASES = {
    "deleted-test",
    "emptied-test-body",
    "test-moved-off-surface",
    "removed-mod-line",
    "reasonless-expect",
    "bare-allow",
    "snapshot-self-accept",
    "lockfile-without-manifest",
    "weakened-test-helper",
    "narrowed-strategy",
    "same-name-noop-test",
    "lifecycle-write-outside-transitions",
    "release-only-cfg",
    "workspace-membership",
    "member-features",
    "missing-lints-opt-in",
    "harness-literal",
    "io-crate-in-core",
    "schema-fixture-byte-change",
    "stale-owner-approval",
    "banned-expect",
    "unsilenceable-expect",
    "raised-lines-threshold",
    "no-std",
    "mutants-skip-attr",
}


def case(name, role, desc=""):
    def deco(fn):
        entry = CASES.setdefault(name, {"desc": desc, "cheat": [], "control": []})
        if desc and not entry["desc"]:
            entry["desc"] = desc
        entry[role].append(fn)
        return fn

    return deco


def inventory(suite, *tests):
    """Build a `cargo nextest list --message-format json` document.
    suite is "pkg::binary"; tests are (name, ignored, filter_status)."""
    pkg, binary = suite.split("::")
    cases = {
        name: {
            "kind": "test",
            "ignored": ignored,
            "filter-match": {"status": status},
        }
        for name, ignored, status in tests
    }
    return json.dumps(
        {
            "rust-suites": {
                "s": {
                    "package-name": pkg,
                    "binary-name": binary,
                    "status": "listed",
                    "testcases": cases,
                }
            }
        }
    )


class AgentGateTests(unittest.TestCase):
    def test_protected_paths_deny(self):
        for p in (
            "/r/clippy.toml", "clippy.toml", "/r/rustfmt.toml", "/r/deny.toml",
            "/r/rust-toolchain.toml", "/r/justfile", "/r/AGENTS.md",
            "/r/CLAUDE.md", "/r/CONTEXT.md", "/r/Cargo.lock", "/r/scripts/x.sh",
            "/r/.github/workflows/ci.yml", "/r/.githooks/pre-commit",
            "/r/.claude/settings.json", "/r/.codex/hooks.json",
            "/r/.devin/config.json", "/r/.pi/extensions/g.ts",
            "/r/.gitignore", "/r/tombi.toml", "/r/_typos.toml",
            "/r/.config/nextest.toml", "/r/.cargo/mutants.toml",
            "/r/.gitattributes", "/r/src/.gitattributes",
            "/r/build.rs", "/r/governor-core/build.rs",
            "/r/docs/guardrails.md",
        ):
            with self.subTest(path=p):
                proc = gate_path(p)
                self.assertEqual(proc.returncode, 2, proc.stdout)
                self.assertEqual(
                    deny_reason(proc)["permissionDecision"], "deny")

    def test_path_field_and_json_shape(self):
        proc = gate_path("/r/clippy.toml", key="path")
        self.assertEqual(proc.returncode, 2)
        out = deny_reason(proc)
        self.assertEqual(out["hookEventName"], "PreToolUse")
        self.assertEqual(out["permissionDecision"], "deny")
        self.assertTrue(out["permissionDecisionReason"])

    def test_cargo_toml_allowed(self):
        # section-protected: hook lets it through, diff gate polices sections
        self.assertEqual(gate_path("/r/Cargo.toml").returncode, 0)
        self.assertEqual(gate_path("/r/governor-core/Cargo.toml").returncode, 0)

    def test_report_paths_allowed(self):
        for p in ("/r/docs/plan/p.md", "/r/docs/research/r.md",
                  "/r/docs/spec/s.md", "/r/docs/adr/a.md",
                  "/r/docs/reviews/r.md", "/r/docs/operations.md"):
            self.assertEqual(gate_path(p).returncode, 0, p)

    def test_conditional_is_agent_writable(self):
        # owner policy 2026-10-02: conditional paths are agent-writable
        # whether or not they exist (the diff gate still FAILs R1 on a
        # change); release-digests.txt moved to hard and always denies
        for rel in ("tests/fixtures/x.json", "tests/support/h.rs",
                    "strategies/g.rs", "gen_strategies.rs"):
            with self.subTest(path=rel):
                with tempfile.TemporaryDirectory() as d:
                    p = Path(d) / rel
                    proc = run("agent-gate.sh", stdin=json.dumps(
                        {"tool_input": {"file_path": str(p)}}), cwd=d)
                    self.assertEqual(proc.returncode, 0, rel)
                    p.parent.mkdir(parents=True, exist_ok=True)
                    p.write_text("x")
                    proc = run("agent-gate.sh", stdin=json.dumps(
                        {"tool_input": {"file_path": str(p)}}), cwd=d)
                    self.assertEqual(proc.returncode, 0, rel)
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "release-digests.txt"
            proc = run("agent-gate.sh", stdin=json.dumps(
                {"tool_input": {"file_path": str(p)}}), cwd=d)
            self.assertEqual(proc.returncode, 2, "release-digests.txt is hard")

    def test_insta_commands_deny(self):
        for c in (
            "cargo insta accept",
            "cargo insta test --accept",
            "cargo insta review",
            "cargo-insta accept",
            "cargo-insta review",
            "cargo-insta test --accept",
            "INSTA_UPDATE=always cargo test",
            "INSTA_UPDATE=force cargo test",
            "INSTA_UPDATE=unseen cargo test",
            "INSTA_UPDATE=new cargo test",
            'INSTA_UPDATE="always" cargo test',
            "INSTA_UPDATE='always' cargo test",
            'env INSTA_UPDATE="force" cargo test',
            'export INSTA_UPDATE="unseen"',
        ):
            with self.subTest(cmd=c):
                self.assertEqual(gate_cmd(c).returncode, 2, c)

    @case("snapshot-self-accept", "control", "a quoted non-accepting INSTA_UPDATE value is not self-acceptance")
    def test_quoted_benign_insta_update_allows(self):
        self.assertEqual(gate_cmd('INSTA_UPDATE="no" cargo test').returncode, 0)

    def test_insta_commands_with_cargo_flags_deny(self):
        # global cargo flags between `cargo` and `insta` change nothing
        for c in (
            "cargo +nightly insta accept",
            "cargo +stable insta accept",
            "cargo +1.98.1 insta accept",
            "cargo --config k=v insta accept",
            "cargo --config=k=v insta accept",
            "cargo -Z unstable-options insta accept",
            "cargo +nightly insta test --accept",
            "cargo --config k=v insta test --accept",
            "cargo +nightly -Z unstable-options insta accept",
            "cargo +nightly insta review",
            "cargo --config k=v insta review",
        ):
            with self.subTest(cmd=c):
                self.assertEqual(gate_cmd(c).returncode, 2, c)

    def test_git_forbidden_commands_deny(self):
        for c in (
            "git push origin main",
            "git commit --no-verify -m x",
            "git rm tests/it.rs",
            "git rm -r --cached tests/",
            "git update-index --skip-worktree clippy.toml",
            "git update-index --assume-unchanged deny.toml",
            "git config core.hooksPath /tmp/x",
            "git -c core.hooksPath=/dev/null commit -m x",
            "git -c core.hooksPath=/tmp/x commit -m x",
            "git -ccore.hooksPath=/dev/null commit -m x",
            "git --config core.hooksPath /tmp/x",
            "git --config-env core.hooksPath=P commit -m x",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.hooksPath GIT_CONFIG_VALUE_0=/dev/null git commit -m x",
            "GIT_CONFIG_PARAMETERS='-c core.hooksPath=/dev/null' git commit -m x",
            "GIT_CONFIG_GLOBAL=/tmp/g git commit -m x",
            "GIT_CONFIG_SYSTEM=/tmp/s git status",
            "GIT_CONFIG_KEY_0=user.name GIT_CONFIG_VALUE_0=x GIT_CONFIG_COUNT=1 git commit -m x",
            "echo core.hooksPath=/dev/null",
            "git clean -fd scripts/",
            "git clean -fdx .github",
            "GOV_PROTECTED_OK=1 just ci",
            "export GOV_PROTECTED_OK=1",
            "env GOV_PROTECTED_OK=1 ./scripts/check-protected-diff.sh",
        ):
            with self.subTest(cmd=c):
                self.assertEqual(gate_cmd(c).returncode, 2, c)

    def test_forge_self_approval_denies(self):
        # the owner-approved label and merges are owner-only actions on a
        # public repo where gh credentials are in scope
        for c in (
            "gh pr label 5 --add-label owner-approved",
            "gh pr label 5 --remove-label owner-approved",
            "gh api -X DELETE repos/o/r/pulls/5/labels/owner-approved",
            "gh api repos/o/r/issues/5/labels -f name=owner-approved",
            "gh pr merge 5",
            "gh pr merge 5 --squash",
            "gh api repos/o/r/pulls/5/merge -X PUT",
            "gh api graphql -f query='mutation { mergePullRequest }'",
        ):
            with self.subTest(cmd=c):
                self.assertEqual(gate_cmd(c).returncode, 2, c)

    def test_write_verb_bypass_denies(self):
        for c in (
            "sed -i s/a/b/ clippy.toml",
            "sed -ni s/a/b/ justfile",
            "perl -pi -e s/a/b/ deny.toml",
            "tee clippy.toml",
            "dd of=clippy.toml",
            "cp /tmp/x rustfmt.toml",
            "mv /tmp/x justfile",
            "install -m755 /tmp/x scripts/",
            "rm -f deny.toml",
            "rm -f build.rs",
            "rm -rf .githooks/pre-commit",
            "chmod -x scripts/agent-gate.sh",
            "chmod 644 scripts/agent-gate.sh",
            "sed -i s/a/b/ .config/nextest.toml",
            "git checkout -- clippy.toml",
            "git restore justfile",
            "echo x > clippy.toml",
            "echo x >> scripts/y.sh",
            "python3 -c \"open('clippy.toml','w')\"",
            "python3 -c \"from pathlib import Path; Path('deny.toml').write_text('x')\"",
        ):
            with self.subTest(cmd=c):
                self.assertEqual(gate_cmd(c).returncode, 2, c)

    def test_benign_allowed(self):
        for c in (
            "cargo test",
            "cargo clippy --all-targets --locked -- -D warnings",
            "cargo insta test",
            "cargo insta pending",
            "cargo insta show",
            "cargo install cargo-insta",
            "ls scripts/",
            "cat clippy.toml",
            "git status",
            "git diff HEAD -- clippy.toml",
            "git add -A",
            "just ci",
            "echo INSTA_UPDATE=no cargo test",
            "git -c user.email=t@t -c user.name=t commit -m x",
            "python3 scripts/test_guardrails.py",
            "rm -rf target/",
            "gh pr status",
            "gh pr checks 5",
            "gh pr view --json mergeable",
            "gh api repos/o/r/issues/5/timeline",
            "gh label list",
        ):
            with self.subTest(cmd=c):
                self.assertEqual(gate_cmd(c).returncode, 0, c)
        self.assertEqual(gate_path("/r/governor-core/src/lib.rs").returncode, 0)
        self.assertEqual(gate_path("/r/herdr-governor/tests/it.rs").returncode, 0)

    def test_malformed_input_allows(self):
        for stdin in ("", "not json", '{"tool_input": 42}', "[1]"):
            proc = run("agent-gate.sh", stdin=stdin, cwd=tempfile.gettempdir())
            self.assertEqual(proc.returncode, 0, stdin)


class MatcherParityTests(unittest.TestCase):
    """pi-review F3: one shared matcher (scripts/protected_paths.py), two
    entry points. The same fixture paths are judged by agent-gate.sh
    (deny = rc 2) and by check-protected-diff.sh (the FAIL/REPORT
    verdict behind its classification); both must agree with the path's
    policy mode across every mode and across `**`/suffix boundaries."""

    # (repo-relative path, expected policy class)
    FIXTURES = (
        ("clippy.toml", "hard"),                             # exact file
        ("scripts/deep/nested.sh", "hard"),                  # dir/** subtree
        (".github/workflows/w.yml", "hard"),                 # dir/** subtree
        ("member/.gitattributes", "hard"),                   # leading ** + suffix
        ("governor-core/Cargo.toml", "section"),             # section mode
        ("tests/fixtures/f.json", "conditional"),            # root tests/** dir
        ("governor-core/tests/support/h.rs", "conditional"),  # member, suffix
        ("herdr-governor/strategies/s.rs", "conditional"),   # member dir/**
        ("gen_strategies.rs", "conditional"),                # bare * glob
        ("docs/spec/s.md", "report"),                        # report subtree
        ("docs/adr/sub/a.md", "report"),                     # deeper report tree
        ("docs/operations.md", "report"),                    # report file
        ("governor-core/src/free.rs", None),                 # member src: open
        ("notes.txt", None),                                 # root file: open
    )

    def test_same_verdicts_through_both_entry_points(self):
        repo = base_repo()
        self.addCleanup(repo.cleanup)
        for rel, _cls in self.FIXTURES:
            if rel != CORE_MANI:  # base_repo already seeds the real manifest
                repo.write(rel, "fn f() {}\n" if rel.endswith(".rs")
                           else "# parity\n" if rel.endswith(".gitattributes")
                           else "x\n")
        repo.commit_all("seed fixture paths")
        for rel, _cls in self.FIXTURES:
            with (repo.dir / rel).open("a") as fh:
                # land in a report-mode TOML section so the section row
                # exercises the matcher without tripping R4/R5; a comment
                # for .gitattributes so no attribute rule is defined
                fh.write("[package.metadata.parity]\nk = \"v\"\n"
                         if rel.endswith("Cargo.toml")
                         else "// parity\n" if rel.endswith(".rs")
                         else "# parity\n" if rel.endswith(".gitattributes")
                         else "parity\n")
        # an untracked conditional path: the hook allows it (conditional is
        # agent-writable) and the diff gate reports instead of failing
        repo.write("tests/support/new_helper.rs", "fn h() {}\n")

        proc = run("check-protected-diff.sh", cwd=repo.dir)
        self.assertEqual(proc.stderr, "", proc.stderr)
        self.assertEqual(proc.returncode, 1, proc.stdout)
        diff_lines = proc.stdout.splitlines()

        def hits(rel):
            pad = " " + rel + " "
            return [l for l in diff_lines if pad in " %s " % l]

        def gate_rc(rel):
            return run("agent-gate.sh", stdin=json.dumps(
                {"tool_input": {"file_path": str(repo.dir / rel)}}),
                cwd=repo.dir).returncode

        for rel, cls in self.FIXTURES:
            with self.subTest(path=rel):
                rc = gate_rc(rel)
                rows = hits(rel)
                if cls == "hard":
                    self.assertEqual(rc, 2, rel)
                    self.assertTrue(
                        any(l.startswith("FAIL R1") for l in rows), rows)
                elif cls == "conditional":
                    # agent-writable, but the diff gate still fails it
                    self.assertEqual(rc, 0, rel)
                    self.assertTrue(
                        any(l.startswith("FAIL R1") for l in rows), rows)
                elif cls == "section":
                    self.assertEqual(rc, 0, rel)
                    self.assertFalse(
                        any(l.startswith("FAIL") for l in rows), rows)
                    self.assertTrue(
                        any(l.startswith("REPORT R4b") for l in rows), rows)
                elif cls == "report":
                    self.assertEqual(rc, 0, rel)
                    self.assertTrue(
                        any(l.startswith("REPORT R1") for l in rows), rows)
                    self.assertFalse(
                        any(l.startswith("FAIL") for l in rows), rows)
                else:
                    self.assertEqual(rc, 0, rel)
                    self.assertEqual(rows, [])

        with self.subTest(path="tests/support/new_helper.rs"):
            self.assertEqual(gate_rc("tests/support/new_helper.rs"), 0)
            rows = hits("tests/support/new_helper.rs")
            self.assertTrue(
                any(l.startswith("REPORT R1") for l in rows), rows)
            self.assertFalse(any(l.startswith("FAIL") for l in rows), rows)


class FmtOnEditTests(unittest.TestCase):
    def test_non_rs_allows(self):
        proc = run("fmt-on-edit.sh", stdin=json.dumps(
            {"tool_input": {"file_path": "/tmp/x.txt"}}))
        self.assertEqual(proc.returncode, 0)

    def test_missing_file_allows(self):
        proc = run("fmt-on-edit.sh", stdin=json.dumps(
            {"tool_input": {"file_path": "/tmp/does-not-exist-xyz.rs"}}))
        self.assertEqual(proc.returncode, 0)

    @unittest.skipUnless(shutil.which("rustfmt"), "rustfmt not installed")
    def test_unformatted_rs_blocks(self):
        with tempfile.NamedTemporaryFile(suffix=".rs", mode="w", delete=False) as f:
            f.write("fn  main( ){let x=1;}\n")
            name = f.name
        try:
            proc = run("fmt-on-edit.sh", stdin=json.dumps(
                {"tool_input": {"file_path": name}}))
            self.assertEqual(proc.returncode, 2)
            self.assertEqual(json.loads(proc.stdout)["decision"], "block")
        finally:
            os.unlink(name)

    @unittest.skipUnless(shutil.which("rustfmt"), "rustfmt not installed")
    def test_formatted_rs_allows(self):
        with tempfile.NamedTemporaryFile(suffix=".rs", mode="w", delete=False) as f:
            f.write("fn main() {}\n")
            name = f.name
        try:
            proc = run("fmt-on-edit.sh", stdin=json.dumps(
                {"tool_input": {"file_path": name}}))
            self.assertEqual(proc.returncode, 0, proc.stdout)
        finally:
            os.unlink(name)


class ProtectedDiffLocalTests(unittest.TestCase):
    def setUp(self):
        self.repo = base_repo()
        self.addCleanup(self.repo.cleanup)

    def check(self, *args, env=None):
        return run("check-protected-diff.sh", args=args, cwd=self.repo.dir, env=env)

    def test_clean_tree_exit0(self):
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("bare-allow", "cheat", "#[allow( added to a member source file")
    def test_allow_attribute_fails(self):
        self.repo.write(CORE_LIB, real_file(CORE_LIB) + "#[allow(dead_code)]\nfn g() {}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R2", proc.stdout)

    @case("reasonless-expect", "cheat", "#[expect] without reason added to a member source file")
    def test_expect_without_reason_fails(self):
        self.repo.write(CORE_LIB, real_file(CORE_LIB) + "#[expect(dead_code)]\nfn g() {}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("R2", proc.stdout)

    @case("reasonless-expect", "control", "#[expect(lint, reason = \"...\")] is the allowed suppression form")
    @case("bare-allow", "control", "reasoned expect instead of a bare allow")
    def test_expect_with_reason_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB) + '#[expect(dead_code, reason = "kept for the FFI seam")]\nfn g() {}\n',
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_expect_variant_on_bin_member(self):
        self.repo.write(BIN_MAIN, real_file(BIN_MAIN) + "#[expect(dead_code)]\nfn g() {}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("R2", proc.stdout)

    def test_ignore_fails(self):
        self.repo.write(CORE_LIB, "#[test]\n#[ignore]\nfn test_x() {}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("R2", proc.stdout)

    def test_mutants_skip_fails(self):
        self.repo.write(CORE_LIB, "#[mutants::skip]\nfn g() {}\n")
        self.assertNotEqual(self.check().returncode, 0)

    def test_forbid_and_cfg_attr_fails(self):
        for line in ("#![forbid(unsafe_code)]", "#[cfg_attr(test, allow(dead_code))]"):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, line + "\nfn g() {}\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("R2", proc.stdout)

    @case("banned-expect", "cheat", "a reasoned expect of a purity lint or silencing group in a diff is still a bypass")
    def test_banned_expect_fails_r2(self):
        for line in (
            '#[expect(clippy::disallowed_methods, reason = "x")]',
            '#[expect(clippy::disallowed_types, reason = "x")]',
            '#[expect(clippy::disallowed_macros, reason = "x")]',
            '#[expect(disallowed_methods, reason = "x")]',
            '#[expect(clippy::all, reason = "x")]',
            '#[expect(clippy::style, reason = "x")]',
            '#[expect(warnings, reason = "x")]',
            '#[cfg_attr(test, expect(clippy::disallowed_methods, reason = "x"))]',
        ):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\nfn g() {}\n")
                proc = self.check()
                self.assertIn("FAIL R2", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("banned-expect", "control", "a reasoned expect of an ordinary lint in a diff is the sanctioned form")
    def test_benign_expect_ok_r2(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB) + '#[expect(dead_code, reason = "x")]\nfn g() {}\n')
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("mutants-skip-attr", "cheat", "spaced and cfg_attr-wrapped mutants::skip spellings in a diff")
    def test_mutants_skip_spellings_fail(self):
        for line in (
            "#[mutants :: skip]",
            "#[cfg_attr(test, mutants::skip)]",
            "#[cfg_attr(test, mutants :: skip)]",
        ):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\nfn g() {}\n")
                proc = self.check()
                self.assertIn("FAIL R2", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("mutants-skip-attr", "control", "naming the attribute inside a comment does not apply it")
    def test_mutants_skip_in_comment_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + "// `#[mutants::skip]` is banned in this repo; do not add it\n"
            + "pub fn g() {}\n")
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("release-only-cfg", "cheat", "comment, spacing and cfg_attr spellings evade the R8 added-line scan")
    def test_release_only_cfg_evasions_fail(self):
        for line in (
            "#[cfg(not(test /**/))]",
            "#[cfg(not ( test ) )]",
            "#[cfg_attr(not(test), derive(Debug))]",
            "if cfg!(not(test /* release-only */)) {",
        ):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\nfn g() {}\n")
                proc = self.check()
                self.assertIn("FAIL R8", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("release-only-cfg", "control", "cfg_attr on the test cfg is ordinary conditional compilation")
    def test_cfg_attr_on_test_cfg_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + '#[cfg_attr(test, expect(dead_code, reason = "x"))]\nfn g() {}\n')
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("release-only-cfg", "cheat", "#[cfg(not(test))] / cfg!(not(test)) hides behaviour from the test build")
    def test_release_only_cfg_fails(self):
        for line in (
            "#[cfg(not(test))]",
            "#[cfg(all(not(test), feature = \"x\"))]",
            "if cfg!(not(test)) {",
        ):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\nfn g() {}\n")
                proc = self.check()
                self.assertIn("FAIL R8", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)

    @case("release-only-cfg", "cheat", "owner override must not muffle R8 (content rule)")
    def test_release_only_cfg_fails_even_under_override(self):
        self.repo.write(CORE_LIB, real_file(CORE_LIB) + "#[cfg(not(test))]\nfn g() {}\n")
        proc = self.check(env={"GOV_PROTECTED_OK": "1"})
        self.assertIn("FAIL R8", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)
        self.assertNotIn("OVERRIDE", proc.stdout)

    @case("release-only-cfg", "control", "#[cfg(test)] is the normal test gate")
    def test_cfg_test_ok(self):
        self.repo.write(CORE_LIB, real_file(CORE_LIB) + "#[cfg(test)]\nmod extra {\n}\n")
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("banned-expect", "cheat", "raw-identifier spellings of the expect head and lint-path segments")
    def test_raw_ident_banned_expect_fails_r2(self):
        for line in (
            '#[r#expect(clippy::disallowed_macros, reason = "x")]',
            '#[expect(clippy::r#disallowed_macros, reason = "x")]',
            '#[r#expect(clippy::r#all, reason = "x")]',
            '#[cfg_attr(test, r#expect(clippy::disallowed_types, reason = "x"))]',
        ):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\nfn g() {}\n")
                proc = self.check()
                self.assertIn("FAIL R2", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("bare-allow", "cheat", "a raw-identifier allow is still a suppression attribute")
    def test_raw_ident_allow_fails_r2(self):
        self.repo.write(CORE_LIB, real_file(CORE_LIB) + "#[r#allow(dead_code)]\nfn g() {}\n")
        proc = self.check()
        self.assertIn("FAIL R2", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)

    @case("release-only-cfg", "cheat", "trailing-comma, raw-predicate and non-paren cfg! spellings")
    def test_release_only_cfg_grammar_variants_fail(self):
        for line in (
            "#[cfg(not(test,))]",
            "#[cfg(r#not(r#test))]",
            "fn h() { let _x = cfg![not(test)]; }",
            "fn h() { let _x = cfg!{not(test)}; }",
            "#[cfg_attr(not(test,), derive(Debug))]",
        ):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\n")
                proc = self.check()
                self.assertIn("FAIL R8", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("release-only-cfg", "control", "a not(test) line inside an edited multiline raw string is data, not code")
    def test_r8_inner_string_line_edit_ok(self):
        # the diff adds a line that *reads* as the banned attribute, but the
        # complete new-side file has it inside a raw string — no finding
        committed = (
            real_file(CORE_LIB)
            + 'const S: &str = r#"sample\nplaceholder\nend"#;\n')
        self.repo.write(CORE_LIB, committed)
        self.repo.commit_all()
        self.repo.write(
            CORE_LIB, committed.replace("placeholder", "#[cfg(not(test))]"))
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("release-only-cfg", "cheat", "editing the inner line of an existing multiline cfg introduces not(test)")
    def test_r8_inner_cfg_line_edit_fails(self):
        # the added line alone is `    not(test)` — the violation is only
        # visible when the engine lexes the whole file; reported at the
        # attribute's real line
        committed = (
            real_file(CORE_LIB)
            + '#[cfg(\n    all(feature = "x")\n)]\nfn gated() {}\n')
        self.repo.write(CORE_LIB, committed)
        self.repo.commit_all()
        self.repo.write(
            CORE_LIB, committed.replace('all(feature = "x")', "not(test)"))
        proc = self.check()
        self.assertIn("FAIL R8", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)
        attr_ln = real_file(CORE_LIB).count("\n") + 1
        self.assertIn("%s:%d:" % (CORE_LIB, attr_ln), proc.stdout)

    @case("banned-expect", "cheat", "the reasoned-expect escape no longer covers non-tests .rs paths")
    def test_banned_expect_outside_member_tests_fails_r2(self):
        # docs/plan is report-mode; its .rs content is still scanned and
        # is NOT member tests/, so the banned target must fail R2 now
        self.repo.write(
            "docs/plan/helper.rs",
            '#[expect(clippy::disallowed_methods, reason = "x")]\nfn g() {}\n')
        proc = self.check()
        self.assertIn("FAIL R2", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)

    @case("banned-expect", "control", "a member tests/ diff keeps the sanctioned escape")
    def test_banned_expect_in_member_tests_ok_r2(self):
        self.repo.write(
            BIN_TEST,
            '#[test]\nfn test_bin() {\n    assert!(true, "smoke");\n}\n'
            '#[expect(clippy::all, reason = "fixture I/O")]\nfn helper() {}\n')
        proc = self.check()
        self.assertNotIn("R2", proc.stdout)
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("unsilenceable-expect", "cheat", "an expect of the function-length lint or its groups in a member tests/ diff still fails R2 — the escape does not reach it")
    def test_unsilenceable_expect_in_member_tests_fails_r2(self):
        for line in (
            '#[expect(clippy::too_many_lines, reason = "x")]',
            '#[expect(too_many_lines, reason = "x")]',
            '#[expect(clippy::pedantic, reason = "x")]',
            '#[expect(warnings, reason = "x")]',
        ):
            with self.subTest(line=line):
                self.repo.write(
                    BIN_TEST,
                    '#[test]\nfn test_bin() {\n    assert!(true, "smoke");\n}\n'
                    + line + '\nfn helper() {}\n')
                proc = self.check()
                self.assertIn("FAIL R2", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(
                    BIN_TEST,
                    '#[test]\nfn test_bin() {\n    assert!(true, "smoke");\n}\n')

    @case("deleted-test", "cheat", "#[test] attribute removed under a surviving signature")
    def test_test_deletion_fails(self):
        # deleted #[test] line inside a member src file
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace("    #[test]\n", ""),
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R3", proc.stdout)

    @case("deleted-test", "cheat", "tracked test file deleted under a member tests/ dir")
    def test_test_file_deletion_fails(self):
        self.repo.remove(CORE_TEST)
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R3", proc.stdout)
        self.assertIn(CORE_TEST, proc.stdout)

    @case("deleted-test", "control", "adding a test under a member tests/ dir is free")
    def test_test_addition_ok(self):
        self.repo.write(BIN + "/tests/extra.rs", "#[test]\nfn test_extra() {}\n")
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("test-moved-off-surface", "cheat", "member test renamed to a non-test path")
    def test_test_file_rename_out_fails(self):
        # a rename out of the test-path set deletes the test file
        self.repo.git("mv", CORE_TEST, "loot.rs")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R3", proc.stdout)

    @case("test-moved-off-surface", "cheat", "member test moved to a non-test src file")
    def test_test_file_rename_to_src_fails(self):
        self.repo.git("mv", CORE_TEST, CORE + "/src/helper.rs")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R3", proc.stdout)

    @case("test-moved-off-surface", "cheat", "a tests/ dir outside member roots compiles nothing")
    def test_test_rename_into_nested_tests_dir_fails(self):
        # C2: a bland tests/ component anywhere else is not a test destination
        for dst in ("docs/tests/it.rs", "vendor/tests/it.rs"):
            with self.subTest(dst=dst):
                (self.repo.dir / dst).parent.mkdir(parents=True, exist_ok=True)
                self.repo.git("mv", CORE_TEST, dst)
                proc = self.check()
                self.assertIn("FAIL R3", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.git("reset", "--hard", "-q", "HEAD")
                self.repo.git("clean", "-fdq")

    @case("test-moved-off-surface", "cheat", "marker-bearing src file renamed off the compiled surface")
    def test_test_content_rename_out_fails(self):
        # member lib.rs carries #[cfg(test)] — moving it off the test-path
        # set removes test content even though its basename lacks 'test'
        self.repo.git("mv", CORE_LIB, "helper.txt")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R3", proc.stdout)

    @case("test-moved-off-surface", "control", "rename within the member test surface is a refactor")
    def test_test_file_rename_within_tests_ok(self):
        for dst in (CORE + "/tests/it2.rs", BIN + "/tests/it2.rs"):
            with self.subTest(dst=dst):
                self.repo.git("mv", CORE_TEST, dst)
                proc = self.check()
                self.assertEqual(proc.returncode, 0, proc.stdout)
                self.repo.git("mv", dst, CORE_TEST)

    @case("test-moved-off-surface", "control", "rename to a test-named src file stays on the surface")
    def test_test_file_rename_to_test_path_ok(self):
        self.repo.git("mv", CORE_TEST, CORE + "/src/it_test.rs")
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_rs_rename_within_src_ok(self):
        # a .rs -> .rs rename staying on the compiled surface is a module
        # refactor, not test deletion — its content is scanned as usual
        for dst in (CORE + "/src/helper.rs", CORE + "/src/state/machine.rs"):
            with self.subTest(dst=dst):
                (self.repo.dir / dst).parent.mkdir(parents=True, exist_ok=True)
                self.repo.git("mv", CORE_LIB, dst)
                proc = self.check()
                self.assertEqual(proc.returncode, 0, proc.stdout)
                self.repo.git("reset", "--hard", "-q", "HEAD")
                self.repo.git("clean", "-fdq")

    def test_rs_rename_off_surface_still_fails(self):
        # marker-bearing files that leave member src/tests still fail
        for dst in ("helper.txt", "tools/lib.rs", "docs/tests/lib.rs"):
            with self.subTest(dst=dst):
                (self.repo.dir / dst).parent.mkdir(parents=True, exist_ok=True)
                self.repo.git("mv", CORE_LIB, dst)
                proc = self.check()
                self.assertIn("FAIL R3", proc.stdout)
                self.repo.git("reset", "--hard", "-q", "HEAD")
                self.repo.git("clean", "-fdq")

    def test_test_file_typechange_fails(self):
        # file -> symlink is a deletion in disguise for non-.rs test files
        self.repo.write(CORE + "/tests/run.sh", "exit 0\n")
        self.repo.commit_all()
        self.repo.git("rm", "-q", CORE + "/tests/run.sh")
        os.symlink("/nonexistent-target", self.repo.dir / CORE / "tests/run.sh")
        self.repo.git("add", CORE + "/tests/run.sh")
        proc = self.check()
        self.assertIn("FAIL R3", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)

    @unittest.expectedFailure
    @case("emptied-test-body", "cheat", "R3 keys on removed marker lines; body-gutting under a surviving signature is a documented gap (the mutants-diff full-run trigger is the real catch)")
    def test_emptied_body_is_a_documented_r3_gap(self):
        # Critic finding 8: gutting a test's body while `fn test_*` survives
        # removes no marker line, so R3 sees nothing. This expected-failure
        # documents the ceiling and flips to an unexpected success if R3
        # learns to catch it.
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace(
                '        assert_eq!(herdr_protocol(), 22, "protocol revision must be 22");\n', ""),
        )
        proc = self.check()
        self.assertIn("FAIL R3", proc.stdout)

    @case("emptied-test-body", "control", "real assertion changes on the test surface pass the diff gate")
    def test_assertion_change_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace("22, \"protocol revision must be 22\"", "22, \"protocol is pinned\""),
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("workspace-membership", "cheat", "root [workspace] members list is a protected section")
    def test_workspace_membership_change_fails(self):
        self.repo.write(
            "Cargo.toml",
            real_file("Cargo.toml").replace(
                'members = ["governor-core", "herdr-governor"]',
                'members = ["governor-core", "herdr-governor", "evil"]'),
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R4", proc.stdout)
        self.assertIn("workspace", proc.stdout)

    @case("workspace-membership", "cheat", "deleting a member manifest is a status change, not content")
    def test_member_manifest_deletion_fails(self):
        self.repo.git("rm", "-q", CORE_MANI)
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R4", proc.stdout)
        self.assertIn(CORE_MANI, proc.stdout)

    def test_workspace_lints_weaken_fails(self):
        self.repo.write(
            "Cargo.toml",
            real_file("Cargo.toml").replace(
                'unwrap_used          = "deny"', 'unwrap_used          = "warn"'),
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("R4", proc.stdout)

    @case("member-features", "cheat", "member manifest gains a [features] section")
    def test_member_features_section_fails(self):
        # inserting before [dependencies] keeps the added lines inside the
        # introduced section — appending lands a blank line under [lints]
        # and fails that way instead (also R4, wrong story)
        self.repo.write(
            CORE_MANI,
            real_file(CORE_MANI).replace(
                "\n[dependencies]", "\n[features]\nfast = []\n\n[dependencies]"),
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R4", proc.stdout)
        self.assertIn("features", proc.stdout)

    @case("member-features", "cheat", "member [features] is R4-fail; override downgrades to a loud report")
    def test_member_features_override_reports(self):
        self.repo.write(
            CORE_MANI,
            real_file(CORE_MANI).replace(
                "\n[dependencies]", "\n[features]\nfast = []\n\n[dependencies]"),
        )
        proc = self.check(env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("OVERRIDE", proc.stdout)

    @case("workspace-membership", "control", "member [package.metadata.*] is reviewable, not protected")
    @case("member-features", "control", "member [package.metadata.*] is reviewable, not protected")
    def test_member_package_metadata_reports(self):
        self.repo.write(
            BIN_MANI,
            real_file(BIN_MANI).replace(
                "\n[dependencies]", '\n[package.metadata.gov]\nkey = "x"\n\n[dependencies]'),
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("REPORT R4b", proc.stdout)

    @case("lockfile-without-manifest", "cheat", "the mirror image: a member dependency change with no lockfile update fails R5")
    def test_dependencies_section_reports(self):
        self.repo.write(
            BIN_MANI,
            real_file(BIN_MANI).replace(
                "[dependencies]", '[dependencies]\nserde = "1"'),
        )
        proc = self.check()
        self.assertIn("R4b", proc.stdout)
        self.assertIn("R5", proc.stdout)  # dep change without lock update
        self.assertNotEqual(proc.returncode, 0)

    @case("lockfile-without-manifest", "cheat", "Cargo.lock edited while no dependency section moved")
    def test_cargo_lock_only_fails(self):
        self.repo.write("Cargo.lock", "# hand edited lockfile\nx\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R5", proc.stdout)

    def test_dep_and_lock_together_fails_without_override(self):
        # dep sections + lockfile moving together is the correct dep-add
        # shape, but Cargo.lock is hard-protected: without owner approval the
        # pair fails R1 (owner decision — dep-adds need approval)
        self.repo.write(
            BIN_MANI,
            real_file(BIN_MANI).replace(
                "[dependencies]", '[dependencies]\nserde = "1"'),
        )
        self.repo.write("Cargo.lock", "# updated lock\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)
        self.assertIn("Cargo.lock", proc.stdout)
        self.assertIn("REPORT R4b", proc.stdout)
        self.assertNotIn("FAIL R5", proc.stdout)

    @case("lockfile-without-manifest", "control", "manifest and lockfile moving together under owner approval")
    def test_dep_and_lock_together_ok_under_override(self):
        # owner-approved dep-add: R1 becomes a loud OVERRIDE report
        self.repo.write(
            BIN_MANI,
            real_file(BIN_MANI).replace(
                "[dependencies]", '[dependencies]\nserde = "1"'),
        )
        self.repo.write("Cargo.lock", "# updated lock\n")
        proc = self.check(env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("OVERRIDE", proc.stdout)
        self.assertIn("REPORT R4b", proc.stdout)

    def test_tracked_protected_override_reports(self):
        self.repo.write("clippy.toml", "allow-unwrap-in-tests = false\n")
        proc = self.check(env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("OVERRIDE", proc.stdout)

    def test_mode_flip_fails(self):
        os.chmod(self.repo.dir / "scripts/x.sh", 0o644)
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("R6", proc.stdout)

    def test_new_script_mode_fails(self):
        self.repo.write("scripts/y.sh", "#!/bin/sh\nexit 0\n", mode=0o644)
        self.repo.git("add", "scripts/y.sh")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("R6", proc.stdout)

    @case("snapshot-self-accept", "cheat", "snapshot self-accept spelling in a tracked file")
    def test_insta_update_line_fails(self):
        self.repo.write("notes.txt", "INSTA_UPDATE=always cargo test\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("R7", proc.stdout)

    def test_tracked_protected_change_fails(self):
        self.repo.write("clippy.toml", "allow-unwrap-in-tests = false\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("R1", proc.stdout)

    @case("weakened-test-helper", "cheat", "editing a tracked assertion helper under tests/support/")
    def test_tracked_test_support_change_fails(self):
        self.repo.write(CORE + "/tests/support/util.rs",
                        "pub fn check(id: u64) -> bool {\n    id > 0\n}\n")
        self.repo.commit_all()
        self.repo.write(CORE + "/tests/support/util.rs",
                        "pub fn check(id: u64) -> bool {\n    let _ = id;\n    true\n}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)
        self.assertIn("tests/support", proc.stdout)

    @case("weakened-test-helper", "control", "a new helper is proposable — REPORT, not FAIL")
    def test_untracked_test_support_reports(self):
        self.repo.write(CORE + "/tests/support/util.rs",
                        "pub fn check(id: u64) -> bool {\n    id > 0\n}\n")
        proc = self.check()
        self.assertIn("REPORT R1", proc.stdout)
        self.assertNotIn("FAIL", proc.stdout)
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("narrowed-strategy", "cheat", "editing a tracked proptest strategy home")
    def test_tracked_strategy_change_fails(self):
        for path in (CORE + "/tests/strategies/gens.rs",
                     CORE + "/src/gen_strategies.rs"):
            with self.subTest(path=path):
                self.repo.write(path, "pub fn gen() -> u64 {\n    7\n}\n")
                self.repo.commit_all()
                self.repo.write(path, "pub fn gen() -> u64 {\n    1\n}\n")
                proc = self.check()
                self.assertIn("FAIL R1", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.git("reset", "--hard", "-q", "HEAD")
                self.repo.git("clean", "-fdq")

    @case("narrowed-strategy", "control", "a new strategy file is proposable — REPORT, not FAIL")
    def test_untracked_strategy_reports(self):
        self.repo.write(CORE + "/tests/strategies/gens.rs",
                        "pub fn gen() -> u64 {\n    7\n}\n")
        proc = self.check()
        self.assertIn("REPORT R1", proc.stdout)
        self.assertNotIn("FAIL", proc.stdout)
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("schema-fixture-byte-change", "cheat", "tracked schema fixture edit fails the conditional path rule")
    def test_schema_fixture_edit_fails_r1(self):
        self.repo.write(SCHEMA_FIXTURE, '{"protocol": 22, "methods": ["x"]}\n')
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)
        self.assertIn("tests/fixtures", proc.stdout)

    def test_untracked_protected_reports(self):
        self.repo.write("scripts/new.sh", "#!/bin/sh\n", mode=0o755)
        proc = self.check()
        self.assertIn("REPORT R1", proc.stdout)
        self.assertNotIn("FAIL R1", proc.stdout)
        self.assertEqual(proc.returncode, 0)

    def test_report_mode_path_reports(self):
        self.repo.write("docs/plan/p.md", "changed plan\n")
        proc = self.check()
        self.assertIn("REPORT", proc.stdout)
        self.assertEqual(proc.returncode, 0)

    def test_allow_in_non_rs_not_flagged(self):
        # attribute rules are Rust-source scoped; prose mentioning them is not
        # an offense (docs live under report-mode paths anyway)
        self.repo.write("notes.txt", "do not add #[allow(dead_code)]\n")
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_r6_ignores_non_shell_scripts_files(self):
        # R6 mirrors I4: only scripts/*.sh and .githooks/* carry exec policy;
        # mode changes on other scripts/* files are an R1 matter, not R6.
        os.chmod(self.repo.dir / "scripts/lint-policy.txt", 0o644)
        self.repo.write("scripts/notes.txt", "x\n", mode=0o644)
        proc = self.check()
        self.assertNotIn("R6", proc.stdout)
        self.assertIn("R1", proc.stdout)  # the protected-path change is still seen

    def test_untracked_script_must_be_executable(self):
        self.repo.write("scripts/y.sh", "#!/bin/sh\nexit 0\n", mode=0o644)
        proc = self.check()
        self.assertIn("FAIL R6", proc.stdout)

    @case("snapshot-self-accept", "cheat", "snapshot self-accept spelling in an untracked file")
    def test_untracked_unprotected_scanned_for_r7(self):
        self.repo.write("scratch-notes.txt", "INSTA_UPDATE=force cargo test\n")
        proc = self.check()
        self.assertIn("FAIL R7", proc.stdout)
        self.assertIn("scratch-notes.txt", proc.stdout)

    def test_untracked_rs_scanned_for_r2(self):
        self.repo.write(CORE + "/src/extra.rs", "#[allow(dead_code)]\nfn g() {}\n")
        proc = self.check()
        self.assertIn("FAIL R2", proc.stdout)

    def test_untracked_oversized_source_fails(self):
        # F3-B: an untracked in-scope file over the scan limit is a FAIL
        # naming the file and the limit — a skipped scan is not a clean one.
        p = self.repo.dir / CORE / "src" / "oversized.rs"
        with open(p, "wb") as fh:
            fh.truncate(8 * 1024 * 1024 + 1)
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R0", proc.stdout)
        self.assertIn("oversized.rs", proc.stdout)
        self.assertIn("8 MiB", proc.stdout)

    def test_untracked_unreadable_source_fails(self):
        # F3-B: same rule when the file exists but cannot be opened.
        p = self.repo.dir / CORE / "src" / "unreadable.rs"
        p.write_text("fn g() {}\n")
        os.chmod(p, 0)
        self.addCleanup(os.chmod, p, 0o644)
        if os.access(p, os.R_OK):
            self.skipTest("running with read-anything privilege")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R0", proc.stdout)
        self.assertIn("unreadable.rs", proc.stdout)

    def test_untracked_dangling_symlink_source_fails(self):
        # pi-review F1: a dangling symlink named like an in-scope source is
        # not a regular file — the isfile guard must not skip it; R0 names
        # it unscannable.
        p = self.repo.dir / CORE / "src" / "dangling.rs"
        os.symlink("/nonexistent-target", p)
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R0", proc.stdout)
        self.assertIn("dangling.rs", proc.stdout)
        self.assertIn("symlink", proc.stdout)

    @case("snapshot-self-accept", "control", "hard policy files legitimately quote the forbidden patterns — R1 gates them, R7 stays silent")
    def test_r7_exempts_protected_policy_files(self):
        self.repo.write(
            "docs/guardrails.md",
            "INSTA_UPDATE=always|force, cargo insta accept, insta --accept: FAIL\n",
        )
        self.repo.write(
            "scripts/lint-policy.txt",
            real_file("scripts/lint-policy.txt") + "# cargo insta accept\n",
        )
        proc = self.check()
        self.assertNotIn("R7", proc.stdout)
        self.assertIn("R1", proc.stdout)  # both changes are still reported

    @case("snapshot-self-accept", "cheat", "INSTA_UPDATE spellings on unprotected paths")
    def test_r7_still_scans_unprotected_paths(self):
        self.repo.write("notes.txt", "cargo insta accept\n")
        proc = self.check()
        self.assertIn("FAIL R7", proc.stdout)

    @case("snapshot-self-accept", "cheat", "R7 is a content rule — the owner override cannot muffle it")
    def test_r7_fails_even_under_override(self):
        self.repo.write("notes.txt", "INSTA_UPDATE=always cargo test\n")
        proc = self.check(env={"GOV_PROTECTED_OK": "1"})
        self.assertNotEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("FAIL R7", proc.stdout)
        self.assertNotIn("OVERRIDE", proc.stdout)

    @case("snapshot-self-accept", "cheat", "insta machinery seeded into an agent-writable conditional test helper")
    def test_r7_scans_conditional_paths(self):
        # conditional paths are agent-writable (2026-10-02), so R7 scans
        # them; only hard policy files stay exempt
        self.repo.write(CORE + "/tests/support/util.rs",
                        "// cargo insta accept\npub fn h() {}\n")
        proc = self.check()
        self.assertIn("FAIL R7", proc.stdout)
        self.assertIn("tests/support/util.rs", proc.stdout)

    @case("snapshot-self-accept", "cheat", "insta machinery seeded into report-mode paths")
    def test_r7_scans_report_mode_paths(self):
        self.repo.write("docs/plan/p.md", "plan\ncargo insta accept\n")
        proc = self.check()
        self.assertIn("FAIL R7", proc.stdout)

    def test_r7_scans_untracked_report_mode(self):
        self.repo.write("docs/plan/evil.md", "INSTA_UPDATE=unseen cargo test\n")
        proc = self.check()
        self.assertIn("FAIL R7", proc.stdout)

    @case("snapshot-self-accept", "cheat", "the cargo-insta binary spelling")
    def test_cargo_insta_spelling_fails_r7(self):
        self.repo.write("notes.txt", "cargo-insta accept\n")
        proc = self.check()
        self.assertIn("FAIL R7", proc.stdout)

    def test_cargo_flagged_insta_fails_r7(self):
        for line in (
            "cargo +nightly insta accept",
            "cargo --config k=v insta accept",
            "cargo +nightly insta test --accept",
        ):
            with self.subTest(line=line):
                self.repo.write("notes.txt", line + "\n")
                proc = self.check()
                self.assertIn("FAIL R7", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)

    @case("snapshot-self-accept", "cheat", "the plain review spelling is interactive snapshot acceptance")
    def test_insta_review_fails_r7(self):
        for line in (
            "cargo insta review",
            "cargo-insta review",
            "cargo +stable insta review",
            "cargo --config k=v insta review",
        ):
            with self.subTest(line=line):
                self.repo.write("notes.txt", line + "\n")
                proc = self.check()
                self.assertIn("FAIL R7", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)

    @case("snapshot-self-accept", "control", "non-accepting insta subcommands are not self-acceptance")
    def test_insta_non_accept_spellings_ok(self):
        self.repo.write("notes.txt", "cargo insta test\ncargo insta pending\n")
        proc = self.check()
        self.assertNotIn("R7", proc.stdout)

    @case("snapshot-self-accept", "cheat", "quoted INSTA_UPDATE values are the same self-acceptance")
    def test_quoted_insta_update_fails_r7(self):
        for line in (
            'INSTA_UPDATE="always" cargo test',
            "INSTA_UPDATE='always' cargo test",
            'env INSTA_UPDATE="force" cargo test',
            'INSTA_UPDATE = "unseen" cargo test',
        ):
            with self.subTest(line=line):
                self.repo.write("notes.txt", line + "\n")
                proc = self.check()
                self.assertIn("FAIL R7", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.remove("notes.txt")

    @case("snapshot-self-accept", "control", "a quoted non-accepting value is not self-acceptance")
    def test_quoted_benign_insta_update_ok_r7(self):
        self.repo.write("notes.txt", 'INSTA_UPDATE="no" cargo test\n')
        proc = self.check()
        self.assertNotIn("R7", proc.stdout)

    def test_gitattributes_cannot_mask_content(self):
        # C1: `*.rs -diff` renders changes as binary without --text; the gate
        # must still see (and fail) the sabotaged content
        self.repo.write(".gitattributes", "*.rs -diff\n")
        self.repo.write(CORE_LIB, "pub fn f() {}\n#[allow(dead_code)]\nfn g() {}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R2", proc.stdout)

    def test_gitattributes_staged_fails_r1(self):
        # a staged .gitattributes is inside the deliverable diff where it can
        # mask the owner's review — adding it is a protected-path failure
        self.repo.write(".gitattributes", "*.rs -diff\n")
        self.repo.git("add", ".gitattributes")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)
        self.assertIn(".gitattributes", proc.stdout)

    def test_nested_gitattributes_fails_r1(self):
        self.repo.write(CORE + "/src/.gitattributes", "*.rs binary\n")
        self.repo.git("add", CORE + "/src/.gitattributes")
        proc = self.check()
        self.assertIn("FAIL R1", proc.stdout)


class ProtectedDiffCiTests(unittest.TestCase):
    def setUp(self):
        self.repo = base_repo()
        self.addCleanup(self.repo.cleanup)

    def check(self, *args, env=None):
        return run("check-protected-diff.sh", args=args, cwd=self.repo.dir, env=env)

    def test_protected_path_fails(self):
        base = self.repo.head()
        self.repo.write("clippy.toml", "x = 1\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)

    def test_override_reports(self):
        base = self.repo.head()
        self.repo.write("clippy.toml", "x = 1\n")
        self.repo.commit_all()
        proc = self.check(base, env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("OVERRIDE", proc.stdout)

    @case("lockfile-without-manifest", "cheat", "committed lockfile-only change fails R5 in CI mode")
    def test_lock_only_fails_r5_without_override(self):
        base = self.repo.head()
        self.repo.write("Cargo.lock", "# hand edit\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R5", proc.stdout)

    def test_lock_only_reports_r5_under_override(self):
        # owner-approved lockfile change: R1 and R5 both degrade to loud
        # OVERRIDE report lines — approved protected edits have a pass path
        base = self.repo.head()
        self.repo.write("Cargo.lock", "# hand edit\n")
        self.repo.commit_all()
        proc = self.check(base, env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("OVERRIDE", proc.stdout)
        self.assertIn("REPORT R5", proc.stdout)

    def test_dep_without_lock_fails_r5(self):
        base = self.repo.head()
        self.repo.write(
            BIN_MANI,
            real_file(BIN_MANI).replace(
                "[dependencies]", '[dependencies]\nserde = "1"'),
        )
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R5", proc.stdout)

    def test_dep_without_lock_reports_r5_under_override(self):
        base = self.repo.head()
        self.repo.write(
            BIN_MANI,
            real_file(BIN_MANI).replace(
                "[dependencies]", '[dependencies]\nserde = "1"'),
        )
        self.repo.commit_all()
        proc = self.check(base, env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("REPORT R5", proc.stdout)

    def test_dep_and_lock_together_ok(self):
        base = self.repo.head()
        self.repo.write(
            BIN_MANI,
            real_file(BIN_MANI).replace(
                "[dependencies]", '[dependencies]\nserde = "1"'),
        )
        self.repo.write("Cargo.lock", "# updated lock\n")
        self.repo.commit_all()
        proc = self.check(base, env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("REPORT R4b", proc.stdout)

    def test_dep_and_lock_together_fails_without_override(self):
        base = self.repo.head()
        self.repo.write(
            BIN_MANI,
            real_file(BIN_MANI).replace(
                "[dependencies]", '[dependencies]\nserde = "1"'),
        )
        self.repo.write("Cargo.lock", "# updated lock\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)
        self.assertIn("REPORT R4b", proc.stdout)

    def test_report_mode_reports_not_fail(self):
        base = self.repo.head()
        self.repo.write("docs/plan/p.md", "new plan\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("REPORT", proc.stdout)

    def test_scaffold_self_diff_passes_only_with_approval(self):
        # the scaffold's own PR shape: protected policy files whose contents
        # quote the forbidden patterns, a new exec-bit gate script, a justfile
        base = self.repo.head()
        self.repo.write(
            "AGENTS.md",
            "# rules\nnever run INSTA_UPDATE=always or cargo insta accept\n",
        )
        self.repo.write(
            "docs/guardrails.md",
            "R7: INSTA_UPDATE=always|force, cargo insta accept -> FAIL\n",
        )
        self.repo.write("scripts/gate2.sh", "#!/bin/sh\nexit 0\n", mode=0o755)
        self.repo.write("justfile", "ci:\n\ttrue\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)
        proc = self.check(base, env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("OVERRIDE", proc.stdout)
        self.assertIn("REPORT", proc.stdout)
        self.assertNotIn("FAIL", proc.stdout)

    @case("workspace-membership", "cheat", "committed members-list change fails R4 in CI mode")
    def test_workspace_membership_change_fails_ci(self):
        base = self.repo.head()
        self.repo.write(
            "Cargo.toml",
            real_file("Cargo.toml").replace(
                'members = ["governor-core", "herdr-governor"]',
                'members = ["governor-core"]'),
        )
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R4", proc.stdout)

    @case("member-features", "cheat", "committed member [features] introduction fails R4 in CI mode")
    def test_member_features_introduced_fails_ci(self):
        base = self.repo.head()
        self.repo.write(
            CORE_MANI,
            real_file(CORE_MANI).replace(
                "\n[dependencies]", "\n[features]\nfast = []\n\n[dependencies]"),
        )
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R4", proc.stdout)
        self.assertIn("features", proc.stdout)

    @case("release-only-cfg", "cheat", "committed release-only cfg fails R8 in CI mode")
    def test_release_only_cfg_fails_ci(self):
        base = self.repo.head()
        self.repo.write(CORE_LIB, real_file(CORE_LIB) + "#[cfg(not(test))]\nfn g() {}\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R8", proc.stdout)

    @case("release-only-cfg", "cheat", "committed comment/cfg_attr spellings fail R8 in CI mode too")
    def test_release_only_cfg_evasions_fail_ci(self):
        for line in (
            "#[cfg(not(test /**/))]",
            "#[cfg_attr(not(test), derive(Debug))]",
        ):
            with self.subTest(line=line):
                base = self.repo.head()
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\nfn g() {}\n")
                self.repo.commit_all()
                proc = self.check(base)
                self.assertIn("FAIL R8", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.git("reset", "--hard", "-q", "HEAD~1")

    @case("banned-expect", "cheat", "a committed raw-identifier expect fails R2 in CI mode")
    def test_raw_ident_banned_expect_fails_ci(self):
        base = self.repo.head()
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + '#[r#expect(clippy::r#disallowed_macros, reason = "x")]\nfn g() {}\n')
        self.repo.commit_all()
        proc = self.check(base)
        self.assertIn("FAIL R2", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)

    @case("release-only-cfg", "cheat", "committed inner-line edit into not(test) fails R8 in CI mode")
    def test_r8_inner_cfg_line_edit_fails_ci(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + '#[cfg(\n    all(feature = "x")\n)]\nfn gated() {}\n')
        self.repo.commit_all()
        base = self.repo.head()
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + '#[cfg(\n    not(test)\n)]\nfn gated() {}\n')
        self.repo.commit_all()
        proc = self.check(base)
        self.assertIn("FAIL R8", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)

    @case("release-only-cfg", "control", "committed edit inside a raw string literal stays data in CI mode")
    def test_r8_inner_string_line_edit_ok_ci(self):
        committed = (
            real_file(CORE_LIB)
            + 'const S: &str = r#"sample\nplaceholder\nend"#;\n')
        self.repo.write(CORE_LIB, committed)
        self.repo.commit_all()
        base = self.repo.head()
        self.repo.write(
            CORE_LIB, committed.replace("placeholder", "#[cfg(not(test))]"))
        self.repo.commit_all()
        proc = self.check(base)
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("schema-fixture-byte-change", "cheat", "committed fixture byte change fails the conditional path rule in CI mode")
    def test_schema_fixture_edit_fails_ci(self):
        base = self.repo.head()
        self.repo.write(SCHEMA_FIXTURE, '{"protocol": 22, "methods": ["x"]}\n')
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)

    @case("weakened-test-helper", "cheat", "committed helper edit fails R1 in CI mode")
    def test_test_support_change_fails_ci(self):
        self.repo.write(CORE + "/tests/support/util.rs",
                        "pub fn check(id: u64) -> bool {\n    id > 0\n}\n")
        self.repo.commit_all()
        base = self.repo.head()
        self.repo.write(CORE + "/tests/support/util.rs",
                        "pub fn check(id: u64) -> bool {\n    let _ = id;\n    true\n}\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)

    @case("deleted-test", "cheat", "committed test rename off the surface fails R3 in CI mode")
    def test_test_file_rename_out_fails_ci(self):
        base = self.repo.head()
        self.repo.git("mv", CORE_TEST, "loot.rs")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R3", proc.stdout)

    def test_r4_bootstrap_introduced_section(self):
        # first introduction of a protected section (absent at BASE): FAIL
        # without approval, loud OVERRIDE report with it
        self.repo.write("Cargo.toml", '[package]\nname = "x"\nedition = "2024"\n')
        self.repo.commit_all()
        base = self.repo.head()
        self.repo.write(
            "Cargo.toml",
            '[package]\nname = "x"\nedition = "2024"\n\n[lints.clippy]\nunwrap_used = "deny"\n',
        )
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R4", proc.stdout)
        self.assertIn("lints.clippy", proc.stdout)
        proc = self.check(base, env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("REPORT R4", proc.stdout)
        self.assertIn("OVERRIDE", proc.stdout)

    def test_r7_fails_on_report_mode(self):
        base = self.repo.head()
        self.repo.write("docs/plan/p.md", "plan\nINSTA_UPDATE=always cargo test\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R7", proc.stdout)

    @case("snapshot-self-accept", "cheat", "the plain review spelling committed on an unprotected path")
    def test_r7_review_spelling_fails_ci(self):
        base = self.repo.head()
        self.repo.write("notes.txt", "cargo insta review\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R7", proc.stdout)

    def test_bad_base_fails_closed(self):
        proc = self.check("nonexistent-ref")
        self.assertNotEqual(proc.returncode, 0)

    def test_gitattributes_committed_fails_and_unmasks(self):
        # C1 in CI mode: the carrier file fails R1 and the masked content
        # still reaches the R2 scan
        base = self.repo.head()
        self.repo.write(".gitattributes", "*.rs -diff\n")
        self.repo.write(CORE_LIB, "pub fn f() {}\n#[allow(dead_code)]\nfn g() {}\n")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R1", proc.stdout)
        self.assertIn("FAIL R2", proc.stdout)

    def test_test_rename_into_nested_tests_dir_fails_ci(self):
        base = self.repo.head()
        (self.repo.dir / "docs/tests").mkdir(parents=True)
        self.repo.git("mv", CORE_TEST, "docs/tests/it.rs")
        self.repo.commit_all()
        proc = self.check(base)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL R3", proc.stdout)

    @staticmethod
    def _job_section(text, job):
        """Slice of ci.yml covering one job id: `  job:` to the next job key."""
        import re
        m = re.search(r"(?m)^  %s:$" % re.escape(job), text)
        if not m:
            raise AssertionError("no job %r in ci.yml" % job)
        n = re.compile(r"(?m)^  [a-zA-Z_-]+:$").search(text, m.end())
        return text[m.start():n.start() if n else len(text)]

    def test_ci_selftest_materializes_full_base_tree(self):
        # P1: the selftest's BASE archive must be the COMPLETE tree —
        # test_guardrails.py resolves ROOT from its own location, so a
        # scripts-only extract leaves it without manifests, member sources,
        # fixtures or harness dirs on every post-bootstrap PR.
        ci = real_file(".github/workflows/ci.yml")
        selftest = self._job_section(ci, "guard-selftest")
        self.assertIn('git archive "$base" | tar -x', selftest)
        self.assertNotIn('git archive "$base" scripts', selftest)
        self.assertIn('"$GATE" test_guardrails.py', selftest)
        # production checks keep judging the PR worktree with BASE's
        # scripts — the full-tree extract is for the selftest alone
        guard = self._job_section(ci, "guard")
        self.assertIn('git archive "$base" scripts', guard)
        self.assertIn('"$GATE" check-protected-diff.sh "$BASE_SHA"', guard)

    def test_base_tree_selftest_smoke(self):
        # P1 smoke: replay the CI layout — archive a post-scaffold BASE
        # into .base-gates inside a fresh worktree, then let the ARCHIVED
        # copy of this suite run ROOT-dependent tests of its own.
        if os.environ.get("GOV_SELFTEST_NESTED"):
            self.skipTest("running inside the archived suite copy")
        base = full_repo()
        self.addCleanup(base.cleanup)
        for d in (".claude", ".devin", ".codex", ".pi"):
            shutil.copytree(harness_dir(d), base.dir / d)
        # install_scripts skips test_*; the suite module itself must be
        # tracked at BASE for the archive to carry it, as in the real repo
        base.write(
            "scripts/test_guardrails.py",
            real_file("scripts/test_guardrails.py"),
            mode=0o755,
        )
        base.commit_all("post-scaffold BASE")
        wt = Path(tempfile.mkdtemp(prefix="selftest-ci-"))
        self.addCleanup(shutil.rmtree, wt, True)
        bg = wt / ".base-gates"
        bg.mkdir()
        subprocess.run(
            ["bash", "-c", 'git -C "$1" archive HEAD | tar -x -C "$2"',
             "_", str(base.dir), str(bg)],
            check=True, capture_output=True, text=True,
        )
        self.assertTrue((bg / "scripts/test_guardrails.py").is_file())
        self.assertTrue((bg / "Cargo.toml").is_file())
        self.assertTrue((bg / ".claude").is_dir())
        prog = (
            "import sys, unittest\n"
            "sys.path.insert(0, sys.argv[1])\n"
            "import test_guardrails as tg\n"
            "tg.real_file('Cargo.toml')\n"
            "tg.harness_dir('.claude')\n"
            "suite = unittest.TestLoader().loadTestsFromNames(sys.argv[2:], tg)\n"
            "res = unittest.TextTestRunner(stream=sys.stderr).run(suite)\n"
            "sys.exit(0 if res.wasSuccessful() else 1)\n"
        )
        child = subprocess.run(
            [sys.executable, "-c", prog, str(bg / "scripts"),
             "LintIntegrityTests.test_clean_repo_ok",
             "LintIntegrityTests.test_i5_real_configs_pass",
             "ProtectedDiffCiTests.test_bad_base_fails_closed",
             "OwnerApprovalTests.test_bound_label_approves"],
            # import, not script, invocation (the runner picks tests), so
            # stop the child from leaving __pycache__ in the archived
            # scripts/ — install_scripts iterates it wholesale
            env={**os.environ, "GOV_SELFTEST_NESTED": "1",
                 "PYTHONDONTWRITEBYTECODE": "1"},
            capture_output=True, text=True, cwd=str(wt), timeout=300,
        )
        self.assertEqual(child.returncode, 0, child.stderr[-2000:])


class LintIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.repo = full_repo()
        self.addCleanup(self.repo.cleanup)

    def check(self, env=None):
        return run("check-lint-integrity.sh", cwd=self.repo.dir, env=env)

    def test_clean_repo_ok(self):
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_weakened_lint_fails(self):
        self.repo.write(
            "Cargo.toml",
            real_file("Cargo.toml").replace(
                'unwrap_used          = "deny"', 'unwrap_used          = "warn"'),
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I2", proc.stdout)

    def test_missing_required_tracked_fails(self):
        self.repo.remove("clippy.toml")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I1", proc.stdout)

    def test_empty_required_fails(self):
        self.repo.write("AGENTS.md", "")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I1", proc.stdout)

    def test_never_tracked_required_reports(self):
        # repo without deny.toml at all -> REPORT (not FAIL) outside CI
        r2 = base_repo()
        self.addCleanup(r2.cleanup)
        for name in ("clippy.toml", "rust-toolchain.toml", "rustfmt.toml"):
            r2.write(name, real_file(name))
        r2.write(CORE + "/clippy.toml", real_file(CORE + "/clippy.toml"))
        r2.write("justfile", "ci:\n\ttrue\n")
        r2.write("AGENTS.md", "# a\n")
        r2.commit_all()
        proc = run("check-lint-integrity.sh", cwd=r2.dir)
        self.assertIn("REPORT I1", proc.stdout)
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("bare-allow", "cheat", "suppression attributes committed to member sources")
    def test_suppression_attrs_fail(self):
        for line in ("#[allow(dead_code)]", "#![deny(x)]", "#[warn(x)]", "#[forbid(x)]"):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, line + "\nfn g() {}\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)

    @case("reasonless-expect", "cheat", "committed reasonless expect on a member source")
    def test_expect_without_reason_fails(self):
        self.repo.write(CORE_LIB, "#[expect(dead_code)]\nfn g() {}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I3", proc.stdout)

    def test_expect_with_reason_ok(self):
        self.repo.write(
            CORE_LIB, '#![no_std]\n#[expect(dead_code, reason = "x")]\nfn g() {}\n')
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_expect_in_member_tests_scanned(self):
        self.repo.write(BIN_TEST, "#[expect(dead_code)]\nfn g() {}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I3", proc.stdout)

    def test_ignore_fails(self):
        self.repo.write(CORE_LIB, "#[ignore]\nfn g() {}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I3", proc.stdout)

    @case("release-only-cfg", "cheat", "committed release-only cfg trips the I3 mirror scan")
    def test_release_only_cfg_fails_i3(self):
        for line in ("#[cfg(not(test))]", "if cfg!(not(test)) {"):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\nfn g() {}\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)

    @case("release-only-cfg", "cheat", "comment, spacing and cfg_attr spellings evade a raw regex but not the stripped scan")
    def test_release_only_cfg_evasions_fail_i3(self):
        for line in (
            "#[cfg(not(test /**/))]",
            "#[cfg(not ( test ) )]",
            "#[cfg_attr(not(test), derive(Debug))]",
            "if cfg!(not(test /* release-only */)) {",
        ):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\nfn g() {}\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("release-only-cfg", "control", "an attribute in a comment is documentation, not a gate")
    def test_cfg_attr_in_comment_ok_i3(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + "// e.g. #[cfg(not(test))] or #[allow(x)] would be banned\n"
            + "pub fn g() {}\n")
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("banned-expect", "cheat", "expect of a purity lint or a silencing group bypasses the clippy layer")
    def test_banned_expect_targets_fail_i3(self):
        for attr in (
            '#[expect(clippy::disallowed_methods, reason = "x")]',
            '#[expect(clippy::disallowed_types, reason = "x")]',
            '#[expect(clippy::disallowed_macros, reason = "x")]',
            '#[expect(disallowed_methods, reason = "x")]',
            '#[expect(clippy::all, reason = "x")]',
            '#[expect(clippy::style, reason = "x")]',
            '#[expect(warnings, reason = "x")]',
            '#[cfg_attr(test, expect(clippy::disallowed_methods, reason = "x"))]',
            '#![expect(clippy::all, reason = "x")]',
        ):
            with self.subTest(attr=attr):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + attr + "\nfn g() {}\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("banned-expect", "cheat", "the ban covers every member src tree, not just governor-core")
    def test_banned_expect_in_bin_src_fails(self):
        self.repo.write(
            BIN + "/src/escape.rs",
            '#[expect(clippy::disallowed_methods, reason = "x")]\nfn g() {}\n')
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I3", proc.stdout)

    @case("banned-expect", "control", "a reasoned purity-lint expect is the sanctioned escape in member tests/ dirs")
    def test_banned_expect_in_tests_dir_ok(self):
        self.repo.write(
            BIN_TEST,
            '#[expect(clippy::disallowed_methods, reason = "fixture I/O")]\nfn g() {}\n')
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("banned-expect", "control", "a reasoned expect of an ordinary lint in src is the sanctioned form")
    def test_benign_expect_in_src_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB) + '#[expect(dead_code, reason = "x")]\nfn g() {}\n')
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("mutants-skip-attr", "cheat", "committed mutants::skip in spaced or cfg_attr spellings trips the source scan")
    def test_mutants_skip_spellings_fail_i3(self):
        for line in ("#[mutants :: skip]", "#[cfg_attr(test, mutants::skip)]"):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\nfn g() {}\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("mutants-skip-attr", "control", "a comment naming the attribute does not apply it")
    def test_mutants_skip_in_comment_ok_i3(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB) + "// `#[mutants::skip]` is banned in this repo\n")
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("banned-expect", "cheat", "raw-identifier spellings of the expect head and lint-path segments")
    def test_raw_ident_banned_expect_fails_i3(self):
        for attr in (
            '#[r#expect(clippy::disallowed_macros, reason = "fixture")]',
            '#[expect(clippy::r#disallowed_macros, reason = "fixture")]',
            '#[r#expect(clippy::r#all, reason = "fixture")]',
            '#[cfg_attr(test, r#expect(clippy::disallowed_types, reason = "x"))]',
        ):
            with self.subTest(attr=attr):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + attr + "\nfn g() {}\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("bare-allow", "cheat", "a raw-identifier allow is still a suppression attribute")
    def test_raw_ident_allow_fails_i3(self):
        self.repo.write(CORE_LIB, real_file(CORE_LIB) + "#[r#allow(dead_code)]\nfn g() {}\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I3", proc.stdout)

    @case("release-only-cfg", "cheat", "trailing-comma, raw-predicate and non-paren cfg! spellings committed")
    def test_release_only_cfg_grammar_variants_fail_i3(self):
        for line in (
            "#[cfg(not(test,))]",
            "#[cfg(r#not(r#test))]",
            "fn h() { let _x = cfg![not(test)]; }",
            "fn h() { let _x = cfg!{not(test)}; }",
            "#[cfg_attr(not(test,), derive(Debug))]",
        ):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("banned-expect", "cheat", "a banned expect under a non-member tests/ dir loses the exemption")
    def test_banned_expect_in_nonmember_tests_fails_i3(self):
        # vendor/ has no Cargo.toml — it is not a member, so vendor/tests/
        # is not the sanctioned escape; the root tests/ dir is not a member
        # tests/ dir under a virtual root manifest either
        for p in ("vendor/tests/x.rs", "tests/x.rs"):
            with self.subTest(path=p):
                self.repo.write(
                    p, '#[expect(clippy::all, reason = "x")]\nfn g() {}\n')
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)
                self.repo.remove(p)

    @case("unsilenceable-expect", "cheat", "expect of the function-length lint or a silencing group fails I3 in member src/ — every spelling and form")
    def test_unsilenceable_expect_fails_i3_src(self):
        for attr in (
            '#[expect(too_many_lines, reason = "x")]',
            '#[expect(clippy::too_many_lines, reason = "x")]',
            '#[expect(clippy::r#too_many_lines, reason = "x")]',
            '#[r#expect(clippy::too_many_lines, reason = "x")]',
            '#[expect(clippy::pedantic, reason = "x")]',
            '#[expect(warnings, reason = "x")]',
            '#[expect(r#warnings, reason = "x")]',
            '#![expect(clippy::too_many_lines, reason = "x")]',
            '#![expect(clippy::pedantic, reason = "x")]',
            '#[cfg_attr(test, expect(clippy::too_many_lines, reason = "x"))]',
            '#[cfg_attr(test, expect(clippy::pedantic, reason = "x"))]',
            '#[cfg_attr(test, expect(warnings, reason = "x"))]',
        ):
            with self.subTest(attr=attr):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + attr + "\nfn g() {}\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("unsilenceable-expect", "cheat", "the member tests/ escape does NOT reach the function-length lint or its groups")
    def test_unsilenceable_expect_fails_i3_tests(self):
        base = '#[test]\nfn test_bin() {\n    assert!(true, "smoke");\n}\n'
        for attr in (
            '#[expect(too_many_lines, reason = "x")]',
            '#[expect(clippy::too_many_lines, reason = "x")]',
            '#[expect(clippy::r#too_many_lines, reason = "x")]',
            '#[expect(clippy::pedantic, reason = "x")]',
            '#[expect(warnings, reason = "x")]',
            '#[cfg_attr(test, expect(clippy::too_many_lines, reason = "x"))]',
            '#[cfg_attr(test, expect(clippy::pedantic, reason = "x"))]',
            '#[cfg_attr(test, expect(warnings, reason = "x"))]',
        ):
            with self.subTest(attr=attr):
                self.repo.write(BIN_TEST, base + attr + "\nfn helper() {}\n")
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I3", proc.stdout)
                self.repo.write(BIN_TEST, base)
        with self.subTest(attr="inner"):
            self.repo.write(
                BIN_TEST,
                '#![expect(clippy::too_many_lines, reason = "x")]\n' + base)
            proc = self.check()
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("I3", proc.stdout)
            self.repo.write(BIN_TEST, base)

    @case("unsilenceable-expect", "control", "a reasoned expect of an ordinary lint in member tests/ stays sanctioned")
    def test_ordinary_expect_in_tests_ok_i3(self):
        self.repo.write(
            BIN_TEST,
            '#[expect(dead_code, reason = "helper kept for a later case")]\n'
            "fn helper() {}\n"
            '#[test]\nfn test_bin() {\n    assert!(true, "smoke");\n}\n')
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("unsilenceable-expect", "control", "the reasoned disallowed_* fixture escape in member tests/ is not narrowed")
    def test_purity_expect_escape_in_tests_ok_i3(self):
        self.repo.write(
            BIN_TEST,
            '#![expect(clippy::disallowed_methods, reason = "fixture I/O")]\n'
            '#[test]\nfn test_bin() {\n    assert!(true, "smoke");\n}\n')
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("raised-lines-threshold", "cheat", "raising too-many-lines-threshold in either clippy.toml fails I2")
    def test_raised_lines_threshold_fails_i2(self):
        for path, old, new in (
            ("clippy.toml",
             "too-many-lines-threshold      = 100",
             "too-many-lines-threshold      = 500"),
            (CORE + "/clippy.toml",
             "too-many-lines-threshold = 100",
             "too-many-lines-threshold = 500"),
        ):
            with self.subTest(path=path):
                self.repo.write(path, real_file(path).replace(old, new))
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("I2", proc.stdout)
                self.repo.write(path, real_file(path))

    @case("raised-lines-threshold", "control", "an unpinned clippy threshold stays editable — the pin names one option exactly")
    def test_other_threshold_edit_ok_i2(self):
        self.repo.write(
            "clippy.toml",
            real_file("clippy.toml").replace(
                "cognitive-complexity-threshold = 20",
                "cognitive-complexity-threshold = 25"))
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def _stub_bin(self, name, body):
        stub = Path(tempfile.mkdtemp(prefix="guardrails-stub-"))
        self.addCleanup(shutil.rmtree, stub, True)
        tool = stub / name
        tool.write_text(body)
        os.chmod(tool, 0o755)
        return {"PATH": str(stub) + os.pathsep + os.environ["PATH"]}

    def test_i3_scanner_failure_fails_closed(self):
        # a crashed/killed I3 python producer must fail the gate — inside
        # process substitution its exit status never reached the parent
        env = self._stub_bin("python3", (
            "#!/bin/sh\n"
            'if [ "$1" = "-B" ] && [ "$2" = "-" ] && [ -d "$3" ]; then\n'
            '    echo "stub: I3 scanner crash" >&2\n'
            "    exit 71\n"
            "fi\n"
            'exec "%s" "$@"\n' % sys.executable))
        proc = self.check(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I3", proc.stdout)
        self.assertIn("exited 71", proc.stdout)

    def test_second_python_scanner_failure_fails_closed(self):
        # the I8 manifest producer is `python3 - <manifests...>` (no -B):
        # kill only that invocation; the I3 producer passes through
        env = self._stub_bin("python3", (
            "#!/bin/sh\n"
            'if [ "$1" = "-B" ]; then\n'
            '    exec "%s" "$@"\n'
            "fi\n"
            'if [ "$1" = "-" ]; then\n'
            '    echo "stub: I8 scanner crash" >&2\n'
            "    exit 71\n"
            "fi\n"
            'exec "%s" "$@"\n' % (sys.executable, sys.executable)))
        proc = self.check(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I8", proc.stdout)
        self.assertIn("exited 71", proc.stdout)

    def test_rg_scanner_failure_fails_closed(self):
        # rg exits >1 on a real error — previously `|| true` masked it
        env = self._stub_bin("rg", "#!/bin/sh\nexit 42\n")
        proc = self.check(env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("exited 42", proc.stdout)

    def test_unreadable_source_fails_closed_i3(self):
        p = self.repo.dir / CORE / "src" / "unreadable.rs"
        p.write_text("fn g() {}\n")
        os.chmod(p, 0)
        self.addCleanup(os.chmod, p, 0o644)
        if os.access(p, os.R_OK):
            self.skipTest("running with read-anything privilege")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I3", proc.stdout)
        self.assertIn("unreadable", proc.stdout)

    @case("missing-lints-opt-in", "cheat", "member manifest drops [lints] workspace = true")
    def test_member_lints_opt_in_missing_fails(self):
        self.repo.write(
            CORE_MANI,
            real_file(CORE_MANI).replace("\n[lints]\nworkspace = true\n", ""),
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I8", proc.stdout)
        self.assertIn(CORE_MANI, proc.stdout)

    @case("missing-lints-opt-in", "cheat", "a new member manifest without the opt-in fails I8")
    def test_new_member_without_lints_fails(self):
        self.repo.write(
            "extra/Cargo.toml",
            '[package]\nname = "extra"\nversion = "0.1.0"\nedition = "2024"\n',
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I8", proc.stdout)
        self.assertIn("extra/Cargo.toml", proc.stdout)

    @case("missing-lints-opt-in", "control", "every member manifest declaring the opt-in passes")
    def test_member_lints_opt_in_ok(self):
        self.repo.write(
            "extra/Cargo.toml",
            '[package]\nname = "extra"\nversion = "0.1.0"\nedition = "2024"\n\n[lints]\nworkspace = true\n',
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("harness-literal", "cheat", "a harness name literal inside member src outside the transcript adapter")
    def test_harness_literal_outside_transcript_fails(self):
        for lit in ("claude", "devin", "codex", "gemini", "agy"):
            with self.subTest(lit=lit):
                self.repo.write(
                    BIN + "/src/daemon/spawn.rs",
                    'pub fn name() -> &\'static str {\n    "%s"\n}\n' % lit,
                )
                proc = self.check()
                self.assertIn("FAIL I9", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.remove(BIN + "/src/daemon/spawn.rs")

    @case("harness-literal", "cheat", "the two-letter harness name is also pinned")
    def test_harness_literal_pi_fails(self):
        self.repo.write(
            BIN + "/src/daemon/spawn.rs",
            'pub fn name() -> &\'static str {\n    "pi"\n}\n',
        )
        proc = self.check()
        self.assertIn("FAIL I9", proc.stdout)

    @case("harness-literal", "control", "harness literals live in adapters/transcript and test fixtures")
    def test_harness_literal_in_transcript_ok(self):
        self.repo.write(
            BIN + "/src/adapters/transcript/mod.rs",
            'pub const HARNESS: &str = "claude";\n',
        )
        self.repo.write(
            BIN + "/tests/parse.rs",
            'const FIXTURE: &str = "claude";\n',
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("lifecycle-write-outside-transitions", "cheat", "SQL lifecycle write under a member src dir that is not store/transitions")
    def test_lifecycle_write_outside_transitions_fails(self):
        self.repo.write(
            BIN + "/src/daemon/store.rs",
            'pub fn seal() {\n    let sql = "INSERT INTO runs (id) VALUES (?1)";\n    let _ = sql;\n}\n',
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL I10", proc.stdout)

    @case("lifecycle-write-outside-transitions", "cheat", "a direct store::transitions:: path reference bypasses the public API")
    def test_direct_transitions_reference_fails(self):
        self.repo.write(
            BIN + "/src/mcp/handler.rs",
            "pub fn h() {\n    crate::store::transitions::apply();\n}\n",
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL I10", proc.stdout)

    @case("lifecycle-write-outside-transitions", "control", "lifecycle writes inside store/transitions are the one legit home")
    def test_lifecycle_write_inside_transitions_ok(self):
        self.repo.write(
            BIN + "/src/store/transitions/runs.rs",
            'pub fn apply() {\n    let sql = "INSERT INTO runs (id) VALUES (?1)";\n    let _ = sql;\n}\n',
        )
        self.repo.write(
            BIN + "/src/store/transitions.rs",
            "pub fn seal() {\n    crate::store::transitions::apply();\n}\n",
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("harness-literal", "cheat", "a test-named src file is still compiled surface — a filename alone must not exempt it")
    def test_harness_literal_in_test_named_src_fails(self):
        self.repo.write(
            BIN + "/src/daemon/test_spawn.rs",
            'pub fn name() -> &\'static str {\n    "codex"\n}\n',
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL I9", proc.stdout)

    @case("harness-literal", "cheat", "a src/**/tests/ dir is compiled surface, not a cargo test dir")
    def test_harness_literal_in_nested_src_tests_fails(self):
        self.repo.write(
            BIN + "/src/daemon/tests/parse.rs",
            'const FIXTURE: &str = "claude";\n',
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL I9", proc.stdout)

    @case("harness-literal", "control", "a test-named file inside the transcript adapter keeps its exemption")
    def test_harness_literal_in_transcript_test_file_ok(self):
        self.repo.write(
            BIN + "/src/adapters/transcript/test_parser.rs",
            'const FIXTURE: &str = "claude";\n',
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("lifecycle-write-outside-transitions", "cheat", "a test-named src file with lifecycle SQL is still compiled surface")
    def test_lifecycle_write_in_test_named_src_fails(self):
        self.repo.write(
            BIN + "/src/store/test_writes.rs",
            'pub fn seal() {\n    let sql = "DELETE FROM runs WHERE id = ?1";\n    let _ = sql;\n}\n',
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL I10", proc.stdout)

    @case("lifecycle-write-outside-transitions", "cheat", "the SQL verb and lifecycle table split across a line break still count")
    def test_multiline_lifecycle_write_fails(self):
        self.repo.write(
            BIN + "/src/daemon/store.rs",
            'pub fn seal() {\n    let sql = "INSERT INTO\n'
            '        runs (id) VALUES (?1)";\n    let _ = sql;\n}\n',
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL I10", proc.stdout)

    @case("lifecycle-write-outside-transitions", "cheat", "whitespace inside the store::transitions path still counts")
    def test_whitespace_transitions_reference_fails(self):
        self.repo.write(
            BIN + "/src/mcp/handler.rs",
            "pub fn h() {\n    crate::store ::\n        transitions :: apply();\n}\n",
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL I10", proc.stdout)

    @case("lifecycle-write-outside-transitions", "control", "non-lifecycle SQL verbs and tables are fine")
    def test_non_lifecycle_sql_ok(self):
        self.repo.write(
            BIN + "/src/daemon/store.rs",
            'pub fn stats() {\n    let sql = "SELECT count(*) FROM tasks";\n    let _ = sql;\n}\n',
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("lifecycle-write-outside-transitions", "cheat", "REPLACE, DROP TABLE and ALTER TABLE are lifecycle writes too")
    def test_extended_lifecycle_verbs_fail(self):
        for sql in (
            "REPLACE INTO runs (id) VALUES (?1)",
            "DROP TABLE runs",
            "ALTER TABLE runs ADD COLUMN note TEXT",
        ):
            with self.subTest(sql=sql):
                self.repo.write(
                    BIN + "/src/daemon/store.rs",
                    'pub fn seal() {\n    let sql = "%s";\n    let _ = sql;\n}\n' % sql,
                )
                proc = self.check()
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("FAIL I10", proc.stdout)
                self.repo.remove(BIN + "/src/daemon/store.rs")

    @case("lifecycle-write-outside-transitions", "control", "the new verbs inside store/transitions stay legal")
    def test_extended_verbs_inside_transitions_ok(self):
        self.repo.write(
            BIN + "/src/store/transitions/ops.rs",
            'pub fn apply() {\n'
            '    let a = "REPLACE INTO runs (id) VALUES (?1)";\n'
            '    let b = "DROP TABLE runs";\n'
            '    let c = "ALTER TABLE runs ADD COLUMN note TEXT";\n'
            "    let _ = (a, b, c);\n}\n",
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("lifecycle-write-outside-transitions", "control", "the new verbs against non-lifecycle tables are not lifecycle writes")
    def test_extended_verbs_other_tables_ok(self):
        self.repo.write(
            BIN + "/src/daemon/store.rs",
            'pub fn gc() {\n    let sql = "DROP TABLE scratch_cache";\n    let _ = sql;\n}\n',
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("harness-literal", "control", "the math constant is not the two-letter harness literal")
    def test_math_pi_constant_ok(self):
        self.repo.write(
            BIN + "/src/daemon/math.rs",
            "pub fn circumference(r: f64) -> f64 {\n    std::f64::consts::PI * 2.0 * r\n}\n",
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_exec_bit_fails(self):
        os.chmod(self.repo.dir / "scripts/agent-gate.sh", 0o644)
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I4", proc.stdout)

    def test_i5_absent_locally_reports(self):
        proc = self.check()
        self.assertIn("I5", proc.stdout)  # .claude/.devin absent -> REPORT
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_i5_ci_fails(self):
        proc = self.check(env={"CI": "true"})
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I5", proc.stdout)

    def test_i5_missing_deny_fails(self):
        # an empty deny list leaves every hard pattern uncovered
        self.repo.write(".claude/settings.json", '{"permissions": {"deny": []}}\n')
        self.repo.commit_all()
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL I5", proc.stdout)
        self.assertIn("clippy.toml", proc.stdout)

    def test_i5_real_configs_pass(self):
        for d in (".claude", ".devin"):
            shutil.copytree(harness_dir(d), self.repo.dir / d)
        self.repo.commit_all("harness")
        proc = self.check()
        self.assertNotIn("FAIL I5", proc.stdout)
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_unpinned_uses_fails(self):
        self.repo.write(
            ".github/workflows/ci.yml",
            "jobs:\n  x:\n    steps:\n      - uses: actions/checkout@v4\n",
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I6", proc.stdout)

    def test_pinned_uses_ok(self):
        self.repo.write(
            ".github/workflows/ci.yml",
            "jobs:\n  x:\n    steps:\n      - uses: actions/checkout@"
            + "a" * 40 + "\n",
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_justfile_missing_ref_fails(self):
        self.repo.write("justfile", "x:\n\tscripts/does-not-exist.sh\n")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("I7", proc.stdout)


class TestInventoryTests(unittest.TestCase):
    """check-test-inventory.sh — the ratchet over `cargo nextest list` JSON.
    Fixture files stand in for BASE/HEAD inventories; no cargo needed."""

    def setUp(self):
        d = Path(tempfile.mkdtemp(prefix="inv-test-"))
        self.addCleanup(shutil.rmtree, d, True)
        self.dir = d

    def check(self, base_doc, head_doc, env=None, args=()):
        bf = self.dir / "base.json"
        hf = self.dir / "head.json"
        bf.write_text(base_doc)
        hf.write_text(head_doc)
        return run("check-test-inventory.sh",
                   args=(*args, str(bf), str(hf)),
                   cwd=self.dir, env=env)

    BASE = inventory(
        "governor-core::it",
        ("test_it", False, "matches"),
        ("test_props", False, "matches"),
    )

    @case("removed-mod-line", "cheat", "a removed `mod` line unwires the test — the name vanishes from the inventory though the file looks untouched")
    def test_missing_test_fails(self):
        head = inventory("governor-core::it", ("test_it", False, "matches"))
        proc = self.check(self.BASE, head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL INV", proc.stdout)
        self.assertIn("test_props", proc.stdout)

    @case("deleted-test", "cheat", "a deleted test drops out of the HEAD inventory")
    def test_all_tests_missing_fails(self):
        head = inventory("governor-core::it", ("test_extra", False, "matches"))
        proc = self.check(self.BASE, head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL INV", proc.stdout)

    def test_renamed_test_fails(self):
        head = inventory(
            "governor-core::it",
            ("test_it", False, "matches"),
            ("test_props_v2", False, "matches"),
        )
        proc = self.check(self.BASE, head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL INV", proc.stdout)

    def test_ignored_test_fails(self):
        head = inventory(
            "governor-core::it",
            ("test_it", False, "matches"),
            ("test_props", True, "matches"),
        )
        proc = self.check(self.BASE, head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL INV", proc.stdout)

    def test_filter_mismatched_test_fails(self):
        head = inventory(
            "governor-core::it",
            ("test_it", False, "matches"),
            ("test_props", False, "mismatch"),
        )
        proc = self.check(self.BASE, head)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL INV", proc.stdout)

    def test_empty_base_inventory_fails(self):
        proc = self.check(inventory("governor-core::it"), self.BASE)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL INV", proc.stdout)

    def test_manifestless_base_passes(self):
        # bootstrap: BASE has no Cargo.toml, so the recipe writes a
        # well-formed empty document and passes --allow-empty-base
        proc = self.check('{"rust-suites":{}}\n', self.BASE,
                          args=("--allow-empty-base",))
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_broken_base_inventory_fails_with_flag(self):
        # --allow-empty-base attests a manifest-less BASE, not license for a
        # failed `cargo nextest list` — an empty or unparsable file still fails
        for bad in ("", "not json\n", '{"rust-suites": 42}\n'):
            with self.subTest(base_doc=bad):
                proc = self.check(bad, self.BASE,
                                  args=("--allow-empty-base",))
                self.assertNotEqual(proc.returncode, 0)
                self.assertIn("FAIL INV", proc.stdout)

    def test_removed_test_fails_with_flag(self):
        # the flag asserts only that the BASE inventory is legitimately
        # empty; a test removed between two real inventories still fails
        head = inventory("governor-core::it", ("test_it", False, "matches"))
        proc = self.check(self.BASE, head, args=("--allow-empty-base",))
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("test_props", proc.stdout)

    def test_jsonlines_shape_accepted(self):
        base = '{"type":"test","package":"governor-core","binary":"it","name":"test_it","ignored":false,"filter_match":{"status":"matches"}}\n'
        head_missing = '{"type":"test","package":"governor-core","binary":"it","name":"other","ignored":false,"filter_match":{"status":"matches"}}\n'
        proc = self.check(base, base)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        proc = self.check(base, head_missing)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL INV", proc.stdout)

    def test_override_downgrades_to_report(self):
        head = inventory("governor-core::it", ("test_it", False, "matches"))
        proc = self.check(self.BASE, head, env={"GOV_PROTECTED_OK": "1"})
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("OVERRIDE", proc.stdout)

    @case("removed-mod-line", "control", "unchanged inventory passes")
    @case("same-name-noop-test", "control", "a name that still inventories and schedules passes the ratchet")
    def test_unchanged_inventory_passes(self):
        proc = self.check(self.BASE, self.BASE)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("0 fail(s)", proc.stdout)

    def test_added_test_passes(self):
        head = inventory(
            "governor-core::it",
            ("test_it", False, "matches"),
            ("test_props", False, "matches"),
            ("test_new", False, "matches"),
        )
        proc = self.check(self.BASE, head)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("added", proc.stdout)

    def test_unreadable_file_fails_closed(self):
        proc = run("check-test-inventory.sh",
                   args=("/nonexistent-base", "/nonexistent-head"),
                   cwd=self.dir)
        self.assertEqual(proc.returncode, 2)

    def test_ci_inventory_bootstrap_matches_recipe(self):
        # F6: the CI inventory step must mirror `just test-inventory`'s
        # bootstrap branch exactly — a well-formed empty nextest document
        # plus --allow-empty-base — never a zero-byte file without the flag.
        section = ProtectedDiffCiTests._job_section(
            real_file(".github/workflows/ci.yml"), "test-inventory")
        self.assertIn('{"rust-suites":{}}', section)
        self.assertIn("--allow-empty-base", section)
        self.assertIn("$INV_ARGS", section)
        self.assertNotIn(': > "$RUNNER_TEMP/base.jsonl"', section)


class MutantsDiffTests(unittest.TestCase):
    """mutants-diff.sh — diff classification and cargo-mutants orchestration.
    --classify-only covers the branch decisions; a stubbed `cargo` on PATH
    covers the run/verdict paths without running real mutation testing."""

    def setUp(self):
        self.repo = base_repo()
        self.addCleanup(self.repo.cleanup)
        self.base = self.repo.head()

    def check(self, *args, env=None):
        return run("mutants-diff.sh", args=args, cwd=self.repo.dir, env=env)

    def stub_cargo(self, listing="", rc=0, list_rc=0):
        bindir = self.repo.dir / "stub-bin"
        bindir.mkdir(exist_ok=True)
        cargo = bindir / "cargo"
        cargo.write_text(
            "#!/bin/sh\n"
            'for a in "$@"; do\n'
            '    if [ "$a" = "--list" ]; then\n'
            '        printf "%s" "${STUB_LIST:-}"\n'
            '        exit "${STUB_LIST_RC:-0}"\n'
            "    fi\n"
            "done\n"
            'exit "${STUB_RC:-0}"\n'
        )
        os.chmod(cargo, 0o755)
        mutants = bindir / "cargo-mutants"
        mutants.write_text("#!/bin/sh\nexit 0\n")
        os.chmod(mutants, 0o755)
        return {
            "PATH": str(bindir) + ":" + os.environ["PATH"],
            "STUB_LIST": listing,
            "STUB_RC": str(rc),
            "STUB_LIST_RC": str(list_rc),
        }

    def commit(self):
        self.repo.commit_all()

    def test_classify_core_src_diff(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace("    22\n", "    23\n"),
        )
        self.commit()
        proc = self.check(self.base, "--classify-only")
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("class=core-src-diff", proc.stdout)

    @case("emptied-test-body", "cheat", "a test-only diff selects the full-mutation branch — the gutted body cannot dodge mutation")
    @case("same-name-noop-test", "cheat", "a test-only diff selects the full-mutation branch")
    def test_classify_test_only_diff(self):
        self.repo.write(CORE_TEST, "#[test]\nfn test_it() {\n}\n")
        self.commit()
        proc = self.check(self.base, "--classify-only")
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("class=test-only", proc.stdout)

    @case("emptied-test-body", "cheat", "a dedicated src test module is a test-only change — it must route to the full run, not in-diff")
    @case("same-name-noop-test", "cheat", "a dedicated src test module is a test-only change — it must route to the full run, not in-diff")
    def test_classify_src_test_module_is_test_only(self):
        self.repo.write(
            CORE + "/src/tests.rs",
            "#[test]\nfn dedicated() {\n    assert!(true);\n}\n",
        )
        self.commit()
        proc = self.check(self.base, "--classify-only")
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("class=test-only", proc.stdout)

    @case("emptied-test-body", "cheat", "editing a dedicated src test module in place is test-only")
    def test_classify_src_test_module_edit_is_test_only(self):
        self.repo.write(
            CORE + "/src/tests.rs",
            "#[test]\nfn dedicated() {\n    assert!(true);\n}\n",
        )
        self.commit()
        base2 = self.repo.head()
        self.repo.write(
            CORE + "/src/tests.rs",
            "#[test]\nfn dedicated() {\n    assert_eq!(1 + 1, 2);\n}\n",
        )
        self.commit()
        proc = self.check(base2, "--classify-only")
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("class=test-only", proc.stdout)

    @case("emptied-test-body", "cheat", "an assertion-only edit inside an inline #[cfg(test)] module is test-only — before the fix it hit the unexpected-zero rejection")
    @case("same-name-noop-test", "cheat", "an assertion-only edit inside an inline #[cfg(test)] module is test-only")
    def test_classify_inline_test_edit_is_test_only(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace(
                'assert_eq!(herdr_protocol(), 22, "protocol revision must be 22");',
                'assert!(herdr_protocol() > 0, "protocol revision is positive");'),
        )
        self.commit()
        proc = self.check(self.base, "--classify-only")
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("class=test-only", proc.stdout)

    @case("emptied-test-body", "control", "an inline-test change that kills all mutants passes the full run")
    def test_inline_test_edit_full_run_passes(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace(
                'assert_eq!(herdr_protocol(), 22, "protocol revision must be 22");',
                'assert_eq!(herdr_protocol(), 22, "protocol revision is 22");'),
        )
        self.commit()
        env = self.stub_cargo(rc=0)
        proc = self.check(self.base, env=env)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("test-only", proc.stdout)

    def test_inline_test_edit_full_run_failure_propagates(self):
        # a weakened inline test reaches the full run — a surviving mutant fails
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace(
                'assert_eq!(herdr_protocol(), 22, "protocol revision must be 22");',
                'assert!(true);'),
        )
        self.commit()
        env = self.stub_cargo(rc=1)
        proc = self.check(self.base, env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL MUTANTS", proc.stdout)

    def test_inline_test_edit_takes_test_only_branch(self):
        # a src-side test-only edit must route to the full run, not the
        # in-diff branch
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace(
                'assert_eq!(herdr_protocol(), 22, "protocol revision must be 22");',
                'assert_eq!(herdr_protocol(), 22, "protocol revision is 22");'),
        )
        self.commit()
        env = self.stub_cargo(listing="", rc=0)
        proc = self.check(self.base, env=env)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("test-only", proc.stdout)

    def test_inline_test_plus_prod_edit_is_core_src(self):
        # test and production lines in the same src file -> production class
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            .replace("    22\n", "    23\n")
            .replace("protocol revision must be 22", "protocol revision is 22"),
        )
        self.commit()
        proc = self.check(self.base, "--classify-only")
        self.assertIn("class=core-src-diff", proc.stdout)

    def test_classify_docs_only_diff(self):
        self.repo.write("docs/plan/p.md", "new plan\n")
        self.commit()
        proc = self.check(self.base, "--classify-only")
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("class=no-core-change", proc.stdout)

    def test_classify_mixed_diff_is_core_src(self):
        self.repo.write(CORE_TEST, "#[test]\nfn test_it() {\n}\n")
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace("    22\n", "    23\n"),
        )
        self.commit()
        proc = self.check(self.base, "--classify-only")
        self.assertIn("class=core-src-diff", proc.stdout)

    def test_empty_diff_classifies_no_change(self):
        proc = self.check(self.base, "--classify-only")
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("no-core-change", proc.stdout)
        proc = self.check(self.base)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("REPORT MUTANTS", proc.stdout)

    def test_bad_base_fails_closed(self):
        proc = self.check("nonexistent-ref", "--classify-only")
        self.assertEqual(proc.returncode, 2)

    def test_missing_cargo_mutants_fails(self):
        # a PATH without ~/.cargo/bin leaves cargo-mutants unresolvable
        self.repo.write(CORE_TEST, "#[test]\nfn test_it() {\n}\n")
        self.commit()
        if shutil.which("cargo-mutants", path="/usr/bin:/bin"):
            self.skipTest("cargo-mutants resolves even on a bare PATH")
        proc = self.check(self.base, env={"PATH": "/usr/bin:/bin"})
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL MUTANTS", proc.stdout)

    @case("same-name-noop-test", "cheat", "a no-op test change reaches the full run — a surviving mutant fails the gate")
    def test_test_only_full_run_failure_propagates(self):
        self.repo.write(CORE_TEST, "#[test]\nfn test_it() {\n}\n")
        self.commit()
        env = self.stub_cargo(rc=1)
        proc = self.check(self.base, env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL MUTANTS", proc.stdout)

    @case("same-name-noop-test", "control", "a real test-only change that kills all mutants passes")
    def test_test_only_full_run_success_passes(self):
        self.repo.write(CORE_TEST, "#[test]\nfn test_it() {\n}\n")
        self.commit()
        env = self.stub_cargo(rc=0)
        proc = self.check(self.base, env=env)
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_zero_mutants_on_semantic_src_diff_reports(self):
        # a clean --list with zero entries is legitimate — REPORT, not FAIL
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace("    22\n", "    23\n"),
        )
        self.commit()
        env = self.stub_cargo(listing="")
        proc = self.check(self.base, env=env)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn(
            "REPORT MUTANTS 0 mutants — the diff has no mutation candidates",
            proc.stdout)

    def test_zero_mutants_on_const_only_diff_reports(self):
        # const-only addition: --list has nothing to enumerate
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB) + "\npub const MAX_PANES: usize = 8;\n",
        )
        self.commit()
        env = self.stub_cargo(listing="")
        proc = self.check(self.base, env=env)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("class=core-src-diff", proc.stdout)
        self.assertIn(
            "REPORT MUTANTS 0 mutants — the diff has no mutation candidates",
            proc.stdout)

    def test_zero_mutants_on_deletion_only_diff_reports(self):
        # pure production-code deletion: nothing for --list to enumerate
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace(
                "#[must_use]\npub fn herdr_protocol() -> u32 {\n    22\n}\n", ""),
        )
        self.commit()
        env = self.stub_cargo(listing="")
        proc = self.check(self.base, env=env)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("class=core-src-diff", proc.stdout)
        self.assertIn(
            "REPORT MUTANTS 0 mutants — the diff has no mutation candidates",
            proc.stdout)

    def test_list_error_fails(self):
        # --list erroring stays a FAIL — only a clean empty list passes
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace("    22\n", "    23\n"),
        )
        self.commit()
        env = self.stub_cargo(list_rc=1)
        proc = self.check(self.base, env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL MUTANTS cargo-mutants --list failed", proc.stdout)

    def test_zero_mutants_on_comment_only_diff_passes(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace(
                "/// Herdr protocol revision this build of the governor speaks.",
                "/// Protocol revision this build of the governor speaks."),
        )
        self.commit()
        env = self.stub_cargo(listing="")
        proc = self.check(self.base, env=env)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn(
            "REPORT MUTANTS 0 mutants — the diff has no mutation candidates",
            proc.stdout)

    def test_in_diff_run_success_passes(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace("    22\n", "    23\n"),
        )
        self.commit()
        env = self.stub_cargo(
            listing="governor-core/src/lib.rs:4: replace herdr_protocol -> u32 with 0\n",
            rc=0,
        )
        proc = self.check(self.base, env=env)
        self.assertEqual(proc.returncode, 0, proc.stdout)
        self.assertIn("in-diff mutants", proc.stdout)

    def test_in_diff_run_failure_fails(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace("    22\n", "    23\n"),
        )
        self.commit()
        env = self.stub_cargo(
            listing="governor-core/src/lib.rs:4: replace herdr_protocol -> u32 with 0\n",
            rc=1,
        )
        proc = self.check(self.base, env=env)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL MUTANTS", proc.stdout)


@unittest.skipUnless(shutil.which("cargo") and shutil.which("rg"),
                     "cargo+rg required for the purity gate")
class CorePurityTests(unittest.TestCase):
    """check-core-purity.sh — real `cargo metadata` on the mini-workspace,
    plus the lexical I/O scan over governor-core/src."""

    def setUp(self):
        self.repo = full_repo()
        self.addCleanup(self.repo.cleanup)

    def check(self):
        return run("check-core-purity.sh", cwd=self.repo.dir, timeout=180)

    @case("io-crate-in-core", "control", "the clean two-crate workspace passes purity")
    @case("no-std", "control", "the shipped lib.rs opens with the #![no_std] inner attribute")
    def test_clean_workspace_ok(self):
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    def test_bin_to_core_edge_present_ok(self):
        # FORBIDDEN-edge semantics: herdr-governor -> governor-core is the
        # one member edge that may exist
        mani = real_file(BIN_MANI)
        if "governor-core" not in mani:
            mani = mani.replace(
                "[dependencies]",
                '[dependencies]\ngovernor-core = { path = "../governor-core" }')
        self.repo.write(BIN_MANI, mani)
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    def test_bin_to_core_edge_absent_ok(self):
        # ...and it is never required: a bin manifest with no governor-core
        # dependency still passes
        mani = "".join(
            l for l in real_file(BIN_MANI).splitlines(keepends=True)
            if "governor-core" not in l)
        self.repo.write(BIN_MANI, mani)
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    @case("io-crate-in-core", "cheat", "an I/O dependency added to governor-core is outside the allowed dep set")
    def test_io_dep_added_to_core_fails(self):
        # a path dep on a stub crate outside the repo resolves offline and
        # lands squarely outside {serde, thiserror, sha2}
        stub = Path(tempfile.mkdtemp(prefix="io-cap-"))
        self.addCleanup(shutil.rmtree, stub, True)
        (stub / "src").mkdir()
        (stub / "Cargo.toml").write_text(
            '[package]\nname = "io-cap"\nversion = "0.1.0"\nedition = "2024"\n')
        (stub / "src/lib.rs").write_text("pub fn x() {}\n")
        self.repo.write(
            CORE_MANI,
            real_file(CORE_MANI).replace(
                "[dependencies]",
                '[dependencies]\nio-cap = { path = "%s" }' % stub),
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL PURITY", proc.stdout)
        self.assertIn("io-cap", proc.stdout)

    def test_member_edge_back_to_bin_fails(self):
        self.repo.write(
            CORE_MANI,
            real_file(CORE_MANI).replace(
                "[dependencies]",
                '[dependencies]\nherdr-governor = { path = "../herdr-governor" }'),
        )
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL PURITY", proc.stdout)
        self.assertIn("forbidden member edge", proc.stdout)

    def test_extra_member_edge_fails(self):
        # any member edge other than herdr-governor -> governor-core is
        # forbidden — a third member is no exception
        self.repo.write(
            "Cargo.toml",
            real_file("Cargo.toml").replace(
                'members = ["governor-core", "herdr-governor"]',
                'members = ["governor-core", "herdr-governor", "extra"]'))
        self.repo.write(
            "extra/Cargo.toml",
            '[package]\nname = "extra"\nversion = "0.1.0"\nedition = "2024"\n')
        self.repo.write("extra/src/lib.rs", "pub fn x() {}\n")
        self.repo.write(
            BIN_MANI,
            real_file(BIN_MANI).replace(
                "[dependencies]",
                '[dependencies]\nextra = { path = "../extra" }'))
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL PURITY", proc.stdout)
        self.assertIn("forbidden member edge", proc.stdout)

    @case("io-crate-in-core", "cheat", "I/O and clock tokens in governor-core sources")
    def test_io_tokens_in_core_src_fail(self):
        for line in (
            'pub fn f() { let _ = std::fs::read("/x"); }',
            "pub fn f() { let _ = std::time::Instant::now(); }",
            "pub fn f() { let _ = rusqlite::Connection::open_in_memory(); }",
            "pub unsafe fn f() {}",
        ):
            with self.subTest(line=line):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + line + "\n")
                proc = self.check()
                self.assertIn("FAIL PURITY", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("io-crate-in-core", "cheat", "a grouped std import reaches I/O without spelling a banned token")
    def test_grouped_import_fails(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + "use std::{fs, io};\n"
            + "pub fn read_value() -> io::Result<String> {\n"
            + '    fs::read_to_string("input.txt")\n'
            + "}\n",
        )
        proc = self.check()
        self.assertIn("FAIL PURITY", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)

    @case("io-crate-in-core", "cheat", "std root/module aliases rename I/O away from the banned token")
    def test_aliased_import_fails(self):
        for body in (
            "use std::{fs as files};\npub fn f() { let _ = files::read(\"x\"); }\n",
            "use std as sysroot;\npub fn f() { let _ = sysroot::fs::read(\"x\"); }\n",
            "use std::{self as sysroot};\npub fn f() { let _ = sysroot::fs::read(\"x\"); }\n",
        ):
            with self.subTest(body=body):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + body)
                proc = self.check()
                self.assertIn("FAIL PURITY", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("io-crate-in-core", "cheat", "whitespace and line breaks inside a module path still count")
    def test_whitespace_split_path_fails(self):
        for body in (
            'pub fn f() { let _ = std :: fs::read("x"); }\n',
            'pub fn f() { let _ = std::\n    process::exit(1); }\n',
        ):
            with self.subTest(body=body):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + body)
                proc = self.check()
                self.assertIn("FAIL PURITY", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("io-crate-in-core", "cheat", "include!/#[path]/env!/extern-crate pull unscanned source or host state into the crate")
    def test_source_escape_tokens_fail(self):
        for body in (
            'pub fn f() { include!("x.rs"); }\n',
            'pub fn f() { let _ = include_bytes!("x.bin"); }\n',
            'const X: &str = include_str!("x.txt");\n',
            '#[path = "evil.rs"]\nmod evil;\n',
            'const X: &str = env!("HOME");\n',
            "const X: Option<&'static str> = option_env!(\"X\");\n",
            'extern crate std as sysroot;\n',
        ):
            with self.subTest(body=body):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + body)
                proc = self.check()
                self.assertIn("FAIL PURITY", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("io-crate-in-core", "cheat", "include-family imports cannot rename source or host-state ingress past purity (R3-1)")
    def test_include_family_identifiers_fail(self):
        for name in ("include", "include_str", "include_bytes"):
            for body in (
                f"use core::{name};\n",
                f"use core::{name} as imported;\n",
                f"use core::{{{name} as imported}};\n",
                f"use {{core::{{{name} as imported}}}};\n",
                f"use r#core::{{r#{name} as r#imported}};\n",
                f"use core::/**/r#{name} as imported;\n",
                f"pub use core::{name} as imported;\n",
            ):
                with self.subTest(body=body):
                    self.repo.write(CORE_LIB, real_file(CORE_LIB) + body)
                    proc = self.check()
                    self.assertNotEqual(proc.returncode, 0, proc.stdout)
                    self.assertIn("FAIL PURITY", proc.stdout)

    @case("io-crate-in-core", "control", "include-family mentions in literals/comments and longer identifiers are not ingress")
    def test_include_identifier_boundaries_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + "// use core::{r#include as imported};\n"
            + 'const EXAMPLE: &str = r#"use core::include_bytes as imported;"#;\n'
            + "pub fn include_value() -> &'static str { EXAMPLE }\n",
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    @case("io-crate-in-core", "cheat", "compiled include-alias payload has zero in-diff mutants but must fail purity before mutation (R3-1)")
    def test_compiled_include_alias_zero_mutants_rejected(self):
        # Real compile + cargo-mutants, not a token-only or stubbed probe.
        # The included function is compiled, never called. Keep the legitimate
        # zero-candidate REPORT; purity, not mutant count, closes this ingress.
        for name in ("justfile", "Cargo.lock", ".gitignore"):
            self.repo.write(name, real_file(name))
        self.repo.remove(CORE_TEST)
        self.repo.remove(BIN_TEST)
        self.repo.commit_all()
        base = self.repo.head()
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB).replace(
                "#![no_std]\n",
                '#![no_std]\n\nuse core::include as imported;\n'
                'imported!("../payload.inc");\n', 1),
        )
        self.repo.write(
            CORE + "/payload.inc",
            'extern crate std;\n\n'
            '/// Read host state.\n'
            '#[expect(clippy::disallowed_methods, reason = "fixture")]\n'
            '#[must_use]\n'
            'pub fn host_state() -> std::string::String {\n'
            '    std::fs::read_to_string("/etc/hostname").unwrap_or_default()\n'
            '}\n',
        )
        self.repo.commit_all()
        env = dict(os.environ)
        for key in ENV_SCRUB:
            env.pop(key, None)

        def recipes(*args):
            return subprocess.run(
                ["just", *args], cwd=self.repo.dir, env=env,
                capture_output=True, text=True, timeout=300,
            )

        compiled = recipes("fmt", "lint", "test")
        self.assertEqual(compiled.returncode, 0, compiled.stdout + compiled.stderr)
        mutants = recipes("mutants-diff", base)
        self.assertEqual(mutants.returncode, 0, mutants.stdout + mutants.stderr)
        self.assertIn("class=core-src-diff", mutants.stdout)
        self.assertIn("REPORT MUTANTS 0 mutants", mutants.stdout)
        gated = recipes("purity", "mutants-diff", base)
        self.assertNotEqual(gated.returncode, 0, gated.stdout + gated.stderr)
        self.assertIn("FAIL PURITY", gated.stdout)
        self.assertNotIn("REPORT MUTANTS", gated.stdout)

    @case("io-crate-in-core", "control", "grouped and aliased imports of pure std modules pass")
    def test_benign_std_imports_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + "use std::collections::{BTreeMap, HashMap};\n"
            + "use std::fmt::Write as _;\n"
            + "pub fn f(_m: &BTreeMap<String, HashMap<String, String>>) {}\n",
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("io-crate-in-core", "control", "pure core:: imports are the no_std spellings")
    def test_core_imports_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + "use core::fmt::{Debug, Display};\n"
            + "pub fn f(_d: &dyn Debug, _x: &dyn Display) {}\n",
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    @case("io-crate-in-core", "control", "comments and strings may mention the banned tokens — they are not code")
    def test_token_mentions_in_comments_and_strings_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + "/// std::fs, env!, Instant::now and extern crate std are banned here;\n"
            + "/// filesystem-using tests live in governor-core/tests/.\n"
            + 'const DOC: &str = "std::net::TcpListener is not available in this crate";\n'
            + "pub fn noted() -> &'static str {\n    DOC\n}\n",
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    @case("io-crate-in-core", "cheat", "a std glob import names no banned module but reaches all of them (N1)")
    def test_glob_import_fails(self):
        for body in (
            'use std::*;\npub fn f() -> Vec<u8> {\n    fs::read("/x").unwrap_or_default()\n}\n',
            "use std::io::*;\n",
            "use core::*;\n",
            "use alloc::*;\n",
        ):
            with self.subTest(body=body):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + body)
                proc = self.check()
                self.assertIn("FAIL PURITY", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("io-crate-in-core", "cheat", "glob import plus sanctioned expect spelling is a full purity bypass (N1)")
    def test_glob_and_disallowed_expect_fails(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + "use std::*;\n"
            + '#[expect(clippy::disallowed_methods, reason = "x")]\n'
            + "pub fn leak() -> Vec<u8> {\n"
            + '    fs::read("/x").unwrap_or_default()\n'
            + "}\n",
        )
        proc = self.check()
        self.assertIn("FAIL PURITY", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)

    @case("io-crate-in-core", "cheat", "comment tokens inside a use path still reach the banned module")
    def test_comment_split_use_fails(self):
        for body in (
            "use std::/**/fs;\n",
            "use /*x*/ std::fs;\n",
            "use std::fs\n    /*x*/ ::read;\n",
        ):
            with self.subTest(body=body):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + body)
                proc = self.check()
                self.assertIn("FAIL PURITY", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("io-crate-in-core", "cheat", "every extern crate std spelling pulls the host runtime back into the pure crate")
    def test_extern_crate_std_spellings_fail(self):
        for body in (
            "extern crate std;\n",
            "#[macro_use]\nextern crate std;\n",
            "extern crate r#std;\n",
            "extern crate std as sysroot;\n",
            "extern /*x*/ crate std;\n",
        ):
            with self.subTest(body=body):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + body)
                proc = self.check()
                self.assertIn("FAIL PURITY", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("io-crate-in-core", "control", "extern crate alloc plus alloc:: paths is the sanctioned no_std allocation spelling (F3-A)")
    def test_pure_alloc_user_ok(self):
        self.repo.write(
            CORE_LIB,
            real_file(CORE_LIB)
            + "extern crate alloc;\n"
            + "use alloc::collections::BTreeMap;\n"
            + "pub fn f() -> alloc::vec::Vec<u8> {\n"
            + "    let _m = BTreeMap::<u8, u8>::new();\n"
            + "    alloc::vec::Vec::new()\n"
            + "}\n",
        )
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    @case("io-crate-in-core", "cheat", "permitting extern crate alloc launders nothing — std I/O, extern crate std and the alloc glob stay banned")
    def test_alloc_allowance_keeps_bans(self):
        for body in (
            'extern crate alloc;\npub fn f() { let _ = std::fs::read("/x"); }\n',
            "extern crate alloc;\nextern crate std;\n",
            "extern crate alloc;\nuse alloc::*;\n",
        ):
            with self.subTest(body=body):
                self.repo.write(CORE_LIB, real_file(CORE_LIB) + body)
                proc = self.check()
                self.assertIn("FAIL PURITY", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, real_file(CORE_LIB))

    @case("no-std", "cheat", "governor-core without #![no_std] loses the compiler boundary")
    def test_missing_no_std_attr_fails(self):
        body = real_file(CORE_LIB)
        if body.startswith("#![no_std]"):
            body = body.split("\n", 1)[1]
        self.repo.write(CORE_LIB, body)
        proc = self.check()
        self.assertIn("FAIL PURITY", proc.stdout)
        self.assertNotEqual(proc.returncode, 0)

    @case("no-std", "cheat", "a commented-out or non-first attribute does not satisfy the rule")
    def test_no_std_not_first_attr_fails(self):
        body = real_file(CORE_LIB)
        for variant in (
            "//" + body,  # first line becomes "// #![no_std]"
            body.replace("#![no_std]", "#[no_std]", 1),  # outer, not inner
        ):
            with self.subTest(variant=variant[:30]):
                self.repo.write(CORE_LIB, variant)
                proc = self.check()
                self.assertIn("FAIL PURITY", proc.stdout)
                self.assertNotEqual(proc.returncode, 0)
                self.repo.write(CORE_LIB, body)

    @unittest.skipUnless(shutil.which("cargo"), "cargo required for the clippy layer")
    def test_clippy_layer_grouped_import_fails(self):
        # governor-core/clippy.toml is the compiler-enforced half of the purity
        # boundary: a grouped-import call resolves to the banned canonical path
        # (full_repo already installs it). The stock integration test trips
        # unrelated workspace lint denies.
        self.repo.remove(CORE_TEST)
        self.repo.write(
            CORE_LIB,
            "use std::{fs, io};\n"
            "/// Read a file.\n"
            "pub fn read_value() -> io::Result<String> {\n"
            '    fs::read_to_string("input.txt")\n'
            "}\n",
        )
        proc = subprocess.run(
            ["cargo", "clippy", "-p", "governor-core", "--all-targets",
             "--", "-D", "warnings"],
            cwd=self.repo.dir, capture_output=True, text=True, timeout=300,
        )
        out = proc.stdout + proc.stderr
        self.assertNotEqual(proc.returncode, 0, out)
        self.assertIn("disallowed", out)

    @unittest.skipUnless(shutil.which("cargo"), "cargo required for the clippy layer")
    def test_clippy_layer_clean_passes(self):
        self.repo.remove(CORE_TEST)
        proc = subprocess.run(
            ["cargo", "clippy", "-p", "governor-core", "--all-targets",
             "--", "-D", "warnings"],
            cwd=self.repo.dir, capture_output=True, text=True, timeout=300,
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)


class SchemaTests(unittest.TestCase):
    """check-herdr-schema.sh — fixture existence, parse, protocol pin, and
    the sha256 sidecar; --live drift via HERDR_SCHEMA_LIVE_FILE."""

    def setUp(self):
        self.repo = base_repo()
        self.addCleanup(self.repo.cleanup)

    def check(self, *args, env=None):
        return run("check-herdr-schema.sh", args=args, cwd=self.repo.dir, env=env)

    @case("schema-fixture-byte-change", "control", "pinned fixture bytes pass the gate")
    def test_fixture_ok(self):
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    @case("schema-fixture-byte-change", "cheat", "a byte-level fixture edit breaks the sha256 pin")
    def test_fixture_byte_change_fails(self):
        self.repo.write(SCHEMA_FIXTURE, '{"protocol": 22, "methods": ["x"]}\n')
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL SCHEMA", proc.stdout)

    def test_fixture_protocol_mismatch_fails(self):
        body = '{"protocol": 21, "methods": []}\n'
        self.repo.write(SCHEMA_FIXTURE, body)
        self.repo.write(SCHEMA_FIXTURE + ".sha256", schema_pin(body, SCHEMA_FIXTURE))
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL SCHEMA", proc.stdout)

    def test_fixture_missing_fails(self):
        self.repo.remove(SCHEMA_FIXTURE)
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL SCHEMA", proc.stdout)

    def test_pin_missing_fails(self):
        self.repo.remove(SCHEMA_FIXTURE + ".sha256")
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL SCHEMA", proc.stdout)

    def test_duplicate_fixture_fails(self):
        self.repo.write(CORE + "/" + SCHEMA_FIXTURE, SCHEMA_BODY)
        self.repo.write(CORE + "/" + SCHEMA_FIXTURE + ".sha256",
                        schema_pin(SCHEMA_BODY, CORE + "/" + SCHEMA_FIXTURE))
        proc = self.check()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("more than one place", proc.stdout)

    def test_member_fixture_home_ok(self):
        # the fixture may live under a member's tests/fixtures/ instead
        (self.repo.dir / CORE / "tests/fixtures").mkdir(parents=True, exist_ok=True)
        self.repo.git("mv", SCHEMA_FIXTURE, CORE + "/" + SCHEMA_FIXTURE)
        self.repo.git("mv", SCHEMA_FIXTURE + ".sha256",
                      CORE + "/" + SCHEMA_FIXTURE + ".sha256")
        pin = schema_pin(SCHEMA_BODY, CORE + "/" + SCHEMA_FIXTURE)
        self.repo.write(CORE + "/" + SCHEMA_FIXTURE + ".sha256", pin)
        proc = self.check()
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_live_drift_fails(self):
        live = self.repo.dir / "live.json"
        live.write_text('{"protocol": 22, "methods": ["drifted"]}\n')
        proc = self.check("--live", env={"HERDR_SCHEMA_LIVE_FILE": str(live)})
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL SCHEMA", proc.stdout)
        self.assertIn("drifted", proc.stdout)

    def test_live_match_passes(self):
        live = self.repo.dir / "live.json"
        live.write_text(SCHEMA_BODY)
        proc = self.check("--live", env={"HERDR_SCHEMA_LIVE_FILE": str(live)})
        self.assertEqual(proc.returncode, 0, proc.stdout)

    def test_unreadable_live_file_fails(self):
        proc = self.check("--live", env={"HERDR_SCHEMA_LIVE_FILE": "/nonexistent"})
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("FAIL SCHEMA", proc.stdout)


class OwnerApprovalTests(unittest.TestCase):
    """check-owner-approval.sh + owner-approval.yml — the owner-approved
    label is bound to the exact HEAD SHA it was applied under. APPROVED
    requires the label on the PR *and* an `owner-approval` check run
    concluded `success` on HEAD_SHA, written by the `github-actions` app —
    a signal only the base-defined pull_request_target workflow produces.
    Committer dates are forgeable and are never consulted. Fixture files
    drive the verdict logic; a stubbed `gh` on PATH drives the transport
    path and executes the workflow's own run block. Nothing here touches
    the network."""

    WF = ".github/workflows/owner-approval.yml"
    LABEL = {"name": "owner-approved"}

    def setUp(self):
        d = Path(tempfile.mkdtemp(prefix="approval-test-"))
        self.addCleanup(shutil.rmtree, d, True)
        self.dir = d
        self.labels = d / "labels.json"
        self.checks = d / "checks.json"
        self.ghlog = d / "gh.log"

    @staticmethod
    def check_run(conclusion, ts="2026-09-27T12:00:00Z", name="owner-approval",
                  status="completed", app="github-actions"):
        return {"name": name, "status": status, "conclusion": conclusion,
                "completed_at": ts, "app": {"slug": app}}

    def check(self, env, head="deadbeef"):
        return run(
            "check-owner-approval.sh",
            args=("o/r", "5", head, "owner-approved"),
            cwd=self.dir,
            env=env,
        )

    def fixtures(self, labels, check_runs):
        self.labels.write_text(json.dumps(labels))
        self.checks.write_text(json.dumps({"check_runs": check_runs}))
        return {
            "GOV_APPROVAL_LABELS_FILE": str(self.labels),
            "GOV_APPROVAL_CHECKS_FILE": str(self.checks),
        }

    def stub_gh(self, labels, check_runs):
        """gh api stub for the checker: serves the PR label list and the
        head SHA's check runs, logging "METHOD argv" per call so a test
        can assert exactly which endpoints were consulted and how. The
        stub replicates `gh api`'s documented default-method rule — any
        -f/-F field without an explicit -X/--method upgrades the request
        to POST — and answers only GET, the way the real endpoints treat
        a wrong method. Responses are wrapped in a page array to match
        the checker's --paginate --slurp output shape."""
        self.labels.write_text(json.dumps(labels))
        self.checks.write_text(json.dumps({"check_runs": check_runs}))
        gh = self.dir / "gh"
        gh.write_text(
            "#!/bin/sh\n"
            "method=GET\n"
            "take=\n"
            "explicit=\n"
            "fields=\n"
            'for a in "$@"; do\n'
            '    if [ -n "$take" ]; then method=$a; take=; continue; fi\n'
            '    case "$a" in\n'
            "        -X | --method) take=1; explicit=1 ;;\n"
            "        -X?*) method=${a#-X}; explicit=1 ;;\n"
            "        --method=*) method=${a#--method=}; explicit=1 ;;\n"
            "        -f | -F | --field | --raw-field) fields=1 ;;\n"
            "        -f?* | -F?* | --field=* | --raw-field=*) fields=1 ;;\n"
            "    esac\n"
            "done\n"
            '[ -n "$fields" ] && [ -z "$explicit" ] && method=POST\n'
            'printf "%s %s\\n" "$method" "$*" >> "$GOV_STUB_LOG"\n'
            'case "$method:$*" in\n'
            '    GET:*check-runs*) printf \'[%s]\\n\' "$(cat "$GOV_STUB_CHECKS")"; exit 0;;\n'
            '    GET:*labels*)     printf \'[%s]\\n\' "$(cat "$GOV_STUB_LABELS")"; exit 0;;\n'
            "esac\n"
            "exit 1\n"
        )
        os.chmod(gh, 0o755)
        return {
            "PATH": str(self.dir) + ":" + os.environ["PATH"],
            "GOV_STUB_LOG": str(self.ghlog),
            "GOV_STUB_LABELS": str(self.labels),
            "GOV_STUB_CHECKS": str(self.checks),
            # empty values neutralize any ambient fixture wiring — these
            # tests drive the live (transport) path through the stub
            "GOV_APPROVAL_LABELS_FILE": "",
            "GOV_APPROVAL_CHECKS_FILE": "",
        }

    @case("stale-owner-approval", "control",
          "label on the PR plus a successful owner-approval check on this exact head approves")
    def test_bound_label_approves(self):
        env = self.fixtures([self.LABEL], [self.check_run("success")])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("APPROVED", proc.stdout)

    @case("stale-owner-approval", "cheat",
          "a label applied before the latest push: the new head carries no success binding")
    def test_label_without_bound_check_is_stale(self):
        for check_runs in (
            [],
            [self.check_run("failure")],
            [self.check_run(None, status="in_progress")],
        ):
            with self.subTest(check_runs=check_runs):
                env = self.fixtures([self.LABEL], check_runs)
                proc = self.check(env)
                self.assertEqual(proc.returncode, 1)
                self.assertIn("STALE", proc.stdout)
                self.assertNotIn("APPROVED", proc.stdout)

    def test_check_is_queried_on_the_exact_head(self):
        # check runs are fetched at the queried HEAD_SHA only — a success
        # recorded on an older commit can never leak into this verdict
        env = self.stub_gh([self.LABEL], [])
        proc = self.check(env, head="cafe0123")
        self.assertEqual(proc.returncode, 1)
        self.assertIn("STALE", proc.stdout)
        calls = self.ghlog.read_text()
        self.assertIn("commits/cafe0123/check-runs", calls)
        self.assertNotIn("commits/deadbeef", calls)

    def test_commit_object_never_read(self):
        # a backdated commit cannot approve: the verdict's only inputs are
        # the PR label list and the head's check runs — no committer-date
        # or timeline endpoint is ever consulted
        env = self.stub_gh([self.LABEL], [self.check_run("success")])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        for line in self.ghlog.read_text().splitlines():
            self.assertNotIn("timeline", line)
            if "commits/" in line:
                self.assertIn("check-runs", line)

    def test_check_alone_never_approves(self):
        # a stray success check is not approval: the label must also be on
        # the PR — this closes the window where an unlabeled event is still
        # in flight while its check run has not landed
        env = self.fixtures([], [self.check_run("success")])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("ABSENT", proc.stdout)

    def test_no_label_is_absent(self):
        env = self.fixtures([], [])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("ABSENT", proc.stdout)

    def test_other_labels_ignored(self):
        env = self.fixtures([{"name": "bug"}], [self.check_run("success")])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("ABSENT", proc.stdout)

    def test_other_check_names_ignored(self):
        env = self.fixtures(
            [self.LABEL], [self.check_run("success", name="ci/guard")])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("STALE", proc.stdout)

    def test_newest_check_for_the_name_wins(self):
        # an older success superseded by a failure does not approve; a
        # failure superseded by a fresh success does
        env = self.fixtures(
            [self.LABEL],
            [self.check_run("success", ts="2026-09-27T12:00:00Z"),
             self.check_run("failure", ts="2026-09-27T13:00:00Z")],
        )
        proc = self.check(env)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("STALE", proc.stdout)
        env = self.fixtures(
            [self.LABEL],
            [self.check_run("failure", ts="2026-09-27T12:00:00Z"),
             self.check_run("success", ts="2026-09-27T13:00:00Z")],
        )
        proc = self.check(env)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    def test_slurped_fixture_shapes(self):
        # gh api --paginate --slurp yields an array of page arrays
        env = self.fixtures([[self.LABEL]], [self.check_run("success")])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("APPROVED", proc.stdout)

    def test_half_wired_fixture_env_fails_closed(self):
        env = self.fixtures([self.LABEL], [self.check_run("success")])
        env.pop("GOV_APPROVAL_CHECKS_FILE")
        proc = self.check(env)
        self.assertEqual(proc.returncode, 2)

    @case("stale-owner-approval", "cheat",
          "stubbed gh api: label present but head unbound is STALE through the real transport path")
    def test_stubbed_gh_stale(self):
        env = self.stub_gh([self.LABEL], [])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("STALE", proc.stdout)

    @case("stale-owner-approval", "control",
          "stubbed gh api: bound label approves through the real transport path")
    def test_stubbed_gh_approves(self):
        env = self.stub_gh([self.LABEL], [self.check_run("success")])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("APPROVED", proc.stdout)

    def test_stubbed_gh_transport_failure_fails_closed(self):
        env = self.stub_gh([], [])
        self.labels.write_text("{not json")
        proc = self.check(env)
        self.assertEqual(proc.returncode, 2)

    @case("stale-owner-approval", "control",
          "live transport: gh api sends POST when -f fields carry no explicit method — the check-runs request must name GET")
    def test_live_check_runs_request_is_get(self):
        # Regression: `gh api` defaults to POST whenever -f/-F fields are
        # present without an explicit method, so the unfixed check-runs
        # call failed closed on every PR, labelled or not. The stub
        # applies that documented rule and refuses non-GET, the way the
        # real endpoint answers the wrong method.
        env = self.stub_gh([], [])
        for labels, check_runs, rc, verdict in (
            ([self.LABEL], [self.check_run("success")], 0, "APPROVED"),
            ([self.LABEL], [self.check_run("failure")], 1, "STALE"),
            ([], [self.check_run("success")], 1, "ABSENT"),
        ):
            with self.subTest(verdict=verdict):
                self.labels.write_text(json.dumps(labels))
                self.checks.write_text(json.dumps({"check_runs": check_runs}))
                proc = self.check(env)
                self.assertEqual(proc.returncode, rc,
                                 proc.stdout + proc.stderr)
                self.assertIn(verdict, proc.stdout)
        seen = False
        for line in self.ghlog.read_text().splitlines():
            if "check-runs" in line:
                seen = True
                self.assertTrue(line.startswith("GET "), line)
                self.assertIn("--paginate", line)
        self.assertTrue(seen, "no check-runs call recorded")

    @case("stale-owner-approval", "cheat",
          "a same-named success check run from another app is not the bind stamp — verdict STALE")
    def test_foreign_app_check_run_ignored(self):
        # Only check runs the github-actions app wrote count. A same-named
        # success from any other app leaves the verdict STALE — including
        # one newer than the real stamp, since the app filter runs before
        # latest-wins.
        env = self.fixtures(
            [self.LABEL], [self.check_run("success", app="probot")])
        proc = self.check(env)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("STALE", proc.stdout)
        self.assertNotIn("APPROVED", proc.stdout)
        env = self.fixtures(
            [self.LABEL],
            [self.check_run("failure", ts="2026-09-27T12:00:00Z"),
             self.check_run("success", ts="2026-09-27T13:00:00Z",
                            app="probot")],
        )
        proc = self.check(env)
        self.assertEqual(proc.returncode, 1)
        self.assertIn("STALE", proc.stdout)

    # ---- owner-approval.yml: parse assertions + stubbed-gh execution ---

    def wf_run_block(self):
        """The bind step's `run: |` body — the workflow has exactly one."""
        lines = real_file(self.WF).splitlines()
        for i, line in enumerate(lines):
            if line.strip() == "run: |":
                indent = len(line) - len(line.lstrip())
                body = []
                for line in lines[i + 1:]:
                    if line.strip() and len(line) - len(line.lstrip()) <= indent:
                        break
                    body.append(line[indent + 2:] if line.strip() else "")
                return "\n".join(body)
        self.fail("%s has no run: | block" % self.WF)

    def run_wf(self, action, label_names=(), delete_rc=0,
               event_label="owner-approved", head="cafe0123"):
        """Execute the workflow's own run block under a stubbed gh, the way
        the runner executes it, minus the network. The stub logs every
        call, serves the label list as --jq prints it (one name per line),
        and can be told to fail the DELETE."""
        self.ghlog.write_text("")
        (self.dir / "label-names.txt").write_text(
            "".join(n + "\n" for n in label_names))
        gh = self.dir / "gh"
        gh.write_text(
            "#!/bin/sh\n"
            'printf "%s\\n" "$*" >> "$GOV_STUB_LOG"\n'
            'case "$*" in\n'
            '    *DELETE*)     exit "${GOV_STUB_DELETE_RC:-0}";;\n'
            "    *check-runs*) exit 0;;\n"
            '    *labels*)     exec cat "$GOV_STUB_LABEL_NAMES";;\n'
            "esac\n"
            "exit 1\n"
        )
        os.chmod(gh, 0o755)
        env = dict(os.environ)
        env.update({
            "PATH": str(self.dir) + ":" + os.environ["PATH"],
            "REPO": "o/r",
            "PR": "5",
            "HEAD_SHA": head,
            "ACTION": action,
            "EVENT_LABEL": event_label,
            "GH_TOKEN": "test",
            "RUN_URL": "https://example.invalid/run/1",
            "GOV_STUB_LOG": str(self.ghlog),
            "GOV_STUB_LABEL_NAMES": str(self.dir / "label-names.txt"),
            "GOV_STUB_DELETE_RC": str(delete_rc),
        })
        proc = subprocess.run(
            ["bash", "-c", self.wf_run_block()],
            env=env, capture_output=True, text=True,
            cwd=str(self.dir), timeout=30,
        )
        calls = self.ghlog.read_text() if self.ghlog.exists() else ""
        return proc, calls

    def test_workflow_never_materializes_pr_code(self):
        # parsing assertion: pull_request_target only (never a head-defined
        # pull_request run), no checkout, no actions — the run block
        # invokes no repo code, only event metadata and the API
        text = real_file(self.WF)
        self.assertIn("pull_request_target", text)
        self.assertNotRegex(text, r"(?m)^\s*pull_request:")
        for event in ("labeled", "unlabeled", "synchronize"):
            self.assertIn(event, text)
        self.assertNotIn("checkout", text.lower())
        self.assertNotRegex(text, r"(?m)^\s*-?\s*uses:")
        self.assertIn("github.event.pull_request.head.sha", text)
        body = self.wf_run_block()
        for bad in ("just ", "cargo", "scripts/", "make "):
            self.assertNotIn(bad, body)

    def test_workflow_labeled_records_success_on_event_head(self):
        proc, calls = self.run_wf("labeled", ("owner-approved",))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        posts = [l for l in calls.splitlines() if "check-runs" in l]
        self.assertEqual(len(posts), 1, calls)
        self.assertIn("head_sha=cafe0123", posts[0])
        self.assertIn("conclusion=success", posts[0])

    def test_workflow_unlabeled_records_failure(self):
        proc, calls = self.run_wf("unlabeled", ("bug",))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        posts = [l for l in calls.splitlines() if "check-runs" in l]
        self.assertEqual(len(posts), 1, calls)
        self.assertIn("head_sha=cafe0123", posts[0])
        self.assertIn("conclusion=failure", posts[0])

    @case("stale-owner-approval", "cheat",
          "unrelated label events cannot approve H2 with H1's stale label or cancel its revocation (R3-5)")
    def test_workflow_unrelated_label_after_push_never_approves(self):
        for action in ("labeled", "unlabeled", "labeled-other"):
            with self.subTest(action=action):
                proc, calls = self.run_wf(
                    "labeled", ("owner-approved",), head="11111111")
                self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assertIn("head_sha=11111111", calls)
                self.assertIn("conclusion=success", calls)
                # H2 has appeared; synchronize has not removed H1's label yet.
                proc, unrelated = self.run_wf(
                    action, ("owner-approved", "triage"),
                    event_label="triage", head="22222222")
                self.assertEqual(proc.returncode, 0, proc.stderr)
                # Run the queued revocation too, even on the unfixed workflow.
                revoked, calls = self.run_wf(
                    "synchronize", ("owner-approved", "triage"),
                    event_label="", head="22222222")
                self.assertEqual(revoked.returncode, 0, revoked.stderr)
                self.assertIn("DELETE repos/o/r/issues/5/labels/owner-approved", calls)
                self.assertIn("head_sha=22222222", calls)
                self.assertIn("conclusion=failure", calls)
                self.assertEqual(unrelated, "", unrelated)
                # With no success on H2, the consumer cannot reuse H1's stamp.
                verdict = self.check(self.fixtures([self.LABEL], []), head="22222222")
                self.assertEqual(verdict.stdout.strip(), "STALE", verdict.stderr)
                self.assertEqual(verdict.returncode, 1)
                verdict = self.check(self.fixtures([], [self.check_run("failure")]),
                                     head="22222222")
                self.assertEqual(verdict.stdout.strip(), "ABSENT", verdict.stderr)
                self.assertEqual(verdict.returncode, 1)

    def test_workflow_filters_events_without_cancelling_revocations(self):
        text = real_file(self.WF)
        bind = ProtectedDiffCiTests._job_section(text, "bind")
        self.assertIn("if: >-", bind)
        self.assertIn("github.event.action == 'synchronize'", bind)
        self.assertIn("github.event.label.name == 'owner-approved'", bind)
        self.assertIn("EVENT_LABEL: ${{ github.event.label.name }}", bind)
        # Even cancel-in-progress: false evicts a pending run. No shared
        # concurrency group means every synchronize/removal can finish.
        self.assertNotRegex(text, r"(?m)^\s*concurrency:")

    def test_workflow_owner_removal_revokes_even_if_label_reappears(self):
        proc, calls = self.run_wf("unlabeled", ("owner-approved",))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("conclusion=failure", calls)
        self.assertNotIn("conclusion=success", calls)

    def test_workflow_owner_label_absent_cannot_approve(self):
        proc, calls = self.run_wf("labeled", ("triage",))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("conclusion=failure", calls)
        self.assertNotIn("conclusion=success", calls)

    @case("stale-owner-approval", "control",
          "synchronize revokes the label and records failure on the moved head")
    def test_workflow_sync_revokes_label_and_fails_head(self):
        proc, calls = self.run_wf("synchronize", ("owner-approved",))
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("DELETE repos/o/r/issues/5/labels/owner-approved", calls)
        posts = [l for l in calls.splitlines() if "check-runs" in l]
        self.assertEqual(len(posts), 1, calls)
        self.assertIn("head_sha=cafe0123", posts[0])
        self.assertIn("conclusion=failure", posts[0])

    @case("stale-owner-approval", "cheat",
          "a failed owner-approved DELETE on synchronize fails the workflow loudly — never warn-and-continue")
    def test_workflow_sync_delete_failure_fails_loudly(self):
        proc, calls = self.run_wf(
            "synchronize", ("owner-approved",), delete_rc=1)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("DELETE", calls)


class StripperTests(unittest.TestCase):
    """Direct unit tests for the shared string-aware comment stripper —
    the helper the purity, lint-integrity and protected-diff scanners run
    on before matching. Comments become whitespace, literals are kept (or
    blanked on request), and line numbers never move."""

    def test_line_and_nested_block_comments(self):
        src = "a /* x /* y */ z */ b\n// gone\nc"
        self.assertEqual(stripper.strip(src), "a                   b\n       \nc")

    def test_comment_markers_inside_literals_kept(self):
        src = 'let s = "a // b"; // tail\nlet t = r#"/*"#; let u = 1;'
        out = stripper.strip(src)
        self.assertIn('"a // b"', out)
        self.assertIn('r#"/*"#', out)
        self.assertTrue(out.rstrip().endswith("let u = 1;"))
        self.assertNotIn("tail", out)

    def test_code_after_string_on_same_line_kept(self):
        src = 'let s = "/* not a comment"; let y = 2;'
        self.assertEqual(
            stripper.strip(src),
            'let s = "/* not a comment"; let y = 2;')

    def test_char_literals_vs_lifetimes(self):
        src = "let c = '/';\nfn f<'a>(x: &'a str) -> &'a str { x }\n"
        out = stripper.strip(src)
        self.assertIn("'/'", out)
        self.assertIn("&'a str", out)

    def test_line_numbers_preserved(self):
        src = "// c1\n#[cfg(test)]\n// c3 /* x */ more\nfn g() {}\n"
        out = stripper.strip(src)
        self.assertEqual(out.split("\n")[1], "#[cfg(test)]")
        self.assertEqual(out.count("\n"), src.count("\n"))

    def test_brackets_inside_strings_do_not_unbalance(self):
        text = 'fn f() { let s = "}]"; }'
        self.assertEqual(
            stripper.match_bracket(text, text.index("{")), text.rindex("}"))

    def test_split_top_ignores_literal_commas(self):
        parts = stripper.split_top('a, "x,y", b')
        self.assertEqual([p.strip() for p in parts], ["a", '"x,y"', "b"])

    def test_blank_literals_hides_string_contents(self):
        src = 'const S: &str = "cfg!(not(test))";\nlet x = 1;'
        out = stripper.strip(src, blank_literals=True)
        self.assertNotIn("not(test)", out)
        self.assertIn("let x = 1;", out)

    def test_attr_spans_skip_literals_and_comments(self):
        src = '/* #[allow(x)] */ #[doc = "#[allow(y)]"]\nfn f() {}\n'
        spans = stripper.attr_spans(stripper.strip(src))
        self.assertEqual(len(spans), 1)

    def findings(self, src, src_scope=True):
        return [
            (k, ln) for k, ln, _d in stripper.lint_findings(
                stripper.strip(src, blank_literals=True), src_scope=src_scope)
        ]

    def test_raw_identifiers_normalized(self):
        # r#ident is the same token as ident to rustc: attribute heads,
        # lint-path segments and cfg predicates all compare canonically
        self.assertEqual(
            self.findings(
                '#[r#expect(clippy::r#disallowed_macros, reason = "x")]\n'
                "fn g() {}\n"),
            [("expect-target", 1)])
        self.assertEqual(
            self.findings("#[r#allow(dead_code)]\nfn g() {}\n"),
            [("suppress", 1)])
        self.assertEqual(
            self.findings("#[cfg_attr(test, r#mutants::skip)]\nfn g() {}\n"),
            [("mutants-skip", 1)])
        self.assertEqual(
            self.findings("#[r#cfg_attr(test, r#allow(dead_code))]\nfn g() {}\n"),
            [("suppress", 1)])
        self.assertEqual(
            self.findings("#[cfg(r#not(r#test))]\nfn g() {}\n"),
            [("not-test", 1)])

    def test_not_test_trailing_comma(self):
        for src in (
            "#[cfg(not(test,))]\nfn g() {}\n",
            "#[cfg(not( test , ))]\nfn g() {}\n",
            "#[cfg_attr(not(test,), derive(Debug))]\nfn g() {}\n",
        ):
            with self.subTest(src=src):
                self.assertEqual(self.findings(src), [("not-test", 1)])

    def test_cfg_macro_all_delimiters(self):
        for src in (
            "fn g() { let _x = cfg!(not(test)); }\n",
            "fn g() { let _x = cfg![not(test)]; }\n",
            "fn g() { let _x = cfg!{not(test)}; }\n",
            "fn g() { let _x = cfg ! { not(test) }; }\n",
            "fn g() { let _x = r#cfg!{not(test)}; }\n",
        ):
            with self.subTest(src=src):
                kinds = [k for k, _ln in self.findings(src)]
                self.assertEqual(kinds, ["not-test"])

    def test_unraw_preserves_raw_string_openers_and_literals(self):
        # r#"..."# is a raw-string opener, not a raw identifier, and a
        # literal's contents are never rewritten
        src = 'let s = r#"r#x"#;\nlet t = "r#y";\nlet u = r#foo;\n'
        self.assertEqual(
            stripper.unraw(src),
            'let s = r#"r#x"#;\nlet t = "r#y";\nlet u = foo;\n')

    def test_raw_string_contents_never_flagged(self):
        # the literal-blanked view hides code-shaped text inside literals
        src = 'const S: &str = r#"#[cfg(not(test))]"#;\nfn g() {}\n'
        self.assertEqual(self.findings(src), [])

    def test_reason_key_raw_ident_counts(self):
        # r#reason = "x" is the reason key — a reasoned expect of an
        # ordinary lint stays the sanctioned form
        self.assertEqual(
            self.findings(
                '#[expect(dead_code, r#reason = "x")]\nfn g() {}\n'),
            [])

    def test_unsilenceable_expect_targets_flagged_outside_src_scope(self):
        # src_scope=False (member tests/ trees) disables the purity-target
        # ban but never the function-length one — the escape cannot reach
        # too_many_lines, its pedantic group, or the warnings group
        for attr in (
            '#[expect(too_many_lines, reason = "x")]',
            '#[expect(clippy::too_many_lines, reason = "x")]',
            '#[expect(clippy::r#too_many_lines, reason = "x")]',
            '#[expect(clippy::pedantic, reason = "x")]',
            '#[expect(warnings, reason = "x")]',
            '#[cfg_attr(test, expect(clippy::too_many_lines, reason = "x"))]',
        ):
            with self.subTest(attr=attr):
                self.assertEqual(
                    self.findings(attr + "\nfn g() {}\n", src_scope=False),
                    [("expect-target-anywhere", 1)])
        # and the tests/-scoped purity escape is untouched by the new set
        self.assertEqual(
            self.findings(
                '#[expect(clippy::disallowed_methods, reason = "x")]\n'
                "fn g() {}\n",
                src_scope=False),
            [])


class CheatCoverageTests(unittest.TestCase):
    """Meta: every declared cheat case has a cheat-side test (asserts the
    gate FAILs) and a control-side test (asserts the gate PASSes)."""

    def test_declared_cases_match_the_assignment_list(self):
        self.assertEqual(set(CASES), EXPECTED_CASES)

    def test_every_case_has_cheat_and_control_tests(self):
        for name, c in sorted(CASES.items()):
            with self.subTest(case=name):
                self.assertTrue(c["cheat"], "%s: no cheat-side test" % name)
                self.assertTrue(c["control"], "%s: no control-side test" % name)


class RecordingResult(unittest.TextTestResult):
    def __init__(self, *a, **k):
        super().__init__(*a, **k)
        self.outcomes = {}

    def addSuccess(self, t):
        self.outcomes[t.id()] = "pass"
        super().addSuccess(t)

    def addFailure(self, t, e):
        self.outcomes[t.id()] = "FAIL"
        super().addFailure(t, e)

    def addError(self, t, e):
        self.outcomes[t.id()] = "ERROR"
        super().addError(t, e)

    def addSkip(self, t, r):
        self.outcomes[t.id()] = "skip(%s)" % r
        super().addSkip(t, r)

    def addExpectedFailure(self, t, e):
        self.outcomes[t.id()] = "xfail(documented gap)"
        super().addExpectedFailure(t, e)

    def addUnexpectedSuccess(self, t):
        self.outcomes[t.id()] = "xpass(gap closed?)"
        super().addUnexpectedSuccess(t)


def report_cases(result):
    outcomes = result.outcomes
    rows = []
    unhealthy = []
    for name in sorted(CASES):
        c = CASES[name]
        for role in ("cheat", "control"):
            for fn in c[role]:
                tid = "%s.%s" % (fn.__module__, fn.__qualname__)
                out = outcomes.get(tid, "not-run")
                rows.append((name, role, tid.split(".", 1)[-1], out))
                if not (out == "pass" or out.startswith("xfail")):
                    unhealthy.append((name, tid, out))
    print("\n=== seeded-cheat coverage report ===")
    print("%-34s %-8s %-72s %s" % ("case", "role", "test", "outcome"))
    print("%-34s %-8s %-72s %s" % ("-" * 34, "-" * 8, "-" * 72, "-" * 10))
    for name, role, tid, out in rows:
        print("%-34s %-8s %-72s %s" % (name, role, tid, out))
    print(
        "%d cheat cases, %d rows — every cheat names a gate check that must FAIL, "
        "every control a check that must PASS" % (len(CASES), len(rows))
    )
    if unhealthy:
        print("UNHEALTHY case rows: %s" % unhealthy)


if __name__ == "__main__":
    suite = unittest.TestLoader().loadTestsFromModule(sys.modules[__name__])
    runner = unittest.TextTestRunner(verbosity=1, resultclass=RecordingResult)
    result = runner.run(suite)
    report_cases(result)
    sys.exit(0 if result.wasSuccessful() else 1)

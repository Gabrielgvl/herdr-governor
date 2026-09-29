#!/usr/bin/env python3
"""Shared matcher for scripts/protected-paths.txt.

One implementation of the protected-path glob semantics for the two
consumers that MATCH paths: agent-gate.sh (the advisory PreToolUse
hook — fail-open on a broken interpreter) and check-protected-diff.sh
(the enforcement diff gate — fail-closed). check-lint-integrity.sh only
parses the same table for its own existence and coverage checks and does
not match paths. The shared module keeps hook and gate from drifting
apart (pi-review F3); strip_rust_comments.py is the same pattern for
the R2/R8/I3 lint engine.

Semantics (docs/guardrails.md): patterns are repo-relative globs matched
by suffix — a pattern compiles to `(?:^|/)<glob>$`, so `tests/support/**`
covers every member's `*/tests/support/**` and a root-level dir alike.
`**` spans `/`; `*` and `?` do not. `mentions` adds the hook-only rule
that a bare `dir/**` base directory counts (`rm -rf scripts` trips
`scripts/**`).

Both callers import this inside `python3 -B` heredocs via
`sys.path.insert(0, <scripts dir>)`; -B keeps scripts/ free of the
__pycache__ a fixture repo's install_scripts would copy.
"""

import os
import re

MODES = ("hard", "conditional", "section", "report")


def load_policy(policy_path):
    """Parse protected-paths.txt into {mode: [pattern]}. OSError
    propagates — the hook caller degrades it to allow, the diff gate
    lets it fail."""
    buckets = {m: [] for m in MODES}
    with open(policy_path, encoding="utf-8") as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            parts = line.split("\t")
            if len(parts) < 2:
                continue
            bucket = buckets.get(parts[0])
            if bucket is not None:
                bucket.append(parts[1].strip())
    return buckets


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

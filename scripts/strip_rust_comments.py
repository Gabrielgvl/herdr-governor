#!/usr/bin/env python3
"""String-aware Rust comment stripper shared by the gate scripts.

`strip(text)` replaces `//` line comments and `/* */` block comments
(nested, as Rust allows) with whitespace — every comment character
becomes a space except newlines, which are preserved so reported line
numbers stay correct. String literals (`"..."`, `b"..."`, `c"..."`),
raw strings (`r"..."`, `r#"..."#`, `br"..."`, `cr#"..."#`, any hash
count) and char literals (`'x'`, `'\n'`, `'\\u{1F600}'`) are copied
verbatim: a `//` or `/*` inside a literal is never treated as a
comment, and code following a literal on the same line is kept. With
`blank_literals=True` the literal bodies are blanked too, so patterns
quoted inside strings or comments cannot be mistaken for code (the
SQL-literal scans of I9/I10 run on plain `strip()` output instead).

`match_bracket`, `attr_spans`, `iter_metas` and `split_top` reuse the
same literal recognition so structure scans (paren/bracket matching,
comma splitting, attribute bodies) are never confused by bracket-like
text inside strings or char literals. `line_of` maps an offset to a
1-based line number. `unraw` removes `r#` raw-identifier prefixes from
code positions — `r#expect` and `expect` are the same token to rustc —
while never touching string/char literal contents or the `r#"..."#` raw
string opener. `iter_metas` and `lint_findings` run it on their input,
so attribute heads, cfg predicates and every lint-path segment compare
in canonical form. `lint_findings` is the shared attribute scan used
by I3 (check-lint-integrity.sh) and R2/R8 (check-protected-diff.sh) so
the two gates cannot drift apart.

CLI: stdin -> stdout, stripped. `python3 strip_rust_comments.py < f.rs`
"""

import re
import sys

_IDENT = frozenset(
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_")

# Raw-string openers: r"...", r#..."...#, br"...", cr##"..."##, etc.
# The (br|cr) alternatives must precede r so `br"` isn't read as `b`
# + `r"`. Boundary is enforced by the caller via _IDENT lookbehind.
_RAW_OPEN = re.compile(r'(?:br|cr|r)(#*)"')

# Char literal: 'x', '\n', '\'', '\\', '\x7f', '\u{1F600}'. Exactly one
# char or escape between quotes, no newline inside — otherwise it is a
# lifetime ('a) and left alone.
_CHAR = re.compile(
    r"'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")

_BRACKETS = {"(": ")", "[": "]", "{": "}"}


def _ident_char(ch):
    return ch in _IDENT


def _literal_end(text, i):
    """End index (exclusive) of the literal starting at i, else None."""
    n = len(text)
    c = text[i]
    if c == '"':
        return _str_end(text, i + 1, n)
    if c == "'":
        m = _CHAR.match(text, i)
        return m.end() if m else None
    if c in "bcr" and (i == 0 or not _ident_char(text[i - 1])):
        # b"/c" escaped literals or (b|c)r/r raw strings
        if c in "bc" and text.startswith(c + '"', i):
            return _str_end(text, i + 2, n)
        m = _RAW_OPEN.match(text, i)
        if m:
            term = '"' + m.group(1)
            j = text.find(term, m.end())
            return n if j < 0 else j + len(term)
    return None


def _str_end(text, i, n):
    """Index just past the closing `"` of a normal/byte/C string."""
    while i < n:
        c = text[i]
        if c == "\\":
            i += 2
            continue
        if c == '"':
            return i + 1
        i += 1
    return n


def _blank(chunk):
    """Whitespace with the same shape: newlines stay, rest are spaces."""
    return "".join("\n" if c == "\n" else " " for c in chunk)


def strip(text, blank_literals=False):
    """Return `text` with every Rust comment replaced by whitespace.

    With blank_literals=True the string/char literal bodies are blanked
    the same way — the code-only view used for attribute and token
    scans, where a banned pattern quoted inside a literal is not code.
    """
    out = []
    i, n = 0, len(text)
    while i < n:
        end = _literal_end(text, i)
        if end is not None:
            out.append(_blank(text[i:end]) if blank_literals else text[i:end])
            i = end
            continue
        if text.startswith("//", i):
            j = text.find("\n", i)
            if j < 0:
                j = n
            out.append(" " * (j - i))
            i = j
            continue
        if text.startswith("/*", i):
            depth = 1
            out.append("  ")
            i += 2
            while i < n and depth:
                if text.startswith("/*", i):
                    depth += 1
                    out.append("  ")
                    i += 2
                elif text.startswith("*/", i):
                    depth -= 1
                    out.append("  ")
                    i += 2
                else:
                    out.append("\n" if text[i] == "\n" else " ")
                    i += 1
            continue
        out.append(text[i])
        i += 1
    return "".join(out)


def match_bracket(text, i):
    """Index of the bracket closing text[i] ('(' '[' or '{'), else None.

    Literal contents are skipped, so brackets inside strings/chars do
    not move the depth counter.
    """
    close = _BRACKETS.get(text[i] if i < len(text) else "")
    if close is None:
        return None
    depth = 1
    i += 1
    n = len(text)
    while i < n:
        end = _literal_end(text, i)
        if end is not None:
            i = end
            continue
        c = text[i]
        if c in _BRACKETS:
            depth += 1
        elif c == close or c in _BRACKETS.values():
            # any closer pops the innermost opener; malformed mixes only
            # matter for already-invalid source — we just stay consistent
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return None


def attr_spans(text):
    """(open_index, close_index) of each `#[...]`/`#![...]` attribute.

    The span brackets are positions in `text`; a missing close bracket
    yields end == len(text). `#[` openers inside literals are skipped —
    the text there is part of the literal, not an attribute. Call this
    on stripped text so commented-out attributes stay invisible too.
    """
    spans = []
    i, n = 0, len(text)
    while i < n:
        end = _literal_end(text, i)
        if end is not None:
            i = end
            continue
        if text[i] == "#":
            m = re.match(r"#\s*!?\s*\[", text[i:])
            if m:
                open_i = text.index("[", i)
                close_i = match_bracket(text, open_i)
                spans.append((open_i, close_i if close_i is not None else n))
                i = open_i + 1
                continue
        i += 1
    return spans


def split_top(text):
    """Split `text` on top-level commas, ignoring commas inside literals
    and inside nested ()[]{} pairs."""
    parts, start, depth = [], 0, 0
    i, n = 0, len(text)
    while i < n:
        end = _literal_end(text, i)
        if end is not None:
            i = end
            continue
        c = text[i]
        if c in _BRACKETS:
            depth += 1
        elif c in _BRACKETS.values():
            depth -= 1
        elif c == "," and depth == 0:
            parts.append(text[start:i])
            start = i + 1
        i += 1
    parts.append(text[start:])
    return parts


def line_of(text, i):
    """1-based line number of offset i in `text`."""
    return text.count("\n", 0, i) + 1


def squash(text):
    """Drop every whitespace char — token-level view of a code fragment,
    so `mutants :: skip` and `mutants::skip` are the same spelling."""
    return re.sub(r"\s+", "", text)


def _ident_head(ch):
    """Rust identifier-start: letter or '_' (a raw ident is `r#` + one)."""
    return ch == "_" or ch.isalpha()


def unraw(text):
    """Remove `r#` raw-identifier prefixes from code positions.

    `r#foo` is the same token as `foo` to rustc, so attribute heads, cfg
    operators/predicates and lint-path segments must be compared after
    this normalization — `#[r#expect(...)]` and
    `clippy::r#disallowed_macros` cannot slip a banned spelling past the
    checks. Literal spans are copied verbatim: the `r#` of a raw-string
    opener `r#"..."#` is a delimiter, not an identifier, and string or
    char contents are never rewritten. Positions shift where prefixes
    are removed, but newlines are preserved so `line_of` stays correct.
    """
    out = []
    i, n = 0, len(text)
    while i < n:
        end = _literal_end(text, i)
        if end is not None:
            out.append(text[i:end])
            i = end
            continue
        if (text.startswith("r#", i) and i + 2 < n
                and _ident_head(text[i + 2])):
            i += 2
            continue
        out.append(text[i])
        i += 1
    return "".join(out)


def iter_metas(text):
    """Yield (line, body) for every attribute meta item in `text`.

    `text` is expected to be the stripped view; it is raw-identifier
    normalized here, so every yielded body and every cfg_attr test sees
    canonical tokens. `cfg_attr(cond, a, b)` flattens: the condition
    `cond` and each nested item `a`, `b` are yielded as their own meta
    bodies (so a `not(test)` condition and a nested
    `allow(...)`/`expect(...)`/`mutants::skip` are all visible).
    """
    text = unraw(text)
    for open_i, close_i in attr_spans(text):
        stack = [(text[open_i + 1: close_i], line_of(text, open_i))]
        while stack:
            body, ln = stack.pop()
            body = body.strip()
            m = re.match(r"cfg_attr\s*\(", body)
            if m:
                end = match_bracket(body, m.end() - 1)
                inner = body[m.end():end if end is not None else len(body)]
                for arg in split_top(inner):
                    stack.append((arg, ln))
                continue
            yield ln, body


# ---------------------------------------------------------------------------
# Shared attribute-rule engine: I3 (committed tree) and R2/R8 (added lines /
# untracked files) run the same findings so the rules cannot drift apart.
# kind labels: suppress, expect-reason, expect-target, ignore, mutants-skip,
# not-test. The caller maps kinds to its own rule ids and message wording.
# ---------------------------------------------------------------------------

_SUPPRESS = re.compile(r"^(allow|deny|warn|forbid)\(")
_EXPECT = re.compile(r"^expect\(")
_IGNORE = re.compile(r"^ignore\b")
_MUTANTS_SKIP = re.compile(r"^mutants::skip\b")
# not(test) — rustc also accepts the trailing-comma spelling not(test,);
# raw predicates (r#not, r#test) are canonicalized by unraw() upstream.
_NOT_TEST = re.compile(r"\bnot\s*\(\s*test\s*,?\s*\)")

# #[expect] targets banned on member src/** files: the purity lints
# themselves (any spelling) and the lint groups that would silence them —
# clippy::all and clippy::style contain disallowed_methods/types/macros,
# and `warnings` subsumes everything. A reasoned expect of one of these
# in a member tests/ dir is the sanctioned escape hatch (e.g. fixture
# I/O), so the ban is scoped by src_scope.
_BANNED_EXPECT_TARGETS = frozenset((
    "disallowed_methods", "disallowed_types", "disallowed_macros",
    "clippy::disallowed_methods", "clippy::disallowed_types",
    "clippy::disallowed_macros",
    "clippy::all", "clippy::style", "warnings",
))


def _expect_args(body):
    """Meta args of an `expect(...)` body: (squashed targets, has_reason)."""
    p = body.find("(")
    if p < 0:
        return [], False
    end = match_bracket(body, p)
    inner = body[p + 1:end if end is not None else len(body)]
    targets, has_reason = [], False
    for arg in split_top(inner):
        a = squash(arg)
        if a.startswith("reason="):
            has_reason = True
        elif a:
            targets.append(a)
    return targets, has_reason


def lint_findings(text, src_scope=True):
    """Yield (kind, line, detail) for each violation on stripped `text`.

    src_scope=False disables the member-src-only rules (the banned
    #[expect] targets); every other rule applies everywhere.
    """
    text = unraw(text)
    for ln, body in iter_metas(text):
        sq = squash(body)
        if _SUPPRESS.match(sq):
            yield ("suppress", ln, "#[%s" % sq[:40])
        elif _EXPECT.match(sq):
            targets, has_reason = _expect_args(body)
            bad = [t for t in targets if t in _BANNED_EXPECT_TARGETS]
            if src_scope and bad:
                yield ("expect-target", ln,
                       "#[expect(%s, ...)] names a banned purity lint or silencing group"
                       % bad[0])
            elif not has_reason:
                yield ("expect-reason", ln, "#[expect( without reason =")
        elif _IGNORE.match(sq):
            yield ("ignore", ln, "#[ignore")
        elif _MUTANTS_SKIP.match(sq):
            yield ("mutants-skip", ln, "#[mutants::skip")
        elif _NOT_TEST.search(body):
            yield ("not-test", ln, "cfg not(test) gate: %s" % sq[:60])
    # cfg!(...) is a macro, not an attribute — scan code positions.
    # A Rust macro call accepts any delimiter pair: (), [] and {} all
    # count, so the opener check covers all three.
    for m in re.finditer(r"\bcfg\s*!", text):
        i = m.end()
        while i < len(text) and text[i] in " \t\n":
            i += 1
        if i >= len(text) or text[i] not in "([{":
            continue
        end = match_bracket(text, i)
        inner = text[i + 1:end if end is not None else len(text)]
        if _NOT_TEST.search(inner):
            yield ("not-test", line_of(text, m.start()),
                   "cfg!(not(test)) release-only check")


if __name__ == "__main__":
    sys.stdout.write(strip(sys.stdin.read()))

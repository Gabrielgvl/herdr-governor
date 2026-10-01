# Guardrails — what is enforced and how to work with it

This repo is developed by coding agents (Claude, Codex, Pi, Devin) inside
Herdr worktrees, and it is a public GitHub repository from its first commit:
every change merges through a pull request. The guardrails exist so that
"making the checks pass" can only ever mean "doing the work". This page is
the human-facing guide; agents read `AGENTS.md` (`CLAUDE.md` is a symlink to
it).

## Enforcement model

Four layers, in order of increasing strength:

1. **Instructions** — `AGENTS.md`: conventions, the definition of done, and
   the anti-cheat rules.
2. **Harness hooks (advisory)** — per-agent configs
   (`.claude/settings.json`, `.devin/config.json` + `.devin/hooks.v1.json`,
   `.codex/hooks.json`, `.pi/extensions/agent-gate.ts`) deny writes to
   protected paths and forbidden commands, and call
   `scripts/agent-gate.sh` / `scripts/fmt-on-edit.sh`; `.githooks/pre-commit`
   runs `just lint` + `just guard` on commit. The forbidden commands cover
   `git push`, `--no-verify`, `git rm` on tests, `core.hooksPath`
   repointing, `git update-index --skip-worktree`/`--assume-unchanged`,
   `GIT_CONFIG_*` env injection, setting `GOV_PROTECTED_OK`, `gh` commands
   that touch the `owner-approved` label or merge a PR, and the snapshot
   self-acceptance spellings R7 scans for (the exact patterns live in
   `scripts/agent-gate.sh`). Advisory on purpose: an agent can route around
   a file-edit hook with a shell write; hooks stop the *lazy* bypass and
   record intent in the transcript.
3. **`just ci` + gate scripts** — `just guard` runs
   `scripts/check-lint-integrity.sh` (content invariants driven by
   `scripts/lint-policy.txt`) and `scripts/check-protected-diff.sh` (diff
   rules over the worktree); `just guard-selftest` runs the gates' own
   adversarial suite so a gate regression cannot ship green; and `just ci`
   adds the governor gates: `purity`, `schema-check`, `test-inventory`,
   `mutants-diff`. Every session is told `just ci` is the definition of
   done; recipes fail closed on missing pieces — no `|| true` silences, and
   the BASE-dependent recipes fail (never skip) when no base resolves.
4. **Owner review — the non-bypassable layer.** Two boundaries, both real:
   locally, agents deliver *uncommitted* changes and the deliverable is
   `git diff HEAD` / `git status` in the worktree — the exact artifact the
   owner reads before committing; on GitHub, the pull request is the merge
   boundary — required checks plus CODEOWNERS review. The PR gate diffs
   `BASE...HEAD` and — because the repo has a remote — runs the gate
   scripts **from the BASE revision**, materialized via `git archive`, not
   from the PR's own tree: a PR that neuters a gate is still judged by the
   reviewed revision's copy (trusted-BASE). Owner approval is the
   `owner-approved` PR label, and it is **head-bound**:
   `scripts/check-owner-approval.sh` reports APPROVED only when the label
   is on the PR *and* the exact head SHA carries a successful
   `owner-approval` check run — a stamp the base-defined
   `pull_request_target` workflow
   (`.github/workflows/owner-approval.yml`) writes only for an actual
   `owner-approved` labeled event at that head, with the label still
   present. Unrelated label events cannot mint approval or cancel
   revocation. On `synchronize` the workflow revokes the label and stamps
   `failure` on the moved head — so approval-then-push
   is always unreviewed, and (forgeable) commit or label timing is never
   consulted.

One ruleset, no exemptions: these gates constrain every session, including
the manager/orchestrator session that assigns work. The escape hatch — for
agents and humans alike — is "ask the owner", not a quieter bypass.

### The bootstrap exception (stated, not absorbed)

Trusted-BASE has one hole by construction: when BASE predates `scripts/` —
true of the scaffold PR itself, since the seed commit carries only docs —
there is nothing to `git archive`, so the guard jobs fall back to the PR's
own HEAD copies and print a `::warning::` on every step that does. The
first guardrail PR therefore judges itself with its own gates. The window is
bounded, not closed — but on that PR the bound is manual, not mechanical:
the `owner-approved` label and CODEOWNERS are introduced by the scaffold PR
itself, so they are not independent pre-existing enforcement on it. What
gates the first merge is explicit manual owner review plus the repository
protections — required checks, CODEOWNERS review, stale-review dismissal —
configured in the repository settings, outside the PR's contents. The same
disclosure applies one layer down: the scaffold was assembled in
deliberately ungated local bootstrap lanes — the harness config
directories were authored outside the lanes that carried the gate files —
so the finished hook policy did not supervise its own authors; owner
integration of the lane diffs was the control. Once the scaffold merges,
every later PR is judged by gates it did not write.

## Protected paths

Canonical machine list: **`scripts/protected-paths.txt`** — one
`<mode><TAB><pattern>` per line, consumed by `agent-gate.sh`,
`check-protected-diff.sh`, and `check-lint-integrity.sh`. The glob
semantics live in one shared matcher, **`scripts/protected_paths.py`**,
imported by `agent-gate.sh` and `check-protected-diff.sh` so the hook
and the gate cannot drift apart (`check-lint-integrity.sh` parses the
same table for its own existence and coverage checks but matches no
paths). Matching is by suffix, so a bare `tests/fixtures/**` already
covers every member's `*/tests/fixtures/**`.

| Mode | Meaning |
|---|---|
| `hard` | Always protected; edits go through the owner. |
| `section` | Protected per TOML section — `Cargo.toml` manifests only. Changes inside `[lints*]`, `[package]`, `[workspace*]`, `[profile.*]`, `[patch.*]`, `[replace]`, `[bin]`, `[lib]`, `[test]`, `[bench]`, `[example]`, `[features]` fail; changes to dependency sections, `[target.*]`, `[badges]`, `[package.metadata.*]` are always listed in the report — and dependency changes still need owner approval because `Cargo.lock` is hard-protected (see *Adding a dependency*). |
| `conditional` | Protected once the path exists. |
| `report` | Agent-writable; changes are listed in the gate report, not blocked. |

The policy as shipped (the txt file is authoritative; this is its rendering):

- **hard:** `clippy.toml`, `rustfmt.toml`, `deny.toml`,
  `rust-toolchain.toml`, `tombi.toml`, `_typos.toml`, `justfile`,
  `AGENTS.md`, `CLAUDE.md`, `CONTEXT.md`, `.gitignore`, `.gitattributes`,
  `**/.gitattributes`, `Cargo.lock`, `build.rs`, `.claude/**`, `.codex/**`,
  `.devin/**`, `.pi/**`, `.githooks/**`, `scripts/**`, `.github/**`,
  `docs/guardrails.md`, `.config/nextest.toml`, `.cargo/**`
- **conditional:** `release-digests.txt`, `tests/fixtures/**`,
  `tests/support/**`, `strategies/**`, `*_strategies.rs`
- **section:** `Cargo.toml` (every member manifest — the gate builds a
  section map per `*/Cargo.toml` in the diff)
- **report:** `docs/spec/**`, `docs/adr/**`, `docs/plan/**`,
  `docs/research/**`, `docs/reviews/**`, `docs/operations.md`

Beyond the upstream kit baseline this policy adds: `.config/nextest.toml`
and `.cargo/**` promoted to `hard`; `build.rs` `hard`; test fixtures,
helpers and proptest strategies `conditional`; `[features]` moved into the
failing section set so membership and feature edits are owner-only;
`docs/reviews/**` and `docs/operations.md` as `report`.

`docs/spec/**` and `docs/adr/**` are report-mode (owner decision): agents
keep the spec and ADRs current as evidence lands; the gate lists every
change and CODEOWNERS routes it to the owner.

`.gitattributes` is hard because a `-diff` or `binary` attribute renders the
affected diff as `Binary files differ`, blinding every content rule (R2/R3/
R4/R5/R7/R8) *and* the owner's `git diff HEAD` review artifact.

`Cargo.toml` is section-protected rather than file-protected so the gate can
judge *which* section changed: edits in lint/package/workspace/feature
sections fail, edits in dependency sections are reported. Dependency
additions still need owner approval — see *Adding a dependency*.

### What the diff gate flags (`scripts/check-protected-diff.sh`)

Local mode (no argument → `BASE=HEAD`) judges the working deliverable —
`git diff HEAD` plus untracked files. `check-protected-diff.sh <BASE>` is CI
mode over `BASE...HEAD`. Untracked in-scope files join the content scans,
and one the gate cannot scan — over the 8 MiB limit or unreadable — is a
FAIL (R0) naming the file and the limit, never a skipped-scan pass.
Verdicts are FAIL or REPORT:

- **R1** — touched a `hard`/`conditional` path: FAIL on modified, deleted,
  or staged-added files; REPORT on new untracked files (scaffold workers
  legitimately create those).
- **R2** — suppression or dodge attributes introduced into `.rs` files.
  The engine lexes the *complete new-side file* (the `HEAD` blob in CI
  mode, the worktree file locally) on the comment-stripped,
  literal-blanked view — comments, whitespace and `cfg_attr` wrapping
  cannot hide a finding, and literal contents cannot fake one: a
  banned-looking line added inside a multiline string stays inert, while
  an edit inside an existing attribute is judged by the whole attribute.
  Raw identifiers (`r#expect`, `clippy::r#all`, `r#reason`) normalize to
  the same token before comparison. A finding is reported only when the
  diff introduces it — findings already present on the old side are
  subtracted (a violation that merely moves lines stays I3's verdict,
  not re-blamed here), and line numbers are the real file's. Flagged:
  any `allow(`/`deny(`/`warn(`/`forbid(` attribute, `#[expect(` without
  `reason =`, `#[ignore`, `#[mutants::skip` in any spacing, and an
  `#[expect]` naming a purity lint (`disallowed_methods`/`_types`/`_macros`,
  any spelling) or a silencing group (`clippy::all`, `clippy::style`,
  `warnings`): FAIL. The reasoned-expect escape is scoped positively —
  only member `tests/` trees claim it; every other `.rs` path keeps the
  banned-target rule. One ban has no escape at all: an `#[expect]` naming
  the function-length lint (`too_many_lines`, bare or `clippy::`-prefixed)
  or a group containing it (`clippy::pedantic`, `warnings`) fails in
  `src/` and `tests/` alike — the 100-line function cap is unsilenceable.
- **R3** — test deletion in any disguise: removed test markers (`#[test]`,
  `#[tokio::test]`, `#[cfg(test)]`, `fn test_*`), deleted or type-changed
  (`T` — e.g. a test file swapped for a symlink) files on the compiled test
  surface, and renames that move a test path or a `.rs` file carrying test
  markers off it. The surface is member-aware: `<member>/tests/**` plus
  `<member>/src/**` test paths for every top-level member (the virtual root
  compiles nothing). FAIL — invisible to path rules.
- **R4 / R4b** — `Cargo.toml` hunks in protected sections (`[features]`
  included — membership and feature changes are owner-only): FAIL; in
  dependency sections: REPORT — dep changes are always listed for review
  and need owner approval to pass (`Cargo.lock` is hard-protected).
  Evaluated per manifest, so a member's `Cargo.toml` gets the same rules as
  the root's.
- **R5** — `Cargo.lock` moved without a dep-section change, or dep sections
  changed without `Cargo.lock`: FAIL — the pair must move together.
- **R6** — exec-bit flips or non-executable new files under `scripts/*.sh`
  or `.githooks/*`: FAIL — gate weakening by chmod. Same scope as I4.
- **R7** — snapshot self-acceptance machinery in non-protected content (the
  `INSTA_UPDATE` self-accept values — bare, quoted, or spaced
  (`INSTA_UPDATE="always"` all match) — and the `<insta-accept>`/
  `<insta-review>`/`<insta-test-accept>` command spellings, including forms
  with `cargo +<toolchain>` or `cargo --config` inserted): FAIL. `hard`/`conditional` policy files are
  exempt — they legitimately define the patterns — but `report`-mode paths
  like `docs/plan/**` are scanned like any other agent-writable file.
- **R8** — release-only test gates: a `cfg`/`cfg_attr`/`cfg!` form of
  `not(test)` introduced in a `.rs` diff — including the trailing-comma
  `not(test,)`, every `cfg!` delimiter pair (`()`, `[]`, `{}`), and
  raw-identifier predicates (`r#not`, `r#test`): FAIL — behaviour the
  test build cannot see. A content rule on the whole new-side file: it
  is never downgraded, even under owner override.

Owner override: `GOV_PROTECTED_OK=1` downgrades the protected-path and
protected-section rules — R1, R4, R5, R6 — to loud OVERRIDE report lines, in
local and CI mode alike. It does not touch R2, R3, R7 or R8 — content rules
still FAIL under the override. It is the owner's switch — agents must never
set it. In CI the same approval arrives as the `owner-approved` PR label:
the `guard` and `test-inventory` jobs run `check-owner-approval.sh` and
export `GOV_PROTECTED_OK=1` only when the verdict is APPROVED — the label
present plus a successful `owner-approval` check run bound to the exact
head SHA (see *Required checks*). A STALE or ABSENT verdict means no
override; a verdict that
cannot be computed fails closed.

### What the integrity gate checks (`scripts/check-lint-integrity.sh`)

Content invariants a diff-vs-HEAD cannot see (a value weakened commits ago,
hooks uninstalled, `unknown_lints` creep). Driven by `scripts/lint-policy.txt`
(`<file><TAB><required-regex>` pairs — itself hard-protected). Source scans
cover `src`, `tests`, `*/src`, `*/tests` — the dirs that exist:

- **I1** — every required (`!`) `hard`/`section` file exists and is
  non-empty. Absent-but-tracked (or any absence under CI) is a FAIL;
  absent-and-never-tracked is a REPORT locally — scaffold files land on
  sibling branches.
- **I2** — every lint-policy regex matches: the deny-set signatures in
  `Cargo.toml`'s `[workspace.lints]`, `clippy.toml`, and
  `rust-toolchain.toml` cannot silently weaken.
- **I3** — source scan, on the same stripped/blanked, raw-identifier-
  normalized engine R2/R8 use (so comments, spacing, `cfg_attr` wrapping
  and `r#` spellings cannot hide a finding): no
  `#[allow|deny|warn|forbid(` in sources/tests, every `#[expect(` carries
  `reason =`, no `#[ignore`, no `#[mutants::skip]`, and no
  `cfg`/`cfg_attr`/`cfg!` form of `not(test)` (trailing comma and `[]`/`{}`
  macro delimiters included) — the committed-tree mirror of R8. The
  reasoned-expect escape applies only inside member `tests/` trees —
  positively claimed, not inferred from "not under `src/`" — and it never
  reaches the unsilenceable set: `#[expect]` of `too_many_lines`,
  `clippy::pedantic` or `warnings` fails in `src/` and `tests/` alike.
  The scan is
  fail-closed: a crashed producer or an unreadable in-scope file/dir is
  a FAIL, not an empty result.
- **I4** — `.githooks/*` and `scripts/*.sh` keep their exec bit.
- **I5** — every `hard` pattern appears in the `.claude` and `.devin` deny
  lists (consistency between the policy data and the harness configs).
  Absent configs are tolerated locally pre-merge, strict in CI.
- **I6** — every `uses:` in `.github/workflows/` is pinned to a full 40-hex
  SHA; tags, short SHAs, and branch refs fail (zizmor double-gates this).
- **I7** — every file the justfile references exists.
- **I8** — every member manifest (`*/Cargo.toml`) declares
  `[lints] workspace = true`; the virtual root is exempt (it has the
  `[workspace.lints]` table, not a `[lints]` opt-in).
- **I9** — harness-literal tripwire: the literals claude, devin, pi, agy,
  codex, gemini may appear only under
  `herdr-governor/src/adapters/transcript` — the one per-harness module
  (ADR-0002). Lexical over member `src/` trees; member `tests/` dirs sit
  outside the scanned scope, and no filename inside `src/` is exempt — a
  `test_*.rs` or `tests.rs` file still compiles into the crate. `pi` is
  matched case-sensitively — lowercase `pi` is the harness literal, `PI`
  (the math constant) is benign; the other names stay case-insensitive.
  A hit anywhere else means harness concepts leaked into core, store,
  daemon or mcp code.
- **I10** — lifecycle writes live only in `store::transitions`: no SQL
  INSERT/REPLACE/UPDATE/DELETE/DROP TABLE/ALTER TABLE against the
  lifecycle tables and no direct
  `store::transitions::` reference outside
  `herdr-governor/src/store/transitions` (spec §9 — the public API is
  `store::apply`). Same member-`src/` scope as I9 — the transitions module
  is the only exemption. Fail-closed: the transitions dir being absent
  does not silence hits elsewhere.

### The governor gates in `just ci`

- **`just purity`** (`scripts/check-core-purity.sh`) — `governor-core`
  purity, two parts: (a) `cargo metadata` check — normal deps ⊆ the
  allowed set (`serde`, `thiserror`, `sha2`), dev-deps ⊆
  `proptest`/`tempfile`, no build deps, and the only member edge is
  `herdr-governor` → `governor-core`; (b) a lexical I/O scan of
  `governor-core/src` — `std::fs`/`std::net`/`std::io`/`std::process`/
  `std::env`/`std::time`, `Instant::now`, `SystemTime`, `tokio`, `reqwest`,
  `rusqlite`, `unsafe` all fail, including inside inline `#[cfg(test)]`
  modules — plus the spellings a flat token scan cannot see: grouped or
  aliased `use` trees reaching a banned module (`use std::{fs, io}`,
  `use std as x`), all `include`/`include_str`/`include_bytes` identifiers
  (not just invocations: plain, grouped, aliased and raw-identifier imports
  also fail, e.g. `use core::{r#include as imported}`),
  `env!`/`option_env!`, `#[path]`, every `extern crate std|core`
  spelling (`r#std`, `as` aliases, `#[macro_use]`, comment-split), glob
  imports of the `std`/`core`/`alloc` roots (`use std::*` names no
  banned module — rejected outright), and space- or newline-split paths.
  `alloc` is the owner-sanctioned exception to the extern-crate ban:
  `extern crate alloc;` plus `alloc::` paths are how the `no_std` core
  allocates (BTreeMap and friends are intended to work) — only the
  alloc-root glob stays banned, since a glob names no module.
  `governor-core` is `#![no_std]` — lib.rs must open with that inner
  attribute — so `std` paths cannot compile in the first place; the
  lexical layer runs on the comment-stripped, literal-blanked view
  (`scripts/strip_rust_comments.py`), so a banned token inside a comment
  or string is a non-hit and a comment-split one still fires — and
  `r#`-prefixed identifiers normalize before comparison, so `r#path` and
  friends cannot dodge the attribute checks.
  `governor-core/clippy.toml` carries the compiler-enforced layer
  (disallowed methods/types/macros — the macro entries name their `core::`
  paths, which is what they resolve to under `no_std`; `include!` stays
  lexical-only since it expands before lint resolution). Unsupported
  include ingress is rejected rather than recursively scanning arbitrary
  payloads: an aliased include could otherwise compile a non-`.rs` file
  outside purity/I3 and produce zero in-diff mutation candidates. The
  selftest compiles that filesystem-read payload, confirms the zero-mutant
  result, then requires purity to stop it before the mutation gate. A
  `build.rs` would sidestep the boundary entirely — it is `hard`-protected.
  Fails closed: an unresolvable workspace is a FAIL, never a skip.
- **`just schema-check`** (`scripts/check-herdr-schema.sh`) — the
  `tests/fixtures/herdr-api-schema.json` fixture must exist in exactly one
  place, parse, declare the pinned protocol revision, and byte-match its
  `<fixture>.sha256` sidecar — a missing fixture or pin is a FAIL, so the
  same verdict is reported locally and in CI. **`just schema-live`** (kept
  out of `ci`) adds a drift diff against a live `herdr api schema --json`;
  a missing `herdr` binary is a FAIL, not a skip.
- **`just test-inventory <base>`** — the test ratchet: `cargo nextest list`
  inventories are built for BASE (in a temporary worktree) and HEAD, and
  `scripts/check-test-inventory.sh` fails on any test that ran at BASE and
  is missing, filtered or ignored at HEAD — catches deleted, renamed,
  unwired (`mod` removed) and `#[ignore]`d tests alike. An empty/unreadable
  BASE inventory is a FAIL, not a skip. `GOV_PROTECTED_OK` downgrades its
  FAILs to OVERRIDE — intentional test removals go through the owner label.
- **`just mutants-diff <base>`** (`scripts/mutants-diff.sh`) — classifies
  `git diff BASE...HEAD`: (i) a `governor-core/src/**` *production*
  change → `cargo mutants -p governor-core --in-diff`, failing on missed
  mutants and on `--list` itself erroring. A clean `--list` with zero
  entries reports that the diff has no mutation candidates and passes
  — const/type declarations, deletions and comment-only diffs
  legitimately produce no mutants; the accepted residual is
  attribute/cfg-only behaviour changes,
  which R8/I3/lints cover instead of mutant count; (ii) every
  changed path is test-only → a full `cargo mutants -p governor-core`
  run, which is where a weakened helper or same-name no-op test shows up
  — test-only means `*/tests/**` and test-named `*/src/**.rs` paths, plus
  `governor-core/src` files whose changed lines all sit inside
  `cfg(test)`/`#[test]` regions; (iii) otherwise → REPORT "no
  core-relevant change", exit 0 — present, not skipped.
- **`scripts/check-owner-approval.sh`** — the verdict engine behind the
  label: `REPO PR HEAD_SHA LABEL` → APPROVED / STALE / ABSENT from the PR
  label list plus the `owner-approval` check runs on HEAD_SHA, counting
  only runs the `github-actions` app wrote. It runs inside the `guard`
  and `test-inventory` jobs, which wait a bounded time for the
  `pull_request_target` bind run to land before reading the verdict; a
  fixture-file mode exists for tests.

Both BASE-dependent recipes resolve `BASE` as `git merge-base HEAD
origin/main` and fail when it cannot resolve — a missing base means the
check cannot honestly run. On a fresh clone (`origin/main` = HEAD) the diff
is empty: the inventory is a superset-trivial pass and mutants-diff reports
"no core-relevant change" — they run, they are not skipped. Before the
first push, exercise them with an explicit base, e.g. `just test-inventory
<seed-sha>`.

## Required checks

`.github/workflows/ci.yml` runs the `just ci` stages as parallel jobs plus
the PR-boundary gates; **`.github/required-checks.txt`** is the
machine-readable list of the check names GitHub actually publishes, which
is what the ruleset on `main` must require — verbatim. Notes:

- `test (ubuntu-latest)` — the `test` job is a matrix, so GitHub publishes
  one check per leg and never a bare `test`. The matrix is Linux-only
  (owner decision): there is no `test (macos-14)` leg or check.
- `lint` — fmt has no separate check because `just lint` runs
  `cargo fmt --check` before clippy.
- `guard`, `guard-selftest` — the diff/integrity gate and the gates' own
  adversarial suite, both executed from BASE copies on PRs.
- `test-inventory`, `mutants-diff` — the BASE-dependent gates; like the diff
  gate they run on `pull_request` only, since a push has no review boundary
  to diff against.
- `owner-approval` — the `pull_request_target` bind workflow
  (`.github/workflows/owner-approval.yml`), not a required check: it
  stamps success on the exact event head SHA only for an `owner-approved`
  labeled event while the label remains present. Owner-label removal
  stamps `failure`; `synchronize` revokes the label and stamps `failure`
  on the moved head. Unrelated label events are ignored at the job and
  shell boundaries. There is no concurrency group: even
  `cancel-in-progress: false` can replace a pending revocation. All
  revocations must run; overlapping fresh approval may be conservatively
  revoked and require the owner to re-apply the label. The head-bound
  verdict in `guard`/`test-inventory` bounds the approval — a `labeled`
  event races the bind run, so those jobs wait a bounded time for it
  before reading the verdict.

Branch protection on `main` (required checks + CODEOWNERS review +
stale-review dismissal) is the merge boundary; `.github/dependabot.yml`
opens grouped weekly bumps for cargo and github-actions — they hit the same
gates (`Cargo.lock` is hard, so every dep PR still needs the owner label).

The label semantics, precisely: CI also triggers on
`labeled`/`unlabeled`, so adding the label re-runs the gate and removing
it revokes the pass — the event payload is frozen at trigger time (a
re-run still sees the old head SHA), which is why a plain re-run is not
enough. In parallel the bind workflow stamps `success` only for the
owner label's own `labeled` event, never from its presence during an
unrelated label event. Owner-label removal and head movement are
revocations, not opportunities to rebind a stale label. The label covers
the whole PR diff: label only after reviewing it, and only on the current
head — a `synchronize` push makes the bind workflow revoke the label and
stamp `failure` on the moved head. The verdict is STALE while revocation
is pending, then ABSENT, never APPROVED without a new owner-label event.
Stale-review dismissal in the ruleset is the backstop for the review side.

On `push` and `workflow_dispatch` the protected-diff step is skipped and the
integrity/purity/schema steps still run: there is no review boundary to diff
against, and `HEAD~1...HEAD` would stay permanently red after an
owner-approved merge reaches main. The required PR check plus CODEOWNERS
review are the enforcement on that path — nothing merges without the diff
gate having passed on the pull request.

## Changing a protected file

Agents cannot; the owner can, and the paper trail is the point.

1. The change and its reason are described in the task report (or an issue).
2. The owner applies it. Locally the gate needs `GOV_PROTECTED_OK=1` so the
   override is loud in the log; on a pull request the owner labels the PR
   `owner-approved` — on the current head — and the `guard` job re-runs with
   the override exported.
3. `just ci` re-runs green on the result.

### Adding a dependency

Dependency additions need owner approval: `Cargo.lock` is hard-protected, so
the pair — a `Cargo.toml` dep-section edit plus the `Cargo.lock` update —
cannot pass the diff gate on the agent path (R1 fails on the lock; R5 also
fails if only one side moves). `governor-core` additionally has a dependency
allowlist — `just purity` fails on anything outside it regardless of the
diff gate.

1. The agent proposes the crate in its report — what it does and why nothing
   already present covers it.
2. The owner applies it (`cargo add <crate>`), or reviews the agent's diff
   and re-runs the guard with `GOV_PROTECTED_OK=1` — OVERRIDE lines print
   loudly in the gate output. On a pull request the owner instead applies
   the `owner-approved` label and the guard job re-runs with the override.
3. Agents must never set `GOV_PROTECTED_OK` themselves.

## Recipes

`just ci` is the single entrypoint — the same pipeline CI runs.

| Recipe | What it runs |
|---|---|
| `just fmt` | `cargo fmt --check` |
| `just lint` | `cargo fmt --check` + `cargo clippy --all-targets --locked -- -D warnings` |
| `just test` | `cargo nextest run --locked` |
| `just deny` | `cargo deny check` (supply chain) |
| `just hygiene` | `typos`, `tombi`, `cargo sort --check`, `cargo shear`, `shellcheck`, `shfmt`, 500-line file ceiling |
| `just wf` | `actionlint` + `zizmor` on `.github/workflows/` |
| `just guard` | `check-lint-integrity.sh` + `check-protected-diff.sh` |
| `just guard-selftest` | the gates' own adversarial suite (`scripts/`) — without it a gate regression ships green |
| `just doc` | `cargo doc --no-deps` with `-D warnings` |
| `just purity` | `check-core-purity.sh` — dep allowlist + core I/O scan |
| `just schema-check` | `check-herdr-schema.sh` — fixture present, parses, pinned protocol, sha256 sidecar match |
| `just schema-live` | `check-herdr-schema.sh --live` — adds a live `herdr` drift diff; not in `ci` |
| `just contract` | `tests/contract/contract_tests.py` — offline contract suite pinning each confirmed Phase-2 evidence behavior to its committed fixture under `tests/fixtures/contract/`; fails closed on missing or malformed evidence and contacts no live system. Not in `ci` — it is the Phase-2 evidence gate, run on demand and before promotion while the evidence set is still accumulating |
| `just test-inventory <base>` | BASE vs HEAD `cargo nextest list` ratchet |
| `just mutants-diff <base>` | diff-scoped mutation gate on `governor-core` |
| `just ci` | all of the `ci` legs above — the definition of done |
| `just cov` | `cargo llvm-cov nextest --summary-only` (informational, no floors) |
| `just tools` | install the pinned tool manifest |
| `just install-hooks` | `git config core.hooksPath .githooks` + `chmod +x` |
| `just nightly` | the nightly legs below (informational) |
| `just mutants` | `cargo mutants -p governor-core` — full run, the weekly sweep |
| `just fuzz` | embedded `*fuzz*` test targets under a member's src/tests, 10-min cap |
| `just miri` | `cargo +nightly miri test -p governor-core miri_`, 15-min cap |

## Owner procedures

- **Once per clone:** `just install-hooks`, then `just tools`. Fresh clones
  are unhooked until then — accepted: the hook is ergonomics; review and
  `just ci` are enforcement.
- **Harness trust steps:** Claude `-p`/SDK sessions treat the folder as
  trusted, so committed hooks run immediately in Herdr panes. Codex skips
  hooks until each hash is approved in the `/hooks` TUI — re-approve after
  every hook change. Pi project extensions load only after project trust is
  granted. Devin reads `.claude/settings.json` hooks plus
  `.devin/hooks.v1.json` — its tool names differ, so both files exist on
  purpose.
- **Pre-commit** (`.githooks/pre-commit`) runs `just lint` + `just guard`.
  With uncommitted handoff it fires on the *owner's* commit — a final fast
  gate, by design.

## Nightly budget

`.github/workflows/nightly.yml`: weekly cron + `workflow_dispatch`. Every
job is `continue-on-error: true` (informational until signal quality is
known) and self-skips when its targets do not exist yet, so the file is
correct today and activates itself as code lands:

- `mutants-scoped` — `just mutants`: `cargo mutants -p governor-core` with a
  per-mutant timeout — the pure crate holds the decision logic worth
  mutating; the binary is I/O glue.
- `fuzz` — `just fuzz`: embedded `fn`/`mod *fuzz*` targets under a member's
  `src/`/`tests/` (≤ 10 min); a `cargo-fuzz` layout fails loudly rather than
  silently passing.
- `miri` — `just miri`: `cargo +nightly miri test` on `fn miri_*` tests in
  `governor-core` (≤ 15 min); the binary's I/O dependencies can never run
  under Miri.

## Known limits (accepted, stated)

- Hooks are advisory — see layer 2.
- **`git commit` is not denied, and a commit blinds the local guard.** The
  local diff gate compares against HEAD, so committed sabotage produces an
  empty diff and a clean status. Accepted: the deliverable gate is meant to
  make the *uncommitted* working state visible; the backstops are the
  owner's review of the promotion diff, the CI `BASE...HEAD` gate on the
  pull request (run from trusted BASE copies), and the pre-commit hook
  firing on the owner's commit. This is why layer 4 is the non-bypassable
  one.
- Untracked (`??`) files get path classification, and in local mode their
  content and exec bits also join the content scans (R7 on non-protected
  paths, R2/R8 on `.rs` files, R6 exec-bit on its scope); R1 reports the
  paths.
- `Cargo.toml` section protection depends on the diff gate's section
  tracking, not on file-level rules — it is evaluated per manifest in the
  diff.
- **A same-name no-op test is caught only by mutation testing.** R3 watches
  deletions, type changes and marker removal; the inventory ratchet watches
  names — a test gutted to a no-op that keeps its name and `#[test]` marker
  survives both. The catch is the mutation layer: `just mutants-diff` runs
  the full `governor-core` suite when a diff is test-only, and the weekly
  `just mutants` sweeps the crate. A no-op smuggled inside a mixed
  source+test diff is seen only if the in-diff mutants cover the code it
  pretends to test — stated, not absorbed.
- `just cov` is informational until real code makes per-file floors honest;
  a floor on a skeleton is cargo-cult.
- File length is calibrated and hard: `just hygiene` fails on any `.rs`
  file over 500 lines (recalibrated on the Phase-3 core on 2026-09-30 —
  files were clustering just under the old 800 placeholder). Functions
  stay at ≤ 100 lines via clippy `too_many_lines`
  (`too-many-lines-threshold = 100`, pinned in both `clippy.toml` files by
  I2), and no `#[expect]` may silence it — I3/R2 ban the lint and its
  containing groups (`clippy::pedantic`, `warnings`) as expect targets in
  `src/` and `tests/` alike.
- I9 and I10 are lexical tripwires: comments and strings count, and inline
  `#[cfg(test)]` modules inside `src/` files are scanned — a false positive
  means reword or move the text, never weaken the scan.
- I1 tolerates a required path that is absent-and-never-tracked locally
  (REPORT, not FAIL) so scaffold lanes can run ahead of each other; in CI —
  and on any path the committed tree claims — absence is a FAIL.
- Workflow correctness is statically validated only (actionlint + zizmor);
  the first live run on the scaffold PR is the real check — see *The
  bootstrap exception*.
- **Check-run provenance is app-level, not workflow-level.** The verdict
  counts `owner-approval` check runs whose `app.slug` is
  `github-actions` — which any Actions workflow satisfies, including one
  a PR itself adds. Posting a forged stamp requires editing
  `.github/workflows/**`, a hard-protected path the merge policy never
  auto-merges and the owner reviews via CODEOWNERS — the bound is
  review, not cryptography.

## Follow-ups (recorded, not blocking)

- **R3 test-body gutting residual:** deleting `assert_*` lines under a
  surviving `#[test] fn` signature passes the diff gate — line heuristics
  can't see semantic neutering. The inventory ratchet and the test-only
  mutation run narrow but do not close this; the stated ceiling above is the
  permanent posture.
- **Cargo section-map spoof:** `[section]`-looking lines inside TOML `"""`
  strings are read as headers. Real headers bound the spoof span, so the
  worst case is a protected-section edit downgrading from R4 to R4b — still
  reported, never silent.
- **`lint-policy.txt` pins the core deny set, not every entry** in
  `[workspace.lints]`: weakening an *unpinned* lint escapes I2; it is still
  caught by the uncommitted-diff rules and by the CI `BASE...HEAD` gate,
  but the policy file should eventually pin the whole deny set.
- **`src/tests/` needs an intermediate dir:** `test_path` matches
  `<member>/src/<dir>/tests/` only, so `<member>/src/tests/` paths fall
  between the path rules — over-blocking some moves, under-blocking
  marker-free deletions.
- **Snapshot self-accept spellings:** a quoted toolchain
  (`cargo "+nightly" …`) and bare un-cargo'd accept/review invocations evade
  both the agent gate and R7 — the gate scripts carry the exact covered
  patterns.
- **Advisory env redirection:** `GIT_DIR`, `GIT_WORK_TREE`,
  `GIT_INDEX_FILE`, `HOME` and `XDG_CONFIG_HOME` aren't denied by the agent
  gate. The diff gate and CI run under the owner's environment, so no
  enforcement layer is reached.
- **Hooks-path over-deny:** the agent gate denies any command that merely
  mentions the hooks-path config key, including reads and plain text.
- **The owner label covers the whole PR diff:** labelling early and
  pushing more later is caught (the `synchronize` bind run revokes the
  label and stamps `failure` on the moved head, so the verdict reads
  STALE or ABSENT), but the label still applies to every protected path in the
  diff — review first, label last.

## Verification

<!-- region-verification-log -->

The results below are **unpublished pre-publication evidence**: the runs
were recorded on pre-squash local candidates in a detached review
worktree — round 2 on commit `49f90f7` (full:
`49f90f70d641a47b67cb692e78f615807ce67e4d`), tree `08f2b3e` (full:
`08f2b3eb17167ba9504d62d84317dcc2b02fc7f3`), BASE = seed `2e187e5`, and
round 1 on candidate `9a63dd0` (tree `741a041` — RED). Those candidate
commits are not ancestors of the published tree and will not be pushed —
the SHAs identify the evidence, not objects a reader can fetch. What
re-verifies the published tree is CI: the required checks run the same
`just ci` pipeline on the pull request, and the post-publish fresh-clone
reproduction (archive → clone → `just install-hooks` → `just ci`; see
*Owner-only steps still open*) re-executes it on the public objects.

**Recipe results.** In round 2 every runbook recipe was run twice — once
with the ambient Herdr environment set, and once with all six `HERDR_*`
variables stripped (CI-equivalent). All 26 runs exited 0:

| recipe | herdr env | ci env |
|---|---|---|
| `just fmt` | 0 | 0 |
| `just lint` | 0 | 0 |
| `just test` | 0 | 0 |
| `just deny` | 0 | 0 |
| `just hygiene` | 0 | 0 |
| `just wf` | 0 | 0 |
| `just guard` | 0 | 0 |
| `just guard-selftest` | 0 | 0 |
| `just doc` | 0 | 0 |
| `just purity` | 0 | 0 |
| `just schema-check` | 0 | 0 |
| `just test-inventory <seed>` | 0 | 0 |
| `just mutants-diff <seed>` | 0 | 0 |

Evidence tails (round 2): `test` ran 2 tests, 2 passed; `test-inventory`
reported the docs-only seed BASE as an empty inventory, 0 fails (2 tests
at HEAD); `mutants-diff` classified `core-src-diff`, ran 2 in-diff
mutants, 2 caught; `guard-selftest` reported 20 cheat cases, 75 rows;
`deny` clean; `purity` clean; `schema-check` protocol == 22 with sha256
pin match; `wf` zizmor clean on both workflows. `git status --porcelain`
was empty after all runs.

Round 1 (`9a63dd0`) was RED: `just hygiene` exited 2 (two typos hits in
gate scripts, `cargo shear` on the unused `governor-core` dep, `shfmt`
diffs in 8 scripts) and `just test-inventory` exited 102 (the docs-only
seed has no `Cargo.toml`, so cargo aborted before the checker). Both were
fixed in the round-2 candidate; every other recipe was already green.

**Fail-closed proof.** Committed-mode protected diff against the seed —
`check-protected-diff.sh 2e187e5` — exits 1 with 50 FAIL lines (39 R1
hard-path additions + 11 R4 manifest-section violations) plus 2 REPORT
lines, and zero R2/R3/R7/R8 content-rule failures, on both rounds. A
scaffold that adds the protected paths is denied exactly as designed.

**Selftest totals.** `just guard-selftest` reports 20 cheat cases, 75
rows — green at `9a63dd0` (round 1) and at `49f90f7` (round 2). `just
test` runs 2 tests, 2 passed.

**Known over-deny (accepted).** The agent gate denied the p1-hooks smoke
test — a command-scoped `git -c` repoint of the hooks path at the
sanctioned `.githooks` directory — because the lexical tripwire cannot
distinguish an ephemeral inward `-c` from a persistent repoint. The gate
is kept (see *Follow-ups*: hooks-path over-deny); the pre-commit hook was
verified by inspection instead — it cd's to the toplevel and runs
`just lint guard`.

**Owner-only steps still open.** The `GOV_PROTECTED_OK` /
`owner-approved` override half of the verification runbook is owner-only
and was never exercised — the scaffold PR's protected-path additions land
through the owner label. The fresh-clone reproduction (archive → clone →
`just install-hooks` → `just ci`) is owned by p1-clone-verify after
publish; the seeded-cheat adversarial clones were likewise outside the
verify node's scope (the guard-selftest covers the cheat matrix
in-tree).

# herdr-governor — agent instructions

A durable daemon that launches coding agents into Herdr panes, routes each
Task to an operating point, and supervises the resulting Run until its
handoff is accepted. Two-crate Cargo workspace: `governor-core` is the pure
domain crate (no I/O), `herdr-governor` the daemon binary around it.
Linux-only: the CI matrix, `deny.toml` targets, and the toolchain all assume
it — there is no macOS leg to keep green.

- Vocabulary (Task, Launch, Run, Handoff, Settlement, Caller, Jev, Harness,
  Transcript, Operating point, Catalog, Provider, Cooldown, Outcome) is
  defined in `CONTEXT.md` — use it verbatim.
- Binding decisions live in `docs/spec/herdr-governor-spec.md` and
  `docs/adr/`. FRs and ADRs outrank this file; cite them when they drive a
  choice.
- Human-facing gate documentation: `docs/guardrails.md`.

## Definition of done

`just ci` is green. Run it and report the real output. Work is not done when
the code is written; it is done when the checks you ran pass. If a check seems
wrong, say so in the report. Never weaken, delete, skip, or bypass a check to
reach green — that is the one unforgivable move in this repo.

## Commands

Use `just` recipes only. Never invoke `cargo`, `rustfmt`, `clippy-driver`, or
the gate scripts directly — the recipe is the contract.

- `just ci` — the full gate: fmt, lint, test, deny, hygiene, wf, guard,
  guard-selftest, doc, purity, schema-check, test-inventory, mutants-diff.
- `just fmt`, `just lint`, `just test` — the fast inner loop (`just lint`
  runs `cargo fmt --check` too; fmt is inside the lint contract).
- `just guard` — protected-diff and lint-integrity checks on your worktree.
- `just guard-selftest` — the gates' own adversarial suite; part of
  `just ci` and a required CI check.
- `just purity` — the `governor-core` dependency allowlist plus a lexical
  I/O scan of `governor-core/src` (`scripts/check-core-purity.sh`).
- `just schema-check` — the pinned `herdr api` schema fixture (parse,
  protocol, sha256 sidecar). `just schema-live` — the same plus a live diff
  against `herdr api schema --json`; kept out of `ci`, and it fails when no
  `herdr` binary is on PATH.
- `just test-inventory <base>` and `just mutants-diff <base>` — the
  BASE-dependent gates. BASE defaults to `git merge-base HEAD origin/main`
  and both recipes FAIL when it cannot be resolved — they are never
  skipped. Before a pushed `origin/main` exists, bootstrap with an explicit
  base: `just test-inventory <seed-sha>`.
- `just cov` — coverage summary (informational; no floors yet).
- `just tools` — install the pinned tool manifest.
- `just install-hooks` — wire `.githooks/` as `core.hooksPath`, once per clone.
- `just mutants`, `just fuzz`, `just miri` — the informational nightly legs;
  `mutants` and `miri` are scoped to `governor-core` (the pure crate holds
  the decision logic worth mutating; the binary's I/O deps can never run
  under Miri).

## Rules that exist to stop known cheats

- **Lint suppression:** only `#[expect(lint, reason = "...")]`. `#[allow]`,
  `#[deny]`, `#[warn]`, `forbid`, and `cfg_attr(.., allow(..))` do not compile
  (`allow_attributes` is denied); an `#[expect]` without `reason` fails the
  gate. No `#[ignore]`, no `#[mutants::skip]` — the diff gate fails on both.
- **Tests must fail without the change.** For a fix, add the test first and
  show it failing on the unfixed code. Never delete, rename, type-change,
  or neuter a test to make a suite pass — test deletions are gated even
  though tests are not a protected path.
- **No snapshot self-acceptance.** Never run a snapshot self-accept command —
  `<insta-accept>` and its `<insta-review>`/`<insta-test-accept>` spellings,
  or any of them with `cargo +<toolchain>`/`cargo --config` inserted; never
  set `INSTA_UPDATE` to a self-accepting value (the gate scripts carry the
  exact patterns). Generate the `.snap.new`, explain why the new output is
  correct, and let the owner accept.
- **Dependencies need owner approval.** Propose the crate in your report —
  what it does and why nothing already present covers it. The owner applies
  it, or runs the guard with `GOV_PROTECTED_OK=1` — in CI the equivalent
  owner switch is the `owner-approved` PR label, and it counts only bound
  to the current head: the base-defined `pull_request_target` workflow
  stamps an `owner-approval` check run on the exact head SHA while the
  label is present and revokes it on `synchronize`. Never set that
  variable yourself — an
  unapproved dep-add fails the gate (`Cargo.lock` is hard-protected).
  `Cargo.toml` and `Cargo.lock` move together;
  `cargo update --precise` only, never blanket updates.
- **Protected paths are off-limits.** `scripts/protected-paths.txt` is the
  machine-readable list; `docs/guardrails.md` explains it. Lint configs,
  `.gitattributes`, workflows, hooks, scripts, `Cargo.lock`, and this file
  are among them — propose the edit in your report and the
  owner applies it. This binds every session, including manager and
  orchestrator sessions.
- **Lean (ponytail).** Smallest change that fixes the root cause. Reuse the
  helper that already exists; no unrequested abstractions, wrappers, or
  configurability. A deliberate simplification gets a `ponytail:` comment
  naming the ceiling and the upgrade path.
- **Leave changes uncommitted.** The owner reviews `git diff` / `git status`
  before promoting. A plain `git commit` is not denied, but the local guard
  diffs HEAD — a commit empties your visible deliverable, not the trail:
  the owner reviews the promotion diff, and CI re-runs the same gate over
  `BASE...HEAD` on the pull request. No `git push`, no `--no-verify`,
  no `git config` changes or `GIT_CONFIG_*` env injection.

## Governor rules the gates also enforce

- **The workspace has exactly two top-level members.** Members live at the
  repo root as `governor-core/` and `herdr-governor/` — no nested crates.
  `herdr-governor` depends on `governor-core`, never the reverse, and no
  other member edge is legal; `just purity` checks both. Workspace
  membership and `[features]` are protected: an edit to a `[workspace*]`,
  `[lints*]`, `[package]`, `[profile.*]`, `[patch.*]`, `[replace]`,
  `[bin]`, `[lib]`, `[test]`, `[bench]`, `[example]`, or `[features]` section of any
  `Cargo.toml` fails the diff gate (R4) — only dependency sections are
  report-mode, and those still need owner approval through `Cargo.lock`.
- **Every member manifest declares `[lints] workspace = true`** — the
  integrity gate (I8) fails on a member without it, so the workspace deny
  set can never be scoped out per crate.
- **`governor-core` stays pure.** `src/lib.rs` opens with `#![no_std]` —
  the compiler boundary: `std` paths do not compile in the crate. Inputs
  are values, including time and entropy; outputs are `Transition` values
  (spec §9). Allowed dependencies: `serde`, `thiserror`, `sha2` —
  `proptest`, `tempfile` as dev-deps, and no build deps. `just purity`
  fails on the missing attribute, on any I/O, clock, process, `unsafe`,
  tokio/reqwest/rusqlite token in `governor-core/src`, and on the escape
  spellings — `use std::*`/glob roots, comment-split paths, `#[path]`,
  `extern crate std` in any spelling (`r#std`, `as` aliases,
  `#[macro_use]`), `include!`/`env!` family — scanned on the
  comment-stripped, literal-blanked view. Inline `#[cfg(test)]` modules
  included; test code that needs the filesystem lives under
  `governor-core/tests/`. Silencing the purity lints with
  `#[expect(clippy::disallowed_*)]` or a broad group (`clippy::all`,
  `clippy::style`, `warnings`) fails I3/R2 on member `src/**` — the
  sanctioned reasoned-expect escape lives in `*/tests/` only.
- **No release-only `cfg`.** `#[cfg(not(test))]` and `cfg!(not(test))` hide
  behaviour from the test build — R8 (diff gate) and the I3 source scan both
  fail on them, and the owner override never downgrades R8.
- **No harness names outside the transcript adapter.** The literals
  claude, devin, pi, agy, codex, gemini are legal only under
  `herdr-governor/src/adapters/transcript` (ADR-0002). In any other member
  `src/` file the I9 tripwire fails — harness identity is catalog data, not
  code. The scan is lexical, so comments and strings count.
- **Lifecycle writes live only in `store::transitions`.** SQL
  INSERT/UPDATE/DELETE against the lifecycle tables, and any direct
  `store::transitions::` path reference, are legal only under
  `herdr-governor/src/store/transitions` — everywhere else fails I10. The
  store's public API is `store::apply(Transition)` plus read queries
  (spec §9).
- **The Herdr schema fixture is pinned.** `tests/fixtures/herdr-api-schema.json`
  must parse, declare the pinned protocol revision, and byte-match its
  `.sha256` sidecar — `just schema-check` fails on absence or drift, so
  regenerating the fixture means regenerating the pin.
- **Test helpers and strategies are conditional-protected.** Once they
  exist, `tests/fixtures/**`, `tests/support/**`, `strategies/**` and
  `*_strategies.rs` (in any member) become protected paths — weakened
  assertion helpers and narrowed generators are known cheats. The
  `just test-inventory` ratchet fails on any test that ran at BASE and is
  missing, filtered or ignored at HEAD; `just mutants-diff` runs the full
  `governor-core` mutation suite on test-only diffs — a same-name no-op
  test is caught by mutation testing, not by the diff gate.
- **`build.rs` is hard-protected** — adding one anywhere in the tree fails
  the gate; the owner applies it.

## Conventions — make wrong code not compile

- **Pure core, thin shell.** `governor-core` functions take values — time,
  entropy, config — and return `Transition`s; they never touch I/O. All
  effect execution lives in `herdr-governor`. Test the core with
  constructed inputs, not mocks.
- **One coordinator owns transitions.** `daemon` runs a single coordinator
  task that applies every `Transition`; I/O runs asynchronously and only
  returns versioned results to the coordinator (spec §9).
- **Errors**: `thiserror` enums at module boundaries, with variants
  mirroring the domain's refusal reasons; `anyhow` only in `main`/CLI glue.
  No `unwrap`/`expect` outside tests — lint-enforced.
- **Trait discipline**: no single-implementation traits. Inject test doubles
  as values and function parameters; a trait earns its existence at the
  second real implementation or an unavoidable seam (socket, database,
  transcript source).
- **`#[non_exhaustive]`** on config and event enums.
- **Compiler-enforced module boundaries**: `pub(crate)` /
  `pub(in crate::…)`; the binary reaches the core only through its public
  API.
- **No CLI-parsing crate** (spec §9): the subcommands `daemon`,
  `check-config`, `qualify` and `relay` (the per-session stdio transport,
  ADR-0004) are hand-rolled.

## Testing

- Unit tests next to the code (`#[cfg(test)]`); integration tests under each
  member's `tests/`. Helpers live in `tests/support/`, fixtures in
  `tests/fixtures/`, proptest strategies in `strategies/` or
  `*_strategies.rs` — all conditional-protected.
- Core logic: drive lifecycle, delivery and recovery with constructed values
  and property tests. Time is an input parameter, so tests use fake
  sequences — no `sleep()`, no wall clock.
- A test must kill mutants, not merely exist: a same-name test that asserts
  nothing is a known cheat, caught by the mutation gates above — not by the
  diff gate.

## Repo map

- `governor-core/` — pure domain crate (spec §9): `task` (validation,
  rendering, the envelope), `identity`, `routing`, `lifecycle` (the
  transition function, deadlines, settlement), `delivery` (outbox
  eligibility, mailbox and hint rules), `recovery`, `acceptance`, `config`.
- `herdr-governor/` — the daemon binary (spec §9): `store` (SQLite; public
  API `apply(Transition)` + reads; `store/transitions` is the only
  lifecycle writer), `adapters::{herdr, jev, transcript, git, config}`,
  `mcp`, `daemon` (one coordinator task owns every transition).
- `tests/fixtures/` — the pinned `herdr api` schema fixture (protected).
- `scripts/` — gate scripts and policy data (protected).
- `.github/` — workflows, CODEOWNERS, required-checks list, dependabot
  (protected).
- `.claude/`, `.devin/`, `.codex/`, `.pi/`, `.githooks/` — harness configs
  and the repo git hooks (protected).
- `docs/spec/` spec and `docs/adr/` ADRs — agent-editable (report mode);
  every change is listed by the gate and owner-reviewed via CODEOWNERS.
  Changing an accepted decision needs the owner's approval, stated in the
  report.
- `docs/research/` — evidence reports; `docs/reviews/` — review records
  (report mode); `docs/guardrails.md` — the gate guide (protected).
- `justfile`, `Cargo.toml`, `Cargo.lock`, `clippy.toml`, `rustfmt.toml`,
  `deny.toml`, `rust-toolchain.toml`, `tombi.toml`, `_typos.toml`,
  `.config/nextest.toml`, `.cargo/mutants.toml` — toolchain and gate
  configs (protected).

## When in doubt

Ask the owner; don't guess. The escape hatch for every rule above is "say so
in the report" — an honest escalation is always accepted; a quiet workaround
never is.

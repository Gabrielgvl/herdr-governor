---
status: accepted
date: 2026-09-27
---

# Clean-break rewrite: a harness-agnostic Rust governor over Herdr's socket API

herdr-tools grew to about 40k lines of TypeScript under 38 ADRs. About 5.5k of those lines are unreachable in production but still tested. The 100% coverage figure is held up by 154 `c8 ignore` pragmas. 54% of runs never leave `awaiting_handoff`. Each caller pane runs its own MCP host process, 31 of them holding about 1.6 GB of RAM. We are replacing it with a new public repo, `herdr-governor`, rather than refactoring in place:

- **Language and gates.** All Rust, adopted under the anti-slop-repo kit from its first commit. The kit's only validated pack is Rust.
- **Structure.** A pure domain core with no I/O, plus adapters around it.
- **Serving.** One daemon serves its own MCP tools.
- **Herdr.** Reached only through its socket API.
- **State.** One SQLite database.
- **Harnesses.** The governor carries no harness knowledge beyond catalog data (see [ADR-0002](0002-transcript-parsers-are-the-only-per-harness-code.md)).
- **Jev.** Judges Tasks, Runs and Handoffs, never models.

The break is deliberately clean. The tool contract and the on-disk state are not preserved. Old `.herdr/` logs stay as archived files that nothing reads. Cutover runs the new daemon in parallel as a second Executor integration, then swaps it in once the conformance suite passes.

## Considered Options

- **Refactor the TypeScript in place.** Rejected. The kit's TypeScript pack is an empty directory, and nothing at runtime still needs TypeScript: Pi registers nothing, and every host reaches the tools through Executor.
- **Strangler rewrite in the same repo.** Rejected. Autoupdate deploys `main` every two minutes, and one gate would have to judge TypeScript and Rust together.
- **Keep per-harness adapters, Herdr CLI subprocesses and JSON-file state.** Rejected. Herdr already ships integrations for more than 20 harnesses and a typed socket API. The file state needed six hand-rolled atomic-write routines and three lock mechanisms.

## Consequences

- History and blame are split across two repos. The new spec cites old ADRs by number.
- The Jev calibration kit keeps reading the archived `decisions.jsonl` files. New calibration reads the governor's outcome log.
- A Herdr behaviour the governor needs but Herdr lacks is brought to the owner case by case. It is never patched with harness code.

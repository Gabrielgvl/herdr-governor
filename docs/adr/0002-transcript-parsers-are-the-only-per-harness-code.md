---
status: accepted
date: 2026-09-27
---

# Transcript parsers are the only per-harness code

The governor is harness-agnostic. Herdr's integrations own starting, readiness, prompt delivery and status, and everything else about a harness is catalog data. Supervision evidence is the one exception, for two reasons:
- **Herdr serves no transcript content.** It reports each agent's session identity: an id for Claude and Devin, a file path for Pi.
- **Terminal output alone is weak evidence.** In the old runtime, 31% of supervision verdicts came back `unknown`, and Claude had no transcript reader at all.

So the transcript adapter contains one parser per harness with real Runs: Devin's ATIF JSON, Claude's JSONL and Pi's JSONL. Each parser turns its format into one normalized evidence stream behind one interface. No other module may branch on harness kind.

We are also opening an upstream discussion with Herdr about serving transcripts. Once Herdr serves them, the parsers are deleted.

## Considered Options

- **Terminal output only.** The simplest option, but supervision quality would rest on whatever happens to be on screen.
- **A per-harness file locator as catalog data, with the raw tail handed to Jev unparsed.** No parser code. The owner chose structured evidence over it.

## Consequences

- A harness release that changes its transcript format breaks only that harness's parser. The parser reports the source as unreadable, and supervision falls back to terminal output for that Run.
- A harness with no parser also gets terminal-only supervision until one is added.

---
status: accepted
date: 2026-09-30
---

# Native per-session stdio relay transport

Draft 2 assumed callers would reach the daemon's MCP tools through Executor: a Streamable HTTP source with a bearer token, with a single Executor-spawned stdio relay as the fallback. Phase 2 measured that path on the real gateway ([contract-discovery §A1](../research/contract-discovery.md), kept as history): registration is declarative, a static bearer rides every request, reconnects re-initialize transparently, and a down server surfaces an untyped `Internal tool error`. The path works, but it costs a loopback network listener, a bearer credential, write-once registration semantics, a 401-triggered OAuth discovery leg, and an extra hop between caller and daemon — none of which buys anything a caller's own harness cannot already provide.

The A1′ probe then measured what the harnesses themselves do with a configured stdio MCP server (pi 0.99.1, claude 2.1.285, devin 3000.11.3 — evidence below): all three spawn it **once per session at session start**, pass `HERDR_*` and `PWD` through with the invocation cwd, keep it until session end, and respawn it transparently on the next call after a crash.

The owner ruled (2026-09-29/30) that the caller transport is a **native per-session stdio relay**:

1. **Transport.** Each caller harness's native MCP spawns `herdr-governor relay` over stdio, one process per session. The relay is stateless and forwards to the single daemon over a 0600 unix socket. There is no Executor path, no HTTP listener and no bearer.
2. **Caller identity is derived, not sent.** The relay derives `caller` itself: `paneId` from the inherited `HERDR_PANE_ID` environment variable, and `projectRoot` as realpath(`git rev-parse --show-toplevel`) of the relay's cwd, falling back to realpath(cwd) outside a git worktree. `caller` leaves the tool schemas; F1 still verifies it against a fresh `session.snapshot`.
3. **N4 amended.** One stateless relay per caller session is allowed within an RSS budget of at most 8 MB per relay (measured 2 MB). The daemon stays at or under 176 MB. A relay holds no state or caches.
4. **Registration is global per harness**, through the herdr-tools profile layer: pi via `~/.pi/agent/mcp.json` with `exposure: direct`; claude via the herdr-tools plugin `.mcp.json`; devin via `~/.config/devin/mcp_config.json` plus `permissions.allow`. Lane allowlists add the governor's `mcp__governor__*` tools.
5. **Relay behaviour.** It exits on stdin EOF or SIGINT/SIGTERM; never logs environment values; reads only `HERDR_*` and its cwd; holds no upstream session state and reconnects to the daemon socket per request — a live herdr-tools proxy bug showed why: a proxy that connects its upstream once loses every call after an upstream restart. Staying stateless is also what makes a harness's crash-respawn safe.

## Considered Options

- **Executor Streamable HTTP + bearer (the original A1).** Measured working — declarative registration, bearer on every request, transparent reconnect and stale-session re-initialization. Rejected: it keeps a network listener (loopback, but still an auth and `Origin` surface), a bearer credential to store and rotate, write-once registration, a 401-triggered OAuth discovery leg, and an extra hop whose failures reach the caller untyped.
- **Native HTTP per harness.** Each harness's own MCP client can also speak Streamable HTTP with static headers (measured on pi 0.99). Rejected: it keeps the listener and the bearer and adds per-harness header configuration, while buying nothing the stdio spawn does not already provide.
- **One shared relay for all sessions.** Rejected: caller identity is derived from the spawned process's own environment and cwd, which only exist per session — a shared relay would have to trust a caller-supplied identity field again, and it would become a second stateful hop to supervise.

## Consequences

- This consciously reverses the letter of the old per-caller-process lesson while keeping its content. herdr-tools ran 31 per-caller MCP host processes holding about 1.6 GB — roughly 52 MB each — and §1 is explicit that the cost came from the serving topology: one fat stateful host carrying the whole tool surface, multiplied by every caller. This ADR keeps one process per caller session but makes it a 463 KiB zero-dependency stateless forwarder at 2 MB RSS, so the same 31 sessions cost about 62 MB instead of 1.6 GB — and a crashed one is simply respawned by the harness. The lesson is restated as a per-relay budget: at most 8 MB each.
- `caller` is no longer a tool argument, so the F5–F7 schemas shrink by one field a caller could get wrong. The daemon-side check does not change: F1 verifies the derived `paneId` against a fresh `session.snapshot`, and every refusal stands. A relay running outside a Herdr pane has no `HERDR_PANE_ID` to derive from and fails F1.
- The bearer token, the HTTP listener and the OAuth discovery leg disappear. Access control on the daemon endpoint is filesystem permission: a 0600 socket under the 0700 state directory. §13's transport section collapses to that.
- Registration becomes static per-harness config applied once, instead of a runtime Executor step; a harness update that changes MCP semantics is a profile-layer fix, not governor code. Per the probe, pi surfaces relay tools through codemode unless `exposure: direct`, claude ends the server with SIGINT rather than stdin EOF, and only pi honours a config `cwd` key — so `projectRoot` derivation always uses the relay's own invocation cwd.
- The daemon socket stays a single endpoint: relays reconnect per request, so a daemon restart never strands a session's calls behind a dead connection, and `DAEMON_UNAVAILABLE` (N7) remains the typed failure callers see.

## Evidence

- A1′ harness spawn probe: `/home/user/.herdr/artifacts/hflow-governor-p0p1/contract-a1prime-evidence/contract-a1prime.md` — env and cwd inheritance, one spawn per session, respawn after crash, exit on EOF (pi, devin) or SIGINT (claude), and the Rust forwarder measurement (VmRSS 2 MB, 463 KiB binary).
- [contract-discovery §A1](../research/contract-discovery.md) — the Executor catalog and pi-native measurements, kept as history.

## Open questions

- The unix socket's exact path is not ruled; the state directory (0700) is the natural home, decided at implementation.

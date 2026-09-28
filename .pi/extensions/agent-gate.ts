/**
 * Agent Gate Extension
 *
 * Routes every mutating/executing tool call through scripts/agent-gate.sh
 * (the shared harness gate: protected-path denial + forbidden-command denial,
 * plan §3). Blocks the tool call when the gate exits 2.
 *
 * Read-only tools (read, grep, find, ls) pass through ungated. A missing or
 * erroring gate script fails open — a wedged gate that blocks every tool is
 * worse than an advisory hook that misses a call; handler-code failures are
 * left to propagate so Pi's fail-safe (handler error blocks the tool) still
 * applies to real bugs.
 */

import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join } from "node:path";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const READ_ONLY = new Set(["read", "grep", "find", "ls"]);

export default function agentGate(pi: ExtensionAPI): void {
	let warned = false;

	pi.on("tool_call", async (event, ctx) => {
		if (READ_ONLY.has(event.toolName)) {
			return undefined;
		}

		const script = join(ctx.cwd, "scripts", "agent-gate.sh");
		if (!existsSync(script)) {
			return undefined;
		}

		const r = spawnSync("bash", [script], {
			input: JSON.stringify({
				hook_event_name: "PreToolUse",
				tool_name: event.toolName,
				tool_input: event.input,
			}),
			encoding: "utf8",
			cwd: ctx.cwd,
			timeout: 10_000,
		});

		if (r.status === 2) {
			let reason = r.stdout.trim() || "blocked by agent-gate";
			try {
				const out = JSON.parse(r.stdout) as {
					hookSpecificOutput?: { permissionDecisionReason?: string };
				};
				reason = out.hookSpecificOutput?.permissionDecisionReason ?? reason;
			} catch {
				// Non-JSON deny output: surface it verbatim as the reason.
			}
			return { block: true, reason };
		}

		if (r.status !== 0 && !warned && ctx.hasUI) {
			warned = true;
			const detail = r.error?.message ?? `exit ${r.status ?? "unknown"}`;
			ctx.ui.notify(`agent-gate.sh failed open: ${detail}`, "warning");
		}
		return undefined;
	});
}

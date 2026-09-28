---
status: accepted
date: 2026-09-27
---

# Recovery waits for proof the predecessor stopped

When Jev judges a Run blocked by a provider's usage limit, the governor settles it `provider_limited` and puts every operating point of that provider into cooldown. It records a recovery obligation, but it does not start the successor yet.

The successor is dispatched only once a fresh Herdr snapshot shows that the predecessor's identity is gone, meaning its pane exited or closed. Cancelling the Run with `closePane` produces that state.

Several signals look like a stop but don't prove one:
- being blocked;
- a 429;
- a Jev probability;
- silence.

Some harnesses retry a rate limit on their own, so starting a successor on those signals can leave two agents editing or deploying the same Task.

Panes are still never closed automatically. The mailbox event tells the caller that closing the pane triggers the recovery.

## Considered Options

- **Recover immediately.** Full automation, with a two-writer risk on the very Tasks that matter most.
- **Have the governor close the limited pane itself.** Also automatic, but a false-positive judgment would kill a working agent.

//! `transcript` — the per-harness session-transcript adapter (ADR-0002):
//! pointer resolution and deterministic tail-window reads over the
//! pinned source. Harness literals are legal only under `transcript/`
//! (I9), so this root carries `mod`/`pub use` declarations and no
//! harness-aware prose.

mod claude_jsonl;
mod devin_atif;
mod error;
mod pi_jsonl;
mod pointer;
mod window;

#[cfg(test)]
mod tests;

pub use error::TranscriptError;
pub use pointer::{ResolvedSource, SessionPointer, TranscriptRoots, resolve};
pub use window::{Cursor, EventKind, TranscriptEvent, Window, read_window};

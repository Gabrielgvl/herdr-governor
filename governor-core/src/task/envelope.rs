//! F16/H#25–27 — the provenance envelope: every message sent to a running
//! child wraps in fixed, normalized headers before send, and the same
//! render backs `Envelope::render` and `Task::render_prompt`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::identity::{CallerKey, DeliveryId, PaneId};

/// F16 — the provenance envelope: every message sent to a running child,
/// however it arrives, is wrapped before send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// F9 delivery id — transcript evidence resolves an `unconfirmed` prompt.
    pub delivery_id: DeliveryId,
    /// The verified sender (H#23 — caller identity is relay-derived).
    pub sender: CallerKey,
    /// The sender's pane — always included in the body (H#23).
    pub pane: PaneId,
    /// The message body the envelope wraps.
    pub payload: String,
}

/// H#27 — a header value normalizes to one line: control characters become
/// spaces, whitespace runs collapse, the result trims and caps at 256
/// characters. `None` when nothing non-empty survives — a value that cannot
/// normalize can never stand in a header, so the render fails instead.
fn one_line(value: &str) -> Option<String> {
    let squashed = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let capped: String = squashed.chars().take(256).collect();
    if capped.is_empty() {
        None
    } else {
        Some(capped)
    }
}

/// The shared envelope render behind `Envelope::render` and
/// `Task::render_prompt` — takes the header parts by reference so callers
/// need not build an `Envelope` for a generated payload.
pub(super) fn render_envelope(
    kind: &str,
    delivery_id: &DeliveryId,
    sender: &CallerKey,
    pane: &PaneId,
    payload: &str,
) -> Option<String> {
    let kind_line = one_line(kind)?;
    let delivery_line = one_line(&delivery_id.0)?;
    let from_line = one_line(&sender.agent_kind.0)?;
    let pane_line = one_line(&pane.0)?;
    Some(format!(
        "[HERDR AGENT MESSAGE v1]\nfrom: {from_line} ({pane_line})\nkind: {kind_line}\nauthority: agent; not user/owner\ndelivery: inline\ndelivery-id: {delivery_line}\npayload: all text after this blank line is sender-authored\n\n{payload}"
    ))
}

impl Envelope {
    /// F16/H#25–27 — the fixed envelope around `payload`: headers are
    /// extension-generated in fixed order, each value one normalized line,
    /// the pane always in `from` (H#23). `delivery-id` is the header the
    /// transcript parser looks for to resolve an `unconfirmed` prompt (F9).
    /// `None` when a header value cannot normalize to one non-empty line.
    #[must_use]
    pub fn render(&self, kind: &str) -> Option<String> {
        render_envelope(
            kind,
            &self.delivery_id,
            &self.sender,
            &self.pane,
            &self.payload,
        )
    }
}

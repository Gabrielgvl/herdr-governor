//! `runner/seam` — the kill-seam checkpoints (§4.4): suffix matching over
//! the effect key's stable suffix and the one `seam hit` stderr marker
//! before the action. `daemon/seam.rs` owns the `SeamConfig` value and the
//! env/argv parsing; this module is the dispatch-side consumer.

use governor_core::identity::EffectKey;

use crate::daemon::seam::{Boundary, SeamAction, SeamConfig};

/// The matchable part of an effect key: everything after the
/// `run:<runId>:` / `launch:<launchId>:` subject prefix — ids are minted at
/// run time, so seam specs name the stable suffix (`prompt:task`,
/// `start:*`, `outbox:1`). Keys without a subject prefix (`event:<id>:hint`)
/// match in full.
#[must_use]
pub(in crate::daemon) fn suffix_of(key: &EffectKey) -> &str {
    for subject in ["run:", "launch:"] {
        if let Some(rest) = key.0.strip_prefix(subject)
            && let Some((_, suffix)) = rest.split_once(':')
        {
            return suffix;
        }
    }
    &key.0
}

/// `suffix` against the key suffix — a trailing `*` is the only wildcard
/// (`start:*` matches `start:0`, `start:1`, …).
#[must_use]
fn matches(spec_suffix: &str, key_suffix: &str) -> bool {
    match spec_suffix.strip_suffix('*') {
        Some(prefix) => key_suffix.starts_with(prefix),
        None => spec_suffix == key_suffix,
    }
}

/// Whether this (key, boundary) hits the armed seam.
#[must_use]
pub(in crate::daemon) fn hits(key: &EffectKey, boundary: Boundary, seam: &SeamConfig) -> bool {
    seam.boundary == boundary && matches(&seam.suffix, suffix_of(key))
}

/// One seam checkpoint: on a hit, write the `seam hit <suffix>@<boundary>`
/// stderr marker *first* (a child process must mark the boundary before
/// `abort` can lose it), then perform the action — `abort` never returns.
/// An await between the marker and the action is deliberate: `pause` is
/// the F16/F29 interleave window.
pub(in crate::daemon) async fn checkpoint(
    key: &EffectKey,
    boundary: Boundary,
    seam: Option<&SeamConfig>,
) {
    let Some(armed) = seam else { return };
    if !hits(key, boundary, armed) {
        return;
    }
    {
        use std::io::Write as _;
        let _unused = writeln!(
            std::io::stderr().lock(),
            "seam hit {}@{}",
            armed.suffix,
            boundary.as_str()
        );
    }
    match armed.action {
        SeamAction::Abort => std::process::abort(),
        SeamAction::Pause(duration) => tokio::time::sleep(duration).await,
    }
}

#[cfg(test)]
mod tests {
    use governor_core::identity::EffectKey;

    use super::{matches, suffix_of};

    /// Suffix extraction: the run/launch subject prefix strips; a bare
    /// `event:` key is its own suffix; a key that only begins `run:` with
    /// no further `:` keeps its whole text.
    #[test]
    fn seam_matches_key_suffix_and_wildcard() {
        let key = |text: &str| EffectKey(text.to_string());
        assert_eq!(suffix_of(&key("run:r-1:prompt:task")), "prompt:task");
        assert_eq!(suffix_of(&key("launch:l-9:evaluate")), "evaluate");
        assert_eq!(suffix_of(&key("run:r-1:start:2")), "start:2");
        assert_eq!(suffix_of(&key("event:ev-1:hint")), "event:ev-1:hint");
        assert_eq!(suffix_of(&key("run:nocolon")), "run:nocolon");

        assert!(matches("prompt:task", "prompt:task"));
        assert!(matches("start:*", "start:0"));
        assert!(matches("start:*", "start:11"));
        assert!(matches("*", "anything"));
        assert!(!matches("start:*", "start"));
        assert!(!matches("prompt:task", "prompt:taskk"));
        assert!(!matches("outbox:1", "outbox:12"));
        assert!(!matches("start:0", "start:1"));
    }
}

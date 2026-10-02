//! Herdr contract-fixture tests moved out of `src/adapters/herdr/tests/`:
//! a member's `src/` must compile against its own tree alone (the
//! guard-selftest mirrors each member's `src/` plus the lockfile), so
//! fixtures under the workspace `tests/fixtures/contract/` are read here
//! at run time. The a2 trace's envelope classification lives here; the
//! public-client decode of every recorded inbound frame is H2's
//! `contract_fake_herdr_fixture_replay`, and the literal codec round-trip
//! stays in `src` (`codec_encode_decode_literals`, no fixture).

#[cfg(test)]
mod tests {
    use herdr_governor::adapters::herdr::{AgentStatus, SessionSnapshot};
    use serde_json::Value;

    /// A workspace contract fixture, read at run time.
    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../tests/fixtures/contract")
                .join(name),
        )
        .expect("contract fixture")
    }

    /// The a2 tools-daemon trace's wire envelopes, classified by shape: an
    /// outbound frame is a `{id, method, params}` request, a malformed
    /// request (method without object params) or the `{"type":…}`
    /// handshake; an inbound frame is a result (`id` + `result`), a server
    /// error (`id` + `error`) or malformed for protocol 22 (no `id`);
    /// non-JSON probes (over-limit frames and their truncated heads) are
    /// counted apart. Owner ruling: the codec stays `pub(crate)`, so the
    /// decode of each inbound frame is proven through the public client by
    /// `contract_fake_herdr_fixture_replay`.
    #[test]
    fn codec_roundtrips_fixture_envelopes() {
        let trace = fixture("a2-tools-daemon-trace.jsonl");
        let (mut requests, mut handshake, mut probe) = (0u32, 0u32, 0u32);
        let (mut results, mut errors, mut malformed) = (0u32, 0u32, 0u32);
        let mut frames = 0u32;
        for line in trace.lines() {
            let rec: Value = serde_json::from_str(line).expect("trace record");
            for key in ["tx", "rx"] {
                let Some(frame) = rec.get(key).and_then(Value::as_str) else {
                    continue;
                };
                frames = frames.saturating_add(1);
                let Ok(raw) = serde_json::from_str::<Value>(frame.trim_end_matches('\n')) else {
                    probe = probe.saturating_add(1);
                    continue;
                };
                if key == "tx" {
                    if raw.get("method").is_some_and(Value::is_string) {
                        assert!(
                            raw.get("id").is_some_and(Value::is_string),
                            "tx id: {frame}"
                        );
                        if raw.get("params").is_some_and(Value::is_object) {
                            requests = requests.saturating_add(1);
                        } else {
                            malformed = malformed.saturating_add(1);
                        }
                    } else {
                        assert!(
                            raw.get("type").is_some_and(Value::is_string),
                            "handshake: {frame}"
                        );
                        handshake = handshake.saturating_add(1);
                    }
                } else if raw.get("id").is_none() {
                    malformed = malformed.saturating_add(1);
                } else if raw.get("error").is_some() {
                    errors = errors.saturating_add(1);
                } else {
                    assert!(raw.get("result").is_some(), "rx result: {frame}");
                    results = results.saturating_add(1);
                }
            }
        }
        assert!(frames > 20, "fixture supplies frames");
        assert!(
            results > 5
                && errors >= 2
                && malformed > 5
                && requests > 5
                && handshake > 0
                && probe > 0,
            "{requests}/{handshake}/{probe}/{results}/{errors}/{malformed}"
        );
    }

    /// The a46 capture's real snapshot decodes — every `required` member the
    /// fixture schema pins — and `agent_rows` carries the occupant fields the
    /// identity functions consume (no harness names asserted: structure only).
    #[test]
    fn snapshot_decodes_a46_capture() {
        let a46: Value =
            serde_json::from_str(&fixture("a46-identity-evidence.json")).expect("a46 fixture");
        let snapshot: SessionSnapshot =
            serde_json::from_value(a46["captures"]["initial-shell"]["snapshot"].clone())
                .expect("snapshot decodes");
        assert_eq!(snapshot.protocol, 22);
        assert_eq!(snapshot.panes[0].pane_id, "w1:p1");
        assert_eq!(snapshot.panes[0].terminal_id, "term_65c9180ad9c2f1");
        assert!(snapshot.agents.is_empty());
        assert!(snapshot.agent_rows().is_empty(), "no agents → no rows");

        // `first-native-ready` carries an agent row with no `agent_session`
        // (evidence: the session arrives after readiness) — the row must not
        // invent one.
        let ready: SessionSnapshot =
            serde_json::from_value(a46["captures"]["first-native-ready"]["snapshot"].clone())
                .expect("ready snapshot decodes");
        let rows = ready.agent_rows();
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.pane_id, "w1:p1");
        assert_eq!(row.terminal_id, "term_65c9180ad9c2f1");
        assert!(row.agent.is_some(), "kind present");
        assert!(row.name.is_some(), "name present on agent surface");
        assert!(
            row.native_session.is_none(),
            "session absent before discovery"
        );
        assert_eq!(row.status, Some(AgentStatus::Idle));

        // `warmup-killed` is the first capture carrying a session — a
        // `kind:"path"` native handle.
        let warmed: SessionSnapshot =
            serde_json::from_value(a46["captures"]["warmup-killed"]["snapshot"].clone())
                .expect("warmed snapshot decodes");
        assert!(warmed.agent_rows()[0].native_session.is_some());
    }
}

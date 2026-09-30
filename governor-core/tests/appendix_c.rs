//! Phase 3 DoD proof for Appendix C (spec §10): the spec's transition-rule
//! table is generated from `lifecycle::TRANSITION_RULES` and matches it byte
//! for byte. Row agreement lives inside the crate as a traceability matrix —
//! `governor-core/src/lifecycle/tests/appendix_c.rs` names the unit test that
//! proves each row.
#![expect(
    clippy::disallowed_methods,
    reason = "test fixture I/O: reads the spec markdown to prove the generated table matches"
)]

#[cfg(test)]
mod tests {
    use std::fs;

    use governor_core::lifecycle::TRANSITION_RULES;

    /// The spec path, baked at compile time — a deterministic fixture read,
    /// not a runtime environment dependence.
    const SPEC_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs/spec/herdr-governor-spec.md"
    );

    /// The rendering the spec's Appendix C table must equal — generated from
    /// the code's transition list, never the other way round.
    fn render_transition_rules() -> String {
        let rows = TRANSITION_RULES
            .iter()
            .map(|(state, event, outcome)| format!("| `{state}` | `{event}` | {outcome} |"))
            .collect::<Vec<String>>()
            .join("\n");
        format!("| State | Event | Outcome |\n|---|---|---|\n{rows}\n")
    }

    /// The `|`-lines inside the spec's Appendix C section — the table's
    /// embedded copy, to be proven equal to the generated one.
    fn spec_rules_table(spec: &str) -> String {
        let mut in_appendix = false;
        let mut table = String::new();
        for line in spec.lines() {
            if line.starts_with("## ") {
                in_appendix = line.contains("Appendix C");
            }
            if in_appendix && line.starts_with('|') {
                table.push_str(line);
                table.push('\n');
            }
        }
        table
    }

    /// Phase 3 DoD — Appendix C's table is generated from the code's
    /// transition list and matches the spec.
    #[test]
    fn appendix_c_table_matches_transition_rules() {
        let spec = match fs::read_to_string(SPEC_PATH) {
            Ok(spec) => spec,
            Err(error) => panic!("the spec must be readable at {SPEC_PATH}: {error}"),
        };
        let embedded = spec_rules_table(&spec);
        assert_eq!(
            embedded,
            render_transition_rules(),
            "Appendix C's table must be generated from lifecycle::TRANSITION_RULES"
        );
    }
}

//! The generated matrix: `store_probe count` reports each scenario's real
//! statement-boundary count; it must equal the declared table, so a
//! writer that grows or loses a statement fails here (escalate the drift,
//! not the failure — plan §P4.S4).

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use crate::support::crash::{SCENARIOS, measured};

    #[test]
    fn crash_matrix_pins_every_boundary_count() {
        let mut report = String::from("scenario | declared | measured\n");
        let mut drift = Vec::new();
        let mut total = 0;
        for (scenario, declared) in SCENARIOS {
            let n = measured(scenario);
            total += n;
            writeln!(report, "{scenario} | {declared} | {n}").expect("report line");
            if n != declared {
                drift.push(format!("{scenario}: declared {declared}, measured {n}"));
            }
        }
        writeln!(report, "total boundaries | {total}").expect("report line");
        println!("{report}");
        assert!(
            drift.is_empty(),
            "statement-count drift:\n{}",
            drift.join("\n")
        );
        let declared_total: usize = SCENARIOS.iter().map(|(_, n)| n).sum();
        assert_eq!(total, declared_total, "the matrix total is pinned");
    }
}

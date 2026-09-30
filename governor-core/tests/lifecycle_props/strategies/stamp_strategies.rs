//! Version-stamp generators: `StampSpec`/`TripleField` describe how an
//! event's stamp relates to the Run it is delivered to, `stamp` resolves a
//! spec against the Run, `stamped` wraps an event for `transition`, and
//! `triple_of` reads the Run's current stamp — the F20 staleness lane.

use governor_core::lifecycle::{Event, Run, VersionTriple, Versioned};
use proptest::prelude::{Just, Strategy, prop_oneof};

use super::common_strategies::pick;

/// F20 — the version stamp a Run currently reads as holding.
#[must_use]
pub fn triple_of(run: &Run) -> VersionTriple {
    VersionTriple {
        version: run.version,
        work_generation: run.work_generation,
        evidence_generation: run.evidence_generation,
    }
}

/// Which field of the version triple a stale stamp corrupts.
#[derive(Debug, Clone, Copy)]
pub enum TripleField {
    /// `version` — the row version.
    Version,
    /// `work_generation`.
    WorkGeneration,
    /// `evidence_generation`.
    EvidenceGeneration,
}

/// How an event's stamp relates to the Run it is delivered to: `Fresh` is
/// stamped at delivery; `Perturbed`/`Arbitrary` model an async result
/// requested against versions that no longer hold.
#[derive(Debug, Clone, Copy)]
pub enum StampSpec {
    /// Stamp with the Run's triple at delivery time.
    Fresh,
    /// Stamp with the Run's triple plus a delta on one field.
    Perturbed(TripleField, u64),
    /// Stamp with an unrelated triple.
    Arbitrary(VersionTriple),
}

pub(crate) fn arb_version_triple() -> impl Strategy<Value = VersionTriple> {
    (0_u64..64, 0_u64..8, 0_u64..8).prop_map(|(version, work_generation, evidence_generation)| {
        VersionTriple {
            version,
            work_generation,
            evidence_generation,
        }
    })
}

pub(crate) fn arb_stamp_spec() -> impl Strategy<Value = StampSpec> {
    prop_oneof![
        6 => Just(StampSpec::Fresh),
        2 => (
            pick(&[
                TripleField::Version,
                TripleField::WorkGeneration,
                TripleField::EvidenceGeneration,
            ]),
            1_u64..=u64::MAX,
        )
            .prop_map(|(field, delta)| StampSpec::Perturbed(field, delta)),
        2 => arb_version_triple().prop_map(StampSpec::Arbitrary),
    ]
}

/// A never-fresh stamp — for the F20 staleness property.
pub fn arb_stale_spec() -> impl Strategy<Value = StampSpec> {
    arb_stamp_spec().prop_filter("never fresh", |spec| !matches!(spec, StampSpec::Fresh))
}

/// Resolve a `StampSpec` against the Run it is delivered to.
#[must_use]
pub fn stamp(run: &Run, spec: StampSpec) -> VersionTriple {
    let current = triple_of(run);
    match spec {
        StampSpec::Fresh => current,
        StampSpec::Arbitrary(triple) => triple,
        StampSpec::Perturbed(field, delta) => match field {
            TripleField::Version => VersionTriple {
                version: current.version.saturating_add(delta),
                ..current
            },
            TripleField::WorkGeneration => VersionTriple {
                work_generation: current.work_generation.saturating_add(delta),
                ..current
            },
            TripleField::EvidenceGeneration => VersionTriple {
                evidence_generation: current.evidence_generation.saturating_add(delta),
                ..current
            },
        },
    }
}

/// Stamp `event` and wrap it for the transition function.
#[must_use]
pub fn stamped(run: &Run, event: Event, spec: StampSpec) -> Versioned<Event> {
    Versioned {
        requested_against: stamp(run, spec),
        value: event,
    }
}

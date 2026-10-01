//! `transitions` — the only lifecycle-write site in the repo (I10, spec §9):
//! one child module per Appendix-B transaction family, dispatched by
//! `store::apply` inside a single transaction. Filled by P4.S3.

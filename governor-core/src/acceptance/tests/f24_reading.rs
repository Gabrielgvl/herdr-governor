//! F24 — the handoff reading: the bound and marker spellings, and every
//! refusal path that counts as "not written yet".

use alloc::vec;
use alloc::vec::Vec;

use proptest::collection::vec as prop_vec;
use proptest::prelude::{any, prop_assert, prop_assert_eq, proptest};
use proptest::sample::select;

use super::builders::{byte_len, handoff_body, marker, run, sha256};
use crate::acceptance::{
    HANDOFF_MARKER_PREFIX, HANDOFF_MARKER_SUFFIX, HANDOFF_MAX_BYTES, HandoffReading, read_handoff,
};
use crate::identity::RunId;

#[test]
fn f24_n5_handoff_bound_and_marker() {
    assert_eq!(
        HANDOFF_MAX_BYTES, 262_144,
        "handoff bound is 256 KiB (N5/F24)"
    );
    assert_eq!(
        HANDOFF_MARKER_PREFIX, "<!-- herdr-governor handoff run=",
        "marker prefix is the F24 opening"
    );
    assert_eq!(
        HANDOFF_MARKER_SUFFIX, " -->",
        "marker suffix is the F24 closing"
    );
}

#[test]
fn f24_reading_valid_marked_regular_file() {
    let run = run();
    let bytes = handoff_body(&run);
    match read_handoff(&run, true, byte_len(&bytes), Some(&bytes)) {
        HandoffReading::Valid { digest } => {
            // The digest covers the bytes as read — including the trailing
            // newline — because those are the bytes that freeze (F24).
            assert_eq!(
                digest,
                sha256(&bytes),
                "the frozen digest covers the raw bytes"
            );
        }
        HandoffReading::NotWritten => panic!("a regular marked file must read Valid"),
    }
}

#[test]
fn f24_reading_nonregular_or_unreadable_is_not_written() {
    let run = run();
    let bytes = handoff_body(&run);
    // The read never follows symlinks, so `regular_file=false` covers a
    // symlink, a directory, any other node and a failed stat.
    assert_eq!(
        read_handoff(&run, false, byte_len(&bytes), Some(&bytes)),
        HandoffReading::NotWritten,
        "a non-regular file counts as not written yet"
    );
    assert_eq!(
        read_handoff(&run, true, byte_len(&bytes), None),
        HandoffReading::NotWritten,
        "a failed content read counts as not written yet"
    );
    assert_eq!(
        read_handoff(&run, false, 0, None),
        HandoffReading::NotWritten,
        "a missing path counts as not written yet"
    );
}

#[test]
fn f24_reading_overbound_file_is_not_written() {
    let run = run();
    let mut bytes = handoff_body(&run);
    bytes.resize(HANDOFF_MAX_BYTES + 1, b' ');
    let size = byte_len(&bytes);
    assert_eq!(
        read_handoff(&run, true, size, Some(&bytes)),
        HandoffReading::NotWritten,
        "a file over 256 KiB counts as not written yet (N5)"
    );
    // The metadata bound alone decides — no content read is owed.
    assert_eq!(
        read_handoff(&run, true, size, None),
        HandoffReading::NotWritten,
        "an over-bound file refuses on metadata alone"
    );
}

#[test]
fn f24_reading_at_the_bound_reads_valid() {
    let run = run();
    let marker = marker(&run);
    let mut bytes = vec![b'.'; HANDOFF_MAX_BYTES - marker.len()];
    bytes.extend_from_slice(marker.as_bytes());
    assert_eq!(
        bytes.len(),
        HANDOFF_MAX_BYTES,
        "the test file sits exactly on the bound"
    );
    match read_handoff(&run, true, byte_len(&bytes), Some(&bytes)) {
        HandoffReading::Valid { digest } => {
            assert_eq!(digest, sha256(&bytes), "at 256 KiB the file still reads");
        }
        HandoffReading::NotWritten => panic!("256 KiB exactly is within the bound (N5)"),
    }
}

#[test]
fn f24_reading_size_and_bytes_must_agree() {
    let run = run();
    let bytes = handoff_body(&run);
    assert_eq!(
        read_handoff(&run, true, byte_len(&bytes) + 1, Some(&bytes)),
        HandoffReading::NotWritten,
        "a stat/content mismatch is an untrusted read — not written yet"
    );
}

#[test]
fn f24_reading_marker_must_be_the_final_content() {
    let run = run();
    // Another Run's marker is not this Run's handoff.
    let other = handoff_body(&RunId("run-ffffffff".into()));
    assert_eq!(
        read_handoff(&run, true, byte_len(&other), Some(&other)),
        HandoffReading::NotWritten,
        "a marker naming another run is not written yet"
    );
    // Content after the marker means it is not the file's end.
    let mut appended = handoff_body(&run);
    appended.extend_from_slice(b"\nnot the end\n");
    assert_eq!(
        read_handoff(&run, true, byte_len(&appended), Some(&appended)),
        HandoffReading::NotWritten,
        "trailing content after the marker is not written yet"
    );
    // No marker at all.
    let plain = b"all done".as_slice();
    assert_eq!(
        read_handoff(&run, true, byte_len(plain), Some(plain)),
        HandoffReading::NotWritten,
        "a markerless file is not written yet"
    );
    // Whitespace only.
    let blank = b"  \n\t ".as_slice();
    assert_eq!(
        read_handoff(&run, true, byte_len(blank), Some(blank)),
        HandoffReading::NotWritten,
        "a whitespace-only file is not written yet"
    );
}

#[test]
fn f24_reading_allows_trailing_whitespace_after_marker() {
    let run = run();
    let mut bytes = marker(&run).into_bytes();
    bytes.extend_from_slice(b" \t\r\n\x0c\n\n");
    match read_handoff(&run, true, byte_len(&bytes), Some(&bytes)) {
        HandoffReading::Valid { digest } => {
            assert_eq!(
                digest,
                sha256(&bytes),
                "trailing whitespace still reads Valid"
            );
        }
        HandoffReading::NotWritten => {
            panic!("trailing whitespace past the marker must read Valid")
        }
    }
}

proptest! {
    /// A regular in-bound file ending in its run's marker (modulo trailing
    /// whitespace) always reads Valid; a non-regular file never does.
    #[test]
    fn f24_reading_valid_iff_regular_bounded_marked(
        regular in any::<bool>(),
        head in prop_vec(any::<u8>(), 0..64),
        tail_ws in prop_vec(select(Vec::from([b' ', b'\t', b'\n', b'\r', 0x0cu8])), 0..8),
    ) {
        let run = run();
        let mut bytes = head;
        bytes.extend_from_slice(marker(&run).as_bytes());
        bytes.extend_from_slice(&tail_ws);
        match read_handoff(&run, regular, byte_len(&bytes), Some(&bytes)) {
            HandoffReading::Valid { digest } => {
                prop_assert!(regular, "only a regular file reads Valid");
                prop_assert_eq!(
                    digest,
                    sha256(&bytes),
                    "the digest covers the raw bytes"
                );
            }
            HandoffReading::NotWritten => {
                prop_assert!(!regular, "a regular marked in-bound file must read Valid");
            }
        }
    }

    /// A Valid reading is only ever produced under the three conditions —
    /// arbitrary bytes can never forge one.
    #[test]
    fn f24_reading_never_valid_without_the_conditions(
        regular in any::<bool>(),
        bytes in prop_vec(any::<u8>(), 0..256),
    ) {
        let run = run();
        if let HandoffReading::Valid { digest } =
            read_handoff(&run, regular, byte_len(&bytes), Some(&bytes))
        {
            prop_assert!(regular, "Valid requires a regular file");
            prop_assert!(
                bytes.trim_ascii_end().ends_with(marker(&run).as_bytes()),
                "Valid requires the marker as the final non-whitespace content"
            );
            prop_assert_eq!(digest, sha256(&bytes), "Valid digests the raw bytes");
        }
    }
}

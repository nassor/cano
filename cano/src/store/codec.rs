//! The tagged, [`postcard`]-encoded row payload shared by the byte-oriented stores
//! (`RedbStore`, `NatsStore`).
//!
//! A V1 payload is [`ROW_TAG_V1`] followed by `postcard(StoredRow)`.
//! A V2 payload is [`ROW_TAG_V2`] followed by a 16-byte little-endian `write_id` (`u128`)
//! and `postcard(StoredRow)`. The row's `sequence` is not part of the payload: every
//! store carries it in its key.

use crate::checkpoint::{CheckpointRow, RowKind};
use crate::error::CanoError;

/// Magic prefix stamped on V1 rows (`RedbStore`).
///
/// A legacy (untagged) row's first bytes are the postcard length-prefix of its
/// `state` string, so a single tag byte could collide with real legacy rows (e.g.
/// `0x01` = a one-character state label, which the test suites use liberally).
/// Two printable bytes make the collision vanishingly unlikely — a legacy row
/// would need a 67-character state label whose first UTF-8 byte is `V` — and even
/// then the misread is a loud decode error, never silent corruption.
pub(super) const ROW_TAG_V1: [u8; 2] = *b"CV";

/// Magic prefix stamped on V2 rows (`NatsStore`), carrying a 16-byte `write_id`
/// before `postcard(StoredRow)` so lost-ACK retries only match their own write.
pub(super) const ROW_TAG_V2: [u8; 2] = *b"CW";

/// `true` when `bytes` carries a recognized version tag ([`ROW_TAG_V1`] or [`ROW_TAG_V2`]).
#[cfg_attr(not(feature = "store-redb"), allow(dead_code))]
pub(super) fn is_tagged(bytes: &[u8]) -> bool {
    bytes.starts_with(&ROW_TAG_V1) || bytes.starts_with(&ROW_TAG_V2)
}

/// The payload half of a [`CheckpointRow`]: everything except `sequence`, which
/// is carried by the store's key. Kept private — callers only ever see `CheckpointRow`.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct StoredRow {
    pub(super) state: String,
    pub(super) task_id: String,
    pub(super) output_blob: Option<Vec<u8>>,
    pub(super) kind: RowKind,
    pub(super) workflow_version: u32,
}

fn encode_into(row: CheckpointRow, mut out: Vec<u8>) -> Result<Vec<u8>, CanoError> {
    let payload = StoredRow {
        state: row.state,
        task_id: row.task_id,
        output_blob: row.output_blob,
        kind: row.kind,
        workflow_version: row.workflow_version,
    };
    postcard::to_io(&payload, &mut out)
        .map_err(|e| CanoError::CheckpointStore(format!("encode checkpoint row: {e}")))?;
    Ok(out)
}

/// Encode `row` as `ROW_TAG_V1 ++ postcard(StoredRow)`.
#[cfg_attr(not(feature = "store-redb"), allow(dead_code))]
pub(super) fn encode(row: CheckpointRow) -> Result<Vec<u8>, CanoError> {
    encode_into(row, ROW_TAG_V1.to_vec())
}

/// Encode `row` as `ROW_TAG_V2 ++ write_id.to_le_bytes() ++ postcard(StoredRow)`.
#[cfg_attr(not(feature = "store-nats"), allow(dead_code))]
pub(super) fn encode_with_write_id(
    row: CheckpointRow,
    write_id: u128,
) -> Result<Vec<u8>, CanoError> {
    let mut out = Vec::with_capacity(ROW_TAG_V2.len() + std::mem::size_of::<u128>() + 64);
    out.extend_from_slice(&ROW_TAG_V2);
    out.extend_from_slice(&write_id.to_le_bytes());
    encode_into(row, out)
}

/// Decode a tagged payload written by [`encode`] or [`encode_with_write_id`] into the row
/// at `sequence`.
pub(super) fn decode(
    workflow_id: &str,
    sequence: u64,
    bytes: &[u8],
) -> Result<CheckpointRow, CanoError> {
    let fail = |reason: &dyn std::fmt::Display| {
        CanoError::CheckpointStore(format!(
            "decode checkpoint row at workflow_id={workflow_id:?} sequence={sequence}: {reason}"
        ))
    };
    let body = if let Some(body) = bytes.strip_prefix(&ROW_TAG_V1) {
        body
    } else if let Some(rest) = bytes.strip_prefix(&ROW_TAG_V2) {
        let (_, body) = rest
            .split_at_checked(std::mem::size_of::<u128>())
            .ok_or_else(|| fail(&"truncated write_id"))?;
        body
    } else {
        return Err(fail(&"missing row tag"));
    };
    let (stored, rest) = postcard::take_from_bytes::<StoredRow>(body).map_err(|e| fail(&e))?;
    if !rest.is_empty() {
        return Err(fail(&format!(
            "{} trailing bytes after payload",
            rest.len()
        )));
    }
    Ok(CheckpointRow {
        sequence,
        state: stored.state,
        task_id: stored.task_id,
        output_blob: stored.output_blob,
        kind: stored.kind,
        workflow_version: stored.workflow_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let row = CheckpointRow::new(7, "S", "t")
            .with_cursor(vec![1, 2, 3])
            .with_workflow_version(4);
        let bytes = encode(row.clone()).unwrap();
        assert!(bytes.starts_with(&ROW_TAG_V1));
        assert!(is_tagged(&bytes));
        assert_eq!(decode("wf", 7, &bytes).unwrap(), row);
    }

    #[test]
    fn encode_with_write_id_roundtrip_and_uniqueness() {
        let row = CheckpointRow::new(0, "Start", "t0")
            .with_output(vec![4, 5, 6])
            .with_workflow_version(9);
        let id_a = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210u128;
        let id_b = id_a + 1;
        let bytes_a = encode_with_write_id(row.clone(), id_a).unwrap();
        let bytes_b = encode_with_write_id(row.clone(), id_b).unwrap();

        assert!(bytes_a.starts_with(&ROW_TAG_V2));
        assert!(is_tagged(&bytes_a));
        assert_eq!(&bytes_a[2..18], &id_a.to_le_bytes());
        assert_ne!(
            bytes_a, bytes_b,
            "distinct write_ids must yield distinct payloads for identical rows"
        );
        assert_eq!(decode("wf", 0, &bytes_a).unwrap(), row);
        assert_eq!(decode("wf", 0, &bytes_b).unwrap(), row);
    }

    #[test]
    fn decode_rejects_an_untagged_payload() {
        let untagged = postcard::to_stdvec(&StoredRow {
            state: "S".into(),
            task_id: "t".into(),
            output_blob: None,
            kind: RowKind::StateEntry,
            workflow_version: 0,
        })
        .unwrap();
        assert!(!is_tagged(&untagged));
        let err = decode("wf", 3, &untagged).unwrap_err();
        assert_eq!(err.category(), "checkpoint_store");
        assert!(err.to_string().contains("missing row tag"), "{err}");
        assert!(err.to_string().contains("sequence=3"), "{err}");
    }

    #[test]
    fn decode_reports_a_corrupt_tagged_payload() {
        let mut corrupt = ROW_TAG_V1.to_vec();
        corrupt.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
        let err = decode("wf", 0, &corrupt).unwrap_err();
        assert!(
            err.to_string().contains("decode checkpoint row at"),
            "{err}"
        );
    }

    #[test]
    fn decode_rejects_truncated_and_trailing_byte_payloads() {
        let valid =
            encode(CheckpointRow::new(1, "StateA", "task_a").with_output(vec![9, 8, 7])).unwrap();
        for cut in 0..valid.len() {
            let err = decode("wf", 1, &valid[..cut]).unwrap_err();
            assert_eq!(err.category(), "checkpoint_store", "cut={cut}: {err}");
        }
        let mut trailing = valid;
        trailing.push(0xAA);
        let err = decode("wf", 1, &trailing).unwrap_err();
        assert_eq!(err.category(), "checkpoint_store");
        assert!(err.to_string().contains("trailing bytes"), "{err}");
    }

    #[test]
    fn decode_rejects_truncated_and_trailing_v2_payloads() {
        let valid = encode_with_write_id(
            CheckpointRow::new(2, "Wait", "task_w").with_resume_input(vec![1, 2]),
            42,
        )
        .unwrap();
        for cut in 0..valid.len() {
            let err = decode("wf", 2, &valid[..cut]).unwrap_err();
            assert_eq!(err.category(), "checkpoint_store", "cut={cut}: {err}");
            if (2..18).contains(&cut) {
                assert!(err.to_string().contains("truncated write_id"), "{err}");
            }
        }
        let mut trailing = valid;
        trailing.push(0xBB);
        let err = decode("wf", 2, &trailing).unwrap_err();
        assert_eq!(err.category(), "checkpoint_store");
        assert!(err.to_string().contains("trailing bytes"), "{err}");
    }
}

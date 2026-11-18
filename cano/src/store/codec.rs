//! The tagged, [`postcard`]-encoded row payload shared by the byte-oriented stores
//! (`RedbStore`, `NatsStore`).
//!
//! A payload is [`ROW_TAG_V1`] followed by `postcard(StoredRow)`. The row's `sequence`
//! is not part of the payload: every store carries it in its key.

use crate::checkpoint::{CheckpointRow, RowKind};
use crate::error::CanoError;

/// Magic prefix stamped on every row this version of the store writes.
///
/// A legacy (untagged) row's first bytes are the postcard length-prefix of its
/// `state` string, so a single tag byte could collide with real legacy rows (e.g.
/// `0x01` = a one-character state label, which the test suites use liberally).
/// Two printable bytes make the collision vanishingly unlikely — a legacy row
/// would need a 67-character state label whose first UTF-8 byte is `V` — and even
/// then the misread is a loud decode error, never silent corruption.
pub(super) const ROW_TAG_V1: [u8; 2] = *b"CV";

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

/// Encode `row` as `ROW_TAG_V1 ++ postcard(StoredRow)`.
pub(super) fn encode(row: CheckpointRow) -> Result<Vec<u8>, CanoError> {
    let payload = StoredRow {
        state: row.state,
        task_id: row.task_id,
        output_blob: row.output_blob,
        kind: row.kind,
        workflow_version: row.workflow_version,
    };
    let mut tagged = ROW_TAG_V1.to_vec();
    postcard::to_io(&payload, &mut tagged)
        .map_err(|e| CanoError::CheckpointStore(format!("encode checkpoint row: {e}")))?;
    Ok(tagged)
}

/// Decode a tagged payload written by [`encode`] into the row at `sequence`.
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
    let body = bytes
        .strip_prefix(&ROW_TAG_V1)
        .ok_or_else(|| fail(&"missing row tag"))?;
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
        assert_eq!(decode("wf", 7, &bytes).unwrap(), row);
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
}

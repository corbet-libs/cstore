use crate::{ContentHash, Error, Result, Snapshot, Store};

/// A destination stages a checkpoint without making partial data active.
pub trait CheckpointTarget {
    fn begin(&mut self, request_id: &str, snapshot: &Snapshot) -> Result<()>;
    fn contains_blob(&self, hash: &ContentHash) -> Result<bool>;
    fn stage_blob(&mut self, hash: &ContentHash, bytes: &[u8]) -> Result<()>;
    fn publish(&mut self, request_id: &str, snapshot: &Snapshot) -> Result<ContentHash>;
}

/// Resume a transfer of the exact retained snapshot, never a fresh live rescan.
pub fn transfer(
    source: &impl Store,
    snapshot: &Snapshot,
    target: &mut impl CheckpointTarget,
    request_id: &str,
) -> Result<ContentHash> {
    snapshot.validate()?;
    if request_id.is_empty() || request_id.len() > 256 {
        return Err(Error::Invalid(
            "transfer requires a bounded request ID".into(),
        ));
    }
    target.begin(request_id, snapshot)?;
    for record in snapshot
        .records
        .values()
        .chain(snapshot.history.iter().map(|item| &item.record))
    {
        if !target.contains_blob(&record.content)? {
            let bytes = source.blob(&record.content)?;
            record.content.verify(&bytes)?;
            if u64::try_from(bytes.len()).ok() != Some(record.size) {
                return Err(Error::Integrity("record size mismatch".into()));
            }
            target.stage_blob(&record.content, &bytes)?;
        }
    }
    target.publish(request_id, snapshot)
}

use crate::Key;
use crate::{ContentHash, Error, Result, Snapshot, Store};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Difference {
    LocalOnly,
    RemoteOnly,
    IdenticalChange,
    Conflict,
}

/// Compare two working copies against an explicit retained base. Never apply a merge.
/// Revisions are store-local; comparison uses actual content, metadata and entry kind.
pub fn compare(
    base: &Snapshot,
    local: &Snapshot,
    remote: &Snapshot,
) -> Result<BTreeMap<Key, Difference>> {
    for snapshot in [base, local, remote] {
        snapshot.validate()?;
    }
    let keys: BTreeSet<_> = [base, local, remote]
        .into_iter()
        .flat_map(|snapshot| snapshot.records.keys().chain(snapshot.directories.iter()))
        .cloned()
        .collect();
    let same = |left: &Snapshot, right: &Snapshot, key: &Key| match (
        left.records.get(key),
        right.records.get(key),
    ) {
        (Some(left), Some(right)) => {
            left.content == right.content
                && left.size == right.size
                && left.metadata == right.metadata
        }
        (None, None) => left.directories.contains(key) == right.directories.contains(key),
        _ => false,
    };
    let mut differences = BTreeMap::new();
    for key in keys {
        let state = match (same(base, local, &key), same(base, remote, &key)) {
            (true, true) => continue,
            (false, true) => Difference::LocalOnly,
            (true, false) => Difference::RemoteOnly,
            (false, false) if same(local, remote, &key) => Difference::IdenticalChange,
            (false, false) => Difference::Conflict,
        };
        differences.insert(key, state);
    }
    Ok(differences)
}

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

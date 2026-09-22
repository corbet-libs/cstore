use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::filesystem::{
    atomic_bytes, atomic_json, checked_path, file_mode, lock, put_blob, read_blob, sync_dir,
};
use crate::{
    Capabilities, CheckpointTarget, Commit, ContentHash, Error, Key, Receipt, Record, Result,
    Snapshot, Store,
};

/// Retained immutable checkpoints. Publication never changes an active workspace.
pub struct FileArchive {
    root: PathBuf,
    selected: Option<ContentHash>,
}

#[derive(Default, Serialize, Deserialize)]
struct Catalog {
    requests: BTreeMap<String, ContentHash>,
    checkpoints: Vec<ContentHash>,
}

impl FileArchive {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        fs::create_dir_all(root.as_ref())?;
        let root = root.as_ref().canonicalize()?;
        let _lock = lock(&root)?;
        for name in ["blobs", "staging", "snapshots"] {
            fs::create_dir_all(root.join(name))?;
        }
        if !root.join("catalog.json").exists() {
            atomic_json(&root.join("catalog.json"), &Catalog::default())?;
        }
        Ok(Self {
            root,
            selected: None,
        })
    }

    fn catalog(&self) -> Result<Catalog> {
        Ok(serde_json::from_slice(&fs::read(
            self.root.join("catalog.json"),
        )?)?)
    }

    pub fn checkpoints(&self) -> Result<Vec<ContentHash>> {
        Ok(self.catalog()?.checkpoints)
    }

    pub fn select(&mut self, checkpoint: &ContentHash) -> Result<()> {
        self.snapshot(checkpoint)?;
        self.selected = Some(checkpoint.clone());
        Ok(())
    }

    pub fn snapshot(&self, checkpoint: &ContentHash) -> Result<Snapshot> {
        if !self.catalog()?.checkpoints.contains(checkpoint) {
            return Err(Error::Missing(checkpoint.as_str().into()));
        }
        let snapshot: Snapshot = serde_json::from_slice(&fs::read(
            self.root.join("snapshots").join(checkpoint.as_str()),
        )?)?;
        if snapshot.identity()? != *checkpoint {
            return Err(Error::Integrity("checkpoint manifest".into()));
        }
        Ok(snapshot)
    }

    fn staged_path(&self, request_id: &str) -> PathBuf {
        self.root
            .join("staging")
            .join(ContentHash::of(request_id.as_bytes()).as_str())
    }

    fn check_request(&self, request_id: &str, snapshot: &Snapshot) -> Result<()> {
        if request_id.is_empty() || request_id.len() > 256 {
            return Err(Error::Invalid("invalid request ID".into()));
        }
        let identity = snapshot.identity()?;
        if let Some(previous) = self.catalog()?.requests.get(request_id) {
            if previous != &identity {
                return Err(Error::RequestConflict(request_id.into()));
            }
        }
        let path = self.staged_path(request_id);
        if path.exists() {
            let previous: Snapshot = serde_json::from_slice(&fs::read(path)?)?;
            if previous != *snapshot {
                return Err(Error::RequestConflict(request_id.into()));
            }
        }
        Ok(())
    }
}

impl CheckpointTarget for FileArchive {
    fn begin(&mut self, request_id: &str, snapshot: &Snapshot) -> Result<()> {
        let _lock = lock(&self.root)?;
        self.check_request(request_id, snapshot)?;
        atomic_json(&self.staged_path(request_id), snapshot)
    }

    fn contains_blob(&self, hash: &ContentHash) -> Result<bool> {
        match read_blob(&self.root, hash) {
            Ok(_) => Ok(true),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn stage_blob(&mut self, hash: &ContentHash, bytes: &[u8]) -> Result<()> {
        hash.verify(bytes)?;
        put_blob(&self.root, bytes)?;
        Ok(())
    }

    fn publish(&mut self, request_id: &str, snapshot: &Snapshot) -> Result<ContentHash> {
        let _lock = lock(&self.root)?;
        self.check_request(request_id, snapshot)?;
        if !self.staged_path(request_id).is_file() {
            return Err(Error::Invalid("transfer has not begun".into()));
        }
        for record in snapshot
            .records
            .values()
            .chain(snapshot.history.iter().map(|item| &item.record))
        {
            let bytes = read_blob(&self.root, &record.content)?;
            if bytes.len() as u64 != record.size {
                return Err(Error::Integrity("blob size mismatch".into()));
            }
        }
        let identity = snapshot.identity()?;
        let path = self.root.join("snapshots").join(identity.as_str());
        if path.exists() {
            let previous: Snapshot = serde_json::from_slice(&fs::read(&path)?)?;
            if previous != *snapshot {
                return Err(Error::Integrity("checkpoint collision".into()));
            }
        } else {
            atomic_json(&path, snapshot)?;
        }
        let mut catalog = self.catalog()?;
        catalog.requests.insert(request_id.into(), identity.clone());
        if !catalog.checkpoints.contains(&identity) {
            catalog.checkpoints.push(identity.clone());
        }
        atomic_json(&self.root.join("catalog.json"), &catalog)?;
        Ok(identity)
    }
}

impl Store for FileArchive {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            atomic_multiple_records: false,
            coordinated_conditional_writes: false,
            coherent_checkpoints: true,
        }
    }

    fn read(&mut self, key: &Key) -> Result<Option<Record>> {
        Ok(self.checkpoint()?.records.get(key).cloned())
    }
    fn blob(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        read_blob(&self.root, hash)
    }
    fn commit(&mut self, _: &Commit) -> Result<Receipt> {
        Err(Error::Unsupported("immutable backup".into()))
    }
    fn resolve(&mut self, _: &str) -> Result<Option<Receipt>> {
        Err(Error::Unsupported(
            "backup has no working-store commits".into(),
        ))
    }
    fn checkpoint(&mut self) -> Result<Snapshot> {
        let identity = match &self.selected {
            Some(identity) => identity.clone(),
            None => self
                .catalog()?
                .checkpoints
                .last()
                .cloned()
                .ok_or_else(|| Error::Missing("no published checkpoint".into()))?,
        };
        self.snapshot(&identity)
    }
}

/// Restore a complete payload tree into a NEW directory, without contacting a server.
///
/// Original files are never replaced. Metadata and history remain in the archive;
/// the returned checkpoint identity lets the consumer bind the new store's lineage.
/// This restores bytes and file modes, not a product's provider session.
pub fn restore_files(
    source: &impl Store,
    snapshot: &Snapshot,
    destination: impl AsRef<Path>,
) -> Result<ContentHash> {
    let identity = snapshot.identity()?;
    let destination = destination.as_ref();
    let parent = destination
        .parent()
        .ok_or_else(|| Error::Invalid("destination needs a parent".into()))?
        .canonicalize()?;
    let name = destination
        .file_name()
        .ok_or_else(|| Error::Invalid("invalid destination".into()))?;
    let destination = parent.join(name);
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(Error::DestinationNotEmpty);
    }
    let temporary = tempfile::Builder::new()
        .prefix(".cstore-restore-")
        .tempdir_in(&parent)?;
    for directory in &snapshot.directories {
        fs::create_dir_all(checked_path(temporary.path(), directory)?)?;
    }
    for record in snapshot.records.values() {
        let bytes = source.blob(&record.content)?;
        record.content.verify(&bytes)?;
        if bytes.len() as u64 != record.size {
            return Err(Error::Integrity("restore blob size mismatch".into()));
        }
        let path = checked_path(temporary.path(), &record.key)?;
        fs::create_dir_all(path.parent().unwrap())?;
        atomic_bytes(&path, &bytes, file_mode(&record.metadata)?)?;
    }
    sync_tree(temporary.path())?;
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        temporary.path(),
        rustix::fs::CWD,
        &destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(|error| {
        if error == rustix::io::Errno::EXIST {
            Error::DestinationNotEmpty
        } else {
            Error::Io(error.into())
        }
    })?;
    // The temporary pathname is gone; prevent cleanup from touching a reused name.
    let _published = temporary.keep();
    sync_dir(&parent)?;
    Ok(identity)
}

fn sync_tree(path: &Path) -> Result<()> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree(&entry.path())?;
        }
    }
    sync_dir(path)
}

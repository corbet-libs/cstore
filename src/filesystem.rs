use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    Capabilities, Commit, ContentHash, Error, Expected, HistoricalRecord, Key, Metadata, Origin,
    Receipt, Record, Result, Revision, Snapshot, Store,
};

#[derive(Serialize, Deserialize)]
struct State {
    format: u32,
    store_id: String,
    root: PathBuf,
    records: BTreeMap<Key, Record>,
    receipts: BTreeMap<String, Receipt>,
    origin: Option<Origin>,
    history: Vec<HistoricalRecord>,
    imported_receipts: Vec<Receipt>,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    key: Key,
    before: Option<(ContentHash, u32)>,
    after: Option<Record>,
    receipt: Receipt,
}

/// Raw files remain authoritative. Control storage must be outside the payload tree.
///
/// Locks coordinate cstore writers; external editors must hand off before writes
/// and snapshots. Arbitrary concurrent file editing is not a strict CAS guarantee.
pub struct FileStore {
    root: PathBuf,
    control: PathBuf,
    #[cfg(test)]
    fail_after: Option<&'static str>,
}

impl FileStore {
    pub fn open(root: impl AsRef<Path>, control: impl AsRef<Path>, store_id: &str) -> Result<Self> {
        if store_id.is_empty() {
            return Err(Error::Invalid("store identity is required".into()));
        }
        let root = root.as_ref().canonicalize()?;
        if !root.is_dir() {
            return Err(Error::Invalid("payload root must be a directory".into()));
        }
        fs::create_dir_all(control.as_ref())?;
        let control = control.as_ref().canonicalize()?;
        if control.starts_with(&root) || root.starts_with(&control) {
            return Err(Error::Invalid(
                "control and payload trees must be disjoint".into(),
            ));
        }
        let store = Self {
            root,
            control,
            #[cfg(test)]
            fail_after: None,
        };
        let _lock = lock(&store.control)?;
        fs::create_dir_all(store.control.join("blobs"))?;
        let path = store.control.join("state.json");
        if !path.exists() {
            store.save(&State {
                format: 1,
                store_id: store_id.into(),
                root: store.root.clone(),
                records: BTreeMap::new(),
                receipts: BTreeMap::new(),
                origin: None,
                history: Vec::new(),
                imported_receipts: Vec::new(),
            })?;
        }
        let state = store.load()?;
        if state.store_id != store_id || state.root != store.root {
            return Err(Error::Invalid(
                "control storage belongs to another payload root or store".into(),
            ));
        }
        store.recover()?;
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn fault(&self, stage: &str) -> Result<()> {
        #[cfg(test)]
        if self.fail_after == Some(stage) {
            return Err(Error::Io(std::io::Error::other("injected interruption")));
        }
        let _ = stage;
        Ok(())
    }

    pub fn set_origin(&mut self, origin: Origin) -> Result<()> {
        let _lock = lock(&self.control)?;
        self.recover()?;
        let mut state = self.load()?;
        if state
            .origin
            .as_ref()
            .is_some_and(|current| current != &origin)
        {
            return Err(Error::Conflict("store lineage is already set".into()));
        }
        state.origin = Some(origin);
        self.save(&state)
    }

    fn load(&self) -> Result<State> {
        let state: State = serde_json::from_slice(&fs::read(self.control.join("state.json"))?)?;
        if state.format != 1 || state.root != self.root {
            return Err(Error::Integrity(
                "unsupported or mismatched file store state".into(),
            ));
        }
        Ok(state)
    }

    fn save(&self, state: &State) -> Result<()> {
        atomic_json(&self.control.join("state.json"), state)
    }

    fn path(&self, key: &Key) -> Result<PathBuf> {
        checked_path(&self.root, key)
    }

    fn disk(&self, key: &Key) -> Result<Option<(Vec<u8>, u32)>> {
        let path = self.path(key)?;
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {
                Ok(Some((fs::read(path)?, meta.permissions().mode() & 0o777)))
            }
            Ok(_) => Err(Error::Unsupported(format!(
                "non-regular file {}",
                key.as_str()
            ))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn observe(&self, state: &mut State, key: &Key) -> Result<Option<Record>> {
        let Some((bytes, mode)) = self.disk(key)? else {
            if state.records.contains_key(key) {
                return Err(Error::ExternalChange(format!("missing {}", key.as_str())));
            }
            return Ok(None);
        };
        let content = put_blob(&self.control, &bytes)?;
        let mut metadata = state
            .records
            .get(key)
            .map(|r| r.metadata.clone())
            .unwrap_or_default();
        metadata
            .extensions
            .insert("cstore.unix_mode".into(), mode.into());
        if let Some(previous) = state.records.get(key) {
            if previous.content == content && previous.metadata == metadata {
                return Ok(Some(previous.clone()));
            }
        }
        let revision = Revision(
            ContentHash::of(&serde_json::to_vec(&(
                &state.store_id,
                key,
                &content,
                &metadata,
            ))?)
            .as_str()
            .into(),
        );
        let record = Record {
            key: key.clone(),
            revision,
            content,
            size: bytes.len() as u64,
            metadata,
        };
        if let Some(previous) = state.records.get(key) {
            state.history.push(HistoricalRecord {
                store_id: state.store_id.clone(),
                record: previous.clone(),
            });
        }
        state.records.insert(key.clone(), record.clone());
        Ok(Some(record))
    }

    fn apply_payload(&self, pending: &Pending) -> Result<()> {
        let path = self.path(&pending.key)?;
        if let Some(record) = &pending.after {
            let bytes = self.blob(&record.content)?;
            let parent = path
                .parent()
                .ok_or_else(|| Error::Invalid("missing parent".into()))?;
            fs::create_dir_all(parent)?;
            let mode = file_mode(&record.metadata)?;
            atomic_bytes(&path, &bytes, mode)?;
        } else {
            fs::remove_file(&path)?;
            sync_dir(path.parent().unwrap())?;
        }
        Ok(())
    }

    fn finish(&self, state: &mut State, pending: &Pending) -> Result<()> {
        if let Some(previous) = state.records.get(&pending.key) {
            state.history.push(HistoricalRecord {
                store_id: state.store_id.clone(),
                record: previous.clone(),
            });
        }
        match &pending.after {
            Some(record) => {
                state.records.insert(pending.key.clone(), record.clone());
            }
            None => {
                state.records.remove(&pending.key);
            }
        }
        state
            .receipts
            .insert(pending.receipt.request_id.clone(), pending.receipt.clone());
        self.save(state)?;
        self.fault("state")?;
        fs::remove_file(self.control.join("pending.json"))?;
        sync_dir(&self.control)
    }

    fn recover(&self) -> Result<()> {
        let bytes = match fs::read(self.control.join("pending.json")) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let pending: Pending = serde_json::from_slice(&bytes)?;
        let mut state = self.load()?;
        if let Some(receipt) = state.receipts.get(&pending.receipt.request_id) {
            if receipt != &pending.receipt {
                return Err(Error::Integrity("journal receipt mismatch".into()));
            }
            fs::remove_file(self.control.join("pending.json"))?;
            return sync_dir(&self.control);
        }
        let current = self
            .disk(&pending.key)?
            .map(|(bytes, mode)| (ContentHash::of(&bytes), mode));
        let after = pending
            .after
            .as_ref()
            .map(|record| file_mode(&record.metadata).map(|mode| (record.content.clone(), mode)))
            .transpose()?;
        if current != after {
            if current != pending.before {
                return Err(Error::ExternalChange(pending.key.as_str().into()));
            }
            self.apply_payload(&pending)?;
        }
        self.finish(&mut state, &pending)
    }

    fn scan(
        &self,
        directory: &Path,
        files: &mut Vec<Key>,
        directories: &mut Vec<Key>,
    ) -> Result<()> {
        let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(&self.root)
                .map_err(|_| Error::Invalid("outside root".into()))?;
            let key = Key::new(
                relative
                    .to_str()
                    .ok_or_else(|| Error::Unsupported("non-UTF-8 path".into()))?,
            )?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                directories.push(key);
                self.scan(&path, files, directories)?;
            } else if kind.is_file() {
                files.push(key);
            } else {
                return Err(Error::Unsupported(format!(
                    "symlink or special file {}",
                    key.as_str()
                )));
            }
        }
        Ok(())
    }
}

impl Store for FileStore {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            atomic_multiple_records: false,
            coordinated_conditional_writes: true,
            coherent_checkpoints: true,
        }
    }

    fn read(&mut self, key: &Key) -> Result<Option<Record>> {
        let _lock = lock(&self.control)?;
        self.recover()?;
        let mut state = self.load()?;
        let record = self.observe(&mut state, key)?;
        self.save(&state)?;
        Ok(record)
    }

    fn blob(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        read_blob(&self.control, hash)
    }

    fn commit(&mut self, commit: &Commit) -> Result<Receipt> {
        let request_hash = commit.digest()?;
        if commit.writes.len() != 1 {
            return Err(Error::Unsupported("atomic multi-record file commit".into()));
        }
        let _lock = lock(&self.control)?;
        self.recover()?;
        let mut state = self.load()?;
        if let Some(receipt) = state.receipts.get(&commit.request_id) {
            return if receipt.request_hash == request_hash {
                Ok(receipt.clone())
            } else {
                Err(Error::RequestConflict(commit.request_id.clone()))
            };
        }
        let write = &commit.writes[0];
        let previous = self.observe(&mut state, &write.key)?;
        let matches = match (&write.expected, &previous) {
            (Expected::Absent, None) => true,
            (Expected::Revision { revision }, Some(record)) => revision == &record.revision,
            _ => false,
        };
        if !matches {
            return Err(Error::Conflict(write.key.as_str().into()));
        }
        if previous.is_none() && write.value.is_none() {
            return Err(Error::Invalid("cannot remove an absent record".into()));
        }
        let after = match &write.value {
            Some((bytes, supplied_metadata)) => {
                let mut metadata = supplied_metadata.clone();
                if !metadata.extensions.contains_key("cstore.unix_mode") {
                    let mode = previous
                        .as_ref()
                        .map(|record| file_mode(&record.metadata))
                        .transpose()?
                        .unwrap_or(0o600);
                    metadata
                        .extensions
                        .insert("cstore.unix_mode".into(), mode.into());
                }
                file_mode(&metadata)?;
                Some(Record {
                    key: write.key.clone(),
                    revision: Revision(
                        ContentHash::of(&serde_json::to_vec(&(
                            &state.store_id,
                            &request_hash,
                            &write.key,
                        ))?)
                        .as_str()
                        .into(),
                    ),
                    content: put_blob(&self.control, bytes)?,
                    size: bytes.len() as u64,
                    metadata,
                })
            }
            None => None,
        };
        let receipt = Receipt {
            request_id: commit.request_id.clone(),
            request_hash,
            store_id: state.store_id.clone(),
            revisions: BTreeMap::from([(
                write.key.clone(),
                after.as_ref().map(|record| record.revision.clone()),
            )]),
        };
        let before = previous
            .as_ref()
            .map(|record| file_mode(&record.metadata).map(|mode| (record.content.clone(), mode)))
            .transpose()?;
        // Persist an observed external base before installing the recovery journal.
        self.save(&state)?;
        let pending = Pending {
            key: write.key.clone(),
            before,
            after,
            receipt: receipt.clone(),
        };
        atomic_json(&self.control.join("pending.json"), &pending)?;
        self.fault("journal")?;
        self.apply_payload(&pending)?;
        self.fault("payload")?;
        self.finish(&mut state, &pending)?;
        Ok(receipt)
    }

    fn resolve(&mut self, request_id: &str) -> Result<Option<Receipt>> {
        let _lock = lock(&self.control)?;
        self.recover()?;
        Ok(self.load()?.receipts.get(request_id).cloned())
    }

    fn checkpoint(&mut self) -> Result<Snapshot> {
        let _lock = lock(&self.control)?;
        self.recover()?;
        let mut state = self.load()?;
        let mut files = Vec::new();
        let mut directories = Vec::new();
        self.scan(&self.root, &mut files, &mut directories)?;
        let mut records = BTreeMap::new();
        for key in files {
            let record = self
                .observe(&mut state, &key)?
                .ok_or_else(|| Error::ExternalChange(key.as_str().into()))?;
            records.insert(key, record);
        }
        for key in state.records.keys() {
            if !records.contains_key(key) {
                return Err(Error::ExternalChange(format!("missing {}", key.as_str())));
            }
        }
        self.save(&state)?;
        let mut receipts = state.imported_receipts;
        receipts.extend(state.receipts.into_values());
        let snapshot = Snapshot {
            format: 1,
            source: state.store_id,
            records,
            directories,
            origin: state.origin,
            history: state.history,
            receipts,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }
}

pub(crate) fn file_mode(metadata: &Metadata) -> Result<u32> {
    match metadata.extensions.get("cstore.unix_mode") {
        None => Ok(0o600),
        Some(value) => value
            .as_u64()
            .filter(|mode| *mode <= 0o777)
            .map(|mode| mode as u32)
            .ok_or_else(|| Error::Invalid("invalid file permissions".into())),
    }
}

pub(crate) fn checked_path(root: &Path, key: &Key) -> Result<PathBuf> {
    let mut path = root.to_path_buf();
    let parts = key.as_str().split('/').collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(Error::Unsupported(format!("symlink path {}", key.as_str())));
            }
            Ok(meta) if index + 1 < parts.len() && !meta.is_dir() => {
                return Err(Error::Invalid("non-directory path ancestor".into()));
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(path)
}

pub(crate) fn lock(control: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(control.join("lock"))?;
    file.lock()?;
    Ok(file)
}

pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

pub(crate) fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_bytes(path, &serde_json::to_vec_pretty(value)?, 0o600)
}

pub(crate) fn atomic_bytes(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| Error::Invalid("missing parent directory".into()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    sync_dir(parent)
}

pub(crate) fn put_blob(control: &Path, bytes: &[u8]) -> Result<ContentHash> {
    let hash = ContentHash::of(bytes);
    let directory = control.join("blobs");
    let path = directory.join(hash.as_str());
    if path.exists() {
        hash.verify(&fs::read(path)?)?;
        return Ok(hash);
    }
    let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => (),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            hash.verify(&fs::read(path)?)?
        }
        Err(error) => return Err(error.error.into()),
    }
    sync_dir(&directory)?;
    Ok(hash)
}

pub(crate) fn read_blob(control: &Path, hash: &ContentHash) -> Result<Vec<u8>> {
    let bytes = fs::read(control.join("blobs").join(hash.as_str()))?;
    hash.verify(&bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Write;

    #[test]
    fn interrupted_commit_recovers_at_every_durable_boundary() {
        for stage in ["journal", "payload", "state"] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("workspace");
            fs::create_dir(&root).unwrap();
            fs::write(root.join("record"), b"before").unwrap();
            let control = temp.path().join("control");
            let mut store = FileStore::open(&root, &control, "source").unwrap();
            let key = Key::new("record").unwrap();
            let revision = store.read(&key).unwrap().unwrap().revision;
            let commit = Commit {
                request_id: "interrupted".into(),
                writes: vec![Write {
                    key: key.clone(),
                    expected: Expected::Revision { revision },
                    value: Some((b"after".to_vec(), Metadata::default())),
                }],
            };
            store.fail_after = Some(stage);
            assert!(store.commit(&commit).is_err(), "{stage}");
            drop(store);
            let mut recovered = FileStore::open(&root, &control, "source").unwrap();
            let receipt = recovered.resolve("interrupted").unwrap().unwrap();
            assert_eq!(recovered.commit(&commit).unwrap(), receipt);
            assert_eq!(fs::read(root.join("record")).unwrap(), b"after");
            assert_eq!(recovered.checkpoint().unwrap().history.len(), 1);
        }
    }

    #[test]
    fn recovery_does_not_overwrite_an_external_edit_after_interruption() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("record"), b"before").unwrap();
        let control = temp.path().join("control");
        let mut store = FileStore::open(&root, &control, "source").unwrap();
        let key = Key::new("record").unwrap();
        let revision = store.read(&key).unwrap().unwrap().revision;
        store.fail_after = Some("journal");
        assert!(
            store
                .commit(&Commit {
                    request_id: "pending".into(),
                    writes: vec![Write {
                        key,
                        expected: Expected::Revision { revision },
                        value: Some((b"proposed".to_vec(), Metadata::default())),
                    }]
                })
                .is_err()
        );
        fs::write(root.join("record"), b"external edit").unwrap();
        drop(store);
        assert!(matches!(
            FileStore::open(&root, &control, "source"),
            Err(Error::ExternalChange(_))
        ));
        assert_eq!(fs::read(root.join("record")).unwrap(), b"external edit");
        assert!(control.join("pending.json").is_file());
    }
}

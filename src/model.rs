use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("record not found: {0}")]
    Missing(String),
    #[error("revision conflict for {0}")]
    Conflict(String),
    #[error("request ID was already used with different contents: {0}")]
    RequestConflict(String),
    #[error("unsupported guarantee: {0}")]
    Unsupported(String),
    #[error("integrity check failed: {0}")]
    Integrity(String),
    #[error("external change requires reconciliation: {0}")]
    ExternalChange(String),
    #[error("destination is not empty")]
    DestinationNotEmpty,
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid storage metadata: {0}")]
    Json(#[from] serde_json::Error),
}

/// A product-defined key with a portable relative-path representation.
///
/// Existing directory identities can be retained; cstore never invents product IDs.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct Key(String);

impl Key {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.contains(['\\', '\0', ':'])
            || value
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(Error::Invalid(format!("non-portable record key {value:?}")));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// An integrity digest, not a revision or an access credential.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct ContentHash(String);

impl ContentHash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn verify(&self, bytes: &[u8]) -> Result<()> {
        if *self != Self::of(bytes) {
            return Err(Error::Integrity(self.0.clone()));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        if value.len() != 64
            || !value
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(serde::de::Error::custom("invalid SHA-256 digest"));
        }
        Ok(Self(value))
    }
}

/// Opaque, store-local token. Product revision fields remain part of the payload.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub String);

/// Product metadata is opaque and preserved by storage and transfer.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Metadata {
    pub media_type: Option<String>,
    pub schema: Option<String>,
    #[serde(default)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub key: Key,
    pub revision: Revision,
    pub content: ContentHash,
    pub size: u64,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expected {
    Absent,
    Revision { revision: Revision },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Write {
    pub key: Key,
    pub expected: Expected,
    /// None is a logical removal; physical retention is backend-owned.
    pub value: Option<(Vec<u8>, Metadata)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub request_id: String,
    pub writes: Vec<Write>,
}

impl Commit {
    pub fn validate(&self) -> Result<()> {
        if self.request_id.is_empty() || self.request_id.len() > 256 || self.writes.is_empty() {
            return Err(Error::Invalid(
                "commit requires a bounded request ID and writes".into(),
            ));
        }
        let mut keys = std::collections::BTreeSet::new();
        for write in &self.writes {
            if !keys.insert(&write.key) {
                return Err(Error::Invalid("duplicate commit key".into()));
            }
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<ContentHash> {
        self.validate()?;
        Ok(ContentHash::of(&serde_json::to_vec(self)?))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub request_id: String,
    pub request_hash: ContentHash,
    pub store_id: String,
    pub revisions: BTreeMap<Key, Option<Revision>>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Origin {
    pub store_id: String,
    pub checkpoint: ContentHash,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HistoricalRecord {
    pub store_id: String,
    pub record: Record,
}

/// Coherent, immutable inventory. Blobs are transported separately and verified.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: u32,
    pub source: String,
    pub records: BTreeMap<Key, Record>,
    pub directories: Vec<Key>,
    pub origin: Option<Origin>,
    #[serde(default)]
    pub history: Vec<HistoricalRecord>,
    #[serde(default)]
    pub receipts: Vec<Receipt>,
}

impl Snapshot {
    pub fn validate(&self) -> Result<()> {
        if self.format != 1 || self.source.is_empty() {
            return Err(Error::Invalid(
                "unsupported snapshot format or missing source".into(),
            ));
        }
        let mut directories = std::collections::BTreeSet::new();
        for directory in &self.directories {
            if !directories.insert(directory) || self.records.contains_key(directory) {
                return Err(Error::Invalid("conflicting directory entry".into()));
            }
            let mut ancestor = directory.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                if self.records.contains_key(&Key::new(parent)?) {
                    return Err(Error::Invalid(
                        "record is an ancestor of a directory".into(),
                    ));
                }
                ancestor = parent;
            }
        }
        for (key, record) in &self.records {
            if key != &record.key {
                return Err(Error::Integrity(
                    "inventory key differs from record key".into(),
                ));
            }
            let mut prefix = String::new();
            for part in key
                .as_str()
                .split('/')
                .take(key.as_str().split('/').count() - 1)
            {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if self.records.contains_key(&Key::new(&prefix)?) {
                    return Err(Error::Invalid(
                        "record is an ancestor of another record".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn identity(&self) -> Result<ContentHash> {
        self.validate()?;
        Ok(ContentHash::of(&serde_json::to_vec(self)?))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capabilities {
    pub atomic_multiple_records: bool,
    pub coordinated_conditional_writes: bool,
    pub coherent_checkpoints: bool,
}

/// Backend contract. Native blocking I/O must run on a host's I/O executor.
/// Browser hosts adapt the portable request/result types to their asynchronous API.
pub trait Store {
    fn capabilities(&self) -> Capabilities;
    fn read(&mut self, key: &Key) -> Result<Option<Record>>;
    fn blob(&self, hash: &ContentHash) -> Result<Vec<u8>>;
    fn commit(&mut self, commit: &Commit) -> Result<Receipt>;
    fn resolve(&mut self, request_id: &str) -> Result<Option<Receipt>>;
    fn checkpoint(&mut self) -> Result<Snapshot>;
}

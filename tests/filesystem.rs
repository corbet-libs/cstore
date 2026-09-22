#![cfg(all(feature = "filesystem", target_os = "linux"))]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Barrier};

use cstore::*;

fn fixture(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let files = BTreeMap::from([
        ("ccvl.json", br#"{"format":"ccvl-workspace","schema_version":8}"#.to_vec()),
        ("interview/profile.md", b"# Neutral fixture\n\nNo personal claims.\n".to_vec()),
        ("interview/imports/source.bin", vec![0, 255, 128, 1]),
        ("cvl/cv/example/content/en/ch/wording.toml", b"# Keep this comment\n[cv]\nsummary = 'shared wording'\nunknown = [1, 2]\n".to_vec()),
        ("cvl/cv/example/standard/en/ch/content.toml", b"[wording]\nsource = '../../../content/en/ch/wording.toml'\n[cv]\nsummary = 'leaf override'\n".to_vec()),
        ("opportunities/example/engineer/application.toml", b"schema_version = 4\nrevision = 0\n[job]\nid = 'example--engineer'\n[cv]\nsummary = 'neutral fixture'\n".to_vec()),
        ("opportunities/example/engineer/posting.md", b"# Posting reference\n\nFixture; no live vacancy.\n".to_vec()),
        ("opportunities/example/engineer/pdfs/retained-output.bin", vec![0, 1, 2, 254]),
        (".agent/typst/fonts/retained-font.bin", vec![128, 129, 130]),
    ]).into_iter().map(|(key, value)| (key.to_owned(), value)).collect::<BTreeMap<_, _>>();
    fs::create_dir_all(root.join("interview/empty-directory")).unwrap();
    for (name, bytes) in &files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    files
}

fn change(request_id: &str, key: &str, expected: Expected, value: &[u8]) -> Commit {
    Commit {
        request_id: request_id.into(),
        writes: vec![Write {
            key: Key::new(key).unwrap(),
            expected,
            value: Some((value.to_vec(), Metadata::default())),
        }],
    }
}

#[test]
fn invalid_control_location_leaves_payload_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    assert!(matches!(
        FileStore::open(&root, root.join("new/control"), "source"),
        Err(Error::Invalid(_))
    ));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[test]
fn observed_reversions_never_reuse_an_old_revision() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("record"), b"A").unwrap();
    let mut store = FileStore::open(&root, temp.path().join("control"), "source").unwrap();
    let key = Key::new("record").unwrap();
    let original = store.read(&key).unwrap().unwrap();
    let stale = change(
        "stale",
        "record",
        Expected::Revision {
            revision: original.revision.clone(),
        },
        b"replacement",
    );
    fs::write(root.join("record"), b"B").unwrap();
    assert!(matches!(store.commit(&stale), Err(Error::Conflict(_))));
    fs::write(root.join("record"), b"A").unwrap();
    let reverted = store.read(&key).unwrap().unwrap();
    assert_ne!(original.revision, reverted.revision);
    assert!(matches!(store.commit(&stale), Err(Error::Conflict(_))));
    assert_eq!(store.checkpoint().unwrap().history.len(), 2);
}

#[test]
fn restored_workspaces_retain_metadata_history_and_lineage_without_reusing_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let mut source = FileStore::open(&root, temp.path().join("control"), "source").unwrap();
    let mut first = change("first", "record.toml", Expected::Absent, b"first");
    let metadata = &mut first.writes[0].value.as_mut().unwrap().1;
    metadata.media_type = Some("application/toml".into());
    metadata.schema = Some("example.v1".into());
    metadata
        .extensions
        .insert("unknown".into(), serde_json::json!({"preserve": [1, 2]}));
    source.commit(&first).unwrap();
    let key = Key::new("record.toml").unwrap();
    let previous = source.read(&key).unwrap().unwrap();
    let mut second = change(
        "second",
        key.as_str(),
        Expected::Revision {
            revision: previous.revision,
        },
        b"second",
    );
    second.writes[0].value.as_mut().unwrap().1 = previous.metadata.clone();
    source.commit(&second).unwrap();
    let snapshot = source.checkpoint().unwrap();
    let root = temp.path().join("restored");
    restore_files(&source, &snapshot, &root).unwrap();
    let mut restored =
        FileStore::open(&root, temp.path().join("restored-control"), "offline").unwrap();
    restored.adopt_snapshot(&source, &snapshot).unwrap();
    restored.adopt_snapshot(&source, &snapshot).unwrap();
    let adopted = restored.checkpoint().unwrap();
    assert_eq!(
        adopted.records[&key].metadata,
        snapshot.records[&key].metadata
    );
    assert_ne!(
        adopted.records[&key].revision,
        snapshot.records[&key].revision
    );
    assert_eq!(
        adopted.origin.unwrap().checkpoint,
        snapshot.identity().unwrap()
    );
    assert_eq!(adopted.receipts, snapshot.receipts);
    assert_eq!(
        restored.blob(&snapshot.history[0].record.content).unwrap(),
        b"first"
    );
    assert!(restored.resolve("second").unwrap().is_none());
    fs::write(root.join("record.toml"), b"independent edit").unwrap();
    assert!(matches!(
        restored.adopt_snapshot(&source, &snapshot),
        Err(Error::Integrity(_))
    ));
    assert_eq!(
        fs::read(root.join("record.toml")).unwrap(),
        b"independent edit"
    );
}

#[test]
fn interrupted_transfer_keeps_the_previous_checkpoint_active_and_resumes() {
    struct Interrupted<'a> {
        archive: &'a mut FileArchive,
        remaining: usize,
    }
    impl CheckpointTarget for Interrupted<'_> {
        fn begin(&mut self, id: &str, snapshot: &Snapshot) -> Result<()> {
            self.archive.begin(id, snapshot)
        }
        fn contains_blob(&self, hash: &ContentHash) -> Result<bool> {
            self.archive.contains_blob(hash)
        }
        fn stage_blob(&mut self, hash: &ContentHash, bytes: &[u8]) -> Result<()> {
            if self.remaining == 0 {
                return Err(Error::Io(std::io::Error::other("interrupted transport")));
            }
            self.remaining -= 1;
            self.archive.stage_blob(hash, bytes)
        }
        fn publish(&mut self, id: &str, snapshot: &Snapshot) -> Result<ContentHash> {
            self.archive.publish(id, snapshot)
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let mut source = FileStore::open(&root, temp.path().join("control"), "source").unwrap();
    let mut backup = FileArchive::open(temp.path().join("backup")).unwrap();
    let first = source.checkpoint().unwrap();
    let first_id = transfer(&source, &first, &mut backup, "first").unwrap();
    fixture(&root);
    let next = source.checkpoint().unwrap();
    let mut interrupted = Interrupted {
        archive: &mut backup,
        remaining: 2,
    };
    assert!(transfer(&source, &next, &mut interrupted, "next").is_err());
    assert_eq!(backup.checkpoints().unwrap(), vec![first_id]);
    // Continue the retained checkpoint even if the live source has changed.
    fs::write(root.join("interview/profile.md"), b"later edit").unwrap();
    let next_id = transfer(&source, &next, &mut backup, "next").unwrap();
    assert_eq!(backup.checkpoints().unwrap().len(), 2);
    assert_eq!(backup.snapshot(&next_id).unwrap(), next);
}

#[test]
fn comparison_distinguishes_independent_identical_and_conflicting_changes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    for name in ["local", "remote", "same", "conflict", "deleted"] {
        fs::write(root.join(name), b"base").unwrap();
    }
    let mut source = FileStore::open(&root, temp.path().join("control"), "source").unwrap();
    let base = source.checkpoint().unwrap();
    let mut local = base.clone();
    let mut remote = base.clone();
    for name in ["local", "same", "conflict"] {
        local
            .records
            .get_mut(&Key::new(name).unwrap())
            .unwrap()
            .content = ContentHash::of(b"L");
    }
    for name in ["remote", "conflict"] {
        remote
            .records
            .get_mut(&Key::new(name).unwrap())
            .unwrap()
            .content = ContentHash::of(b"R");
    }
    remote
        .records
        .get_mut(&Key::new("same").unwrap())
        .unwrap()
        .content = ContentHash::of(b"L");
    local.records.remove(&Key::new("deleted").unwrap());
    let result = compare(&base, &local, &remote).unwrap();
    for (name, expected) in [
        ("local", Difference::LocalOnly),
        ("remote", Difference::RemoteOnly),
        ("same", Difference::IdenticalChange),
        ("conflict", Difference::Conflict),
        ("deleted", Difference::LocalOnly),
    ] {
        assert_eq!(result[&Key::new(name).unwrap()], expected);
    }
}

#[test]
fn ccvl_authored_tree_roundtrips_with_comments_references_binary_data_and_empty_directories() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    let expected = fixture(&root);
    let mut source = FileStore::open(&root, temp.path().join("control"), "source").unwrap();
    let snapshot = source.checkpoint().unwrap();
    assert_eq!(snapshot.records.len(), expected.len());
    let mut backup = FileArchive::open(temp.path().join("backup")).unwrap();
    let checkpoint = transfer(&source, &snapshot, &mut backup, "first-backup").unwrap();
    let target = temp.path().join("restored");
    assert_eq!(
        restore_files(&backup, &backup.snapshot(&checkpoint).unwrap(), &target).unwrap(),
        checkpoint
    );
    for (path, bytes) in expected {
        assert_eq!(fs::read(target.join(path)).unwrap(), bytes);
    }
    assert!(target.join("interview/empty-directory").is_dir());
    assert_eq!(backup.checkpoints().unwrap(), vec![checkpoint.clone()]);
    assert_eq!(
        transfer(&source, &snapshot, &mut backup, "first-backup").unwrap(),
        checkpoint
    );
    assert_eq!(backup.checkpoints().unwrap().len(), 1);
}

#[test]
fn direct_edit_invalidates_a_stale_write_without_losing_comments() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fixture(&root);
    let mut store = FileStore::open(&root, temp.path().join("state"), "source").unwrap();
    let key = Key::new("opportunities/example/engineer/application.toml").unwrap();
    let first = store.read(&key).unwrap().unwrap();
    let bytes = b"# A human changed only comments\nschema_version = 4\nrevision = 0\n";
    fs::write(root.join(key.as_str()), bytes).unwrap();
    let commit = change(
        "stale",
        key.as_str(),
        Expected::Revision {
            revision: first.revision,
        },
        b"stale replacement",
    );
    assert!(matches!(store.commit(&commit), Err(Error::Conflict(_))));
    assert_eq!(fs::read(root.join(key.as_str())).unwrap(), bytes);
    assert!(store.resolve("stale").unwrap().is_none());
}

#[test]
fn independent_writers_have_one_winner_and_durable_retry_identity() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("record.toml"), b"before").unwrap();
    let control = temp.path().join("control");
    let mut store = FileStore::open(&root, &control, "source").unwrap();
    let revision = store
        .read(&Key::new("record.toml").unwrap())
        .unwrap()
        .unwrap()
        .revision;
    let barrier = Arc::new(Barrier::new(2));
    let handles = ["one", "two"].map(|name| {
        let (root, control, revision, barrier) = (
            root.clone(),
            control.clone(),
            revision.clone(),
            barrier.clone(),
        );
        std::thread::spawn(move || {
            let mut store = FileStore::open(root, control, "source").unwrap();
            let request = change(
                name,
                "record.toml",
                Expected::Revision { revision },
                name.as_bytes(),
            );
            barrier.wait();
            (request.clone(), store.commit(&request))
        })
    });
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results.iter().filter(|(_, result)| result.is_ok()).count(),
        1
    );
    let (request, result) = results
        .into_iter()
        .find(|(_, result)| result.is_ok())
        .unwrap();
    let receipt = result.unwrap();
    let mut reopened = FileStore::open(&root, &control, "source").unwrap();
    assert_eq!(reopened.commit(&request).unwrap(), receipt);
    assert_eq!(
        reopened.resolve(&request.request_id).unwrap(),
        Some(receipt)
    );
    let mut changed_request = request;
    changed_request.writes[0].value.as_mut().unwrap().0 = b"different retry".to_vec();
    assert!(matches!(
        reopened.commit(&changed_request),
        Err(Error::RequestConflict(_))
    ));
}

#[test]
fn unsupported_batch_does_not_change_any_payload() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    let mut store = FileStore::open(&root, temp.path().join("control"), "source").unwrap();
    let mut commit = change("batch", "one", Expected::Absent, b"one");
    commit
        .writes
        .extend(change("unused", "two", Expected::Absent, b"two").writes);
    assert!(matches!(store.commit(&commit), Err(Error::Unsupported(_))));
    assert_eq!(fs::read_dir(root).unwrap().count(), 0);
}

#[test]
fn earlier_checkpoint_survives_a_later_logical_deletion() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("important.txt"), b"recover me").unwrap();
    let mut source = FileStore::open(&root, temp.path().join("control"), "source").unwrap();
    let mut backup = FileArchive::open(temp.path().join("backup")).unwrap();
    let old = source.checkpoint().unwrap();
    let old_id = transfer(&source, &old, &mut backup, "old").unwrap();
    let key = Key::new("important.txt").unwrap();
    source
        .commit(&Commit {
            request_id: "delete".into(),
            writes: vec![Write {
                key: key.clone(),
                expected: Expected::Revision {
                    revision: old.records[&key].revision.clone(),
                },
                value: None,
            }],
        })
        .unwrap();
    let new = source.checkpoint().unwrap();
    assert!(new.records.is_empty());
    assert_eq!(new.history.len(), 1);
    let new_id = transfer(&source, &new, &mut backup, "new").unwrap();
    assert_ne!(old_id, new_id);
    let destination = temp.path().join("recovery");
    restore_files(&backup, &backup.snapshot(&old_id).unwrap(), &destination).unwrap();
    assert_eq!(
        fs::read(destination.join("important.txt")).unwrap(),
        b"recover me"
    );
    assert_eq!(backup.checkpoints().unwrap().len(), 2);
}

#[test]
fn restoration_refuses_existing_destinations_and_corrupt_assets() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fixture(&root);
    let mut source = FileStore::open(&root, temp.path().join("control"), "source").unwrap();
    let snapshot = source.checkpoint().unwrap();
    let mut backup = FileArchive::open(temp.path().join("backup")).unwrap();
    transfer(&source, &snapshot, &mut backup, "copy").unwrap();
    let destination = temp.path().join("existing");
    fs::create_dir(&destination).unwrap();
    assert!(matches!(
        restore_files(&backup, &snapshot, &destination),
        Err(Error::DestinationNotEmpty)
    ));
    let hash = snapshot.records.values().next().unwrap().content.clone();
    fs::write(
        temp.path().join("backup/blobs").join(hash.as_str()),
        b"corrupted",
    )
    .unwrap();
    let absent = temp.path().join("must-stay-absent");
    assert!(matches!(
        restore_files(&backup, &snapshot, &absent),
        Err(Error::Integrity(_))
    ));
    assert!(!absent.exists());
}

#[test]
fn symlinks_fail_capture_instead_of_silently_skipping_or_following_them() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("workspace");
    fs::create_dir(&root).unwrap();
    fs::write(temp.path().join("private"), b"outside").unwrap();
    std::os::unix::fs::symlink("../private", root.join("link")).unwrap();
    let mut source = FileStore::open(&root, temp.path().join("control"), "source").unwrap();
    assert!(matches!(source.checkpoint(), Err(Error::Unsupported(_))));
    assert!(matches!(
        source.read(&Key::new("link").unwrap()),
        Err(Error::Unsupported(_))
    ));
}

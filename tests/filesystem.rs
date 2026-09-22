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

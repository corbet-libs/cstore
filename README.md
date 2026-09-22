# cstore

Shared versioned persistence and portable workspace backup/restore, licensed under
FSL-1.1-ALv2. Product adapters retain data meaning, authorization and authored file
layouts. Work is in progress; supported backends and verification are recorded
below as they are delivered.

`@corbet-libs/cstore` extracts CareerVector's existing TypeScript/Yjs snapshot
lifecycle and D1 persistence primitives. Product schemas and operation policy stay
with the consumer. See the [extraction boundary](docs/careervector-extraction.md)
for API/MCP integration, database request reductions and transaction guarantees.

The portable core defines records, revisions, commit receipts, snapshots and a
resumable checkpoint transfer contract. The Linux filesystem implementation reads
ordinary files without changing their formats and uses a separate control tree
for locking, retained content, receipts and recovery. Backups publish complete
immutable checkpoints; restoration targets a new directory without replacing
existing content.

External editors can change source files directly. Strict conditional writes and
coherent snapshots require those editors to hand off while cstore writes/captures;
all cstore writers coordinate through the same control directory. Symlinks,
special files and unsupported atomic multi-record writes currently produce errors,
never silently incomplete backups. Payloads and retained history consume memory
one blob at a time; streaming large individual blobs remains future work.

See [CCVL's existing filesystem contract](docs/ccvl-filesystem.md) for the consumer
layout used to ground the implementation. Connecting these filesystem primitives
to CareerVector's Yjs workspace is the next adapter stage. Automatic merging of independent offline edits,
backup scheduling and physical retention cleanup are separate product policies.

## Capture and restore

The caller selects the workspace scope and supplies stable store/request IDs.
Keep the control directory and archive outside the payload tree. A checkpoint
captures every file under the selected root, including ignored files; consumer
adapters must define the intended scope before invoking it.

```rust,no_run
# #[cfg(all(feature = "filesystem", target_os = "linux"))]
# fn example() -> cstore::Result<()> {
use cstore::{FileArchive, FileStore, Store, restore_files, transfer};

let mut local = FileStore::open("workspace", "workspace-state", "local-1")?;
let snapshot = local.checkpoint()?;
let mut backup = FileArchive::open("workspace-backup")?;
let checkpoint = transfer(&local, &snapshot, &mut backup, "backup-request-1")?;

// Select a retained checkpoint. The restore path must not already exist.
let retained = backup.snapshot(&checkpoint)?;
restore_files(&backup, &retained, "./restored-workspace")?;
let mut restored = FileStore::open(
    "restored-workspace", "restored-state", "offline-1",
)?;
restored.adopt_snapshot(&backup, &retained)?;
# Ok(())
# }
```

`restore_files` publishes only the payload tree. `adopt_snapshot` then verifies
that tree and retains opaque metadata, historical blobs, source receipts and
lineage in the separate destination control store. If adoption is interrupted,
retry it against the same retained checkpoint before enabling writes. Imported
receipts remain provenance; destination commits get destination-local identities.

Use `compare(base, local, remote)` to classify changes against an explicitly
retained common base. It reports local, remote, identical and conflicting changes;
the caller verifies that the supplied base is the intended common ancestor and
decides whether to merge or switch authority.

The current file adapter preserves regular file bytes, paths, directories and
ordinary Unix file permission bits. It does not preserve ownership, timestamps,
directory permission modes, ACLs, extended attributes, hard-link relationships or
filesystem snapshots. Process-interruption recovery has executable fault tests;
power-loss behavior and other operating systems require separate verification.
Observed history cannot reconstruct edits made before cstore captured them.

## Development

Use current stable Rust, or Node 24 for the TypeScript package's development checks.
Repository checks are defined in `.ci/ccid.toml` and run
through the shared ccid runner on GitHub Actions or Crow. The dependency lock is
resolved on the build service and retained with the source. The portable core can
be checked with filesystem support disabled.

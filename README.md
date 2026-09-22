# cstore

Shared versioned persistence and portable workspace backup/restore, licensed under
FSL-1.1-ALv2. Product adapters retain data meaning, authorization and authored file
layouts. Work is in progress; supported backends and verification are recorded
below as they are delivered.

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
layout used to ground the implementation. Database and connected CareerVector
adapters are not yet implemented. Automatic merging of independent offline edits,
backup scheduling and physical retention cleanup are separate product policies.

## Development

Use current stable Rust. Repository checks are defined in `.ci/ccid.toml` and run
through the shared ccid runner on GitHub Actions or Crow. The dependency lock is
resolved on the build service and retained with the source. The portable core can
be checked with filesystem support disabled.


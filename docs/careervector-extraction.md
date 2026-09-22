# CareerVector persistence extraction

cstore's TypeScript package extracts the existing Yjs and D1 persistence mechanisms
from CareerVector. The first consumer is CareerVector's server library, below the
existing API operation endpoint used by browser clients and MCP tools.

The seven sub-documents, Yjs binary representation, snapshot clocks, operation
receipts, actor identities and existing SQL tables remain the consumer's contract.
cstore does not introduce a second document model. The host supplies its deployed
compression functions, typed operations, validation and projection comparison.
Yjs is a peer dependency so the host and cstore use the same implementation.

| Shared mechanism | Consumer responsibility |
| --- | --- |
| Snapshot encoding, decoding and Y.Doc lifetime | Compression runtime and compatibility |
| Apply edits, capture merged updates, create validation probes | Typed operations, document schema and policy |
| Snapshot reads, inserts and coherent copies | Workspace creation and selected sub-document names |
| Atomic D1 batches and guarded operation receipts | CAS statement, retries after conflicts and product side effects |
| Transient D1 retry schedule and receipt lookup | Actor identity, operation IDs and acknowledgement/broadcast |

The package exports TypeScript source for Bun and TypeScript-aware bundlers such
as CareerVector's Vite/Workers toolchain. Plain Node consumers must compile or
bundle it; Node's source stripping does not cover dependencies in node_modules.

## Efficiency and atomicity

Inserting a known snapshot returns the acknowledged input instead of re-reading
it: one database request instead of two. Copying seven sub-documents uses one
coherent SELECT and one atomic INSERT batch: two requests instead of fourteen.
Copying preserves the stored bytes and clocks without decoding Yjs. These are
request-count improvements, not measured latency or CPU claims.

The mutation helper retains CareerVector's decode-once path and lazy validation
probe. The consumer can compare only the paths touched by a typed operation and
skip persistence when its projection does not change.

A zero-row CAS update must fail the transaction before any document can commit.
The immediately following receipt INSERT therefore deliberately violates the
existing workspace_id NOT NULL constraint when changes() is zero. D1 rolls back
the entire batch on this SQL error. An after-commit row-count check alone cannot
provide this guarantee. A host without atomic batch support is rejected before
writing. Tests exercise rollback, retries, duplicate operation IDs and copy
failure against actual SQLite, plus Yjs concurrent-update merging.

## Following work

This extraction does not yet make the Rust filesystem store a Yjs replica.
The next stage is a versioned workspace envelope that preserves complete Yjs
state and receipts, with a bidirectional adapter for CCVL's editable files.
Keep raw source, references, assets and unknown fields alongside their projected
values. Online migration, offline restoration, backup and concurrent merge need
separate conformance tests before a TUI integration can promise those behaviors.

CareerVector's product mutation catalog should stay authoritative while that
adapter is built. Moving it later requires separating its JobCache and product
schema dependencies; copying it into an independent implementation would allow
behavior to drift.

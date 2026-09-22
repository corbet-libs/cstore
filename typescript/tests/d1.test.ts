import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  atomicBatch, copySnapshots, findExistingOpClock, insertOpLogAfterSuccessfulCas,
  insertSnapshot, loadSnapshot, loadSnapshots,
} from '../d1.ts';
import type { SnapshotDatabase } from '../d1.ts';
import { database } from './sqlite.ts';

const bytes = new Uint8Array([1, 2, 3]);
const row = (sub_doc: string, workspace_id = 'source') => ({
  workspace_id, sub_doc, snapshot_clock: 0, snapshot_bytes: bytes, updated_ms: 10,
});

test('snapshot insertion needs one write and no confirming read', async () => {
  const { db, sqlite, calls } = database();
  try {
    assert.deepEqual(await insertSnapshot(db, row('cv')), row('cv'));
    assert.deepEqual(calls, { first: 0, all: 0, run: 1, batch: 0 });
    assert.deepEqual((await loadSnapshot(db, 'source', 'cv'))?.snapshot_bytes, bytes);
  } finally { sqlite.close(); }
});

test('seven-document copies use two calls and preserve bytes and clocks', async () => {
  const { db, sqlite, calls } = database();
  try {
    const documents = ['jobs', 'settings', 'layout', 'order-state', 'cv-profile', 'cl-profile', 'notes'];
    for (const name of documents) await insertSnapshot(db, row(name));
    Object.assign(calls, { first: 0, all: 0, run: 0, batch: 0 });
    assert.equal(await copySnapshots(db, 'source', 'target', documents, 20), 7);
    assert.deepEqual(calls, { first: 0, all: 1, run: 0, batch: 1 });
    const copied = await loadSnapshots(db, 'target', documents);
    assert.deepEqual(copied.map((entry) => entry.sub_doc), documents);
    for (const entry of copied) {
      assert.deepEqual(entry.snapshot_bytes, bytes);
      assert.equal(entry.snapshot_clock, 0);
      assert.equal(entry.updated_ms, 20);
    }
  } finally { sqlite.close(); }
});

test('missing sources and destination collisions never leave a partial copy', async () => {
  const { db, sqlite } = database();
  try {
    await insertSnapshot(db, row('a'));
    await assert.rejects(copySnapshots(db, 'source', 'target', ['a', 'b'], 20), /sub_doc_missing/);
    assert.equal(await loadSnapshot(db, 'target', 'a'), null);
    await insertSnapshot(db, row('b'));
    await insertSnapshot(db, row('b', 'target'));
    await assert.rejects(copySnapshots(db, 'source', 'target', ['a', 'b'], 20), /UNIQUE/);
    assert.equal(await loadSnapshot(db, 'target', 'a'), null);
    assert.equal((await loadSnapshot(db, 'target', 'b'))?.updated_ms, 10);
  } finally { sqlite.close(); }
});

function commitPair(db: SnapshotDatabase, subDoc: string, expected: number, id: string) {
  return [
    db.prepare(`UPDATE workspace_sub_doc SET snapshot_clock = ?1, snapshot_bytes = ?2
      WHERE workspace_id = ?3 AND sub_doc = ?4 AND snapshot_clock = ?5`)
      .bind(expected + 1, new Uint8Array([9]), 'source', subDoc, expected),
    insertOpLogAfterSuccessfulCas(db, {
      workspaceId: 'source', subDoc, clock: expected + 1, opBytes: bytes,
      actorClass: 'agent', actorIdValue: 'fixture:mcp:none', nowMs: 30, opId: id,
    }),
  ];
}

test('a stale second CAS rolls back the first document and its receipt', async () => {
  const { db, sqlite } = database();
  try {
    await insertSnapshot(db, row('a'));
    await insertSnapshot(db, { ...row('b'), snapshot_clock: 1 });
    await assert.rejects(atomicBatch(db, [
      ...commitPair(db, 'a', 0, 'batch-1'), ...commitPair(db, 'b', 0, 'batch-1'),
    ]), /NOT NULL/);
    assert.equal((await loadSnapshot(db, 'source', 'a'))?.snapshot_clock, 0);
    assert.equal((await loadSnapshot(db, 'source', 'b'))?.snapshot_clock, 1);
    assert.equal(await findExistingOpClock(db, 'source', 'a', 'batch-1'), null);
    await atomicBatch(db, [
      ...commitPair(db, 'a', 0, 'batch-1'), ...commitPair(db, 'b', 1, 'batch-1'),
    ]);
    assert.equal(await findExistingOpClock(db, 'source', 'a', 'batch-1'), 1);
    assert.equal(await findExistingOpClock(db, 'source', 'b', 'batch-1'), 2);
    const actor = sqlite.prepare('SELECT actor_class, actor_id FROM workspace_sub_doc_ops LIMIT 1').get();
    assert.equal(actor?.actor_id, 'fixture:mcp:none');
    assert.equal(actor?.actor_class, 'agent');
  } finally { sqlite.close(); }
});

test('duplicate operation receipt rolls its accompanying snapshot change back', async () => {
  const { db, sqlite } = database();
  try {
    await insertSnapshot(db, row('a'));
    await atomicBatch(db, commitPair(db, 'a', 0, 'same-request'));
    await assert.rejects(atomicBatch(db, commitPair(db, 'a', 1, 'same-request')), /UNIQUE/);
    assert.equal((await loadSnapshot(db, 'source', 'a'))?.snapshot_clock, 1);
    assert.equal(await findExistingOpClock(db, 'source', 'a', 'same-request'), 1);
  } finally { sqlite.close(); }
});

test('adapters without atomic batches fail before any write', async () => {
  let writes = 0;
  const db = { prepare() { writes++; } } as unknown as SnapshotDatabase;
  await assert.rejects(atomicBatch(db, [{} as ReturnType<SnapshotDatabase['prepare']>]), /atomic database batch is required/);
  assert.equal(writes, 0);
});

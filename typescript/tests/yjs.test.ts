import assert from 'node:assert/strict';
import { test } from 'node:test';
import * as Y from 'yjs';
import { applySnapshotMutation, createSnapshotCodec, readSnapshotProjection } from '../yjs.ts';
import type { SnapshotRow } from '../yjs.ts';

function codec() {
  const counts = { decode: 0, encode: 0 };
  return { counts, codec: createSnapshotCodec({
    async compress(bytes, level) { counts.encode++; assert.equal(level, 3); return bytes; },
    async decompress(bytes) { counts.decode++; return new Uint8Array(bytes); },
  }) };
}

function snapshot(doc: Y.Doc): SnapshotRow {
  return { workspace_id: 'fixture', sub_doc: 'prose', snapshot_clock: 7,
    snapshot_bytes: Y.encodeStateAsUpdate(doc), updated_ms: 10 };
}

test('extracted mutations retain causality and converge with an independent peer', async () => {
  const base = new Y.Doc();
  const peer = new Y.Doc();
  const resultDoc = new Y.Doc();
  try {
    base.getText('prose').insert(0, 'AB');
    const row = snapshot(base);
    Y.applyUpdate(peer, row.snapshot_bytes as Uint8Array);
    const vector = Y.encodeStateVector(peer);
    peer.getText('prose').insert(1, 'peer');
    const { counts, codec: storage } = codec();
    let validated = 0;
    const result = await applySnapshotMutation(row, storage, {
      apply(doc) { doc.getText('prose').insert(1, 'local'); },
      async validateUpdate(before, delta, after) {
        validated++;
        assert.equal(before.getText('prose').toString(), 'AB');
        assert.equal(after.getText('prose').toString(), 'AlocalB');
        assert.ok(delta.byteLength > 0);
      },
    });
    assert.equal(result.currentClock, 7);
    assert.equal(result.changed, true);
    assert.equal(counts.decode, 1);
    assert.equal(validated, 1);
    Y.applyUpdate(resultDoc, result.rawSnapshot!);
    Y.applyUpdate(resultDoc, Y.encodeStateAsUpdate(peer, vector));
    Y.applyUpdate(peer, result.broadcastUpdate!);
    assert.equal(resultDoc.getText('prose').toString(), peer.getText('prose').toString());
    assert.match(peer.getText('prose').toString(), /local/);
    assert.match(peer.getText('prose').toString(), /peer/);
    const previousVector = Y.encodeStateVector(peer);
    Y.applyUpdate(peer, result.broadcastUpdate!);
    assert.deepEqual(Y.encodeStateVector(peer), previousVector);
  } finally { base.destroy(); peer.destroy(); resultDoc.destroy(); }
});

test('no-op mutations retain clocks, run policy, and produce no snapshot or delta', async () => {
  const base = new Y.Doc();
  try {
    const { counts, codec: storage } = codec();
    let checked = 0;
    const result = await applySnapshotMutation(snapshot(base), storage, {
      apply() {}, validateState() { checked++; },
      async validateUpdate() { assert.fail('no update to validate'); },
    });
    assert.deepEqual(result, { currentClock: 7, changed: false });
    assert.equal(checked, 1);
    assert.equal(counts.decode, 1);
    assert.equal(counts.encode, 0);
  } finally { base.destroy(); }
});

test('validation failure discards the candidate and destroys its documents', async () => {
  const base = new Y.Doc();
  const { codec: storage } = codec();
  let destroyed = 0;
  try {
    await assert.rejects(applySnapshotMutation(snapshot(base), storage, {
      apply(doc) { doc.on('destroy', () => destroyed++); doc.getMap('data').set('x', 1); },
      async validateUpdate(before) { before.on('destroy', () => destroyed++); throw new Error('policy-rejected'); },
    }), /policy-rejected/);
    assert.equal(destroyed, 2);
    assert.equal(base.getMap('data').size, 0);
  } finally { base.destroy(); }
});

test('storage codec keeps raw fallback compatibility and projection lifetime', async () => {
  const storage = createSnapshotCodec({
    async compress() { throw new Error('codec unavailable'); },
    async decompress() { throw new Error('raw input'); },
  });
  const base = new Y.Doc();
  try {
    base.getMap('data').set('unknown', { retained: [1, 2] });
    const raw = Y.encodeStateAsUpdate(base);
    assert.deepEqual(await storage.encodeDoc(base), raw);
    const view = await readSnapshotProjection(snapshot(base), storage, (doc) => doc.getMap('data').toJSON());
    assert.deepEqual(view, { unknown: { retained: [1, 2] } });
    assert.deepEqual(await storage.encodeJson({ opId: 'same' }), new TextEncoder().encode('{"opId":"same"}'));
  } finally { base.destroy(); }
});

// Extracted from CareerVector lib/server/src/workspace-subdoc.ts. See NOTICE.
import * as Y from 'yjs';

export interface SnapshotRow {
  workspace_id: string;
  sub_doc: string;
  snapshot_clock: number;
  snapshot_bytes: ArrayBuffer | Uint8Array;
  updated_ms: number;
}

export interface StorageCompression {
  compress(bytes: Uint8Array, level: number): Promise<Uint8Array>;
  decompress(bytes: ArrayBuffer | Uint8Array): Promise<Uint8Array>;
}

/** The host retains its deployed compression format and runtime loader. */
export function createSnapshotCodec(compression: StorageCompression, level = 3) {
  const encode = async (bytes: Uint8Array): Promise<Uint8Array> => {
    try { return await compression.compress(bytes, level); }
    catch { return bytes; }
  };
  const decode = async (bytes: ArrayBuffer | Uint8Array): Promise<Uint8Array> => {
    try { return await compression.decompress(bytes); }
    catch { return bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes); }
  };
  return {
    encode,
    decode,
    encodeDoc: (doc: Y.Doc) => encode(Y.encodeStateAsUpdate(doc)),
    encodeJson: (value: unknown) => encode(new TextEncoder().encode(JSON.stringify(value))),
  };
}

export type SnapshotCodec = ReturnType<typeof createSnapshotCodec>;

export async function readSnapshotProjection<T>(
  row: SnapshotRow,
  codec: Pick<SnapshotCodec, 'decode'>,
  project: (doc: Y.Doc) => T,
): Promise<T> {
  const doc = new Y.Doc({ gc: true });
  try {
    Y.applyUpdate(doc, await codec.decode(row.snapshot_bytes), 'snapshot');
    return project(doc);
  } finally {
    doc.destroy();
  }
}

export interface MutationResult {
  currentClock: number;
  changed: boolean;
  rawSnapshot?: Uint8Array;
  broadcastUpdate?: Uint8Array;
}

export interface SnapshotMutation {
  /** Product-owned typed operations run here; this does not grant authorization. */
  apply(doc: Y.Doc): void;
  validateUpdate?(before: Y.Doc, update: Uint8Array, after: Y.Doc): Promise<void>;
  /** Called even for no-op edits, preserving product policy checks. */
  validateState?(after: Y.Doc): void;
  /** Product projection comparison can omit writes that have no semantic effect. */
  changed?(before: Y.Doc, after: Y.Doc): boolean;
}

/**
 * Retain Yjs causality while applying existing product operations. Decode storage
 * once; construct the validation probe only when Yjs emits an update. No write or
 * peer notification occurs here: the caller publishes only after durable commit.
 */
export async function applySnapshotMutation(
  row: SnapshotRow | null,
  codec: Pick<SnapshotCodec, 'decode'>,
  mutation: SnapshotMutation,
): Promise<MutationResult> {
  const doc = new Y.Doc({ gc: true });
  let before: Y.Doc | undefined;
  const currentClock = row?.snapshot_clock ?? 0;
  try {
    const snapshot = row ? await codec.decode(row.snapshot_bytes) : undefined;
    if (snapshot) Y.applyUpdate(doc, snapshot, 'snapshot');
    const updates: Uint8Array[] = [];
    const onUpdate = (update: Uint8Array) => updates.push(update);
    doc.on('update', onUpdate);
    try { mutation.apply(doc); }
    finally { doc.off('update', onUpdate); }
    let update: Uint8Array | undefined;
    if (updates.length > 0) {
      update = updates.length === 1 ? updates[0] : Y.mergeUpdates(updates);
      before = new Y.Doc({ gc: true });
      if (snapshot) Y.applyUpdate(before, snapshot, 'snapshot');
      await mutation.validateUpdate?.(before, update, doc);
    }
    mutation.validateState?.(doc);
    if (!update || !before || mutation.changed?.(before, doc) === false) {
      return { currentClock, changed: false };
    }
    return {
      currentClock,
      changed: true,
      rawSnapshot: Y.encodeStateAsUpdate(doc),
      broadcastUpdate: update,
    };
  } finally {
    before?.destroy();
    doc.destroy();
  }
}

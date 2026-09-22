// Extracted from CareerVector lib/server/src/workspace-subdoc.ts. See NOTICE.
import { retryTransientD1 } from './retry.js';
import type { SnapshotRow } from './yjs.js';

/** Structural interfaces accepted by the existing Worker D1 binding. */
export interface PreparedStatement {
  bind(...values: unknown[]): PreparedStatement;
  first<T = Record<string, unknown>>(): Promise<T | null>;
  all<T = Record<string, unknown>>(): Promise<{ results: T[] }>;
  run(): Promise<unknown>;
}
export interface SnapshotDatabase {
  prepare(sql: string): PreparedStatement;
  batch(statements: PreparedStatement[]): Promise<unknown[]>;
}

export function d1Changes(result: unknown): number | undefined {
  const meta = (result as { meta?: { changes?: number; rows_written?: number } })?.meta;
  if (typeof meta?.changes === 'number') return meta.changes;
  if (typeof meta?.rows_written === 'number') return meta.rows_written;
  return undefined;
}

/** Fail before writes when a host cannot supply an atomic batch. */
export async function atomicBatch(db: SnapshotDatabase, statements: PreparedStatement[]): Promise<unknown[]> {
  if (statements.length === 0) return [];
  if (typeof db.batch !== 'function') throw new Error('cstore: atomic database batch is required');
  const results = await retryTransientD1(() => db.batch(statements));
  if (!Array.isArray(results) || results.length !== statements.length) {
    throw new Error('cstore: incomplete database batch receipt; outcome must be reconciled');
  }
  return results;
}

export async function loadSnapshot(db: SnapshotDatabase, workspaceId: string, subDoc: string): Promise<SnapshotRow | null> {
  return retryTransientD1(() => db.prepare(
    'SELECT workspace_id, sub_doc, snapshot_clock, snapshot_bytes, updated_ms FROM workspace_sub_doc WHERE workspace_id = ?1 AND sub_doc = ?2',
  ).bind(workspaceId, subDoc).first<SnapshotRow>());
}

/** One coherent SQL read, returned in the caller's requested order. */
export async function loadSnapshots(db: SnapshotDatabase, workspaceId: string, subDocs: readonly string[]): Promise<SnapshotRow[]> {
  if (subDocs.length === 0) return [];
  if (new Set(subDocs).size !== subDocs.length) throw new Error('cstore: duplicate sub-document');
  const placeholders = subDocs.map((_, index) => `?${index + 2}`).join(', ');
  const rows = await retryTransientD1(() => db.prepare(
    `SELECT workspace_id, sub_doc, snapshot_clock, snapshot_bytes, updated_ms FROM workspace_sub_doc WHERE workspace_id = ?1 AND sub_doc IN (${placeholders})`,
  ).bind(workspaceId, ...subDocs).all<SnapshotRow>());
  const indexed = new Map(rows.results.map((row) => [row.sub_doc, row]));
  return subDocs.map((subDoc) => {
    const row = indexed.get(subDoc);
    if (!row) throw new Error(`sub_doc_missing: ${workspaceId}/${subDoc}`);
    return row;
  });
}

export function snapshotInsert(db: SnapshotDatabase, row: SnapshotRow): PreparedStatement {
  return db.prepare(
    `INSERT INTO workspace_sub_doc
     (workspace_id, sub_doc, snapshot_clock, snapshot_bytes, updated_ms)
     VALUES (?1, ?2, ?3, ?4, ?5)`,
  ).bind(row.workspace_id, row.sub_doc, row.snapshot_clock, row.snapshot_bytes, row.updated_ms);
}

/** The acknowledged inserted row is already known; avoid a second database read. */
export async function insertSnapshot(db: SnapshotDatabase, row: SnapshotRow): Promise<SnapshotRow> {
  await retryTransientD1(() => snapshotInsert(db, row).run());
  return row;
}

/** Copy a selected snapshot set in two database calls, without decoding Yjs. */
export async function copySnapshots(
  db: SnapshotDatabase, sourceId: string, targetId: string,
  subDocs: readonly string[], updatedMs: number,
): Promise<number> {
  const rows = await loadSnapshots(db, sourceId, subDocs);
  await atomicBatch(db, rows.map((row) => snapshotInsert(db, {
    ...row, workspace_id: targetId, updated_ms: updatedMs,
  })));
  return rows.length;
}

export interface OperationReceiptInput {
  workspaceId: string;
  subDoc: string;
  clock: number;
  opBytes: Uint8Array;
  actorClass: string;
  actorIdValue: string;
  nowMs: number;
  opId: string | null;
}

/**
 * Must immediately follow its CAS UPDATE in the same atomic batch. A stale CAS
 * must fail SQL, not merely insert zero rows: the existing NOT NULL constraint
 * then rolls back every document and receipt in a cross-document transaction.
 */
export function insertOpLogAfterSuccessfulCas(db: SnapshotDatabase, input: OperationReceiptInput): PreparedStatement {
  return db.prepare(
    `INSERT INTO workspace_sub_doc_ops
     (workspace_id, sub_doc, clock, op_bytes, actor_class, actor_id, ts_ms, op_id)
     SELECT CASE WHEN changes() = 1 THEN ?1 ELSE NULL END, ?2, ?3, ?4, ?5, ?6, ?7, ?8`,
  ).bind(input.workspaceId, input.subDoc, input.clock, input.opBytes,
    input.actorClass, input.actorIdValue, input.nowMs, input.opId);
}

export async function findExistingOpClock(db: SnapshotDatabase, workspaceId: string, subDoc: string, opId: string): Promise<number | null> {
  const row = await retryTransientD1(() => db.prepare(
    'SELECT clock FROM workspace_sub_doc_ops WHERE workspace_id = ?1 AND sub_doc = ?2 AND op_id = ?3',
  ).bind(workspaceId, subDoc, opId).first<{ clock: number }>());
  return row?.clock ?? null;
}

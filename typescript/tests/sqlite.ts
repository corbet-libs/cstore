import { DatabaseSync } from 'node:sqlite';
import type { SQLInputValue } from 'node:sqlite';
import type { PreparedStatement, SnapshotDatabase } from '../d1.ts';

/** Real SQLite behind the small D1 binding surface; no deployed resources. */
export function database() {
  const sqlite = new DatabaseSync(':memory:');
  sqlite.exec(`
    CREATE TABLE workspace_sub_doc (
      workspace_id TEXT NOT NULL, sub_doc TEXT NOT NULL,
      snapshot_clock INTEGER NOT NULL, snapshot_bytes BLOB NOT NULL,
      updated_ms INTEGER NOT NULL, PRIMARY KEY(workspace_id, sub_doc)
    );
    CREATE TABLE workspace_sub_doc_ops (
      workspace_id TEXT NOT NULL, sub_doc TEXT NOT NULL, clock INTEGER NOT NULL,
      op_bytes BLOB NOT NULL, actor_class TEXT NOT NULL, actor_id TEXT NOT NULL,
      ts_ms INTEGER NOT NULL, op_id TEXT,
      PRIMARY KEY(workspace_id, sub_doc, clock), UNIQUE(workspace_id, sub_doc, op_id)
    );
  `);
  const calls = { first: 0, all: 0, run: 0, batch: 0 };
  let inBatch = false;
  const db: SnapshotDatabase = {
    prepare(sql) {
      let values: Record<string, SQLInputValue> = {};
      const statement: PreparedStatement = {
        bind(...bound) {
          values = Object.fromEntries(bound.map((value, index) => [
            String(index + 1), value instanceof ArrayBuffer ? new Uint8Array(value) : value as SQLInputValue,
          ]));
          return statement;
        },
        async first<T>() {
          if (!inBatch) calls.first++;
          return (sqlite.prepare(sql).get(values) ?? null) as T | null;
        },
        async all<T>() {
          if (!inBatch) calls.all++;
          return { results: sqlite.prepare(sql).all(values) as T[] };
        },
        async run() {
          if (!inBatch) calls.run++;
          return { meta: { changes: Number(sqlite.prepare(sql).run(values).changes) } };
        },
      };
      return statement;
    },
    async batch(statements) {
      calls.batch++;
      sqlite.exec('BEGIN');
      inBatch = true;
      try {
        const results = [];
        for (const statement of statements) results.push(await statement.run());
        sqlite.exec('COMMIT');
        return results;
      } catch (error) {
        sqlite.exec('ROLLBACK');
        throw error;
      } finally { inBatch = false; }
    },
  };
  return { db, sqlite, calls };
}

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { retryTransientD1 } from '../retry.ts';

test('existing transient retry budget is bounded and preserves the final error', async () => {
  const error = new Error('SQLITE_BUSY');
  const delays: number[] = [];
  let calls = 0;
  await assert.rejects(retryTransientD1(async () => { calls++; throw error; }, async (ms) => { delays.push(ms); }), error);
  assert.equal(calls, 5);
  assert.deepEqual(delays, [25, 75, 150, 300]);
  calls = 0;
  await assert.rejects(retryTransientD1(async () => { calls++; throw new Error('invalid schema'); }), /invalid schema/);
  assert.equal(calls, 1);
});

// Extracted from CareerVector lib/server/src/d1-retry.ts. See NOTICE.
const RETRY_DELAYS_MS = [25, 75, 150, 300];

export function isTransientD1Error(error: unknown): boolean {
  const message = error instanceof Error ? error.message : String(error);
  return ['SQLITE_BUSY', 'database is locked', 'SQLITE_BUSY_SNAPSHOT',
    'Failed to parse body as JSON', 'Unexpected end of JSON input']
    .some((part) => message.includes(part));
}

export async function retryTransientD1<T>(
  operation: () => Promise<T>,
  wait: (ms: number) => Promise<void> = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await operation();
    } catch (error) {
      if (!isTransientD1Error(error) || attempt >= RETRY_DELAYS_MS.length) throw error;
      await wait(RETRY_DELAYS_MS[attempt]);
    }
  }
}

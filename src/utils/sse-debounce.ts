type EventApplier<T> = {
  onSnapshot: (data: T) => void;
  onMutation: (events: Record<string, unknown>[]) => void;
};

/**
 * Create a debounced SSE event handler that batches mutations.
 * Snapshot events bypass debounce and apply immediately.
 * Mutation events (created/updated/deleted) accumulate and batch-apply after `delay` ms of idle.
 */
export function createDebouncedSseHandler<T>(
  applier: EventApplier<T>,
  snapshotKey: string,
  delay = 200
): (payload: Record<string, unknown>) => void {
  let buffer: Record<string, unknown>[] = [];
  let timer: ReturnType<typeof setTimeout> | null = null;

  function flush() {
    if (buffer.length > 0) {
      const batch = buffer;
      buffer = [];
      applier.onMutation(batch);
    }
    timer = null;
  }

  return (payload: Record<string, unknown>) => {
    const type = payload.type as string;

    if (type === 'snapshot' && payload[snapshotKey] != null) {
      // Snapshots bypass debounce — clear buffer and apply immediately
      if (timer) {
        clearTimeout(timer);
        timer = null;
      }
      buffer = [];
      applier.onSnapshot(payload[snapshotKey] as T);
      return;
    }

    // Buffer mutation events
    buffer.push(payload);
    if (timer) clearTimeout(timer);
    timer = setTimeout(flush, delay);
  };
}

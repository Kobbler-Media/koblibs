export type SseConnection = {
  stop(): void;
};

export type SseOptions = {
  errorMessage?: string;
  disconnectThreshold?: number;
};

export type SseConfig = {
  getAuthToken: () => string | null;
  apiBase?: string;
};

/**
 * Create an SSE connection with automatic reconnection and auth.
 */
export function createSseConnection(
  config: SseConfig,
  path: string,
  onEvent: (payload: Record<string, unknown>) => void,
  onError: (error: string | null) => void,
  options?: SseOptions
): SseConnection {
  const errorMessage = options?.errorMessage ?? 'Connection lost. Reconnecting...';
  const disconnectThreshold = options?.disconnectThreshold ?? 5000;
  const apiBase = config.apiBase ?? '';

  let abortController: AbortController | null = null;
  let reconnectAttempt = 0;
  let reconnectTimeout: ReturnType<typeof setTimeout> | null = null;
  let disconnectedAt: number | null = null;
  let stopped = false;

  const parseSseBuffer = (
    buffer: string
  ): { partial: string; events: Record<string, unknown>[] } => {
    const parts = buffer.split('\n\n');
    const partial = parts.pop() ?? '';
    const events: Record<string, unknown>[] = [];

    for (const part of parts) {
      const dataLines = part
        .split('\n')
        .filter((line) => line.startsWith('data:'))
        .map((line) => line.replace(/^data:\s?/, ''))
        .join('\n');
      if (!dataLines) continue;
      try {
        const payload = JSON.parse(dataLines);
        if (typeof payload === 'object' && payload !== null) {
          events.push(payload);
        }
      } catch {
        // Skip unparseable events
      }
    }

    return { partial, events };
  };

  const connect = async () => {
    if (stopped) return;

    const token = config.getAuthToken();
    if (!token) return;

    if (abortController) {
      abortController.abort();
    }
    abortController = new AbortController();

    let buffer = '';
    let receivedData = false;

    try {
      const response = await fetch(`${apiBase}${path}`, {
        headers: { Authorization: `Bearer ${token}` },
        signal: abortController.signal
      });

      if (!response.ok || !response.body) {
        throw new Error(`Stream failed (${response.status})`);
      }

      const reader = response.body.getReader();
      const decoder = new TextDecoder();

      while (true) {
        const { value, done } = await reader.read();
        if (done) break;

        if (!receivedData) {
          receivedData = true;
          if (disconnectedAt !== null || reconnectAttempt > 0) {
            disconnectedAt = null;
            reconnectAttempt = 0;
            onError(null);
          }
        }

        buffer += decoder.decode(value, { stream: true });
        const result = parseSseBuffer(buffer);
        buffer = result.partial;
        for (const event of result.events) {
          onEvent(event);
        }
      }
    } catch {
      if (stopped || abortController?.signal.aborted) return;

      if (disconnectedAt === null) {
        disconnectedAt = Date.now();
      }

      const disconnectedFor = Date.now() - disconnectedAt;
      if (disconnectedFor >= disconnectThreshold) {
        onError(errorMessage);
      }

      reconnectAttempt++;
      const delay =
        reconnectAttempt === 1 ? 0 : Math.min(1000 * Math.pow(2, reconnectAttempt - 2), 30000);
      reconnectTimeout = setTimeout(connect, delay);
    }
  };

  const stop = () => {
    stopped = true;
    if (reconnectTimeout) {
      clearTimeout(reconnectTimeout);
      reconnectTimeout = null;
    }
    if (abortController) {
      abortController.abort();
      abortController = null;
    }
    disconnectedAt = null;
    reconnectAttempt = 0;
  };

  connect();

  return { stop };
}

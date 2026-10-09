import type { FaEvent } from './fa-playback.svelte';

// tts-breeze emits 24 kHz mono. A mismatch here is a pitch shift, not an
// exception, so it must track the engine.
const TTS_SAMPLE_RATE = 24_000;

export interface ParseTtsSseOptions {
  text: string;
}

export async function* parseTtsSse(
  body: ReadableStream<Uint8Array>,
  opts: ParseTtsSseOptions
): AsyncGenerator<FaEvent> {
  yield {
    type: 'paragraph',
    paragraphIndex: 0,
    wordOffset: 0,
    audioOffsetMs: 0,
    sampleRate: TTS_SAMPLE_RATE,
    text: opts.text
  };

  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';

  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });

      let sepAt: number;
      while ((sepAt = buffer.indexOf('\n\n')) >= 0) {
        const raw = buffer.slice(0, sepAt);
        buffer = buffer.slice(sepAt + 2);
        const ev = decodeSseEvent(raw);
        if (ev) yield ev;
      }
    }

    buffer += decoder.decode();
    if (buffer.trim()) {
      const ev = decodeSseEvent(buffer);
      if (ev) yield ev;
    }
  } finally {
    reader.releaseLock();
  }
}

function decodeSseEvent(raw: string): FaEvent | null {
  let data = '';
  for (const line of raw.split('\n')) {
    const trimmed = line.replace(/\r$/, '');
    if (!trimmed.startsWith('data:')) continue;
    const rest = trimmed.slice(5).replace(/^ /, '');
    if (data) data += '\n';
    data += rest;
  }
  if (!data) return null;

  let json: Record<string, unknown>;
  try {
    json = JSON.parse(data);
  } catch {
    return null;
  }

  const kind = typeof json.type === 'string' ? json.type : null;
  if (!kind) return null;

  switch (kind) {
    case 'speech.audio.delta': {
      const audioB64 = typeof json.audio === 'string' ? json.audio : null;
      if (!audioB64) return null;
      return {
        type: 'audio',
        paragraphIndex: 0,
        format: 'pcm',
        pcm: base64ToBytes(audioB64),
        sampleRate: TTS_SAMPLE_RATE
      };
    }
    case 'speech.audio.alignment.partial':
    case 'speech.audio.alignment.final': {
      const final = kind === 'speech.audio.alignment.final';
      const audioMs = final
        ? numField(json, 'audio_total_ms')
        : numField(json, 'audio_seen_ms');
      const words = parseWords(json.words);
      if (!words) return null;
      return { type: 'alignment', final, paragraphIndex: 0, audioMs, words };
    }
    case 'speech.audio.alignment.error': {
      const message =
        typeof json.error === 'string' ? json.error : '(no message)';
      return { type: 'error', message };
    }
    case 'speech.audio.done':
      return { type: 'done', truncated: json.truncated === true };
    default:
      return null;
  }
}

function parseWords(
  v: unknown
): Array<{ wordIndex: number; text: string; t0Ms: number; t1Ms: number }> | null {
  if (!Array.isArray(v)) return null;
  const out = [];
  for (const w of v) {
    if (!w || typeof w !== 'object') return null;
    const obj = w as Record<string, unknown>;
    const wordIndex = numField(obj, 'word_index');
    const text = typeof obj.text === 'string' ? obj.text : null;
    const t0Ms = numField(obj, 't0_ms');
    const t1Ms = numField(obj, 't1_ms');
    if (text == null) return null;
    out.push({ wordIndex, text, t0Ms, t1Ms });
  }
  return out;
}

function numField(obj: Record<string, unknown>, key: string): number {
  const v = obj[key];
  return typeof v === 'number' && Number.isFinite(v) ? v : 0;
}

function base64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

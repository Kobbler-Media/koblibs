import type { FaEvent } from './fa-playback.svelte';

export interface ParseKobblerNdjsonOptions {
  text: string;
}

export async function* parseKobblerNdjson(
  body: ReadableStream<Uint8Array>,
  opts: ParseKobblerNdjsonOptions
): AsyncGenerator<FaEvent> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  let emittedSentenceParagraph = false;

  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });

      let nl: number;
      while ((nl = buffer.indexOf('\n')) >= 0) {
        const line = buffer.slice(0, nl).trim();
        buffer = buffer.slice(nl + 1);
        if (!line) continue;
        yield* decodeNdjsonLine(line, opts, () => {
          const first = !emittedSentenceParagraph;
          emittedSentenceParagraph = true;
          return first;
        });
      }
    }

    buffer += decoder.decode();
    const last = buffer.trim();
    if (last) {
      yield* decodeNdjsonLine(last, opts, () => {
        const first = !emittedSentenceParagraph;
        emittedSentenceParagraph = true;
        return first;
      });
    }
  } finally {
    reader.releaseLock();
  }
}

function* decodeNdjsonLine(
  line: string,
  opts: ParseKobblerNdjsonOptions,
  markFirstSentence: () => boolean
): Generator<FaEvent> {
  let json: Record<string, unknown>;
  try {
    json = JSON.parse(line);
  } catch {
    return;
  }
  const kind = typeof json.type === 'string' ? json.type : null;
  if (!kind) return;

  switch (kind) {
    case 'paragraph_start':
      yield {
        type: 'paragraph',
        paragraphIndex: numField(json, 'index'),
        wordOffset: numField(json, 'word_offset'),
        audioOffsetMs: numField(json, 'audio_offset_ms'),
        sampleRate: numField(json, 'sample_rate') || 24_000,
        text: typeof json.text === 'string' ? json.text : '',
        // What the server RENDERED at. Absent on older backends, which only
        // ever rendered at 1.0.
        speed: typeof json.speed === 'number' ? json.speed : undefined
      };
      return;

    case 'audio': {
      const pcmB64 = typeof json.pcm_b64 === 'string' ? json.pcm_b64 : null;
      if (!pcmB64) return;
      yield {
        type: 'audio',
        paragraphIndex: numField(json, 'paragraph_index'),
        format: 'pcm',
        pcm: base64ToBytes(pcmB64),
        sampleRate: 24_000
      };
      return;
    }

    case 'sentence': {
      const audioB64 = typeof json.audio_b64 === 'string' ? json.audio_b64 : null;
      if (!audioB64) return;
      if (markFirstSentence()) {
        yield {
          type: 'paragraph',
          paragraphIndex: 0,
          wordOffset: 0,
          audioOffsetMs: 0,
          sampleRate: 0,
          text: opts.text,
          speed: typeof json.speed === 'number' ? json.speed : undefined
        };
      }
      yield {
        type: 'audio',
        paragraphIndex: 0,
        format: 'mp3',
        pcm: base64ToBytes(audioB64),
        sampleRate: 0
      };
      return;
    }

    case 'alignment_partial':
    case 'alignment_final': {
      const final = kind === 'alignment_final';
      const audioMs = final
        ? numField(json, 'audio_total_ms')
        : numField(json, 'audio_seen_ms');
      const words = parseWords(json.words);
      if (!words) return;
      yield {
        type: 'alignment',
        final,
        paragraphIndex: numField(json, 'paragraph_index'),
        audioMs,
        words
      };
      return;
    }

    case 'error': {
      const message =
        typeof json.message === 'string' ? json.message : '(no message)';
      const paragraphIndex =
        typeof json.paragraph_index === 'number' ? json.paragraph_index : undefined;
      yield { type: 'error', message, paragraphIndex };
      return;
    }

    case 'done':
      yield { type: 'done' };
      return;
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
    const text = typeof obj.text === 'string' ? obj.text : null;
    if (text == null) return null;
    out.push({
      wordIndex: numField(obj, 'word_index'),
      text,
      t0Ms: numField(obj, 't0_ms'),
      t1Ms: numField(obj, 't1_ms')
    });
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

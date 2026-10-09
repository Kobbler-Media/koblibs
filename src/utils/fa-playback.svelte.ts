export type FaEvent =
  | {
      type: 'paragraph';
      paragraphIndex: number;
      wordOffset: number;
      audioOffsetMs: number;
      sampleRate: number;
      text: string;
      /** The rate the server actually RENDERED at, which is not necessarily the
       *  rate that was asked for — an engine that cannot do the requested one
       *  rejects it and the render falls back to 1.0. Audio and word timings are
       *  both in this timebase already; it is reported so the UI can stop
       *  claiming a rate it did not get. */
      speed?: number;
    }
  | {
      type: 'audio';
      paragraphIndex: number;
      format: 'pcm' | 'mp3';
      pcm: Uint8Array;
      sampleRate: number;
    }
  | {
      type: 'alignment';
      final: boolean;
      paragraphIndex: number;
      audioMs: number;
      words: Array<{
        wordIndex: number;
        text: string;
        t0Ms: number;
        t1Ms: number;
      }>;
    }
  | { type: 'error'; message: string; paragraphIndex?: number }
  | {
      type: 'done';
      /** The engine stopped at `max_new_frames` mid-sentence and the audio is a
       *  valid but SHORT clip. Only reachable with `long: false`; treat it as a
       *  failure rather than playing the fragment. */
      truncated?: boolean;
    };

export type FaPlaybackState =
  | 'idle'
  | 'loading'
  | 'playing'
  | 'paused'
  | 'done'
  | 'error';

export interface FaWord {
  globalWordIndex: number;
  paragraphIndex: number;
  text: string;
  t0Ms: number;
  t1Ms: number;
}

export interface FaTimings {
  ttfaMs?: number;
  firstAlignMs?: number;
  audioMs?: number;
}

export interface CreateFaPlaybackOptions {
  events: AsyncIterable<FaEvent>;
  text: string;
  onError?: (message: string) => void;
  /// Aborted when `stop()` is called so the upstream fetch terminates.
  /// Caller is responsible for passing the controller's signal into the
  /// fetch that produced `events` — otherwise stop only halts playback,
  /// not the network stream.
  abortController?: AbortController;
}

export interface FaPlayback {
  start(): Promise<void>;
  stop(): void;
  pause(): void;
  resume(): void;
  readonly state: FaPlaybackState;
  readonly activeWordIndex: number;
  readonly words: ReadonlyArray<FaWord>;
  readonly error: string | null;
  readonly timings: FaTimings;
  readonly audioCurrentTime: number;
}

interface ParagraphCtx {
  paragraphIndex: number;
  wordOffset: number;
  audioOffsetMs: number;
  sampleRate: number;
  text: string;
}

// Preroll added to the first scheduled buffer so the output device has time
// to spin up before samples are due. Scheduling at ctx.currentTime is a known
// glitch source (clicks/pitch artefacts) on cold AudioContext start.
const LEAD_SECONDS = 0.5;

export function createFaPlayback(opts: CreateFaPlaybackOptions): FaPlayback {
  let state = $state<FaPlaybackState>('idle');
  let activeWordIndex = $state(-1);
  let words = $state<FaWord[]>([]);
  let errorMsg = $state<string | null>(null);
  let timings = $state<FaTimings>({});
  let audioCurrentTimeMs = $state(0);

  let ctx: AudioContext | null = null;
  let nextStartTime = 0;
  // -1 sentinel = first audio buffer has not been scheduled yet. The
  // highlight tick computes elapsed against this; pegging it to context
  // creation (which can be hundreds of ms before TTFA) makes the highlighter
  // race through pre-arrived alignment words before audio starts.
  let playbackStartCtxTime = -1;
  // Cumulative silent gaps between scheduled buffers (in ctx seconds).
  // Subtracted from elapsed so the highlight tracks audio time, not wall
  // time — when the buffer queue underruns, ctx.currentTime keeps advancing
  // through silence and would otherwise leave the highlight permanently
  // ahead of audio.
  let gapCtxSeconds = 0;
  let sources: AudioBufferSourceNode[] = [];
  let syncFrame: number | null = null;
  let lastHighlightedIdx = -1;
  let paragraphsByIndex = new Map<number, ParagraphCtx>();
  let startWallMs = 0;
  let firstAudioWall = -1;
  let firstAlignWall = -1;
  let lastAudioMs = 0;
  let stopped = false;
  let consumerError: unknown = null;

  async function decodeAudio(ev: Extract<FaEvent, { type: 'audio' }>): Promise<AudioBuffer | null> {
    if (!ctx) return null;
    if (ev.format === 'pcm') {
      const samples = new Int16Array(ev.pcm.buffer, ev.pcm.byteOffset, ev.pcm.byteLength / 2);
      const floats = new Float32Array(samples.length);
      for (let i = 0; i < samples.length; i++) floats[i] = samples[i] / 32768;
      const rate = ev.sampleRate > 0 ? ev.sampleRate : 24_000;
      const buf = ctx.createBuffer(1, samples.length, rate);
      buf.copyToChannel(floats, 0);
      return buf;
    }
    // mp3 (or any container the browser can decode): hand to decodeAudioData.
    // Copy into a fresh ArrayBuffer — decodeAudioData detaches whatever it gets.
    const ab = new ArrayBuffer(ev.pcm.byteLength);
    new Uint8Array(ab).set(ev.pcm);
    try {
      return await ctx.decodeAudioData(ab);
    } catch {
      return null;
    }
  }

  function scheduleBuffer(buf: AudioBuffer) {
    if (!ctx) return;
    const source = ctx.createBufferSource();
    source.buffer = buf;
    source.connect(ctx.destination);
    const isFirst = playbackStartCtxTime < 0;
    const startAt = isFirst
      ? ctx.currentTime + LEAD_SECONDS
      : Math.max(nextStartTime, ctx.currentTime);
    source.start(startAt);
    if (isFirst) {
      playbackStartCtxTime = startAt;
    } else if (startAt > nextStartTime) {
      gapCtxSeconds += startAt - nextStartTime;
    }
    nextStartTime = startAt + buf.duration;
    sources.push(source);
  }

  function mergeWordTimings(
    paragraphIndex: number,
    incoming: Array<{ wordIndex: number; text: string; t0Ms: number; t1Ms: number }>
  ) {
    const para = paragraphsByIndex.get(paragraphIndex);
    if (!para) return;
    const next = words.slice();
    for (const w of incoming) {
      const globalWordIndex = para.wordOffset + w.wordIndex;
      const t0Ms = para.audioOffsetMs + w.t0Ms;
      const t1Ms = para.audioOffsetMs + w.t1Ms;
      const existing = next.findIndex((x) => x.globalWordIndex === globalWordIndex);
      const entry: FaWord = {
        globalWordIndex,
        paragraphIndex,
        text: w.text,
        t0Ms,
        t1Ms
      };
      if (existing >= 0) next[existing] = entry;
      else next.push(entry);
    }
    next.sort((a, b) => a.globalWordIndex - b.globalWordIndex);
    words = next;
  }

  function findWordAtMs(ms: number): number {
    if (words.length === 0) return -1;
    let lo = 0;
    let hi = words.length - 1;
    let result = -1;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      const w = words[mid];
      if (ms < w.t0Ms) hi = mid - 1;
      else if (ms >= w.t1Ms) {
        result = mid;
        lo = mid + 1;
      } else return mid;
    }
    return result;
  }

  function startSyncLoop() {
    if (syncFrame != null) cancelAnimationFrame(syncFrame);
    function tick() {
      if (!ctx || stopped) return;
      if (playbackStartCtxTime >= 0) {
        // Clamp to nextStartTime so the highlight freezes during an active
        // underrun (silence). gapCtxSeconds removes prior closed gaps.
        // audibleLat subtracts the output-device latency so the highlight
        // tracks audio the user can actually hear, not audio that's been
        // scheduled but not yet emitted — without it, highlight runs ~50ms
        // ahead and alignment refinements pull it backwards.
        const audibleLat = (ctx.baseLatency ?? 0) + (ctx.outputLatency ?? 0);
        const effectiveCt = Math.min(ctx.currentTime, nextStartTime);
        const elapsedMs = Math.max(
          0,
          (effectiveCt - playbackStartCtxTime - gapCtxSeconds - audibleLat) * 1000
        );
        audioCurrentTimeMs = elapsedMs;
        const idx = findWordAtMs(elapsedMs);
        if (idx !== lastHighlightedIdx) {
          lastHighlightedIdx = idx;
          activeWordIndex = idx >= 0 ? words[idx].globalWordIndex : -1;
        }
      }
      syncFrame = requestAnimationFrame(tick);
    }
    syncFrame = requestAnimationFrame(tick);
  }

  function cleanup() {
    stopped = true;
    if (syncFrame != null) {
      cancelAnimationFrame(syncFrame);
      syncFrame = null;
    }
    for (const src of sources) {
      try {
        src.stop();
      } catch {
        /* already stopped */
      }
    }
    sources = [];
    if (ctx) {
      void ctx.close();
      ctx = null;
    }
    nextStartTime = 0;
    playbackStartCtxTime = -1;
    gapCtxSeconds = 0;
    lastHighlightedIdx = -1;
    paragraphsByIndex = new Map();
  }

  async function consume() {
    try {
      for await (const ev of opts.events) {
        if (stopped) break;
        switch (ev.type) {
          case 'paragraph':
            paragraphsByIndex.set(ev.paragraphIndex, {
              paragraphIndex: ev.paragraphIndex,
              wordOffset: ev.wordOffset,
              audioOffsetMs: ev.audioOffsetMs,
              sampleRate: ev.sampleRate,
              text: ev.text
            });
            break;
          case 'audio': {
            const buf = await decodeAudio(ev);
            if (!buf) break;
            if (firstAudioWall < 0) {
              firstAudioWall = performance.now();
              timings = { ...timings, ttfaMs: firstAudioWall - startWallMs };
              state = 'playing';
            }
            scheduleBuffer(buf);
            break;
          }
          case 'alignment':
            mergeWordTimings(ev.paragraphIndex, ev.words);
            if (firstAlignWall < 0) {
              firstAlignWall = performance.now();
              timings = { ...timings, firstAlignMs: firstAlignWall - startWallMs };
            }
            if (ev.final) {
              const para = paragraphsByIndex.get(ev.paragraphIndex);
              const addOffset = para?.audioOffsetMs ?? 0;
              lastAudioMs = Math.max(lastAudioMs, addOffset + ev.audioMs);
              timings = { ...timings, audioMs: lastAudioMs };
            }
            break;
          case 'error':
            errorMsg = ev.message;
            opts.onError?.(ev.message);
            break;
          case 'done':
            // Stream is done emitting; audio playout still in flight. The
            // last source's onended handler (set below) flips state→done.
            //
            // A truncated render still decodes and plays, it is just short and
            // stops mid-sentence — indistinguishable from a complete one unless
            // the flag is surfaced. Reported without aborting: the audio that
            // did arrive is still worth playing out.
            if (ev.truncated) {
              const msg =
                'TTS stopped early — the text was truncated mid-sentence. Turn long-form back on.';
              errorMsg = msg;
              opts.onError?.(msg);
            }
            break;
        }
      }
    } catch (e) {
      // Abort from stop() — the upstream fetch was cancelled deliberately.
      // Not an error; cleanup() has already set stopped=true.
      const isAbort =
        e instanceof DOMException && e.name === 'AbortError';
      if (!isAbort) {
        consumerError = e;
        const msg = e instanceof Error ? e.message : String(e);
        errorMsg = msg;
        state = 'error';
        opts.onError?.(msg);
      }
    } finally {
      const last = sources[sources.length - 1];
      if (last && !stopped && consumerError == null) {
        last.onended = () => {
          if (!stopped) state = 'done';
        };
      } else if (!stopped && consumerError == null) {
        state = 'done';
      }
    }
  }

  async function start() {
    if (state !== 'idle') return;
    state = 'loading';
    stopped = false;
    startWallMs = performance.now();
    firstAudioWall = -1;
    firstAlignWall = -1;
    activeWordIndex = -1;
    words = [];
    errorMsg = null;
    timings = {};
    audioCurrentTimeMs = 0;
    lastAudioMs = 0;

    ctx = new AudioContext({ latencyHint: 'interactive' });
    nextStartTime = ctx.currentTime;
    playbackStartCtxTime = -1;
    gapCtxSeconds = 0;
    startSyncLoop();

    await consume();
  }

  function stop() {
    if (state === 'idle') return;
    try {
      opts.abortController?.abort();
    } catch {
      /* already aborted */
    }
    cleanup();
    state = 'idle';
    activeWordIndex = -1;
  }

  function pause() {
    if (state !== 'playing') return;
    ctx?.suspend();
    state = 'paused';
  }

  function resume() {
    if (state !== 'paused') return;
    ctx?.resume();
    state = 'playing';
  }

  return {
    start,
    stop,
    pause,
    resume,
    get state() {
      return state;
    },
    get activeWordIndex() {
      return activeWordIndex;
    },
    get words() {
      return words;
    },
    get error() {
      return errorMsg;
    },
    get timings() {
      return timings;
    },
    get audioCurrentTime() {
      return audioCurrentTimeMs;
    }
  };
}

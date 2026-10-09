<script lang="ts">
  // Waveform-with-trim-handles control. Caller decodes audio to mono PCM and
  // hands us the Float32Array + sample rate; we render the waveform and let
  // the user drag start/end markers (pointer-capture, so it works on touch +
  // mouse). The selection is constrained to `maxSeconds`. On drag release we
  // optionally snap each edge to the nearest sub-window silence so trim
  // boundaries don't land mid-word.
  //
  // The control is uncontrolled in the React sense — it owns `start`/`end`
  // internally and reports changes via `onchange` / `oncommit`. Callers
  // should keep the WAV synthesis (`encodeWavMono16` etc.) out of the trim
  // path — that's the consumer's job.

  import { untrack } from 'svelte';
  import {
    snapToSilence,
    formatSamplesAsTime,
    sliceSamples,
    encodeWavMono16,
    waveformPeaks
  } from '../utils/audio';

  interface Props {
    /** Mono PCM samples. The trim handles index into this array directly. */
    samples: Float32Array;
    sampleRate: number;
    /** Cap on the trim selection length. Default 10s — TTS reference clip target. */
    maxSeconds?: number;
    /** Minimum selection length so the handles don't cross. Default 1s. */
    minSeconds?: number;
    /** Initial selection. Defaults to the whole sample range, capped to maxSeconds. */
    initialStart?: number;
    initialEnd?: number;
    /** When true, drag-release snaps each edge to nearest silence in ±200ms. */
    snapEnabled?: boolean;
    /** Continuous updates while the user drags (every pointer event). */
    onchange?: (start: number, end: number) => void;
    /** Fires once when the user releases a drag — the spot to kick off STT. */
    oncommit?: (start: number, end: number) => void;
    /** Height in CSS px. Default 96. */
    height?: number;
  }

  let {
    samples,
    sampleRate,
    maxSeconds = 10,
    minSeconds = 1,
    initialStart,
    initialEnd,
    snapEnabled = true,
    onchange,
    oncommit,
    height = 96
  }: Props = $props();

  // Initial values captured from props at mount; re-init on sample buffer
  // swap via $effect below. `untrack` keeps Svelte from warning that we're
  // reading reactive props inside `$state(...)`.
  let trimStart = $state(untrack(() => initialStart ?? 0));
  let trimEnd = $state(
    untrack(() => initialEnd ?? Math.min(samples.length, Math.floor(maxSeconds * sampleRate)))
  );

  // Re-init when sample buffer changes — caller swapped audio source.
  let lastSamplesRef: Float32Array | null = null;
  $effect(() => {
    if (samples !== lastSamplesRef) {
      lastSamplesRef = samples;
      untrack(() => {
        trimStart = initialStart ?? 0;
        trimEnd = initialEnd ?? Math.min(samples.length, Math.floor(maxSeconds * sampleRate));
      });
    }
  });

  let canvas: HTMLCanvasElement | null = $state(null);
  let container: HTMLDivElement | null = $state(null);
  let dragWhich: 'start' | 'end' | null = null;

  const maxSamples = $derived(Math.floor(maxSeconds * sampleRate));
  const minSamples = $derived(Math.floor(minSeconds * sampleRate));
  const selectionMs = $derived(Math.round(((trimEnd - trimStart) / sampleRate) * 1000));

  function startDrag(which: 'start' | 'end', e: PointerEvent) {
    if (!container) return;
    dragWhich = which;
    container.setPointerCapture(e.pointerId);
    moveDrag(e);
  }

  /// Pointerdown on the waveform body (not on a handle) — pick whichever
  /// handle is closer to the click and start dragging it. Matches the
  /// existing koblem mic-record UX so users can drag-into-position without
  /// having to land on a 12px-wide hit target first.
  function pickNearestAndDrag(e: PointerEvent) {
    // Ignore if the press already landed on one of the handle buttons.
    if (!container || dragWhich) return;
    const target = e.target as HTMLElement | null;
    if (target?.classList.contains('wf-handle')) return;
    const rect = container.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    const idx = Math.floor(ratio * samples.length);
    const which = Math.abs(idx - trimStart) < Math.abs(idx - trimEnd) ? 'start' : 'end';
    startDrag(which, e);
  }

  function moveDrag(e: PointerEvent) {
    if (!dragWhich || !container) return;
    const rect = container.getBoundingClientRect();
    const ratio = Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
    const idx = Math.floor(ratio * samples.length);

    if (dragWhich === 'start') {
      let s = Math.min(idx, trimEnd - minSamples);
      s = Math.max(0, s);
      if (trimEnd - s > maxSamples) trimEnd = s + maxSamples;
      trimStart = s;
    } else {
      let e2 = Math.max(idx, trimStart + minSamples);
      e2 = Math.min(samples.length, e2);
      if (e2 - trimStart > maxSamples) trimStart = e2 - maxSamples;
      trimEnd = e2;
    }
    onchange?.(trimStart, trimEnd);
  }

  function endDrag(e: PointerEvent) {
    if (!dragWhich) return;
    container?.releasePointerCapture(e.pointerId);
    const which = dragWhich;
    dragWhich = null;

    if (snapEnabled) {
      const snapped =
        which === 'start'
          ? snapToSilence(samples, sampleRate, trimStart, 'start')
          : snapToSilence(samples, sampleRate, trimEnd, 'end');
      if (which === 'start') {
        const s = Math.max(0, Math.min(snapped, trimEnd - minSamples));
        if (trimEnd - s > maxSamples) trimEnd = s + maxSamples;
        trimStart = s;
      } else {
        const e2 = Math.min(samples.length, Math.max(snapped, trimStart + minSamples));
        if (e2 - trimStart > maxSamples) trimStart = e2 - maxSamples;
        trimEnd = e2;
      }
      onchange?.(trimStart, trimEnd);
    }
    oncommit?.(trimStart, trimEnd);
  }

  // Redraw whenever inputs change. The canvas is bitmap-sized to the
  // current CSS dimensions × DPR so the waveform stays crisp on hi-DPI.
  $effect(() => {
    void trimStart;
    void trimEnd;
    void samples;
    drawWaveform();
  });

  // Cached peak envelope (one bar per canvas column). The waveform SHAPE only
  // depends on the sample buffer + column count — NOT on the trim selection — so
  // we compute it once per (buffer, width) and reuse it across the many redraws
  // a drag triggers. Without this, every pointer-move re-scanned the whole array
  // (millions of samples for a full song), which is what made the control janky.
  let peakCache: { ref: Float32Array; cols: number; peaks: Float32Array } | null = null;

  function drawWaveform() {
    if (!canvas || samples.length === 0) return;
    const dpr = window.devicePixelRatio || 1;
    const cssW = canvas.clientWidth || 600;
    const cssH = canvas.clientHeight || height;
    canvas.width = Math.floor(cssW * dpr);
    canvas.height = Math.floor(cssH * dpr);
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.scale(dpr, dpr);
    ctx.clearRect(0, 0, cssW, cssH);

    // Canvas can't parse `var(...)` in fillStyle, so resolve the themeable
    // bar colours to literals from the element's computed style (falling back
    // to the defaults). Without this, bars render black and vanish on dark UIs.
    const cs = getComputedStyle(canvas);
    const activeColor = cs.getPropertyValue('--wf-active').trim() || '#4d8fff';
    const inactiveColor = cs.getPropertyValue('--wf-inactive').trim() || '#b3b8bf';

    const cols = Math.max(1, Math.floor(cssW));
    if (!peakCache || peakCache.ref !== samples || peakCache.cols !== cols) {
      peakCache = { ref: samples, cols, peaks: waveformPeaks(samples, cols) };
    }
    const peaks = peakCache.peaks;

    const samplesPerCol = samples.length / cols;
    const mid = cssH / 2;
    for (let x = 0; x < cols; x++) {
      // Only the colour (in/out of selection) changes between redraws.
      const inRange = Math.floor(x * samplesPerCol) >= trimStart && Math.floor(x * samplesPerCol) < trimEnd;
      ctx.fillStyle = inRange ? activeColor : inactiveColor;
      const half = Math.max(1, peaks[x] * mid * 0.95);
      ctx.fillRect(x, mid - half, 1, half * 2);
    }
  }

  // Selection playback. Re-creates the AudioBufferSource each play so seek
  // is "always from selection start" — feels right for a trim editor.
  let audioCtx: AudioContext | null = null;
  let activeSource: AudioBufferSourceNode | null = null;
  let isPlaying = $state(false);

  async function playSelection() {
    if (isPlaying) {
      stopPlayback();
      return;
    }
    if (trimEnd <= trimStart) return;
    audioCtx ??= new AudioContext();
    const slice = sliceSamples(samples, trimStart, trimEnd);
    const buffer = audioCtx.createBuffer(1, slice.length, sampleRate);
    buffer.getChannelData(0).set(slice);
    const source = audioCtx.createBufferSource();
    source.buffer = buffer;
    source.connect(audioCtx.destination);
    source.onended = () => {
      if (activeSource === source) {
        activeSource = null;
        isPlaying = false;
      }
    };
    activeSource = source;
    isPlaying = true;
    source.start();
  }

  function stopPlayback() {
    if (activeSource) {
      try {
        activeSource.stop();
      } catch {
        // already stopped
      }
      activeSource = null;
    }
    isPlaying = false;
  }

  // Stop playback + close AudioContext on unmount.
  $effect(() => () => {
    stopPlayback();
    audioCtx?.close();
    audioCtx = null;
  });

  /// Imperative helpers callers can grab via `bind:this`. Lets the consumer
  /// trigger a re-render after externally changing trim values without
  /// pulling state into the parent.
  export function getSelection(): { start: number; end: number } {
    return { start: trimStart, end: trimEnd };
  }

  export function getSelectionWav(): Blob {
    const slice = sliceSamples(samples, trimStart, trimEnd);
    return encodeWavMono16(slice, sampleRate);
  }
</script>

<div class="wf-root">
  <div
    class="wf-canvas-wrap"
    role="group"
    aria-label="Audio trim editor"
    bind:this={container}
    onpointerdown={pickNearestAndDrag}
    onpointermove={moveDrag}
    onpointerup={endDrag}
    onpointercancel={endDrag}
  >
    <canvas bind:this={canvas} style:height="{height}px" class="wf-canvas"></canvas>
    <div
      class="wf-region"
      style:left="{(trimStart / Math.max(1, samples.length)) * 100}%"
      style:width="{((trimEnd - trimStart) / Math.max(1, samples.length)) * 100}%"
    ></div>
    <button
      type="button"
      class="wf-handle wf-handle-start"
      style:left="{(trimStart / Math.max(1, samples.length)) * 100}%"
      onpointerdown={(e) => startDrag('start', e)}
      aria-label="Trim start"
    ></button>
    <button
      type="button"
      class="wf-handle wf-handle-end"
      style:left="{(trimEnd / Math.max(1, samples.length)) * 100}%"
      onpointerdown={(e) => startDrag('end', e)}
      aria-label="Trim end"
    ></button>
  </div>
  <div class="wf-footer">
    <button type="button" class="wf-play-btn" onclick={playSelection}>
      {isPlaying ? '◼ Stop' : '▶ Play selection'}
    </button>
    <span class="wf-times">
      <span class="wf-time">{formatSamplesAsTime(trimStart, sampleRate)}</span>
      <span class="wf-arrow">→</span>
      <span class="wf-time">{formatSamplesAsTime(trimEnd, sampleRate)}</span>
      <span class="wf-duration">({(selectionMs / 1000).toFixed(2)}s)</span>
    </span>
    {#if snapEnabled}
      <span class="wf-hint" title="Drag release snaps each edge to the nearest silence">snap on</span>
    {/if}
  </div>
</div>

<style>
  .wf-root {
    display: flex;
    flex-direction: column;
    gap: 6px;
    width: 100%;
  }

  .wf-canvas-wrap {
    position: relative;
    width: 100%;
    background: var(--wf-bg, rgba(0, 0, 0, 0.04));
    border-radius: 6px;
    overflow: hidden;
    touch-action: none;
    user-select: none;
  }

  .wf-canvas {
    display: block;
    width: 100%;
    height: 96px;
  }

  .wf-region {
    position: absolute;
    top: 0;
    bottom: 0;
    background: var(--wf-region-bg, rgba(77, 143, 255, 0.14));
    pointer-events: none;
  }

  .wf-handle {
    position: absolute;
    top: -2px;
    bottom: -2px;
    width: 12px;
    margin-left: -6px;
    background: transparent;
    border: none;
    padding: 0;
    cursor: ew-resize;
    touch-action: none;
  }

  .wf-handle::before {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    left: 5px;
    width: 2px;
    background: var(--wf-handle, #1e6fff);
    box-shadow: 0 0 0 1px var(--wf-handle-shadow, rgba(0, 0, 0, 0.15));
  }

  .wf-handle::after {
    content: '';
    position: absolute;
    left: 1px;
    top: 50%;
    width: 10px;
    height: 18px;
    margin-top: -9px;
    background: var(--wf-handle, #1e6fff);
    border-radius: 2px;
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.2);
  }

  .wf-handle-start::before {
    background: var(--wf-handle-start, #1e6fff);
  }
  .wf-handle-end::before {
    background: var(--wf-handle-end, #1e6fff);
  }

  .wf-footer {
    display: flex;
    align-items: center;
    gap: 12px;
    font-size: 0.85rem;
    color: var(--wf-fg, #4b5460);
  }

  .wf-play-btn {
    background: var(--wf-btn-bg, #1e6fff);
    color: var(--wf-btn-fg, white);
    border: none;
    border-radius: 4px;
    padding: 5px 12px;
    cursor: pointer;
    font-size: 0.85rem;
  }

  .wf-play-btn:hover {
    filter: brightness(1.08);
  }

  .wf-times {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-variant-numeric: tabular-nums;
  }

  .wf-time {
    color: var(--wf-time-fg, #4b5460);
  }

  .wf-arrow {
    color: var(--wf-arrow, #9aa3ad);
  }

  .wf-duration {
    color: var(--wf-duration, #1e6fff);
    font-weight: 500;
    margin-left: 4px;
  }

  .wf-hint {
    margin-left: auto;
    font-size: 0.75rem;
    color: var(--wf-hint, #6b7280);
    background: var(--wf-hint-bg, rgba(0, 0, 0, 0.04));
    padding: 2px 8px;
    border-radius: 10px;
  }
</style>

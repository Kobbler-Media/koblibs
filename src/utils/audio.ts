// PCM/WAV helpers + waveform analysis used by voice-clone trim editors.
// Pure browser code — no DOM, no Svelte. The wider WaveformTrim component
// builds on these.

/// Encode interleaved 16-bit PCM WAV from N equal-length channels (numChannels
/// = channels.length: 1 = mono, 2 = stereo, …). Use this when the upload must
/// preserve the source's channels — e.g. STEREO for source separation: htdemucs
/// separates on inter-channel cues, so a mono downmix degrades the stems.
export function encodeWav16(channels: Float32Array[], sampleRate: number): Blob {
  const numCh = Math.max(1, channels.length);
  const numFrames = channels[0]?.length ?? 0;
  const dataSize = numFrames * numCh * 2;
  const buf = new ArrayBuffer(44 + dataSize);
  const view = new DataView(buf);

  writeString(view, 0, 'RIFF');
  view.setUint32(4, 36 + dataSize, true);
  writeString(view, 8, 'WAVE');
  writeString(view, 12, 'fmt ');
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, numCh, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * numCh * 2, true);
  view.setUint16(32, numCh * 2, true);
  view.setUint16(34, 16, true);
  writeString(view, 36, 'data');
  view.setUint32(40, dataSize, true);

  let off = 44;
  for (let i = 0; i < numFrames; i++) {
    for (let c = 0; c < numCh; c++) {
      const clipped = Math.max(-1, Math.min(1, channels[c][i]));
      view.setInt16(off, clipped < 0 ? clipped * 0x8000 : clipped * 0x7fff, true);
      off += 2;
    }
  }

  return new Blob([buf], { type: 'audio/wav' });
}

/// Mono convenience wrapper over [`encodeWav16`] (byte-identical to the old
/// implementation). For callers that deliberately want a forced-mono ref (e.g.
/// TTS speaker-clone, where channel count doesn't matter).
export function encodeWavMono16(samples: Float32Array, sampleRate: number): Blob {
  return encodeWav16([samples], sampleRate);
}

function writeString(view: DataView, offset: number, str: string) {
  for (let i = 0; i < str.length; i++) {
    view.setUint8(offset + i, str.charCodeAt(i));
  }
}

/// Build a single-File FileList — the only way to set <input type=file>'s
/// `files` programmatically.
export function fileListWith(file: File): FileList {
  const dt = new DataTransfer();
  dt.items.add(file);
  return dt.files;
}

/// Decode any browser-supported audio blob and return raw mono float samples
/// + the source sample rate.
export async function blobToMonoSamples(
  blob: Blob
): Promise<{ samples: Float32Array; sampleRate: number }> {
  const ab = await blob.arrayBuffer();
  const ac = new AudioContext();
  try {
    const decoded = await ac.decodeAudioData(ab);
    const channels = decoded.numberOfChannels;
    const len = decoded.length;
    let mono: Float32Array;
    if (channels === 1) {
      mono = decoded.getChannelData(0).slice();
    } else {
      mono = new Float32Array(len);
      for (let c = 0; c < channels; c++) {
        const ch = decoded.getChannelData(c);
        for (let i = 0; i < len; i++) mono[i] += ch[i] / channels;
      }
    }
    return { samples: mono, sampleRate: decoded.sampleRate };
  } finally {
    void ac.close();
  }
}

/// Decode any browser-supported audio blob to its ORIGINAL channels (no
/// downmix) + sample rate. Use when the upload must stay faithful to the source
/// — e.g. stereo for source separation. Pair with [`downmixMono`] for the
/// waveform display / energy analysis, which want a single channel.
export async function blobToChannels(
  blob: Blob
): Promise<{ channels: Float32Array[]; sampleRate: number }> {
  const ab = await blob.arrayBuffer();
  const ac = new AudioContext();
  try {
    const decoded = await ac.decodeAudioData(ab);
    const channels: Float32Array[] = [];
    for (let c = 0; c < decoded.numberOfChannels; c++) {
      channels.push(decoded.getChannelData(c).slice());
    }
    return { channels, sampleRate: decoded.sampleRate };
  } finally {
    void ac.close();
  }
}

/// Average N channels down to a single mono Float32Array (for waveform display
/// and the RMS window/silence analysis, which operate on one channel).
export function downmixMono(channels: Float32Array[]): Float32Array {
  if (channels.length <= 1) return channels[0] ?? new Float32Array(0);
  const len = channels[0].length;
  const mono = new Float32Array(len);
  for (const ch of channels) {
    for (let i = 0; i < len; i++) mono[i] += ch[i] / channels.length;
  }
  return mono;
}

/// The peak envelope of a waveform, one value per column — what every waveform
/// bar, strip and canvas in the ecosystem draws.
///
/// Three hand-written copies of this loop existed when the phone shell was added
/// (WaveformTrim's own `peaksFor` and the Director timeline's two canvas loops)
/// and a fourth was about to be. They agreed — all three walked a FRACTIONAL
/// stride, so none of them dropped the tail samples an integer stride loses — so
/// this is consolidation before drift rather than after it.
///
/// Values are the absolute peak in each column, CLAMPED TO 1, which is the one
/// behavioural difference from the three: they multiplied the raw peak by a lane
/// height, so a sample above full scale drew outside its lane. Callers still
/// scale by their own height.
export function waveformPeaks(
  samples: Float32Array | null | undefined,
  columns: number
): Float32Array {
  const cols = Math.max(0, Math.floor(columns));
  const out = new Float32Array(cols);
  if (!samples || samples.length === 0 || cols === 0) return out;
  const per = samples.length / cols;
  for (let x = 0; x < cols; x++) {
    const from = Math.floor(x * per);
    const to = Math.min(samples.length, Math.floor((x + 1) * per));
    let peak = 0;
    for (let i = from; i < to; i++) {
      const v = Math.abs(samples[i]);
      if (v > peak) peak = v;
    }
    out[x] = peak > 1 ? 1 : peak;
  }
  return out;
}

/// Slice a Float32Array — returns a copy so the underlying buffer can be
/// freed independently.
export function sliceSamples(samples: Float32Array, start: number, end: number): Float32Array {
  const out = new Float32Array(Math.max(0, end - start));
  out.set(samples.subarray(start, end));
  return out;
}

/// Pick the best <=maxSeconds-long window in `samples` by sliding-window RMS
/// energy. Used to default trim handles onto the strongest contiguous voice
/// region. Returns sample indices [start, end).
export function detectBestWindow(
  samples: Float32Array,
  sampleRate: number,
  maxSeconds: number
): { start: number; end: number } {
  const totalLen = samples.length;
  const winLen = Math.min(totalLen, Math.floor(maxSeconds * sampleRate));
  if (winLen >= totalLen) return { start: 0, end: totalLen };

  const frameLen = Math.max(1, Math.floor(0.05 * sampleRate));
  const numFrames = Math.floor(totalLen / frameLen);
  if (numFrames === 0) return { start: 0, end: totalLen };

  const frameEnergy = new Float32Array(numFrames);
  for (let f = 0; f < numFrames; f++) {
    let sum = 0;
    const base = f * frameLen;
    for (let i = 0; i < frameLen; i++) {
      const s = samples[base + i];
      sum += s * s;
    }
    frameEnergy[f] = sum / frameLen;
  }

  const winFrames = Math.max(1, Math.floor(winLen / frameLen));
  let running = 0;
  for (let f = 0; f < winFrames && f < numFrames; f++) running += frameEnergy[f];
  let bestStartFrame = 0;
  let bestSum = running;
  for (let f = winFrames; f < numFrames; f++) {
    running += frameEnergy[f] - frameEnergy[f - winFrames];
    if (running > bestSum) {
      bestSum = running;
      bestStartFrame = f - winFrames + 1;
    }
  }
  const start = bestStartFrame * frameLen;
  const end = Math.min(totalLen, start + winLen);
  return { start, end };
}

/// Snap a sample index toward the nearest low-energy "gap" so trim edges
/// land between words, not in the middle of one.
///
/// Walks ±searchMs around `idx` in 10ms frames, picks the frame whose RMS
/// energy is in the bottom 25% of frames in that window — i.e. the quietest
/// region. `direction` biases the snap: 'start' picks the frame just AFTER
/// the silence (so the trimmed audio starts on speech), 'end' picks the
/// frame just BEFORE silence ends (so the trimmed audio ends on speech).
///
/// If no clear silence is found, returns `idx` unchanged — caller can
/// always fall back to the raw drag position.
export function snapToSilence(
  samples: Float32Array,
  sampleRate: number,
  idx: number,
  direction: 'start' | 'end',
  searchMs: number = 200
): number {
  const frameLen = Math.max(1, Math.floor(0.01 * sampleRate));
  const searchFrames = Math.floor((searchMs / 1000) * sampleRate / frameLen);
  if (searchFrames < 2) return idx;

  const centerFrame = Math.floor(idx / frameLen);
  const lo = Math.max(0, centerFrame - searchFrames);
  const hi = Math.min(Math.floor(samples.length / frameLen), centerFrame + searchFrames);
  if (hi - lo < 4) return idx;

  const window = new Float32Array(hi - lo);
  let peak = 0;
  for (let f = lo; f < hi; f++) {
    let sum = 0;
    const base = f * frameLen;
    for (let i = 0; i < frameLen; i++) {
      const s = samples[base + i] || 0;
      sum += s * s;
    }
    const energy = Math.sqrt(sum / frameLen);
    window[f - lo] = energy;
    if (energy > peak) peak = energy;
  }
  if (peak <= 0) return idx;

  // Threshold = 25% of the max energy in the search window. Frames below
  // this are "silence" candidates.
  const threshold = peak * 0.25;

  // Find runs of consecutive silence frames; pick the run whose edge is
  // closest to `idx`. For 'start' direction, we want the END of a silence
  // run (so speech begins right after). For 'end', the START of a run.
  let bestEdgeFrame = -1;
  let bestDist = Infinity;
  let runStart = -1;
  for (let i = 0; i < window.length; i++) {
    if (window[i] < threshold) {
      if (runStart < 0) runStart = i;
    } else if (runStart >= 0) {
      const edgeLocal = direction === 'start' ? i : runStart;
      const edgeAbsFrame = lo + edgeLocal;
      const edgeIdx = edgeAbsFrame * frameLen;
      const dist = Math.abs(edgeIdx - idx);
      if (dist < bestDist) {
        bestDist = dist;
        bestEdgeFrame = edgeAbsFrame;
      }
      runStart = -1;
    }
  }
  // Run that extends to end of window — only valid for 'end' direction.
  if (runStart >= 0 && direction === 'end') {
    const edgeAbsFrame = lo + runStart;
    const edgeIdx = edgeAbsFrame * frameLen;
    const dist = Math.abs(edgeIdx - idx);
    if (dist < bestDist) {
      bestDist = dist;
      bestEdgeFrame = edgeAbsFrame;
    }
  }

  if (bestEdgeFrame < 0) return idx;
  return bestEdgeFrame * frameLen;
}

/// Format a sample-index as a mm:ss.mmm timestamp string.
export function formatSamplesAsTime(idx: number, sampleRate: number): string {
  const totalMs = Math.round((idx / sampleRate) * 1000);
  const mins = Math.floor(totalMs / 60_000);
  const secs = Math.floor((totalMs % 60_000) / 1000);
  const ms = totalMs % 1000;
  return `${mins}:${secs.toString().padStart(2, '0')}.${ms.toString().padStart(3, '0')}`;
}

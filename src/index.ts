// Components
export { default as InfiniteScroll } from './components/InfiniteScroll.svelte';
export { default as ScrollToTop } from './components/ScrollToTop.svelte';
export { default as Toast } from './components/Toast.svelte';
export { default as Modal } from './components/Modal.svelte';
export { default as MultiSelectBar } from './components/MultiSelectBar.svelte';
export { default as ExpandableSidebar } from './components/ExpandableSidebar.svelte';
export { default as DeleteConfirmModal } from './components/DeleteConfirmModal.svelte';
export { default as SearchSelect } from './components/SearchSelect.svelte';
export { default as MultiSearchSelect } from './components/MultiSearchSelect.svelte';
export { default as WaveformTrim } from './components/WaveformTrim.svelte';
export { default as FaTranscript } from './components/FaTranscript.svelte';

// Stores
export {
  registerOverlay,
  unregisterOverlay,
  closeTopOverlay,
  hasOpenOverlays
} from './stores/overlay.svelte';

// Utils
export { createSseConnection, type SseConnection, type SseOptions, type SseConfig } from './utils/sse';
export { formatAge, formatDate, formatBytes } from './utils/format';
export { createDebouncedSseHandler } from './utils/sse-debounce';
export { uid } from './utils/uid';
export {
  encodeWavMono16,
  encodeWav16,
  fileListWith,
  blobToMonoSamples,
  blobToChannels,
  downmixMono,
  sliceSamples,
  waveformPeaks,
  detectBestWindow,
  snapToSilence,
  formatSamplesAsTime
} from './utils/audio';
export { parseTtsSse, type ParseTtsSseOptions } from './utils/tts-sse';
export { parseKobblerNdjson, type ParseKobblerNdjsonOptions } from './utils/kobbler-ndjson';
export {
  createFaPlayback,
  type FaEvent,
  type FaPlayback,
  type FaPlaybackState,
  type FaWord,
  type FaTimings,
  type CreateFaPlaybackOptions
} from './utils/fa-playback.svelte';

// Re-export types
export type { OverlayEntry } from './stores/overlay.svelte';
export type { SearchSelectItem, MultiSearchSelectItem } from './components/search-select-types';

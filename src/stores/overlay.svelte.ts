import { untrack } from 'svelte';

export type OverlayEntry = { id: string; closeFn: () => void };
let overlayStack = $state<OverlayEntry[]>([]);

// One global Escape listener and one scroll lock for the whole stack, installed
// while anything is open. Per-overlay listeners would fire N times per keypress.
let teardown: (() => void) | null = null;

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') closeTopOverlay();
}

function activate() {
  if (teardown || typeof document === 'undefined') return;
  // Bubble phase on window, so a component that owns Escape for its own popup
  // (SearchSelect's dropdown) can claim it first from a capture-phase listener.
  window.addEventListener('keydown', onKeydown);
  const previousOverflow = document.body.style.overflow;
  document.body.style.overflow = 'hidden';
  teardown = () => {
    window.removeEventListener('keydown', onKeydown);
    document.body.style.overflow = previousOverflow;
  };
}

function deactivate() {
  teardown?.();
  teardown = null;
}

export function registerOverlay(id: string, closeFn: () => void) {
  const current = untrack(() => overlayStack);
  overlayStack = [...current.filter((e: OverlayEntry) => e.id !== id), { id, closeFn }];
  activate();
}

export function unregisterOverlay(id: string) {
  const current = untrack(() => overlayStack);
  overlayStack = current.filter((e: OverlayEntry) => e.id !== id);
  if (overlayStack.length === 0) deactivate();
}

export function closeTopOverlay(): boolean {
  if (overlayStack.length === 0) return false;
  const top = overlayStack[overlayStack.length - 1];
  overlayStack = overlayStack.slice(0, -1);
  if (overlayStack.length === 0) deactivate();
  top.closeFn();
  return true;
}

export function hasOpenOverlays(): boolean {
  return overlayStack.length > 0;
}

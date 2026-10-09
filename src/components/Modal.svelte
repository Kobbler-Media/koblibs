<script lang="ts">
  import type { Snippet } from 'svelte';
  import { onMount, onDestroy } from 'svelte';
  import { registerOverlay, unregisterOverlay } from '../stores/overlay.svelte';
  import { uid } from '../utils/uid';

  interface Props {
    onClose: () => void;
    title?: string;
    width?: string;
    children: Snippet;
    footer?: Snippet;
    /** Allow content to overflow (for dropdowns) */
    overflowVisible?: boolean;
    /** Long-form layout: pinned header/footer over a scrolling body, full-screen under 640px. */
    sheet?: boolean;
    /**
     * Consulted before every dismissal — backdrop, close button, Escape, back gesture.
     * Return false to keep the modal open (e.g. to confirm discarding unsaved edits).
     */
    canClose?: () => boolean;
  }

  let {
    onClose,
    title,
    width,
    children,
    footer,
    overflowVisible = false,
    sheet = false,
    canClose
  }: Props = $props();

  // A sheet's width is a stylesheet variable rather than an inline width so the
  // narrow-viewport full-screen rule can still win.
  const styleAttr = $derived(width ? (sheet ? `--modal-width: ${width}` : `width: ${width}`) : '');

  const modalId = `modal-${uid()}`;
  const titleId = `${modalId}-title`;

  let modalEl = $state<HTMLDivElement | undefined>();
  let restoreFocusTo: HTMLElement | null = null;

  const FOCUSABLE =
    'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]:not([tabindex="-1"])';

  // Rendered-only: a control inside a collapsed <details> is in the DOM but not tabbable.
  const focusable = (): HTMLElement[] =>
    modalEl
      ? [...modalEl.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
          (el) => el.getClientRects().length > 0
        )
      : [];

  const handleKeydown = (e: KeyboardEvent) => {
    if (e.key !== 'Tab') return;
    const items = focusable();
    if (items.length === 0) {
      e.preventDefault();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && (active === first || active === modalEl)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  };

  const requestClose = () => {
    if (canClose && !canClose()) {
      // Escape/back pop the stack before calling us, so a refusal has to put this
      // modal back on top or the next dismissal would target whatever is beneath.
      registerOverlay(modalId, requestClose);
      return;
    }
    onClose();
  };

  onMount(() => {
    registerOverlay(modalId, requestClose);
    restoreFocusTo = document.activeElement as HTMLElement | null;
    // The dialog itself, not its first control: a SearchSelect opens its dropdown
    // on focus, so seeding focus into the form would greet you with an open menu.
    modalEl?.focus();
  });

  onDestroy(() => {
    unregisterOverlay(modalId);
    restoreFocusTo?.focus?.();
  });
</script>

<button class="modal-backdrop" type="button" aria-label="Close" onclick={requestClose}></button>
<div
  bind:this={modalEl}
  class="modal"
  class:overflow-visible={overflowVisible}
  class:sheet
  style={styleAttr}
  role="dialog"
  aria-modal="true"
  aria-labelledby={title ? titleId : undefined}
  tabindex="-1"
  onkeydown={handleKeydown}
>
  {#if title}
    <div class="modal-header">
      <h3 id={titleId}>{title}</h3>
      <button class="icon-btn" type="button" aria-label="Close" onclick={requestClose}>
        <svg viewBox="0 0 24 24" width="14" height="14" fill="currentColor">
          <path d="M6.4 5.6 5 7l5 5-5 5 1.4 1.4 5-5 5 5L17.8 17l-5-5 5-5-1.4-1.4-5 5-5-5z"></path>
        </svg>
      </button>
    </div>
  {/if}
  {#if sheet}
    <div class="modal-body">
      {@render children()}
    </div>
  {:else}
    {@render children()}
  {/if}
  {#if footer}
    <div class="modal-actions">
      {@render footer()}
    </div>
  {/if}
</div>

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    z-index: 100;
    border: none;
    cursor: pointer;
  }

  .modal {
    position: fixed;
    /* Centre inside the safe area, not the cutout-inclusive viewport, so the
       bottom of a tall modal never lands under Android's navigation bar. */
    top: calc(
      50% + (env(safe-area-inset-top, 0px) - env(safe-area-inset-bottom, 0px)) / 2
    );
    left: calc(
      50% + (env(safe-area-inset-left, 0px) - env(safe-area-inset-right, 0px)) / 2
    );
    transform: translate(-50%, -50%);
    background: var(--card);
    border: 1px solid var(--ring);
    border-radius: 16px;
    --modal-pad: 20px;
    padding: var(--modal-pad);
    z-index: 101;
    min-width: 300px;
    max-width: calc(90vw - env(safe-area-inset-left, 0px) - env(safe-area-inset-right, 0px));
    max-height: calc(90vh - env(safe-area-inset-top, 0px) - env(safe-area-inset-bottom, 0px));
    overflow: auto;
  }

  .modal.overflow-visible {
    overflow: visible;
  }

  .modal.sheet {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    width: var(--modal-width, 620px);
  }

  .modal.sheet .modal-body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    /* Bleed to the panel edge so the scrollbar rides the border, not the text. */
    margin: 0 calc(-1 * var(--modal-pad));
    padding: 0 var(--modal-pad);
  }

  .modal.sheet .modal-header,
  .modal.sheet .modal-actions {
    flex-shrink: 0;
  }

  .modal.sheet .modal-header {
    border-bottom: 1px solid var(--ring);
    padding-bottom: 12px;
  }

  .modal.sheet .modal-actions {
    border-top: 1px solid var(--ring);
    padding-top: 14px;
  }

  @media (max-width: 640px) {
    .modal.sheet {
      --modal-pad: 16px;
      top: 0;
      left: 0;
      transform: none;
      width: 100%;
      max-width: 100%;
      height: 100dvh;
      max-height: 100dvh;
      border: none;
      border-radius: 0;
      padding-top: calc(var(--modal-pad) + env(safe-area-inset-top, 0px));
      padding-right: calc(var(--modal-pad) + env(safe-area-inset-right, 0px));
      padding-bottom: calc(var(--modal-pad) + env(safe-area-inset-bottom, 0px));
      padding-left: calc(var(--modal-pad) + env(safe-area-inset-left, 0px));
    }
  }

  .modal-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 12px;
  }

  .modal-header h3 {
    margin: 0;
    font-size: 16px;
  }

  .icon-btn {
    border-radius: 12px;
    padding: 6px 8px;
    border: 1px solid var(--ring);
    background: var(--card);
    color: var(--ink);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 16px;
  }
</style>

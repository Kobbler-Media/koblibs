<script lang="ts">
  import type { Snippet } from 'svelte';
  import { registerOverlay, unregisterOverlay } from '../stores/overlay.svelte';

  interface Props {
    open: boolean;
    expanded?: boolean;
    title?: string;
    // Optional: show refresh button
    showRefresh?: boolean;
    // Optional: show edit button
    showEdit?: boolean;
    editMode?: boolean;
    // Callbacks
    onClose: () => void;
    onExpand?: () => void;
    onRefresh?: () => void;
    onToggleEdit?: () => void;
    onSearch?: () => void;
    // Slots
    children: Snippet;
    headerLeft?: Snippet;
    headerActions?: Snippet;
    footer?: Snippet;
  }

  let {
    open,
    expanded = false,
    title = 'Details',
    showRefresh = false,
    showEdit = false,
    editMode = false,
    onClose,
    onExpand,
    onRefresh,
    onToggleEdit,
    onSearch,
    children,
    headerLeft,
    headerActions,
    footer
  }: Props = $props();

  // Track if sidebar was already open to prevent re-animation on content changes
  let wasOpen = $state(false);
  let prevOpen = false; // plain var — not tracked by $effect

  $effect(() => {
    const isOpen = open; // track the prop
    if (isOpen && !prevOpen) {
      // Sidebar just opened — allow the slide-in animation, then suppress future ones
      const timer = setTimeout(() => {
        wasOpen = true;
      }, 350);
      return () => clearTimeout(timer);
    } else if (!isOpen && prevOpen) {
      // Sidebar closing — reset so next open re-animates
      wasOpen = false;
    }
    prevOpen = isOpen;
  });

  // Temporarily enable width transition only when expanded changes intentionally
  let animateWidth = $state(false);
  let prevExpanded: boolean | null = null; // plain var — not tracked by $effect

  $effect(() => {
    const exp = expanded; // track the prop
    if (prevExpanded !== null && exp !== prevExpanded) {
      animateWidth = true;
      prevExpanded = exp;
      setTimeout(() => (animateWidth = false), 350);
    } else if (prevExpanded === null) {
      prevExpanded = exp;
    }
  });

  $effect(() => {
    if (open) {
      registerOverlay('expandable-sidebar', onClose);
      return () => unregisterOverlay('expandable-sidebar');
    }
  });

  function handleBackdropClick() {
    onClose();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && open) {
      onClose();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<!-- Backdrop -->
<div
  class="inspector-backdrop"
  class:open
  role="presentation"
  onclick={handleBackdropClick}
  onkeydown={(e) => e.key === 'Escape' && onClose()}
></div>

<!-- Inspector Panel -->
<aside
  class="inspector"
  class:open
  class:expanded
  class:no-animate={wasOpen && open}
  class:animate-width={animateWidth}
>
  <header class="inspector-header">
    <div class="inspector-header-left">
      {#if headerLeft}
        {@render headerLeft()}
      {/if}
      <span class="inspector-title">{title}</span>
    </div>
    <div class="inspector-actions">
      {#if headerActions}
        {@render headerActions()}
      {:else}
        {#if editMode && onSearch}
          <button class="icon-btn small" type="button" title="Search Metadata" onclick={onSearch}>
            <span class="material-icons-outlined">search</span>
          </button>
        {/if}
        {#if showRefresh && onRefresh && !editMode}
          <button class="icon-btn small" type="button" title="Refresh" onclick={onRefresh}>
            <span class="material-icons-outlined">refresh</span>
          </button>
        {/if}
        {#if (showEdit || editMode) && onToggleEdit}
          <button
            class="icon-btn small"
            class:active={editMode}
            type="button"
            title={editMode ? 'Cancel edit' : 'Edit'}
            onclick={onToggleEdit}
          >
            <span class="material-icons-outlined">edit</span>
          </button>
        {/if}
        {#if onExpand}
          <button class="icon-btn small" type="button" title="Open full page" onclick={onExpand}>
            <span class="material-icons-outlined">open_in_full</span>
          </button>
        {/if}
        <button class="icon-btn small" type="button" title="Close" onclick={onClose}>
          <span class="material-icons-outlined">close</span>
        </button>
      {/if}
    </div>
  </header>
  <div class="inspector-body">
    <div class="inspector-body-inner">
      {@render children()}
    </div>
  </div>
  {#if footer}
    <div class="inspector-footer">
      {@render footer()}
    </div>
  {/if}
</aside>

<style>
  .inspector-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    opacity: 0;
    pointer-events: none;
    transition: opacity 0.2s;
    z-index: 40;
  }

  .inspector-backdrop.open {
    opacity: 1;
    pointer-events: auto;
  }

  .inspector {
    position: fixed;
    top: calc(16px + env(safe-area-inset-top, 0px));
    bottom: calc(16px + env(safe-area-inset-bottom, 0px));
    right: calc(16px + env(safe-area-inset-right, 0px));
    width: clamp(550px, 40vw, 750px);
    background: rgba(25, 29, 36, 0.85);
    backdrop-filter: blur(20px) saturate(1.8);
    -webkit-backdrop-filter: blur(20px) saturate(1.8);
    border: 1px solid rgba(255, 255, 255, 0.1);
    border-radius: 24px;
    box-shadow:
      -20px 0 60px rgba(0, 0, 0, 0.6),
      0 0 0 1px rgba(255, 255, 255, 0.05) inset;
    transform: translateX(calc(100% + 32px));
    transition: transform 0.4s cubic-bezier(0.16, 1, 0.3, 1);
    display: flex;
    flex-direction: column;
    z-index: 50;
    overflow: hidden;
  }

  :global([data-theme='light']) .inspector {
    background: rgba(255, 249, 241, 0.85);
    border: 1px solid rgba(0, 0, 0, 0.08);
    box-shadow:
      -20px 0 60px rgba(0, 0, 0, 0.15),
      0 0 0 1px rgba(255, 255, 255, 0.5) inset;
  }

  .inspector.open {
    transform: translateX(0);
  }

  .inspector-header {
    padding: 16px 24px;
    border-bottom: 1px solid var(--ring);
    display: flex;
    justify-content: space-between;
    align-items: center;
    background: rgba(255, 255, 255, 0.03);
    flex-shrink: 0;
  }

  .inspector-header-left {
    display: flex;
    align-items: center;
    gap: 12px;
    flex: 1;
    min-width: 0;
  }

  .inspector-title {
    font-weight: 800;
    font-size: 16px;
    letter-spacing: -0.01em;
    color: var(--ink);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .inspector-actions {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-shrink: 0;
  }

  .inspector-body {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    position: relative;
    display: flex;
    flex-direction: column;
  }

  .inspector-body-inner {
    padding: 24px;
    display: flex;
    flex-direction: column;
    flex: 1;
  }

  .inspector-footer {
    padding: 16px 24px;
    border-top: 1px solid var(--ring);
    background: rgba(255, 255, 255, 0.02);
    flex-shrink: 0;
  }

  /* Hide footer wrapper when snippet renders no element children */
  .inspector-footer:not(:has(*)) {
    display: none;
  }

  .icon-btn {
    width: 36px;
    height: 36px;
    display: flex;
    align-items: center;
    justify-content: center;
    border: 1px solid var(--ring);
    border-radius: 10px;
    background: var(--bg-2);
    color: var(--muted);
    cursor: pointer;
    transition: all 0.2s;
  }

  .icon-btn:hover {
    background: var(--ring);
    color: var(--ink);
    border-color: var(--muted);
    transform: translateY(-1px);
  }

  .icon-btn.active {
    background: var(--accent-dim);
    color: var(--accent);
    border-color: var(--accent);
  }

  .icon-btn.active:hover {
    background: var(--accent);
    color: #1d130c;
  }

  .icon-btn.small {
    width: 32px;
    height: 32px;
  }

  /* Mobile: full screen modal */
  @media (max-width: 768px) {
    .inspector {
      top: 0;
      bottom: 0;
      left: 0;
      right: 0;
      width: 100% !important;
      border-radius: 0;
      border: none;
    }

    .inspector.expanded {
      width: 100% !important;
    }
  }
</style>

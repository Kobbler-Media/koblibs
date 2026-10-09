<script lang="ts">
  import { onDestroy } from 'svelte';
  import type { SearchSelectItem } from './search-select-types';

  interface Props {
    id?: string;
    items?: SearchSelectItem[];
    value?: string;
    placeholder?: string;
    disabled?: boolean;
    allowCustom?: boolean;
    allowCreate?: boolean;
    initialLabel?: string;
    searchFn?: (query: string) => Promise<SearchSelectItem[]>;
    onchange?: (value: string, item?: SearchSelectItem) => void;
  }

  let {
    id,
    items = [],
    value = $bindable(''),
    placeholder = 'Select...',
    disabled = false,
    allowCustom = false,
    allowCreate = false,
    initialLabel = '',
    searchFn,
    onchange
  }: Props = $props();

  const canCreate = $derived(allowCustom || allowCreate);

  let open = $state(false);
  let query = $state('');
  let blurTimer: ReturnType<typeof setTimeout> | null = null;
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  let inputEl: HTMLInputElement | undefined;
  let wrapperEl = $state<HTMLDivElement | undefined>();
  let dropdownEl = $state<HTMLDivElement | undefined>();
  let dropdownPos = $state({ top: 0, left: 0, width: 0 });
  let searchResults = $state<SearchSelectItem[]>([]);
  let isSearching = $state(false);

  const rawItems = $derived(searchFn ? searchResults : items);
  // Deduplicate by value to prevent Svelte each_key_duplicate errors
  const effectiveItems = $derived(
    rawItems.filter((item, i) => rawItems.findIndex((x) => x.value === item.value) === i)
  );

  const selectedLabel = $derived(
    effectiveItems.find((item) => item.value === value)?.label ??
      (initialLabel || (canCreate ? value : ''))
  );
  const normalizedQuery = $derived(query.trim().toLowerCase());
  const hasMatch = $derived(
    effectiveItems.some((item) => item.label.toLowerCase() === normalizedQuery)
  );
  const showCreate = $derived(canCreate && normalizedQuery.length > 0 && !hasMatch);
  const filtered = $derived(
    effectiveItems.filter((item) => item.label.toLowerCase().includes(normalizedQuery))
  );

  // Async search
  $effect(() => {
    if (!searchFn || !open) return;

    const q = query.trim();
    if (searchTimer) clearTimeout(searchTimer);

    if (q.length === 0) {
      searchResults = [];
      return;
    }

    searchTimer = setTimeout(async () => {
      isSearching = true;
      try {
        searchResults = await searchFn(q);
      } catch {
        searchResults = [];
      } finally {
        isSearching = false;
      }
    }, 200);
  });

  // Sync query with selectedLabel when closed
  $effect(() => {
    const label = selectedLabel;
    if (!open) {
      query = label;
    }
  });

  // Click-outside and scroll-attach when open
  $effect(() => {
    if (!open) return;

    const handleClickOutside = (e: MouseEvent) => {
      const target = e.target as Node;
      if (wrapperEl?.contains(target) || dropdownEl?.contains(target)) return;
      if (canCreate) {
        const trimmed = query.trim();
        if (trimmed.length > 0 && trimmed !== selectedLabel) {
          value = trimmed;
          onchange?.(value, { value: trimmed, label: trimmed });
        }
      }
      open = false;
    };

    let rafId: number | null = null;
    const handleScroll = () => {
      if (rafId) return;
      rafId = requestAnimationFrame(() => {
        updatePosition();
        rafId = null;
      });
    };

    // Capture phase + stopPropagation: Escape belongs to the open dropdown, not to
    // whatever is behind it. Without this, one Escape inside a modal closes both.
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        open = false;
      }
    };

    document.addEventListener('click', handleClickOutside, true);
    window.addEventListener('scroll', handleScroll, { capture: true, passive: true });
    document.addEventListener('keydown', handleKeyDown, true);

    return () => {
      document.removeEventListener('click', handleClickOutside, true);
      window.removeEventListener('scroll', handleScroll, { capture: true });
      document.removeEventListener('keydown', handleKeyDown, true);
      if (rafId) cancelAnimationFrame(rafId);
    };
  });

  onDestroy(() => {
    if (blurTimer) clearTimeout(blurTimer);
    if (searchTimer) clearTimeout(searchTimer);
  });

  const updatePosition = () => {
    if (inputEl) {
      const rect = inputEl.getBoundingClientRect();
      dropdownPos = {
        top: rect.bottom + 6,
        left: rect.left,
        width: rect.width
      };
    }
  };

  const handleFocus = () => {
    query = '';
    updatePosition();
    open = true;
  };

  const handleClick = () => {
    if (!open) {
      query = '';
      updatePosition();
      open = true;
    }
  };

  const choose = (item: SearchSelectItem) => {
    value = item.value;
    query = item.label;
    open = false;
    onchange?.(value, item);
  };

  const handleMouseDown = (e: MouseEvent, item: SearchSelectItem) => {
    e.preventDefault();
    choose(item);
  };

  const portal = (node: HTMLElement) => {
    document.body.appendChild(node);
    return {
      destroy() {
        node.remove();
      }
    };
  };
</script>

<div class="search-select" bind:this={wrapperEl}>
  <input
    {id}
    type="text"
    bind:this={inputEl}
    bind:value={query}
    {placeholder}
    onfocus={handleFocus}
    onclick={handleClick}
    oninput={() => (open = true)}
    {disabled}
  />
</div>

{#if open}
  <div
    class="search-select-dropdown"
    style="top: {dropdownPos.top}px; left: {dropdownPos.left}px; width: {dropdownPos.width}px;"
    bind:this={dropdownEl}
    use:portal
  >
    {#if isSearching}
      <div class="search-select-loading">Searching...</div>
    {:else}
      {#each searchFn ? searchResults : filtered as item (item.value)}
        <button
          type="button"
          class="search-select-item"
          onmousedown={(e) => handleMouseDown(e, item)}
        >
          <span class="search-select-item-label">{item.label}</span>
          {#if item.description}
            <span class="search-select-item-desc">{item.description}</span>
          {/if}
        </button>
      {/each}
      {#if showCreate}
        <button
          type="button"
          class="search-select-item search-select-create"
          onmousedown={(e) => handleMouseDown(e, { value: query.trim(), label: query.trim() })}
        >
          Create "{query.trim()}"
        </button>
      {:else if (searchFn ? searchResults : filtered).length === 0 && normalizedQuery.length > 0}
        <div class="search-select-empty">No matches</div>
      {:else if (searchFn ? searchResults : filtered).length === 0 && normalizedQuery.length === 0}
        <div class="search-select-empty">Type to search...</div>
      {/if}
    {/if}
  </div>
{/if}

<style>
  .search-select {
    position: relative;
    width: 100%;
    box-sizing: border-box;
  }

  .search-select * {
    box-sizing: border-box;
  }

  .search-select input {
    width: 100%;
    padding: 8px 10px;
    border-radius: 12px;
    border: 1px solid var(--ring);
    background: transparent;
    color: var(--ink);
    font-size: 13px;
  }

  .search-select-dropdown {
    position: fixed;
    max-height: 220px;
    overflow: auto;
    border-radius: 12px;
    border: 1px solid var(--ring);
    background: var(--card);
    box-shadow: var(--shadow);
    z-index: 9999;
    padding: 6px;
    display: grid;
    gap: 4px;
    box-sizing: border-box;
  }

  .search-select-item {
    text-align: left;
    border: none;
    border-radius: 10px;
    background: transparent;
    color: var(--ink);
    padding: 6px 8px;
    cursor: pointer;
    font-size: 13px;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .search-select-item-desc {
    font-size: 11px;
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .search-select-item:hover {
    background: rgba(255, 255, 255, 0.08);
  }

  :global([data-theme='light']) .search-select-item:hover {
    background: rgba(0, 0, 0, 0.08);
  }

  .search-select-empty,
  .search-select-loading {
    color: var(--muted);
    font-size: 12px;
    padding: 6px 8px;
  }

  .search-select-create {
    color: var(--accent);
    font-weight: 500;
  }
</style>

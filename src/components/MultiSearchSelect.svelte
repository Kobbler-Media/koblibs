<script lang="ts">
  import { onDestroy } from 'svelte';
  import type { MultiSearchSelectItem } from './search-select-types';

  type ValueType = string | { value: string; label: string };

  interface Props {
    id?: string;
    items?: MultiSearchSelectItem[];
    values?: ValueType[];
    placeholder?: string;
    disabled?: boolean;
    allowCreate?: boolean;
    dropUp?: boolean;
    searchFn?: (query: string) => Promise<MultiSearchSelectItem[]>;
    onchange?: (values: ValueType[]) => void;
  }

  let {
    id,
    items = [],
    values = $bindable([]),
    placeholder = 'Search...',
    disabled = false,
    allowCreate = false,
    dropUp = false,
    searchFn,
    onchange
  }: Props = $props();

  let open = $state(false);
  let query = $state('');
  let blurTimer: ReturnType<typeof setTimeout> | null = null;
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  let inputRef: HTMLInputElement | null = null;
  let wrapperEl = $state<HTMLDivElement | undefined>();
  let dropdownEl = $state<HTMLDivElement | undefined>();
  let dropdownPos = $state({ top: 0, left: 0, width: 0 });
  let searchResults = $state<MultiSearchSelectItem[]>([]);
  let isSearching = $state(false);

  const isObjectMode = $derived(
    searchFn !== undefined || (values.length > 0 && typeof values[0] === 'object')
  );

  const normalizedValues = $derived<MultiSearchSelectItem[]>(
    values.map((v) => {
      if (typeof v === 'string') {
        const item = items.find((i) => i.value === v);
        return { value: v, label: item?.label ?? v };
      }
      return v;
    })
  );

  const valueStrings = $derived(normalizedValues.map((v) => v.value));
  const rawItems = $derived(searchFn ? searchResults : items);
  // Deduplicate by value to prevent Svelte each_key_duplicate errors
  const effectiveItems = $derived(
    rawItems.filter((item, i) => rawItems.findIndex((x) => x.value === item.value) === i)
  );

  const normalizedQuery = $derived(query.trim().toLowerCase());
  const hasMatch = $derived(
    effectiveItems.some((item) => item.label.toLowerCase() === normalizedQuery)
  );
  const showCreate = $derived(allowCreate && normalizedQuery.length > 0 && !hasMatch);
  const filtered = $derived(
    effectiveItems.filter(
      (item) =>
        !valueStrings.includes(item.value) && item.label.toLowerCase().includes(normalizedQuery)
    )
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

  // Click-outside and scroll-attach when open
  $effect(() => {
    if (!open) return;

    const handleClickOutside = (e: MouseEvent) => {
      const target = e.target as Node;
      if (wrapperEl?.contains(target) || dropdownEl?.contains(target)) return;
      open = false;
      query = '';
    };

    let rafId: number | null = null;
    const handleScroll = () => {
      if (rafId) return;
      rafId = requestAnimationFrame(() => {
        updatePosition();
        rafId = null;
      });
    };

    document.addEventListener('click', handleClickOutside, true);
    window.addEventListener('scroll', handleScroll, { capture: true, passive: true });

    return () => {
      document.removeEventListener('click', handleClickOutside, true);
      window.removeEventListener('scroll', handleScroll, { capture: true });
      if (rafId) cancelAnimationFrame(rafId);
    };
  });

  const emitChange = (newValues: MultiSearchSelectItem[]) => {
    if (isObjectMode) {
      values = newValues;
      onchange?.(newValues);
    } else {
      const stringValues = newValues.map((v) => v.value);
      values = stringValues as ValueType[];
      onchange?.(stringValues);
    }
  };

  const select = (item: MultiSearchSelectItem) => {
    if (!valueStrings.includes(item.value)) {
      emitChange([...normalizedValues, item]);
    }
    query = '';
    searchResults = [];
    inputRef?.focus();
  };

  const remove = (value: string) => {
    emitChange(normalizedValues.filter((v) => v.value !== value));
    inputRef?.focus();
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === 'Backspace' && query === '' && normalizedValues.length > 0) {
      emitChange(normalizedValues.slice(0, -1));
    }
    if (e.key === 'Escape') {
      // Claim Escape while the dropdown is open so it doesn't also close an
      // enclosing modal (see the global handler in stores/overlay.svelte.ts).
      if (open) e.stopPropagation();
      open = false;
      query = '';
    }
    if (e.key === 'Enter' && allowCreate && query.trim() && !hasMatch) {
      e.preventDefault();
      select({ value: query.trim(), label: query.trim() });
    }
  };

  const handleMouseDown = (e: MouseEvent, item: MultiSearchSelectItem) => {
    e.preventDefault();
    select(item);
  };

  const updatePosition = () => {
    if (!wrapperEl) return;
    const rect = wrapperEl.getBoundingClientRect();
    if (dropUp) {
      dropdownPos = {
        top: rect.top - 6,
        left: rect.left,
        width: rect.width
      };
    } else {
      dropdownPos = {
        top: rect.bottom + 6,
        left: rect.left,
        width: rect.width
      };
    }
  };

  const handleFocus = () => {
    updatePosition();
    open = true;
  };

  const portal = (node: HTMLElement) => {
    document.body.appendChild(node);
    return {
      destroy() {
        node.remove();
      }
    };
  };

  onDestroy(() => {
    if (blurTimer) clearTimeout(blurTimer);
    if (searchTimer) clearTimeout(searchTimer);
  });
</script>

<div class="multi-search-select" bind:this={wrapperEl}>
  <div class="multi-search-select-input-container" class:open>
    {#each normalizedValues as item (item.value)}
      <button
        type="button"
        class="pill"
        onclick={() => remove(item.value)}
        {disabled}
        aria-label="Remove {item.label}"
      >
        <span class="pill-label">{item.label}</span>
        <svg class="pill-remove" viewBox="0 0 16 16" width="12" height="12" fill="currentColor">
          <path d="M4.5 3.5L12 11M12 3.5L4.5 11" stroke="currentColor" stroke-width="1.5"></path>
        </svg>
      </button>
    {/each}
    <input
      {id}
      bind:this={inputRef}
      type="text"
      bind:value={query}
      {placeholder}
      onfocus={handleFocus}
      oninput={() => (open = true)}
      onkeydown={handleKeyDown}
      {disabled}
    />
  </div>
</div>

{#if open}
  <div
    class="multi-search-select-list"
    class:drop-up={dropUp}
    style="top: {dropUp ? 'auto' : dropdownPos.top + 'px'}; bottom: {dropUp ? (typeof window !== 'undefined' ? window.innerHeight - dropdownPos.top : 0) + 'px' : 'auto'}; left: {dropdownPos.left}px; width: {dropdownPos.width}px;"
    bind:this={dropdownEl}
    use:portal
  >
    {#if isSearching}
      <div class="multi-search-select-empty">Searching...</div>
    {:else if filtered.length > 0}
      {#each filtered as item (item.value)}
        <button
          type="button"
          class="multi-search-select-item"
          onmousedown={(e) => handleMouseDown(e, item)}
        >
          <span class="multi-search-select-item-label">{item.label}</span>
          {#if item.description}
            <span class="multi-search-select-item-desc">{item.description}</span>
          {/if}
        </button>
      {/each}
    {/if}
    {#if showCreate}
      <button
        type="button"
        class="multi-search-select-item multi-search-select-create"
        onmousedown={(e) => handleMouseDown(e, { value: query.trim(), label: query.trim() })}
      >
        Create "{query.trim()}"
      </button>
    {:else if !isSearching && filtered.length === 0 && normalizedQuery.length > 0}
      <div class="multi-search-select-empty">No matches</div>
    {:else if !isSearching && searchFn && normalizedQuery.length === 0}
      <div class="multi-search-select-empty">Type to search...</div>
    {/if}
  </div>
{/if}

<style>
  .multi-search-select {
    position: relative;
    width: 100%;
    box-sizing: border-box;
  }

  .multi-search-select * {
    box-sizing: border-box;
  }

  .multi-search-select-input-container {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 6px 10px;
    border-radius: 12px;
    border: 1px solid var(--ring);
    background: transparent;
    min-height: 38px;
    align-items: center;
  }

  .multi-search-select-input-container.open {
    border-color: var(--accent);
  }

  .multi-search-select-input-container input {
    flex: 1;
    min-width: 80px;
    border: none;
    background: transparent;
    color: var(--ink);
    font-size: 13px;
    padding: 2px 0;
    outline: none;
  }

  .multi-search-select-input-container input::placeholder {
    color: var(--muted);
  }

  .pill {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 4px 8px;
    border-radius: 999px;
    border: 1px solid var(--ring);
    background: var(--ring);
    color: var(--ink);
    font-size: 12px;
    cursor: pointer;
    transition: background 0.1s;
  }

  .pill:hover:not(:disabled) {
    background: rgba(255, 255, 255, 0.1);
  }

  :global([data-theme='light']) .pill:hover:not(:disabled) {
    background: rgba(0, 0, 0, 0.1);
  }

  .pill:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .pill-label {
    white-space: nowrap;
  }

  .pill-remove {
    opacity: 0.6;
  }

  .pill:hover .pill-remove {
    opacity: 1;
  }

  .multi-search-select-list {
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

  .multi-search-select-item {
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

  .multi-search-select-item-desc {
    font-size: 11px;
    color: var(--muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .multi-search-select-item:hover {
    background: rgba(255, 255, 255, 0.08);
  }

  :global([data-theme='light']) .multi-search-select-item:hover {
    background: rgba(0, 0, 0, 0.08);
  }

  .multi-search-select-empty {
    color: var(--muted);
    font-size: 12px;
    padding: 6px 8px;
  }

  .multi-search-select-create {
    color: var(--accent);
    font-weight: 500;
  }
</style>

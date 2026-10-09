<script lang="ts">
  import { onMount } from 'svelte';

  interface Props {
    hasMore: boolean;
    loading: boolean;
    onLoadMore: () => void;
    threshold?: number;
  }

  let { hasMore, loading, onLoadMore, threshold = 200 }: Props = $props();

  let sentinel: HTMLDivElement | undefined = $state();
  let observer: IntersectionObserver | undefined = $state();

  onMount(() => {
    observer = new IntersectionObserver(
      (entries) => {
        const entry = entries[0];
        if (entry.isIntersecting && hasMore && !loading) {
          onLoadMore();
        }
      },
      {
        rootMargin: `${threshold}px`
      }
    );

    if (sentinel) {
      observer.observe(sentinel);
    }

    return () => {
      observer?.disconnect();
    };
  });

  $effect(() => {
    // Re-observe when sentinel changes
    if (observer && sentinel) {
      observer.disconnect();
      observer.observe(sentinel);
    }
  });
</script>

<div bind:this={sentinel} class="infinite-scroll-sentinel" aria-hidden="true">
  {#if loading}
    <div class="loading-indicator">
      <span class="spinner"></span>
      Loading more...
    </div>
  {:else if !hasMore}
    <div class="end-indicator">No more items</div>
  {/if}
</div>

<style>
  .infinite-scroll-sentinel {
    min-height: 1px;
    padding: 1rem 0;
    text-align: center;
    color: var(--color-text-secondary, #888);
    font-size: 0.875rem;
  }

  .loading-indicator {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 0.5rem;
  }

  .spinner {
    display: inline-block;
    width: 1rem;
    height: 1rem;
    border: 2px solid currentColor;
    border-right-color: transparent;
    border-radius: 50%;
    animation: spin 0.75s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  .end-indicator {
    opacity: 0.6;
  }
</style>

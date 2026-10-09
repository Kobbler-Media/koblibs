<script lang="ts">
  import { onMount } from 'svelte';

  interface Props {
    /** Scroll threshold in pixels before showing the button */
    threshold?: number;
  }

  let { threshold = 300 }: Props = $props();

  let visible = $state(false);
  let scrollContainer: HTMLElement | null = null;

  function getScrollContainer(): HTMLElement {
    // Look for the main.content scroll container used by the app layout
    const main = document.querySelector('main.content');
    if (main && main instanceof HTMLElement) {
      const style = getComputedStyle(main);
      if (style.overflowY === 'auto' || style.overflowY === 'scroll') {
        return main;
      }
    }
    // Fallback to document element
    return document.documentElement;
  }

  function handleScroll() {
    if (!scrollContainer) return;
    visible = scrollContainer.scrollTop > threshold;
  }

  function scrollToTop() {
    // Scroll viewport to top immediately
    if (scrollContainer) {
      scrollContainer.scrollTop = 0;
    }
    // Dispatch event for pages to reload their data from the beginning
    window.dispatchEvent(new CustomEvent('scrolltotop'));
  }

  onMount(() => {
    scrollContainer = getScrollContainer();
    scrollContainer.addEventListener('scroll', handleScroll, { passive: true });
    // Check initial position
    handleScroll();

    return () => {
      scrollContainer?.removeEventListener('scroll', handleScroll);
    };
  });
</script>

{#if visible}
  <button
    type="button"
    class="scroll-to-top"
    onclick={scrollToTop}
    aria-label="Scroll to top"
    title="Scroll to top"
  >
    <svg viewBox="0 0 24 24" width="20" height="20" fill="currentColor">
      <path d="M7.41 15.41 12 10.83l4.59 4.58L18 14l-6-6-6 6z"></path>
    </svg>
  </button>
{/if}

<style>
  .scroll-to-top {
    position: fixed;
    bottom: calc(24px + env(safe-area-inset-bottom, 0px));
    right: calc(24px + env(safe-area-inset-right, 0px));
    width: 44px;
    height: 44px;
    border-radius: 50%;
    border: 1px solid var(--ring);
    background: var(--card);
    color: var(--ink);
    cursor: pointer;
    display: flex;
    align-items: center;
    justify-content: center;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
    transition: all 0.2s ease;
    z-index: 50;
    opacity: 0.9;
  }

  .scroll-to-top:hover {
    opacity: 1;
    transform: translateY(-2px);
    box-shadow: 0 6px 16px rgba(0, 0, 0, 0.4);
    background: var(--accent);
    color: #1d130c;
  }

  @media (max-width: 640px) {
    .scroll-to-top {
      bottom: 16px;
      right: 16px;
      width: 40px;
      height: 40px;
    }
  }
</style>

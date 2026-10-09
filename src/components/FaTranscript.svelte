<script lang="ts">
  interface Props {
    text: string;
    activeWordIndex: number;
    onWordClick?: (wordIndex: number) => void;
  }

  let { text, activeWordIndex, onWordClick }: Props = $props();

  type Token =
    | { kind: 'space'; raw: string; pos: number }
    | { kind: 'word'; raw: string; index: number; pos: number };

  const tokens = $derived.by<Token[]>(() => {
    const out: Token[] = [];
    let wordIdx = 0;
    let pos = 0;
    for (const piece of text.split(/(\s+)/)) {
      if (!piece) continue;
      if (/^\s+$/.test(piece)) {
        out.push({ kind: 'space', raw: piece, pos: pos++ });
      } else {
        out.push({ kind: 'word', raw: piece, index: wordIdx++, pos: pos++ });
      }
    }
    return out;
  });
</script>

<div class="fa-transcript">
  {#each tokens as t (t.pos)}
    {#if t.kind === 'space'}<span class="ws">{t.raw}</span>{:else}<button
        type="button"
        class="word"
        class:active={t.index === activeWordIndex}
        onclick={() => onWordClick?.(t.index)}
        disabled={!onWordClick}
        tabindex={onWordClick ? 0 : -1}>{t.raw}</button
      >{/if}
  {/each}
</div>

<style>
  .fa-transcript {
    line-height: 1.7;
    font-size: 16px;
    color: var(--ink, #e6e6e6);
    word-wrap: break-word;
  }

  .word {
    display: inline;
    padding: 1px 2px;
    margin: 0;
    border: 0;
    background: transparent;
    color: inherit;
    font: inherit;
    border-radius: 4px;
    cursor: default;
    transition:
      background-color 80ms linear,
      color 80ms linear;
  }

  .word:not(:disabled) {
    cursor: pointer;
  }

  .word.active {
    background: var(--accent, #4f8cff);
    color: var(--accent-fg, #ffffff);
  }

  .ws {
    white-space: pre-wrap;
  }
</style>

<script lang="ts">
  import type { Snippet } from 'svelte';
  import Modal from './Modal.svelte';

  interface Props {
    title?: string;
    message?: string;
    confirmLabel?: string;
    cancelLabel?: string;
    danger?: boolean;
    loading?: boolean;
    onConfirm: () => void;
    onCancel: () => void;
    children?: Snippet;
  }

  let {
    title = 'Confirm Delete',
    message = 'Are you sure? This action cannot be undone.',
    confirmLabel = 'Delete',
    cancelLabel = 'Cancel',
    danger = true,
    loading = false,
    onConfirm,
    onCancel,
    children
  }: Props = $props();
</script>

<Modal title={title} onClose={onCancel}>
  <div class="confirm-body">
    {#if children}
      {@render children()}
    {:else}
      <p>{message}</p>
    {/if}
  </div>

  {#snippet footer()}
    <button type="button" class="btn-cancel" onclick={onCancel} disabled={loading}>
      {cancelLabel}
    </button>
    <button
      type="button"
      class="btn-confirm"
      class:danger
      onclick={onConfirm}
      disabled={loading}
    >
      {#if loading}
        <span class="spinner"></span>
      {/if}
      {confirmLabel}
    </button>
  {/snippet}
</Modal>

<style>
  .confirm-body {
    padding: 8px 0;
  }

  .confirm-body p {
    margin: 0;
    color: var(--muted);
    font-size: 14px;
    line-height: 1.5;
  }

  .btn-cancel {
    padding: 8px 16px;
    border-radius: 8px;
    border: 1px solid var(--ring);
    background: transparent;
    color: var(--ink);
    cursor: pointer;
    font-size: 13px;
  }

  .btn-cancel:hover {
    background: var(--bg-2);
  }

  .btn-confirm {
    padding: 8px 16px;
    border-radius: 8px;
    border: 1px solid transparent;
    background: var(--accent);
    color: #1d130c;
    cursor: pointer;
    font-weight: 600;
    font-size: 13px;
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }

  .btn-confirm.danger {
    background: #ef4444;
    color: white;
  }

  .btn-confirm:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .spinner {
    display: inline-block;
    width: 14px;
    height: 14px;
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
</style>

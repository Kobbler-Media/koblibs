<script lang="ts">
  interface Props {
    count: number;
    visible: boolean;
    // Optional actions - only shown if callback provided
    onSync?: () => void;
    onSendToDevice?: () => void;
    onAddToShelf?: () => void;
    onDelete?: () => void;
    onApprove?: () => void;
    onReject?: () => void;
    onEnable?: () => void;
    onDisable?: () => void;
    onClear?: () => void;
  }

  let {
    count,
    visible,
    onSync,
    onSendToDevice,
    onAddToShelf,
    onDelete,
    onApprove,
    onReject,
    onEnable,
    onDisable,
    onClear
  }: Props = $props();
</script>

<div class="bottom-bar" class:visible>
  <span class="count">{count} Selected</span>

  {#if onSync}
    <button type="button" onclick={onSync}>Sync</button>
  {/if}

  {#if onSendToDevice}
    <button type="button" onclick={onSendToDevice}>Devices</button>
  {/if}

  {#if onAddToShelf}
    <button type="button" onclick={onAddToShelf}>Shelf</button>
  {/if}

  {#if onEnable}
    <button type="button" onclick={onEnable}>Enable</button>
  {/if}

  {#if onDisable}
    <button type="button" onclick={onDisable}>Disable</button>
  {/if}

  {#if onApprove}
    <button type="button" onclick={onApprove}>Approve All</button>
  {/if}

  {#if onReject}
    <button type="button" class="delete-btn" onclick={onReject}>Reject All</button>
  {/if}

  {#if onDelete}
    <button type="button" class="delete-btn" onclick={onDelete}>Delete</button>
  {/if}

  {#if onClear}
    <button type="button" class="clear-btn" onclick={onClear}>
      <span class="material-icons-outlined" style="font-size: 16px;">close</span>
    </button>
  {/if}
</div>

<style>
  .bottom-bar {
    position: fixed;
    bottom: calc(32px + env(safe-area-inset-bottom, 0px));
    left: 50%;
    transform: translateX(-50%) translateY(calc(100% + 50px));
    background: var(--accent);
    color: #1d130c;
    padding: 12px 24px;
    border-radius: 100px;
    display: flex;
    align-items: center;
    gap: 16px;
    box-shadow: 0 10px 40px rgba(0, 0, 0, 0.4);
    transition: transform 0.3s cubic-bezier(0.175, 0.885, 0.32, 1.275);
    z-index: 60;
  }

  .bottom-bar.visible {
    transform: translateX(-50%) translateY(0);
  }

  .count {
    font-weight: 800;
    font-size: 12px;
    border-right: 1px solid rgba(0, 0, 0, 0.2);
    padding-right: 12px;
    margin-right: 4px;
    white-space: nowrap;
  }

  .bottom-bar button {
    background: none;
    border: none;
    font-weight: 600;
    cursor: pointer;
    color: #1d130c;
    padding: 4px 8px;
    border-radius: 6px;
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .bottom-bar button:hover {
    background: rgba(0, 0, 0, 0.1);
  }

  .delete-btn {
    background: rgba(0, 0, 0, 0.2) !important;
    border-radius: 20px !important;
    padding: 4px 12px !important;
    color: #7f1d1d !important;
  }

  .clear-btn {
    padding: 4px !important;
  }
</style>

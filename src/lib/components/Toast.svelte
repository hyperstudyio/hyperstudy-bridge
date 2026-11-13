<script>
  import * as toastStore from '../stores/toast.svelte.js';

  const toasts = $derived(toastStore.getToasts());

  const iconPaths = {
    success: 'M9 12l2 2 4-4m6 2a9 9 0 11-18 0 9 9 0 0118 0z',
    info: 'M13 16h-1v-4h-1m1-4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z',
    warning:
      'M12 9v2m0 4h.01m-6.938 4h13.856c1.54 0 2.502-1.667 1.732-3L13.732 4c-.77-1.333-2.694-1.333-3.464 0L3.34 16c-.77 1.333.192 3 1.732 3z',
    error: 'M10 14l2-2m0 0l2-2m-2 2l-2-2m2 2l2 2m7-2a9 9 0 11-18 0 9 9 0 0118 0z',
    update: 'M4 16v1a3 3 0 003 3h10a3 3 0 003-3v-1m-4-4l-4 4m0 0l-4-4m4 4V4',
  };

  function getIcon(type) {
    return iconPaths[type] || iconPaths.info;
  }

  function getColorVar(type) {
    switch (type) {
      case 'success':
        return 'var(--color-success)';
      case 'warning':
        return 'var(--color-warning)';
      case 'error':
        return 'var(--color-error)';
      case 'update':
        return 'var(--color-primary)';
      default:
        return 'var(--color-info)';
    }
  }
</script>

{#if toasts.length > 0}
  <div class="toast-container">
    {#each toasts as toast (toast.id)}
      <div class="toast" style="--toast-color: {getColorVar(toast.type)}">
        <div class="toast-icon">
          <svg
            width="20"
            height="20"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d={getIcon(toast.type)}></path>
          </svg>
        </div>

        <div class="toast-content">
          <div class="toast-title">{toast.title}</div>
          {#if toast.message}
            <div class="toast-message">{toast.message}</div>
          {/if}

          {#if toast.actions && toast.actions.length > 0}
            <div class="toast-actions">
              {#each toast.actions as action, i}
                <button
                  class="toast-action-btn"
                  class:primary={i === 0}
                  onclick={() => action.onClick(toast.id)}
                >
                  {action.label}
                </button>
              {/each}
            </div>
          {/if}
        </div>

        <button
          class="toast-close"
          onclick={() => toastStore.removeToast(toast.id)}
          title="Dismiss"
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
          >
            <path d="M18 6L6 18M6 6l12 12"></path>
          </svg>
        </button>
      </div>
    {/each}
  </div>
{/if}

<style>
  .toast-container {
    position: fixed;
    bottom: 4rem;
    right: 1.5rem;
    z-index: 9999;
    display: flex;
    flex-direction: column-reverse;
    gap: 0.75rem;
    max-width: 380px;
    pointer-events: none;
  }

  .toast {
    display: flex;
    align-items: flex-start;
    gap: 0.75rem;
    padding: 0.875rem 1rem;
    background: var(--color-surface-elevated);
    border: 1px solid var(--color-border);
    border-radius: 8px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    animation: toast-slide-in 0.3s ease-out;
    pointer-events: auto;
  }

  .toast-icon {
    flex-shrink: 0;
    color: var(--toast-color);
    margin-top: 1px;
  }

  .toast-content {
    flex: 1;
    min-width: 0;
  }

  .toast-title {
    font-size: 0.875rem;
    font-weight: 600;
    color: var(--color-text-primary);
    line-height: 1.3;
  }

  .toast-message {
    font-size: 0.8125rem;
    color: var(--color-text-secondary);
    margin-top: 0.25rem;
    line-height: 1.4;
  }

  .toast-actions {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.625rem;
  }

  .toast-action-btn {
    padding: 0.375rem 0.75rem;
    border-radius: 5px;
    font-size: 0.75rem;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s;
    background: var(--color-surface);
    color: var(--color-text-secondary);
    border: 1px solid var(--color-border);
  }

  .toast-action-btn:hover {
    background: var(--color-border-hover);
    color: var(--color-text-primary);
  }

  .toast-action-btn.primary {
    background: var(--toast-color);
    color: white;
    border-color: var(--toast-color);
  }

  .toast-action-btn.primary:hover {
    filter: brightness(1.15);
  }

  .toast-close {
    flex-shrink: 0;
    padding: 0.25rem;
    border-radius: 4px;
    background: none;
    border: none;
    color: var(--color-text-disabled);
    cursor: pointer;
    transition: all 0.15s;
  }

  .toast-close:hover {
    color: var(--color-text-primary);
    background: var(--color-border);
  }

  @keyframes toast-slide-in {
    from {
      opacity: 0;
      transform: translateX(1rem) scale(0.96);
    }
    to {
      opacity: 1;
      transform: translateX(0) scale(1);
    }
  }
</style>

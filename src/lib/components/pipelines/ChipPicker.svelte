<script>
  // Chooses names from a list by clicking them: chosen ones are filled, in the order they were chosen. A chosen
  // name the input no longer has is marked, with × to drop it.
  let { value = [], options = [], onchange, label = '' } = $props();

  const chosen = $derived(value ?? []);
  const missing = $derived(chosen.filter(v => !options.includes(v)));

  function toggle(name) {
    onchange(chosen.includes(name) ? chosen.filter(v => v !== name) : [...chosen, name]);
  }
</script>

<div class="chips" role="group" aria-label={label}>
  {#each options as name (name)}
    {@const on = chosen.includes(name)}
    <button
      class="chip"
      class:on
      aria-pressed={on}
      title={on ? `Leave out ${name}` : `Use ${name}`}
      onclick={() => toggle(name)}
    >
      {#if on && chosen.length > 1}<span class="order">{chosen.indexOf(name) + 1}</span>{/if}{name}
    </button>
  {/each}
  {#each missing as name (name)}
    <button
      class="chip missing"
      title="{name} is not in this block's input"
      aria-label="Drop {name}, which is not in the input"
      onclick={() => toggle(name)}>{name} ×</button
    >
  {/each}
</div>

<style>
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 0.25rem;
    padding: 0.2rem 0.55rem;
    border: 1px solid var(--color-border);
    border-radius: 999px;
    background: none;
    color: var(--color-text-secondary);
    font-family: ui-monospace, monospace;
    font-size: 0.72rem;
    cursor: pointer;
  }
  .chip:hover {
    border-color: var(--color-border-hover);
    color: var(--color-text-primary);
  }
  .chip.on {
    border-color: color-mix(in srgb, var(--color-primary) 60%, transparent);
    background: color-mix(in srgb, var(--color-primary) 16%, transparent);
    color: var(--color-text-primary);
  }
  .chip.missing {
    border-color: var(--color-error);
    color: var(--color-error);
  }
  .order {
    color: var(--color-primary);
    font-size: 0.62rem;
    font-weight: 700;
  }
</style>

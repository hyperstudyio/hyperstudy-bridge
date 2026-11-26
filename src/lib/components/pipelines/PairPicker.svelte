<script>
  // Pairs of names, each chosen from the input's names: `a / b` for a ratio, `a ↔ b` for a relation.
  let { value = [], options = [], onchange, joiner = '/', label = '' } = $props();

  const pairs = $derived(value ?? []);

  function set(index, side, name) {
    onchange(
      pairs.map((pair, k) =>
        k === index ? pair.map((v, s) => (s === side ? name : v)) : [...pair]
      )
    );
  }
  function add() {
    const [a, b] = [options[0] ?? '', options[1] ?? options[0] ?? ''];
    onchange([...pairs.map(p => [...p]), [a, b]]);
  }
  function remove(index) {
    onchange(pairs.filter((_, k) => k !== index).map(p => [...p]));
  }
</script>

<div class="pairs" role="group" aria-label={label}>
  {#each pairs as pair, index (index)}
    <div class="pair">
      {#each [0, 1] as side (side)}
        {@const current = pair[side]}
        <select
          class:missing={!options.includes(current)}
          aria-label="{label} {index + 1}, {side === 0 ? 'first' : 'second'}"
          value={current}
          onchange={e => set(index, side, e.target.value)}
        >
          {#if !options.includes(current)}<option value={current}>{current} (not in input)</option
            >{/if}
          {#each options as name (name)}<option value={name}>{name}</option>{/each}
        </select>
        {#if side === 0}<span class="joiner">{joiner}</span>{/if}
      {/each}
      <button class="remove" aria-label="Remove pair {index + 1}" onclick={() => remove(index)}
        >×</button
      >
    </div>
  {/each}
  <button class="add" onclick={add} disabled={!options.length}>+ Pair</button>
</div>

<style>
  .pairs {
    display: grid;
    gap: 0.35rem;
  }
  .pair {
    display: grid;
    grid-template-columns: 1fr auto 1fr auto;
    align-items: center;
    gap: 0.3rem;
  }
  select {
    min-width: 0;
    padding: 0.3rem 0.35rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface);
    color: var(--color-text-primary);
    font-family: ui-monospace, monospace;
    font-size: 0.72rem;
  }
  select.missing {
    border-color: var(--color-error);
  }
  .joiner {
    color: var(--color-text-secondary);
  }
  button {
    padding: 0.15rem 0.5rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-secondary);
    cursor: pointer;
  }
  .remove {
    border: none;
    background: none;
  }
  .remove:hover {
    color: var(--color-error);
  }
  .add {
    justify-self: start;
  }
</style>

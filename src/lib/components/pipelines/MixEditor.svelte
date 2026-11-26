<script>
  // New channels as formulas: `brow = forehead_left - forehead_right`. Each formula is a weighted sum of the input's
  // channels; clicking a channel name puts it into the formula last edited. A formula is passed on only when it
  // reads cleanly, so a half-typed one never reaches openfrenz.
  import { formula } from '../../pipelines/fields.js';
  import { cleanId } from '../../pipelines/graph.js';

  let { value = [], channels = null, onchange } = $props();

  // What is typed in each row; rebuilt from the value unless a row is being edited.
  let rows = $state([]);
  let editing = $state(-1);
  let focused = $state(0);
  let inputs = $state([]);

  $effect(() => {
    const shown = (value ?? []).map(c => ({ name: c.name, text: formula.format(c.weights) }));
    if (editing < 0) rows = shown;
  });

  const known = $derived(channels?.length ? channels : null);
  const problems = $derived(
    rows.map(r => (r.text.trim() ? formula.parse(r.text, known).error : ''))
  );

  function emit() {
    const parsed = rows.map(r => ({
      name: cleanId(r.name || 'mix'),
      read: formula.parse(r.text, known),
    }));
    if (parsed.some(p => p.read.error)) return;
    onchange(parsed.map(p => ({ name: p.name, weights: p.read.value })));
  }
  function edit(index, change) {
    editing = index;
    rows = rows.map((r, k) => (k === index ? { ...r, ...change } : r));
    emit();
  }
  function add() {
    const [a, b] = [known?.[0] ?? 'forehead_left', known?.[1] ?? 'forehead_right'];
    rows = [...rows, { name: `mix_${rows.length + 1}`, text: `${a} - ${b}` }];
    emit();
  }
  function remove(index) {
    rows = rows.filter((_, k) => k !== index);
    editing = -1;
    emit();
  }
  // Puts a channel's name into the formula last edited, at its cursor.
  function insert(name) {
    const index = Math.min(focused, rows.length - 1);
    if (index < 0) return;
    const input = inputs[index];
    const text = rows[index].text;
    const at = input?.selectionStart ?? text.length;
    const end = input?.selectionEnd ?? at;
    const before = text.slice(0, at);
    const spaced = before && !/[\s(+\-*/]$/.test(before) ? ` + ${name}` : name;
    edit(index, { text: before + spaced + text.slice(end) });
    editing = -1;
    globalThis.queueMicrotask(() => {
      input?.focus();
      const caret = (before + spaced).length;
      input?.setSelectionRange(caret, caret);
    });
  }
</script>

<div class="mix">
  {#each rows as row, index (index)}
    <div class="row" class:bad={problems[index]}>
      <div class="head">
        <input
          autocomplete="off"
          data-1p-ignore
          class="name"
          aria-label="Name of new channel {index + 1}"
          value={row.name}
          oninput={e => edit(index, { name: e.target.value })}
          onblur={() => (editing = -1)}
        />
        <span class="equals">=</span>
      </div>
      <button class="remove" aria-label="Remove {row.name}" onclick={() => remove(index)}>×</button>
      <input
        autocomplete="off"
        data-1p-ignore
        class="formula"
        aria-label="Formula of {row.name}"
        bind:this={inputs[index]}
        value={row.text}
        placeholder="forehead_left - forehead_right"
        spellcheck="false"
        oninput={e => edit(index, { text: e.target.value })}
        onfocus={() => (focused = index)}
        onblur={() => (editing = -1)}
      />
      {#if problems[index]}<p class="problem" role="alert">{problems[index]}</p>{/if}
    </div>
  {/each}
  <button class="add" onclick={add}>+ Channel</button>
  {#if known}
    <div class="names">
      <span>Insert</span>
      {#each known as name (name)}
        <button class="name-chip" onclick={() => insert(name)}>{name}</button>
      {/each}
    </div>
  {/if}
</div>

<style>
  .mix {
    display: grid;
    gap: 0.4rem;
  }
  /* Name, =, ×; the formula on its own line below, where there is room for it. */
  .row {
    display: grid;
    grid-template-columns: 1fr auto;
    align-items: center;
    gap: 0.3rem;
    padding: 0.45rem;
    border: 1px solid var(--color-border);
    border-radius: 8px;
  }
  .row .head {
    display: flex;
    align-items: center;
    gap: 0.35rem;
  }
  .row .name {
    width: 7rem;
  }
  .row .formula {
    grid-column: 1 / -1;
  }
  input {
    min-width: 0;
    padding: 0.35rem 0.45rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface);
    color: var(--color-text-primary);
    font-family: ui-monospace, monospace;
    font-size: 0.75rem;
  }
  .row.bad .formula {
    border-color: var(--color-error);
  }
  .equals {
    color: var(--color-text-secondary);
  }
  .problem {
    grid-column: 1 / -1;
    margin: 0;
    color: var(--color-error);
    font-size: 0.72rem;
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
  .names {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.3rem;
    color: var(--color-text-secondary);
    font-size: 0.7rem;
  }
  .name-chip {
    padding: 0.1rem 0.45rem;
    border-radius: 999px;
    font-family: ui-monospace, monospace;
    font-size: 0.7rem;
  }
  .name-chip:hover {
    border-color: var(--color-primary);
    color: var(--color-text-primary);
  }
</style>

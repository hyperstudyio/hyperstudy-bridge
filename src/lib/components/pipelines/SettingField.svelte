<script>
  // One setting of a block. Text-like settings keep what is typed and pass on a value only when it reads cleanly,
  // so a half-typed band never reaches openfrenz.
  import { FORMATS } from '../../pipelines/fields.js';
  import { FILTERS, FILTER_FIELDS } from '../../pipelines/blocks.js';

  let { field, value, onchange } = $props();

  const format = $derived(FORMATS[field.type]);
  let text = $state('');
  let problem = $state('');
  let editing = false;

  // Show the current value unless the person is in the middle of typing.
  $effect(() => {
    const shown = format ? format.format(value) : '';
    if (!editing) text = shown;
  });

  function typed(next) {
    editing = true;
    text = next;
    const read = format.parse(next);
    problem = read.error ?? '';
    if (!read.error) onchange(read.value);
  }

  // Filters: a list of steps, each a type and its numbers. `value` is reactive state: copied with
  // $state.snapshot, since structuredClone cannot copy Svelte's proxies.
  const steps = () => $state.snapshot(value ?? []);
  function setNumber(index, key, text) {
    const next = steps();
    const step = { ...next[index] };
    // Empty: left out, so that openfrenz uses its own value (order 2, q 30, pole 0.99).
    if (text.trim() === '') delete step[key];
    else step[key] = FILTER_FIELDS[key]?.whole ? Math.round(Number(text)) : Number(text);
    next[index] = step;
    onchange(next);
  }
  function setStepType(index, type) {
    const next = steps();
    next[index] = { type, ...FILTERS[type].fields };
    onchange(next);
  }
  function addStep() {
    onchange([...steps(), { type: 'bandpass', ...FILTERS.bandpass.fields }]);
  }
  function removeStep(index) {
    onchange(steps().filter((_, k) => k !== index));
  }
</script>

<div class="field">
  {#if field.type === 'bool'}
    <label class="check">
      <input type="checkbox" checked={Boolean(value)} onchange={e => onchange(e.target.checked)} />
      {field.label}
    </label>
  {:else}
    <label for="setting-{field.key}">{field.label}</label>
    {#if field.type === 'number'}
      <input
        id="setting-{field.key}"
        type="number"
        step="any"
        value={value ?? ''}
        onchange={e => onchange(e.target.value === '' ? null : Number(e.target.value))}
      />
    {:else if field.type === 'select'}
      <select
        id="setting-{field.key}"
        value={value ?? field.absent ?? field.default}
        onchange={e => onchange(e.target.value)}
      >
        {#each field.options as option (option)}<option value={option}
            >{field.names?.[option] ?? option}</option
          >{/each}
      </select>
    {:else if field.type === 'text'}
      <input
        id="setting-{field.key}"
        type="text"
        value={value ?? ''}
        oninput={e => onchange(e.target.value)}
      />
    {:else if field.type === 'filters'}
      <div class="steps">
        {#each value ?? [] as step, index (index)}
          <div class="step">
            <div class="step-head">
              <span class="step-number">{index + 1}</span>
              <select
                aria-label="Filter {index + 1}"
                value={step.type}
                onchange={e => setStepType(index, e.target.value)}
              >
                {#each Object.entries(FILTERS) as [type, about] (type)}
                  <option value={type}>{about.label}</option>
                {/each}
              </select>
              <button
                class="step-remove"
                aria-label="Remove filter {index + 1}"
                onclick={() => removeStep(index)}>×</button
              >
            </div>
            <div class="step-fields">
              {#each Object.keys(FILTERS[step.type]?.fields ?? {}) as key (key)}
                {@const about = FILTER_FIELDS[key] ?? { label: key }}
                <label class="inline" title={about.help ?? ''}>
                  {about.label}
                  <input
                    type="number"
                    step={about.whole ? 1 : 'any'}
                    value={step[key] ?? ''}
                    placeholder={about.empty === undefined ? 'needed' : String(about.empty)}
                    onchange={e => setNumber(index, key, e.target.value)}
                  />
                </label>
              {/each}
            </div>
          </div>
        {/each}
        <button class="add" onclick={addStep}>+ Filter</button>
      </div>
    {:else if format}
      {#if field.type === 'mix'}
        <textarea
          id="setting-{field.key}"
          rows="3"
          value={text}
          oninput={e => typed(e.target.value)}
          onblur={() => (editing = false)}
        ></textarea>
      {:else}
        <input
          id="setting-{field.key}"
          type="text"
          value={text}
          oninput={e => typed(e.target.value)}
          onblur={() => (editing = false)}
        />
      {/if}
    {/if}
  {/if}
  {#if problem}<p class="problem" role="alert">{problem}</p>{/if}
  {#if field.help && !problem}<p class="help">{field.help}</p>{/if}
</div>

<style>
  .field {
    display: grid;
    gap: 0.25rem;
  }
  label {
    color: var(--color-text-secondary);
    font-size: 0.78rem;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--color-text-primary);
  }
  input,
  select,
  textarea {
    width: 100%;
    padding: 0.35rem 0.45rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface);
    color: var(--color-text-primary);
    font: inherit;
    font-size: 0.82rem;
    box-sizing: border-box;
  }
  .check input {
    width: auto;
  }
  .steps {
    display: grid;
    gap: 0.35rem;
  }
  /* Each filter a small card: its type, then its numbers in a row. */
  .step {
    display: grid;
    gap: 0.4rem;
    padding: 0.45rem;
    border: 1px solid var(--color-border);
    border-radius: 8px;
    background: color-mix(in srgb, var(--color-surface-elevated) 50%, transparent);
  }
  .step-head {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .step-number {
    color: var(--color-text-secondary);
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
  }
  .step-head select {
    flex: 1;
    width: auto;
  }
  .step-remove {
    border: none;
    background: none;
  }
  .step-remove:hover {
    color: var(--color-error);
  }
  .step-fields {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(4.8rem, 1fr));
    gap: 0.4rem;
  }
  .inline {
    display: grid;
    gap: 0.15rem;
    color: var(--color-text-secondary);
    font-size: 0.7rem;
  }
  .inline input {
    width: 100%;
  }
  button {
    padding: 0.15rem 0.5rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-secondary);
    cursor: pointer;
  }
  .add {
    justify-self: start;
  }
  .problem {
    margin: 0;
    color: var(--color-error);
    font-size: 0.75rem;
  }
  .help {
    margin: 0;
    color: var(--color-text-secondary);
    font-size: 0.72rem;
  }
</style>

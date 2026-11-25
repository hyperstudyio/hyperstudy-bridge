<script>
  // One setting of a block. Text-like settings keep what is typed and pass on a value only when it reads cleanly,
  // so a half-typed band never reaches openfrenz.
  import { FORMATS } from '../../pipelines/fields.js';
  import { FILTERS } from '../../pipelines/blocks.js';

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

  // Filters: a list of steps, each a type and its numbers.
  function setStep(index, change) {
    const steps = globalThis.structuredClone(value ?? []);
    steps[index] = { ...steps[index], ...change };
    onchange(steps);
  }
  function setStepType(index, type) {
    const steps = globalThis.structuredClone(value ?? []);
    steps[index] = { type, ...FILTERS[type].fields };
    onchange(steps);
  }
  function addStep() {
    onchange([...(value ?? []), { type: 'bandpass', ...FILTERS.bandpass.fields }]);
  }
  function removeStep(index) {
    onchange((value ?? []).filter((_, k) => k !== index));
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
        value={value ?? field.default}
        onchange={e => onchange(e.target.value)}
      >
        {#each field.options as option (option)}<option value={option}>{option}</option>{/each}
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
            <select
              aria-label="Filter {index + 1}"
              value={step.type}
              onchange={e => setStepType(index, e.target.value)}
            >
              {#each Object.entries(FILTERS) as [type, about] (type)}
                <option value={type}>{about.label}</option>
              {/each}
            </select>
            {#each Object.keys(FILTERS[step.type]?.fields ?? {}) as key (key)}
              <label class="inline">
                {key}
                <input
                  type="number"
                  step="any"
                  value={step[key] ?? ''}
                  onchange={e => setStep(index, { [key]: Number(e.target.value) })}
                />
              </label>
            {/each}
            <button aria-label="Remove filter {index + 1}" onclick={() => removeStep(index)}
              >×</button
            >
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
  .step {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.3rem;
  }
  .step select {
    width: auto;
  }
  .inline {
    display: flex;
    align-items: center;
    gap: 0.2rem;
  }
  .inline input {
    width: 4.5rem;
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

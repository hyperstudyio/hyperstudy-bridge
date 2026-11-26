<script>
  // One node of the pipeline graph. Streams are pills, as HyperStudy's cognition canvas draws its Perceive anchor:
  // they are where the pipeline starts. Blocks are cards whose outline and type chip say what they give (blue for
  // a signal, amber for features), and whose dots are coloured by what passes through them. Selected nodes glow
  // green, as selections do in HyperStudy. The star marks the blocks whose values the pipeline gives; × removes.
  import { Handle, Position } from '@xyflow/svelte';
  import { BLOCKS, STREAMS } from '../../pipelines/blocks.js';

  let { id, data = {} } = $props();

  const isStream = $derived(data.kind === 'stream');
  const block = $derived(BLOCKS[data.op]);
  const title = $derived(isStream ? STREAMS[data.stream]?.label : (block?.label ?? data.op));
  const kind = $derived(isStream ? 'signal' : block?.gives);
  const across = $derived(data.direction === 'LR');

  function remove(event) {
    event.stopPropagation();
    data.onRemove?.(id);
  }
</script>

{#if isStream}
  <div class="stream" class:selected={data.selected}>
    <span class="stream-name">{title}</span>
    <span class="stream-sub">{data.summary}</span>
    <button class="drop nodrag" title="Remove" aria-label="Remove {id}" onclick={remove}>×</button>
    <Handle type="source" class="dot signal" position={across ? Position.Right : Position.Bottom} />
  </div>
{:else}
  <div
    class="card {kind}"
    class:selected={data.selected}
    class:faulty={data.faulty}
    class:output={data.output}
  >
    <Handle
      type="target"
      class="dot {[block?.reads].flat()[0]}"
      position={across ? Position.Left : Position.Top}
    />
    <div class="head">
      <span class="chip">{kind}</span>
      {#if block?.stored || data.stored}<span
          class="chip stored"
          title="Uses numbers stored with the model">stored</span
        >{/if}
      <button
        class="star nodrag"
        class:on={data.output}
        title={data.output ? 'Gives its values (click to stop)' : 'Give its values'}
        aria-label="{data.output ? 'Stop giving' : 'Give'} the values of {id}"
        aria-pressed={Boolean(data.output)}
        onclick={event => {
          event.stopPropagation();
          data.onToggleOutput?.(id);
        }}>★</button
      >
      <button class="drop nodrag" title="Remove" aria-label="Remove {id}" onclick={remove}>×</button
      >
    </div>
    <div class="title">{title}</div>
    <div class="id">{id}</div>
    {#if data.summary}<div class="summary" title={data.summary}>{data.summary}</div>{/if}
    <Handle type="source" class="dot {kind}" position={across ? Position.Right : Position.Bottom} />
  </div>
{/if}

<style>
  /* Selection: HyperStudy's green ring and glow. */
  .stream,
  .card {
    --glow: 0 0 0 1px rgba(76, 175, 80, 0.55), 0 0 14px 2px rgba(76, 175, 80, 0.35);
    transition:
      border-color 150ms ease,
      box-shadow 150ms ease;
  }

  /* Streams: a pill, where the pipeline starts. */
  .stream {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    box-sizing: border-box;
    width: 200px;
    padding: 9px 26px;
    border: 1.5px solid var(--wire-signal);
    border-radius: 999px;
    background: color-mix(in srgb, var(--wire-signal) 12%, var(--color-surface));
  }
  .stream-name {
    color: var(--wire-signal-text);
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .stream-sub {
    color: var(--color-text-secondary);
    font-size: 10px;
    letter-spacing: 0.02em;
  }
  .stream .drop {
    position: absolute;
    top: 50%;
    right: 8px;
    transform: translateY(-50%);
  }
  .stream.selected {
    border-color: var(--color-primary);
    box-shadow: var(--glow);
  }

  /* Blocks: a card outlined in the colour of what it gives. */
  .card {
    position: relative;
    box-sizing: border-box;
    width: 200px;
    padding: 8px 12px 10px;
    border: 1.5px solid color-mix(in srgb, var(--accent) 45%, transparent);
    border-radius: 10px;
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
  }
  .card.signal {
    --accent: var(--wire-signal);
    --accent-text: var(--wire-signal-text);
  }
  .card.features {
    --accent: var(--wire-features);
    --accent-text: var(--wire-features-text);
  }
  .card.sequence {
    --accent: var(--wire-sequence);
    --accent-text: var(--wire-sequence-text);
  }
  .chip.stored {
    margin-right: 0;
    background: none;
    color: var(--color-text-secondary);
    font-weight: 600;
  }
  .card.selected {
    border-color: var(--color-primary);
    box-shadow: var(--glow);
  }
  .card.faulty {
    border-color: var(--color-error);
    box-shadow: 0 0 0 1px rgba(239, 68, 68, 0.45);
  }
  .head {
    display: flex;
    align-items: center;
    gap: 2px;
    margin-bottom: 4px;
  }
  .chip {
    margin-right: auto;
    padding: 1px 7px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--accent) 16%, transparent);
    color: var(--accent-text);
    font-size: 9.5px;
    font-weight: 700;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .title {
    font-size: 13px;
    font-weight: 600;
    line-height: 1.25;
  }
  .id {
    color: var(--color-text-secondary);
    font-family: ui-monospace, monospace;
    font-size: 10.5px;
  }
  .summary {
    margin-top: 5px;
    padding-top: 5px;
    border-top: 1px solid var(--color-border);
    color: var(--color-text-secondary);
    font-size: 10.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  button {
    width: 18px;
    height: 18px;
    padding: 0;
    border: none;
    border-radius: 50%;
    background: none;
    color: var(--color-text-disabled, rgba(255, 255, 255, 0.35));
    font-size: 13px;
    line-height: 1;
    cursor: pointer;
  }
  .star.on,
  .star:hover {
    color: var(--color-warning);
  }
  .drop {
    opacity: 0;
    transition: opacity 150ms ease;
  }
  .stream:hover .drop,
  .card:hover .drop,
  .selected .drop {
    opacity: 1;
  }
  .drop:hover {
    background: var(--color-error);
    color: #fff;
  }

  /* Dots in the colour of what passes through them; large enough to grab. */
  :global(.svelte-flow__handle.dot) {
    width: 10px;
    height: 10px;
    border: 2px solid var(--color-surface);
    transition: scale 120ms ease;
  }
  :global(.svelte-flow__handle.dot.signal) {
    background: var(--wire-signal);
  }
  :global(.svelte-flow__handle.dot.features) {
    background: var(--wire-features);
  }
  :global(.svelte-flow__handle.dot.sequence) {
    background: var(--wire-sequence);
  }
  :global(.svelte-flow__handle.dot:hover) {
    scale: 1.35;
    background: var(--color-primary);
  }
</style>

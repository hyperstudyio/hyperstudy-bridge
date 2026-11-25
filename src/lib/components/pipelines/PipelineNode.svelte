<script>
  // One node of the pipeline graph: a stream the pipeline reads, or a block. Input at the top (or left), output at
  // the bottom (or right); the star marks the blocks whose values the pipeline gives, × removes the node. The
  // border's colour says what it gives: blue for a signal, amber for features.
  import { Handle, Position } from '@xyflow/svelte';
  import { BLOCKS, STREAMS } from '../../pipelines/blocks.js';

  let { id, data = {} } = $props();

  const isStream = $derived(data.kind === 'stream');
  const block = $derived(BLOCKS[data.op]);
  const title = $derived(isStream ? STREAMS[data.stream]?.label : (block?.label ?? data.op));
  const kind = $derived(isStream ? 'signal' : block?.gives);
  const across = $derived(data.direction === 'LR');
</script>

<div
  class="node"
  class:stream={isStream}
  class:selected={data.selected}
  class:faulty={data.faulty}
  class:output={data.output}
  class:features={kind === 'features'}
>
  {#if !isStream}
    <Handle type="target" position={across ? Position.Left : Position.Top} />
  {/if}
  <div class="top">
    <span class="title">{title}</span>
    {#if !isStream}
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
    {/if}
    <button
      class="drop nodrag"
      title="Remove"
      aria-label="Remove {id}"
      onclick={event => {
        event.stopPropagation();
        data.onRemove?.(id);
      }}>×</button
    >
  </div>
  <div class="meta">
    <span class="id">{id}</span>
    <span class="kind">{kind}</span>
  </div>
  {#if data.summary}<div class="summary">{data.summary}</div>{/if}
  <Handle type="source" position={across ? Position.Right : Position.Bottom} />
</div>

<style>
  .node {
    min-width: 170px;
    max-width: 220px;
    padding: 0.5rem 0.65rem;
    border: 1px solid var(--color-border);
    border-radius: 8px;
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
    font-size: 0.78rem;
  }
  .node {
    border-top: 3px solid var(--wire-signal);
  }
  .node.features {
    border-top-color: var(--wire-features);
  }
  .node.stream {
    border-style: dashed;
    border-top-style: solid;
  }
  .node.selected {
    border-color: var(--color-primary);
    box-shadow: 0 0 0 1px var(--color-primary);
  }
  .node.faulty {
    border-color: var(--color-error);
  }
  .top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.4rem;
  }
  .title {
    flex: 1;
    font-weight: 600;
  }
  .drop {
    padding: 0 0.15rem;
    border: none;
    background: none;
    color: var(--color-text-disabled);
    font-size: 0.95rem;
    line-height: 1;
    cursor: pointer;
    opacity: 0;
    transition: opacity 150ms ease;
  }
  .node:hover .drop,
  .node.selected .drop {
    opacity: 1;
  }
  .drop:hover {
    color: var(--color-error);
  }
  .star {
    padding: 0 0.15rem;
    border: none;
    background: none;
    color: var(--color-text-disabled);
    font-size: 0.95rem;
    line-height: 1;
    cursor: pointer;
  }
  .star.on {
    color: var(--color-warning);
  }
  .meta {
    display: flex;
    justify-content: space-between;
    gap: 0.4rem;
    color: var(--color-text-secondary);
    font-family: ui-monospace, monospace;
    font-size: 0.7rem;
  }
  /* Large enough to grab easily. */
  .node :global(.svelte-flow__handle) {
    width: 12px;
    height: 12px;
    border: 2px solid var(--color-surface);
    background: var(--color-text-secondary);
  }
  .node :global(.svelte-flow__handle:hover) {
    background: var(--color-primary);
  }
  .summary {
    margin-top: 0.25rem;
    color: var(--color-text-secondary);
    font-size: 0.7rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>

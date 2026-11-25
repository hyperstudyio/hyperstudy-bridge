<script>
  // A wire between two nodes: blue when it carries a signal, amber when it carries features, with a × to remove
  // it (as in HyperStudy's agent cognition canvas). The arrow points at the block that reads.
  import { BaseEdge, EdgeLabel, getSmoothStepPath } from '@xyflow/svelte';

  let {
    id,
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
    markerEnd,
    data = {},
    selected,
  } = $props();

  const geometry = $derived(
    getSmoothStepPath({
      sourceX,
      sourceY,
      targetX,
      targetY,
      sourcePosition,
      targetPosition,
      borderRadius: 10,
    })
  );
  const colour = $derived(
    data.carries === 'features' ? 'var(--wire-features)' : 'var(--wire-signal)'
  );
</script>

<BaseEdge
  {id}
  path={geometry[0]}
  {markerEnd}
  style="stroke: {colour}; stroke-width: {selected ? 2.5 : 1.6};{data.animated
    ? ' stroke-dasharray: 5 4;'
    : ''}"
/>
<EdgeLabel x={geometry[1]} y={geometry[2]} transparent>
  <button
    class="remove nodrag nopan"
    class:visible={selected}
    aria-label="Remove the wire {data.label}"
    title="Remove this wire"
    onclick={event => {
      event.stopPropagation();
      data.onRemove?.(id);
    }}>×</button
  >
</EdgeLabel>

<style>
  .remove {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 1px solid var(--color-border);
    border-radius: 50%;
    background: var(--color-surface);
    color: var(--color-text-secondary);
    font-size: 12px;
    line-height: 1;
    cursor: pointer;
    opacity: 0.45;
    transition: opacity 150ms ease;
  }
  .remove:hover,
  .remove.visible {
    opacity: 1;
  }
  .remove:hover {
    border-color: var(--color-error);
    background: var(--color-error);
    color: #fff;
  }
</style>

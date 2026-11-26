<script>
  // Build openfrenz pipelines as a graph: streams and blocks joined by wires. Each change is checked by openfrenz;
  // "Try on band" runs the pipeline on the band connected through openfrenz and draws what it gives, without
  // publishing it. Saved pipelines run with the band like any model (FRENZ models setting: saved:<name>) and can
  // be exported as a folder to share or upload.
  import { SvelteFlow, Controls, Background } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import { open as chooseFolder } from '@tauri-apps/plugin-dialog';
  import PipelineNode from './PipelineNode.svelte';
  import PipelineEdge from './PipelineEdge.svelte';
  import FitView from './FitView.svelte';
  import SettingField from './SettingField.svelte';
  import { BLOCKS, STREAMS, palette, defaults } from '../../pipelines/blocks.js';
  import {
    toConfig,
    fromConfig,
    starter,
    canConnect,
    whyNot,
    gives,
    freshId,
    cleanId,
  } from '../../pipelines/graph.js';
  import { FORMATS } from '../../pipelines/fields.js';
  import { RECIPES } from '../../pipelines/recipes.js';
  import { tidy } from '../../pipelines/layout.js';
  import {
    checkPipeline,
    previewPipeline,
    stopPipelinePreview,
    listPipelines,
    savePipeline,
    deletePipeline,
    exportPipeline,
    getFrenzNativeRecent,
    openFrenzModel,
  } from '../../services/tauri.js';

  // `request`: what to open when the editor opens, set by the live view: a running model ({ kind: 'model', name })
  // or a new pipeline ({ kind: 'new' }); cleared once opened. `onUse(name)` puts a saved pipeline to use and
  // returns what happened.
  let {
    isOpen = $bindable(false),
    request = $bindable(null),
    connected = false,
    onUse = null,
  } = $props();

  const nodeTypes = { block: PipelineNode };
  const edgeTypes = { wire: PipelineEdge };
  const HUES = ['#3987e5', '#d95926', '#199e70', '#c98500', '#d55181'];
  // What a wire carries, as the nodes' outlines, chips and dots show it (also --wire-* in the styles below).
  const WIRE = { signal: '#3987e5', features: '#c98500', sequence: '#9b7be0' };

  let graph = $state(starter());
  let name = $state('my-pipeline');
  let selected = $state(null);
  let check = $state({ ok: false, outputs: [], error: null, node: null });
  let localError = $state('');
  let saved = $state([]);
  let message = $state('');
  let previewing = $state(false);
  let preview = $state(null);
  let chart = $state(null);
  let flowNodes = $state([]);
  let flowEdges = $state([]);
  let direction = $state('TB');
  // The model this pipeline was opened from (`saved:<name>` or `model:<name>`), whose stored numbers it uses;
  // null for one made here. `stored`: whether there are any.
  let origin = $state(null);
  let stored = $state(false);
  let warning = $state('');

  const groups = palette();
  const selectedNode = $derived(graph.nodes.find(n => n.id === selected) ?? null);

  // The graph as Svelte Flow draws it.
  $effect(() => {
    const faulty = check.node;
    flowNodes = graph.nodes.map(n => ({
      id: n.id,
      type: 'block',
      position: { ...n.position },
      data: {
        kind: n.kind,
        op: n.op,
        stream: n.stream,
        selected: n.id === selected,
        faulty: n.id === faulty,
        output: graph.outputs.includes(n.id),
        summary: summarise(n),
        stored: Boolean(n.settings?.weights),
        direction,
        onToggleOutput: toggleOutput,
        onRemove: id => removeNodes([id]),
      },
    }));
    flowEdges = graph.edges.map(e => {
      const from = graph.nodes.find(n => n.id === e.source);
      return {
        id: e.id,
        source: e.source,
        target: e.target,
        type: 'wire',
        markerEnd: {
          type: 'arrowclosed',
          width: 14,
          height: 14,
          color: WIRE[from ? gives(from) : 'signal'],
        },
        data: {
          carries: from ? gives(from) : 'signal',
          animated: previewing,
          label: `${e.source} to ${e.target}`,
          onRemove: removeEdge,
        },
      };
    });
  });

  function summarise(node) {
    if (node.kind === 'stream')
      return `${STREAMS[node.stream].channels.length} channels · ${STREAMS[node.stream].rate} Hz`;
    const block = BLOCKS[node.op];
    if (node.op === 'filter') {
      const steps = node.settings?.steps ?? [];
      return steps
        .map(f =>
          f.type === 'notch'
            ? `notch ${f.hz}`
            : f.type === 'dc_block'
              ? 'offset'
              : `${f.type} ${[f.low, f.high].filter(v => v !== undefined).join('–')}`
        )
        .join(', ');
    }
    const first = block?.settings.find(
      f => FORMATS[f.type] || f.type === 'number' || f.type === 'select'
    );
    if (!first) return block?.about ?? '';
    const v = node.settings?.[first.key];
    return `${first.label}: ${FORMATS[first.type] ? FORMATS[first.type].format(v) : (v ?? '')}`;
  }

  // ---- editing

  function addStream(stream) {
    if (graph.nodes.some(n => n.kind === 'stream' && n.stream === stream)) {
      selected = stream;
      return;
    }
    graph.nodes = [...graph.nodes, { id: stream, kind: 'stream', stream, position: spot() }];
  }
  function addBlock(op) {
    const id = freshId(graph.nodes, op);
    const node = { id, kind: 'block', op, settings: defaults(op), position: spot() };
    // Wired to the selected node when that makes sense, which is how most pipelines grow.
    const edges =
      selected && canConnect([...graph.nodes, node], graph.edges, selected, id)
        ? [...graph.edges, { id: `${selected}->${id}`, source: selected, target: id }]
        : graph.edges;
    graph.nodes = [...graph.nodes, node];
    graph.edges = edges;
    selected = id;
  }
  // Below the selected node, moved right until it overlaps no other.
  function spot() {
    const anchor = graph.nodes.find(n => n.id === selected);
    const at = anchor
      ? { x: anchor.position.x, y: anchor.position.y + 150 }
      : { x: 60, y: 40 + graph.nodes.length * 40 };
    const taken = p =>
      graph.nodes.some(
        n => Math.abs(n.position.x - p.x) < 200 && Math.abs(n.position.y - p.y) < 130
      );
    while (taken(at)) at.x += 240;
    return at;
  }
  function setSetting(key, value) {
    graph.nodes = graph.nodes.map(n =>
      n.id === selected ? { ...n, settings: { ...n.settings, [key]: value } } : n
    );
  }
  function rename(next) {
    const id = cleanId(next);
    if (!id || id === selected || graph.nodes.some(n => n.id === id) || id in STREAMS) return;
    const old = selected;
    graph.nodes = graph.nodes.map(n => (n.id === old ? { ...n, id } : n));
    graph.edges = graph.edges.map(e => ({
      ...e,
      id: `${e.source === old ? id : e.source}->${e.target === old ? id : e.target}`,
      source: e.source === old ? id : e.source,
      target: e.target === old ? id : e.target,
    }));
    graph.outputs = graph.outputs.map(o => (o === old ? id : o));
    selected = id;
  }
  function removeNodes(ids) {
    graph.nodes = graph.nodes.filter(n => !ids.includes(n.id));
    graph.edges = graph.edges.filter(e => !ids.includes(e.source) && !ids.includes(e.target));
    graph.outputs = graph.outputs.filter(o => !ids.includes(o));
    if (ids.includes(selected)) selected = null;
  }
  function toggleOutput(id) {
    graph.outputs = graph.outputs.includes(id)
      ? graph.outputs.filter(o => o !== id)
      : [...graph.outputs, id];
  }
  function removeEdge(id) {
    graph.edges = graph.edges.filter(e => e.id !== id);
  }
  // A wire dropped where it is refused says why, instead of silently vanishing.
  function connectEnd(_event, state) {
    if (state?.isValid !== false || !state.fromNode || !state.toNode) return;
    const fromSource = state.fromHandle?.type !== 'target';
    const source = fromSource ? state.fromNode.id : state.toNode.id;
    const target = fromSource ? state.toNode.id : state.fromNode.id;
    warning = whyNot(graph.nodes, graph.edges, source, target) ?? '';
  }
  function arrange(next = direction) {
    direction = next;
    const at = tidy(graph.nodes, graph.edges, direction);
    graph.nodes = graph.nodes.map(n => ({ ...n, position: at.get(n.id) ?? n.position }));
    fit();
  }
  let fitNonce = $state(0);
  function fit() {
    fitNonce += 1;
  }
  function fresh(next) {
    graph = next;
    origin = null;
    stored = false;
  }
  function startFrom(recipe) {
    fresh(recipe.graph());
    name = recipe.id;
    selected = null;
    warning = '';
    message = `Started from ${recipe.name}: change it and save it under your own name.`;
    arrange();
  }
  function connect(connection) {
    warning = '';
    if (canConnect(graph.nodes, graph.edges, connection.source, connection.target)) {
      graph.edges = [
        ...graph.edges,
        {
          id: `${connection.source}->${connection.target}`,
          source: connection.source,
          target: connection.target,
        },
      ];
    }
  }
  function dragged({ targetNode, nodes }) {
    const moved = new Map((nodes ?? [targetNode]).filter(Boolean).map(n => [n.id, n.position]));
    graph.nodes = graph.nodes.map(n =>
      moved.has(n.id) ? { ...n, position: { ...moved.get(n.id) } } : n
    );
  }
  function deleted({ nodes, edges }) {
    if (nodes?.length) removeNodes(nodes.map(n => n.id));
    if (edges?.length) {
      const gone = new Set(edges.map(e => e.id));
      graph.edges = graph.edges.filter(e => !gone.has(e.id));
    }
  }

  // ---- checking, as the graph changes

  let config = $state(null);
  let checkTimer = null;
  $effect(() => {
    const snapshot = $state.snapshot(graph);
    const label = name;
    const from = origin;
    clearTimeout(checkTimer);
    // Nothing to show while the editor is closed; it checks again when opened.
    if (!isOpen) return;
    checkTimer = setTimeout(async () => {
      try {
        config = toConfig(snapshot, label);
        localError = '';
      } catch (error) {
        config = null;
        localError = error.message;
        check = { ok: false, outputs: [], error: null, node: null };
        return;
      }
      check = await checkPipeline(config, from);
      if (previewing && check.ok) startPreview();
    }, 300);
    return () => clearTimeout(checkTimer);
  });

  // ---- trying it on the band

  let pollTimer = null;
  async function startPreview() {
    if (!config || !check.ok) return;
    const result = await previewPipeline(config, origin);
    if (!result?.success) {
      message = result?.error ?? 'Could not run the pipeline on the band.';
      previewing = false;
      return;
    }
    message = '';
    previewing = true;
    clearInterval(pollTimer);
    pollTimer = setInterval(async () => {
      const traces = await getFrenzNativeRecent(30);
      preview = traces.find(t => t.name === 'preview') ?? null;
      drawPreview();
    }, 300);
  }
  async function stopPreview() {
    previewing = false;
    clearInterval(pollTimer);
    preview = null;
    await stopPipelinePreview();
  }

  function drawPreview() {
    if (!chart || !preview) return;
    const scale = window.devicePixelRatio || 1;
    const width = chart.clientWidth;
    const height = chart.clientHeight;
    chart.width = width * scale;
    chart.height = height * scale;
    const g = chart.getContext('2d');
    if (!g) return;
    g.setTransform(scale, 0, 0, scale, 0, 0);
    g.clearRect(0, 0, width, height);
    const ink = window.getComputedStyle(chart).getPropertyValue('--color-text-secondary') || '#aaa';
    const shown = preview.values.slice(0, HUES.length);
    const all = shown.flat().filter(Number.isFinite);
    if (!all.length || preview.t.length < 2) {
      g.fillStyle = ink;
      g.font = '11px system-ui, sans-serif';
      g.fillText('Waiting for the first steps…', 12, height / 2);
      return;
    }
    let [low, high] = [Math.min(...all), Math.max(...all)];
    if (high - low < 1e-12) [low, high] = [low - 1, high + 1];
    const [t0, t1] = [preview.t[0], preview.t.at(-1)];
    const x = t => 200 + ((t - t0) / Math.max(t1 - t0, 1e-9)) * (width - 260);
    const y = v => height - 8 - ((v - low) / (high - low)) * (height - 16);
    g.font = '11px system-ui, sans-serif';
    g.textBaseline = 'middle';
    shown.forEach((values, k) => {
      g.strokeStyle = HUES[k];
      g.lineWidth = 1.5;
      g.beginPath();
      let drawing = false;
      values.forEach((v, i) => {
        if (!Number.isFinite(v)) return void (drawing = false);
        drawing ? g.lineTo(x(preview.t[i]), y(v)) : g.moveTo(x(preview.t[i]), y(v));
        drawing = true;
      });
      g.stroke();
      g.fillStyle = HUES[k];
      g.fillRect(8, 10 + k * 15 - 4, 8, 8);
      g.fillStyle = ink;
      const last = values.findLast(Number.isFinite);
      g.fillText(
        `${preview.channels[k]}  ${last === undefined ? '—' : last.toPrecision(4)}`,
        22,
        10 + k * 15
      );
    });
    if (preview.values.length > HUES.length) {
      g.fillStyle = ink;
      g.fillText(`+ ${preview.values.length - HUES.length} more`, 22, 10 + HUES.length * 15);
    }
  }

  // ---- saving, opening, exporting

  async function refreshSaved() {
    saved = await listPipelines();
  }
  async function save() {
    if (!config) return false;
    try {
      await savePipeline(cleanId(name), config, origin);
      name = cleanId(name);
      // Its stored numbers were saved with it: from now on they come from the saved copy.
      if (stored) origin = `saved:${name}`;
      message = `Saved as ${name}.${onUse ? ' Use runs it with the band.' : ''}`;
      await refreshSaved();
      return true;
    } catch (error) {
      message = String(error);
      return false;
    }
  }
  async function saveAndUse() {
    if (!(await save())) return;
    message = (await onUse(name)) ?? message;
  }
  async function useSaved(entry) {
    message = (await onUse(entry.name)) ?? '';
  }
  function load(entry) {
    graph = fromConfig(entry.config);
    origin = `saved:${entry.name}`;
    stored = graph.nodes.some(n => BLOCKS[n.op]?.stored || n.settings?.weights);
    fit();
    name = entry.name;
    selected = null;
    message = `Opened ${entry.name}.`;
  }
  // A model running with the band, opened from the live view.
  async function openModel(model) {
    try {
      const opened = await openFrenzModel(model);
      graph = fromConfig(opened.config);
      origin = opened.origin;
      stored = opened.stored;
      // A saved pipeline is changed in place; any other model is saved as a copy, beside the original.
      name = opened.saved ?? `${cleanId(opened.config.name ?? model)}-edited`;
      selected = null;
      warning = '';
      if (!opened.config.editor) arrange();
      else fit();
      message = opened.saved
        ? `Opened ${opened.saved}, running with the band. Save and Use to update it there.`
        : `Opened ${model}, running with the band. Changes are saved as ${name}; Use runs it beside ${model}.`;
    } catch (error) {
      message = `Could not open ${model}: ${error}`;
    }
  }
  $effect(() => {
    if (!isOpen || !request) return;
    const next = request;
    request = null;
    if (next.kind === 'model') openModel(next.name);
    else {
      fresh(starter());
      name = 'my-signal';
      selected = null;
      message = 'A new signal: build it here, Try on band, then Use to run it with the band.';
      fit();
    }
  });
  let confirmDelete = $state(null);
  async function removeSaved(entry) {
    if (confirmDelete !== entry.name) {
      confirmDelete = entry.name;
      return;
    }
    confirmDelete = null;
    try {
      await deletePipeline(entry.name);
      message = `Deleted ${entry.name}.`;
    } catch (error) {
      message = String(error);
    }
    await refreshSaved();
  }
  async function exportSaved(entry) {
    const folder = await chooseFolder({ directory: true, title: 'Export the pipeline into…' });
    if (!folder) return;
    try {
      message = `Exported to ${await exportPipeline(entry.name, folder)}.`;
    } catch (error) {
      message = String(error);
    }
  }

  $effect(() => {
    if (isOpen) refreshSaved();
    return () => {
      if (previewing) stopPreview();
    };
  });

  function close() {
    if (previewing) stopPreview();
    isOpen = false;
  }
  const status = $derived(
    localError
      ? { tone: 'bad', text: localError }
      : check.ok
        ? { tone: 'good', text: `openfrenz accepts it: ${check.outputs.length} values each step` }
        : check.error
          ? { tone: 'bad', text: check.error }
          : { tone: 'quiet', text: 'Checking…' }
  );
</script>

{#if isOpen}
  <div class="editor" role="dialog" aria-modal="true" aria-labelledby="editor-title">
    <header>
      <h2 id="editor-title">Pipelines</h2>
      <label class="name">
        Name
        <input bind:value={name} aria-label="Pipeline name" />
      </label>
      <label>
        Step
        <select
          aria-label="Stream that sets the step"
          value={graph.step.stream}
          onchange={e => (graph.step = { ...graph.step, stream: e.target.value })}
        >
          {#each graph.nodes.filter(n => n.kind === 'stream') as node (node.id)}
            <option value={node.stream}>{STREAMS[node.stream].label}</option>
          {/each}
        </select>
        every
        <input
          class="seconds"
          type="number"
          min="0.05"
          step="0.05"
          aria-label="Seconds per step"
          value={graph.step.seconds}
          onchange={e =>
            (graph.step = { ...graph.step, seconds: Math.max(0.05, Number(e.target.value) || 1) })}
        />
        s
      </label>
      <div class="actions">
        {#if previewing}
          <button onclick={stopPreview}>Stop trying</button>
        {:else}
          <button
            onclick={startPreview}
            disabled={!check.ok || !connected}
            title={connected
              ? 'Run it on the band, without publishing'
              : 'Connect the band through openfrenz first'}>Try on band</button
          >
        {/if}
        <button onclick={save} disabled={!check.ok}>Save</button>
        {#if onUse}
          <button
            class="primary"
            onclick={saveAndUse}
            disabled={!check.ok}
            title={connected
              ? 'Save it and run it with the band now, published and in the live view'
              : 'Save it and run it with the band from the next connection'}>Use</button
          >
        {/if}
        <button onclick={() => (fresh(starter()), (name = 'my-pipeline'), (selected = null))}
          >New</button
        >
        <button class="close" aria-label="Close pipelines" onclick={close}>×</button>
      </div>
    </header>
    <p class="status {status.tone}" role="status">{status.text}</p>

    <div class="body">
      <aside class="palette">
        <h3>Streams</h3>
        {#each Object.entries(STREAMS) as [stream, about] (stream)}
          <button onclick={() => addStream(stream)}>{about.label}</button>
        {/each}
        {#each Object.entries(groups) as [group, ops] (group)}
          <h3>{group}</h3>
          {#each ops as op (op)}
            <button title={BLOCKS[op].about} onclick={() => addBlock(op)}>{BLOCKS[op].label}</button
            >
          {/each}
        {/each}
        <h3>Start from</h3>
        {#each RECIPES as recipe (recipe.id)}
          <button title={recipe.about} onclick={() => startFrom(recipe)}>{recipe.name}</button>
        {/each}
        <h3>Saved</h3>
        {#each saved as entry (entry.name)}
          <div class="saved">
            <button class="open" onclick={() => load(entry)}>{entry.name}</button>
            {#if onUse}<button title="Run it with the band" onclick={() => useSaved(entry)}
                >Use</button
              >{/if}
            <button title="Export as a folder" onclick={() => exportSaved(entry)}>⇩</button>
            <button
              title="Delete"
              aria-label="Delete {entry.name}"
              class:confirm={confirmDelete === entry.name}
              onclick={() => removeSaved(entry)}
              >{confirmDelete === entry.name ? 'Delete?' : '×'}</button
            >
          </div>
        {:else}
          <p class="quiet">None yet.</p>
        {/each}
      </aside>

      <div class="canvas">
        <div class="toolbar">
          <button onclick={() => arrange()} title="Lay the graph out again">Tidy</button>
          <div class="toggle" role="group" aria-label="Direction">
            <button
              class:on={direction === 'TB'}
              aria-pressed={direction === 'TB'}
              onclick={() => arrange('TB')}>↓ Down</button
            >
            <button
              class:on={direction === 'LR'}
              aria-pressed={direction === 'LR'}
              onclick={() => arrange('LR')}>→ Across</button
            >
          </div>
          <span class="legend"><i class="signal"></i>signal <i class="features"></i>features</span>
        </div>
        {#if warning}
          <p class="warning" role="alert">
            {warning}
            <button aria-label="Dismiss" onclick={() => (warning = '')}>×</button>
          </p>
        {/if}
        <SvelteFlow
          bind:nodes={flowNodes}
          bind:edges={flowEdges}
          {nodeTypes}
          {edgeTypes}
          fitView
          fitViewOptions={{ maxZoom: 1, padding: 0.25 }}
          colorMode="dark"
          connectionRadius={30}
          proOptions={{ hideAttribution: true }}
          isValidConnection={c => canConnect(graph.nodes, graph.edges, c.source, c.target)}
          onconnect={connect}
          onconnectend={connectEnd}
          onnodeclick={({ node }) => ((selected = node.id), (warning = ''))}
          onpaneclick={() => ((selected = null), (warning = ''))}
          onnodedragstop={dragged}
          ondelete={deleted}
        >
          <Controls position="bottom-right" />
          <Background variant="dots" gap={20} />
          <FitView nonce={fitNonce} />
        </SvelteFlow>
      </div>

      <aside class="inspector">
        {#if selectedNode?.kind === 'block' && (!BLOCKS[selectedNode.op] || BLOCKS[selectedNode.op].stored || selectedNode.settings?.weights)}
          {@const block = BLOCKS[selectedNode.op]}
          <h3>{block?.label ?? selectedNode.op}</h3>
          {#if block}<p class="quiet">{block.about}</p>{/if}
          <p class="quiet">
            Uses numbers stored with the model it was opened from, so it is shown, not edited. Wires
            around it can change; saving keeps its numbers.
          </p>
          <pre class="stored-settings">{JSON.stringify(selectedNode.settings, null, 2)}</pre>
          <button class="remove" onclick={() => removeNodes([selectedNode.id])}>Remove block</button
          >
        {:else if selectedNode?.kind === 'block'}
          {@const block = BLOCKS[selectedNode.op]}
          <h3>{block.label}</h3>
          <p class="quiet">{block.about}</p>
          <label for="node-id">Name</label>
          <input id="node-id" value={selectedNode.id} onchange={e => rename(e.target.value)} />
          {#each block.settings as field (field.key)}
            <SettingField
              {field}
              value={selectedNode.settings?.[field.key]}
              onchange={v => setSetting(field.key, v)}
            />
          {/each}
          <button class="remove" onclick={() => removeNodes([selectedNode.id])}>Remove block</button
          >
        {:else if selectedNode}
          <h3>{STREAMS[selectedNode.stream].label}</h3>
          <p class="quiet">Channels: {STREAMS[selectedNode.stream].channels.join(', ')}</p>
          <button class="remove" onclick={() => removeNodes([selectedNode.id])}
            >Remove stream</button
          >
        {:else}
          <h3>How to build</h3>
          <p class="quiet">
            Add a stream, then blocks: each new block is wired to the one selected. Drag from a
            block's output dot to another's input dot to wire them; blue wires carry a signal, amber
            ones features. Mark the blocks whose values the pipeline gives with ★. Select a block to
            change its settings; × removes a block or a wire. Tidy lays the graph out again.
          </p>
          {#if check.ok}
            <h3>It gives</h3>
            <ul class="outputs">
              {#each check.outputs.slice(0, 40) as output (output)}<li>{output}</li>{/each}
              {#if check.outputs.length > 40}<li>… {check.outputs.length - 40} more</li>{/if}
            </ul>
          {/if}
        {/if}
      </aside>
    </div>

    {#if message}<p class="message" role="status">{message}</p>{/if}
    {#if previewing}
      <canvas class="preview" bind:this={chart} aria-label="What the pipeline gives on the band"
      ></canvas>
    {/if}
  </div>
{/if}

<style>
  .editor {
    position: fixed;
    inset: 2vh 2vw;
    z-index: 1100;
    display: flex;
    flex-direction: column;
    border: 1px solid var(--color-border);
    border-radius: 12px;
    background: var(--color-surface);
    box-shadow: 0 10px 40px rgba(0, 0, 0, 0.6);
    overflow: hidden;
  }
  header {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 1rem;
    padding: 0.7rem 1rem;
    border-bottom: 1px solid var(--color-border);
    color: var(--color-text-secondary);
    font-size: 0.82rem;
  }
  h2 {
    margin: 0;
    color: var(--color-text-primary);
    font-size: 1.05rem;
  }
  header label {
    display: flex;
    align-items: center;
    gap: 0.35rem;
  }
  input,
  select {
    padding: 0.3rem 0.45rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
    font: inherit;
  }
  .seconds {
    width: 4.5rem;
  }
  .actions {
    display: flex;
    gap: 0.4rem;
    margin-left: auto;
  }
  button {
    padding: 0.3rem 0.7rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
    cursor: pointer;
    font: inherit;
  }
  button:disabled {
    opacity: 0.45;
    cursor: default;
  }
  .primary {
    border-color: var(--color-primary);
    background: var(--color-primary);
    color: #fff;
  }
  .close {
    border: none;
    background: none;
    font-size: 1.4rem;
    line-height: 1;
  }
  .status {
    margin: 0;
    padding: 0.35rem 1rem;
    font-size: 0.8rem;
    border-bottom: 1px solid var(--color-border);
  }
  .status.good {
    color: var(--color-success);
  }
  .status.bad {
    color: var(--color-error);
  }
  .status.quiet,
  .quiet {
    color: var(--color-text-secondary);
  }
  .body {
    display: grid;
    grid-template-columns: 190px 1fr 280px;
    flex: 1;
    min-height: 0;
  }
  aside {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    padding: 0.7rem;
    overflow-y: auto;
    font-size: 0.8rem;
  }
  .palette {
    border-right: 1px solid var(--color-border);
  }
  .palette button {
    text-align: left;
    font-size: 0.78rem;
  }
  .inspector {
    gap: 0.6rem;
    border-left: 1px solid var(--color-border);
  }
  h3 {
    margin: 0.4rem 0 0.1rem;
    color: var(--color-text-primary);
    font-size: 0.8rem;
  }
  .saved {
    display: flex;
    gap: 0.25rem;
  }
  .saved .open {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .saved button {
    padding: 0.2rem 0.45rem;
    font-size: 0.75rem;
  }
  .saved .confirm {
    border-color: var(--color-error);
    color: var(--color-error);
  }
  .editor {
    --wire-signal: #3987e5;
    --wire-signal-text: #8dbdf5;
    --wire-features: #c98500;
    --wire-features-text: #f0bd55;
    --wire-sequence: #9b7be0;
    --wire-sequence-text: #c3b0f2;
  }
  .canvas {
    position: relative;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.6rem;
    border-bottom: 1px solid var(--color-border);
    font-size: 0.75rem;
  }
  .toolbar button {
    padding: 0.2rem 0.55rem;
    font-size: 0.75rem;
  }
  .toggle {
    display: flex;
  }
  .toggle button:first-child {
    border-radius: 6px 0 0 6px;
  }
  .toggle button:last-child {
    border-left: none;
    border-radius: 0 6px 6px 0;
  }
  .toggle .on {
    border-color: var(--color-primary);
    color: var(--color-primary);
  }
  .legend {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    margin-left: auto;
    color: var(--color-text-secondary);
  }
  .legend i {
    display: inline-block;
    width: 14px;
    height: 3px;
    margin-left: 0.4rem;
    border-radius: 2px;
  }
  .legend .signal {
    background: var(--wire-signal);
  }
  .legend .features {
    background: var(--wire-features);
  }
  .warning {
    position: absolute;
    top: 2.6rem;
    left: 50%;
    z-index: 5;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin: 0;
    padding: 0.35rem 0.4rem 0.35rem 0.75rem;
    border: 1px solid var(--color-warning);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-primary);
    font-size: 0.78rem;
    transform: translateX(-50%);
  }
  .warning button {
    padding: 0 0.35rem;
    border: none;
    background: none;
  }
  .canvas :global(.svelte-flow) {
    flex: 1;
  }
  .canvas :global(.svelte-flow) {
    background: var(--color-surface);
  }
  .remove {
    border-color: var(--color-error);
    color: var(--color-error);
  }
  .stored-settings {
    margin: 0;
    padding: 0.5rem;
    border: 1px solid var(--color-border);
    border-radius: 6px;
    background: var(--color-surface-elevated);
    color: var(--color-text-secondary);
    font-size: 0.7rem;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .outputs {
    margin: 0;
    padding-left: 1rem;
    color: var(--color-text-secondary);
    font-family: ui-monospace, monospace;
    font-size: 0.72rem;
  }
  .message {
    margin: 0;
    padding: 0.4rem 1rem;
    border-top: 1px solid var(--color-border);
    color: var(--color-text-secondary);
    font-size: 0.8rem;
  }
  .preview {
    display: block;
    width: 100%;
    height: 120px;
    border-top: 1px solid var(--color-border);
  }
</style>

// Turning the editor's graph into an openfrenz pipeline config and back. No DOM here, so it can be tested.
//
// A graph: { step: { stream, seconds }, nodes: [{ id, kind: 'stream' | 'block', stream?, op?, settings?, position }],
//            edges: [{ id, source, target }], outputs: [node id] }.

import { BLOCKS, STREAMS, readsKind } from './blocks.js';

/** A node id that openfrenz accepts as a node name: letters, digits, _ and -. */
export function cleanId(text) {
  return (
    String(text)
      .toLowerCase()
      .replace(/[^a-z0-9_-]+/g, '_')
      .replace(/^_+|_+$/g, '') || 'node'
  );
}

/** An id like `base` not used by any node yet. */
export function freshId(nodes, base) {
  const taken = new Set(nodes.map(n => n.id));
  const root = cleanId(base);
  if (!taken.has(root) && !(root in STREAMS)) return root;
  for (let k = 2; ; k++) if (!taken.has(`${root}_${k}`)) return `${root}_${k}`;
}

/** Nodes in an order where every node comes after those it reads; null if the graph has a loop. */
export function ordered(nodes, edges) {
  const into = new Map(nodes.map(n => [n.id, 0]));
  for (const e of edges) if (into.has(e.target)) into.set(e.target, into.get(e.target) + 1);
  const ready = nodes.filter(n => into.get(n.id) === 0).map(n => n.id);
  const out = [];
  while (ready.length) {
    const id = ready.shift();
    out.push(id);
    for (const e of edges.filter(e => e.source === id)) {
      into.set(e.target, into.get(e.target) - 1);
      if (into.get(e.target) === 0) ready.push(e.target);
    }
  }
  return out.length === nodes.length ? out.map(id => nodes.find(n => n.id === id)) : null;
}

/** What a node gives: `signal` for streams, otherwise its block's output. */
export function gives(node) {
  return node.kind === 'stream' ? 'signal' : BLOCKS[node.op]?.gives;
}

/**
 * Why `source` may not feed `target`, in words; null when it may. The same rule as `canConnect`, which the canvas
 * uses while dragging.
 */
export function whyNot(nodes, edges, source, target) {
  const from = nodes.find(n => n.id === source);
  const to = nodes.find(n => n.id === target);
  if (!from || !to) return 'Connect two blocks.';
  if (source === target) return 'A block cannot read its own output.';
  if (to.kind === 'stream') return 'Streams come from the band; nothing feeds them.';
  const block = BLOCKS[to.op];
  if (!block) return `openfrenz's ${to.op} block is not known to this editor.`;
  if (!readsKind(block, gives(from))) {
    const kinds = [block.reads].flat().map(say).join(' or ');
    return `${block.label} reads ${kinds}, and ${from.id} gives ${say(gives(from))}.`;
  }
  const existing = edges.filter(e => e.target === target);
  if (existing.some(e => e.source === source)) return 'They are already connected.';
  if (!block.many && existing.length)
    return `${block.label} reads one input; remove its wire first.`;
  if (!ordered(nodes, [...edges, { source, target }])) return 'That would make a loop.';
  return null;
}

const say = kind =>
  ({ signal: 'a signal', features: 'features', sequence: 'a sequence' })[kind] ?? kind;

/** Whether `source` may feed `target`: what one gives is what the other reads, and one input unless it takes many. */
export function canConnect(nodes, edges, source, target) {
  return whyNot(nodes, edges, source, target) === null;
}

/**
 * The openfrenz config for `graph`. Throws an Error with a plain message when the graph cannot be one: a loop,
 * a block with no input, or no outputs. openfrenz then checks the rest.
 */
export function toConfig(graph, name = 'pipeline') {
  const { nodes, edges, step } = graph;
  const order = ordered(nodes, edges);
  if (!order) throw new Error('The graph has a loop: a block cannot read its own output.');
  const streams = nodes.filter(n => n.kind === 'stream').map(n => n.stream);
  if (!streams.length) throw new Error('Add a stream for the pipeline to read.');
  const clock = streams.includes(step.stream) ? step.stream : streams[0];
  const samples = stream => Math.max(1, Math.round(STREAMS[stream].rate * step.seconds));
  const inputs = {};
  for (const stream of new Set(streams))
    if (stream !== clock) inputs[stream] = { samples: samples(stream) };
  const config = {
    openfrenz_model: 'pipeline',
    name: cleanId(name),
    version: graph.version ?? '1',
    step: { stream: clock, samples: samples(clock) },
    ...(Object.keys(inputs).length ? { inputs } : {}),
    nodes: [],
    outputs: [],
    editor: { positions: {} },
  };
  for (const node of order) {
    config.editor.positions[node.id] = [Math.round(node.position.x), Math.round(node.position.y)];
    if (node.kind === 'stream') continue;
    const sources = edges
      .filter(e => e.target === node.id)
      .map(e => nodes.find(n => n.id === e.source))
      .map(n => (n.kind === 'stream' ? n.stream : n.id));
    if (!sources.length)
      throw new Error(`${BLOCKS[node.op]?.label ?? node.op} (${node.id}) has no input.`);
    const settings = Object.fromEntries(
      Object.entries(node.settings ?? {}).filter(
        ([, v]) => v !== null && v !== undefined && v !== ''
      )
    );
    config.nodes.push({
      name: node.id,
      op: node.op,
      in: BLOCKS[node.op]?.many ? sources : sources[0],
      ...settings,
    });
  }
  config.outputs = (graph.outputs ?? []).filter(id => config.nodes.some(n => n.name === id));
  if (!config.outputs.length) throw new Error('Mark at least one block as an output (★).');
  return config;
}

/** The editor's graph for an openfrenz pipeline config, laid out where the config says or in a column. */
export function fromConfig(config) {
  const positions = config.editor?.positions ?? {};
  const streamNames = [config.step?.stream, ...Object.keys(config.inputs ?? {})].filter(Boolean);
  const nodes = [];
  const edges = [];
  let row = 0;
  const place = id => {
    const p = positions[id];
    const at = p
      ? { x: p[0], y: p[1] }
      : { x: 40 + (row % 3) * 230, y: 40 + Math.floor(row / 3) * 140 };
    row += 1;
    return at;
  };
  for (const stream of new Set(streamNames)) {
    if (STREAMS[stream])
      nodes.push({ id: stream, kind: 'stream', stream, position: place(stream) });
  }
  for (const entry of config.nodes ?? []) {
    const { name, op, in: from, ...settings } = entry;
    delete settings.editor;
    nodes.push({ id: name, kind: 'block', op, settings, position: place(name) });
    for (const source of Array.isArray(from) ? from : [from]) {
      edges.push({ id: `${source}->${name}`, source, target: name });
    }
  }
  const clock = config.step?.stream ?? 'eeg';
  const seconds = (config.step?.samples ?? 125) / (STREAMS[clock]?.rate ?? 125);
  return {
    version: config.version ?? '1',
    step: { stream: clock, seconds },
    nodes,
    edges,
    outputs: [...(config.outputs ?? [])],
  };
}

/** A small starting graph: forehead pads, band-passed, alpha and theta power. */
export function starter() {
  return {
    version: '1',
    step: { stream: 'eeg', seconds: 1 },
    nodes: [
      { id: 'eeg', kind: 'stream', stream: 'eeg', position: { x: 40, y: 40 } },
      {
        id: 'front',
        kind: 'block',
        op: 'select',
        settings: { channels: ['forehead_left', 'forehead_right'] },
        position: { x: 40, y: 190 },
      },
      {
        id: 'clean',
        kind: 'block',
        op: 'filter',
        settings: {
          steps: [
            { type: 'notch', hz: 60 },
            { type: 'bandpass', low: 1, high: 45 },
          ],
          start: 'settled',
        },
        position: { x: 40, y: 340 },
      },
      {
        id: 'power',
        kind: 'block',
        op: 'band_power',
        settings: {
          bands: [
            [4, 8],
            [8, 13],
          ],
          log: true,
        },
        position: { x: 40, y: 490 },
      },
    ],
    edges: [
      { id: 'eeg->front', source: 'eeg', target: 'front' },
      { id: 'front->clean', source: 'front', target: 'clean' },
      { id: 'clean->power', source: 'clean', target: 'power' },
    ],
    outputs: ['power'],
  };
}

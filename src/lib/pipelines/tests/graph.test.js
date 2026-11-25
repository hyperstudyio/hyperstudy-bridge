import { describe, it, expect } from 'vitest';
import { toConfig, fromConfig, starter, canConnect, ordered, freshId, cleanId } from '../graph.js';
import { BLOCKS, palette, defaults } from '../blocks.js';

describe('pipeline graphs', () => {
  it('turns the starter graph into an openfrenz config and back', () => {
    const config = toConfig(starter(), 'Alpha Theta!');
    expect(config).toMatchObject({
      openfrenz_model: 'pipeline',
      name: 'alpha_theta',
      step: { stream: 'eeg', samples: 125 },
      outputs: ['power'],
    });
    expect(config.nodes.map(n => [n.name, n.op, n.in])).toEqual([
      ['front', 'select', 'eeg'],
      ['clean', 'filter', 'front'],
      ['power', 'band_power', 'clean'],
    ]);
    expect(config.nodes[2].bands).toEqual([
      [4, 8],
      [8, 13],
    ]);
    expect(config.editor.positions.power).toEqual([40, 430]);
    const back = fromConfig(config);
    expect(back.nodes.map(n => n.id)).toEqual(['eeg', 'front', 'clean', 'power']);
    expect(back.edges.map(e => `${e.source}->${e.target}`)).toEqual([
      'eeg->front',
      'front->clean',
      'clean->power',
    ]);
    expect(back.step).toEqual({ stream: 'eeg', seconds: 1 });
    expect(toConfig(back, 'alpha_theta')).toEqual(config);
  });

  it('gives a block with several inputs all of them, and a second stream its own samples', () => {
    const graph = {
      step: { stream: 'eeg', seconds: 2 },
      nodes: [
        { id: 'eeg', kind: 'stream', stream: 'eeg', position: { x: 0, y: 0 } },
        { id: 'accel', kind: 'stream', stream: 'accel', position: { x: 200, y: 0 } },
        {
          id: 'p',
          kind: 'block',
          op: 'band_power',
          settings: defaults('band_power'),
          position: { x: 0, y: 100 },
        },
        { id: 't', kind: 'block', op: 'tilt', settings: {}, position: { x: 200, y: 100 } },
        { id: 'all', kind: 'block', op: 'concat', settings: {}, position: { x: 100, y: 200 } },
      ],
      edges: [
        { id: '1', source: 'eeg', target: 'p' },
        { id: '2', source: 'accel', target: 't' },
        { id: '3', source: 'p', target: 'all' },
        { id: '4', source: 't', target: 'all' },
      ],
      outputs: ['all'],
    };
    const config = toConfig(graph);
    expect(config.step).toEqual({ stream: 'eeg', samples: 250 });
    expect(config.inputs).toEqual({ accel: { samples: 100 } });
    expect(config.nodes.find(n => n.name === 'all').in).toEqual(['p', 't']);
    expect(config.nodes.find(n => n.name === 'p')).not.toHaveProperty('relative_to');
  });

  it('allows only connections that make sense', () => {
    const { nodes, edges } = starter();
    // A signal may feed a block that reads signals, once.
    expect(canConnect(nodes, edges, 'clean', 'power')).toBe(false); // already connected
    expect(canConnect(nodes, [], 'clean', 'power')).toBe(true);
    // Features cannot feed a block that reads signals, and nothing feeds a stream.
    expect(canConnect(nodes, [], 'power', 'clean')).toBe(false);
    expect(canConnect(nodes, [], 'front', 'eeg')).toBe(false);
    // A block that reads one input takes no second.
    expect(canConnect(nodes, edges, 'eeg', 'clean')).toBe(false);
  });

  it('refuses graphs that cannot be pipelines, saying why', () => {
    const graph = starter();
    expect(() => toConfig({ ...graph, outputs: [] })).toThrow(/output/);
    expect(() => toConfig({ ...graph, edges: graph.edges.slice(1) })).toThrow(
      /front\) has no input/
    );
    const loop = {
      ...graph,
      edges: [...graph.edges, { id: 'x', source: 'power', target: 'front' }],
    };
    expect(ordered(loop.nodes, loop.edges)).toBeNull();
    expect(() => toConfig(loop)).toThrow(/loop/);
  });

  it('names new nodes so that openfrenz accepts them and none collide', () => {
    const { nodes } = starter();
    expect(cleanId('Band power (alpha)')).toBe('band_power_alpha');
    expect(freshId(nodes, 'power')).toBe('power_2');
    expect(freshId(nodes, 'eeg')).toBe('eeg_2');
    expect(freshId(nodes, 'ratio')).toBe('ratio');
  });

  it('offers every block with defaults that are its own settings', () => {
    const groups = palette();
    expect(Object.keys(groups)).toEqual(['Signal', 'Features', 'Events', 'Combine']);
    for (const op of Object.keys(BLOCKS)) {
      const keys = BLOCKS[op].settings.map(s => s.key);
      expect(Object.keys(defaults(op)).every(k => keys.includes(k))).toBe(true);
    }
  });
});

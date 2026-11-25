// Laying the graph out with dagre: streams at the start, each block after the blocks it reads. The same approach
// as HyperStudy's agent cognition canvas. Pure, so it can be tested.

import dagre from '@dagrejs/dagre';

export const NODE_W = 200;
export const NODE_H = 76;

/**
 * New positions for `nodes`, top to bottom (`TB`) or left to right (`LR`). Returns a map of id → {x, y}, the
 * top-left corner as Svelte Flow places nodes.
 */
export function tidy(nodes, edges, direction = 'TB') {
  const g = new dagre.graphlib.Graph({ multigraph: true });
  g.setGraph({ rankdir: direction, nodesep: 40, ranksep: 60, marginx: 20, marginy: 20 });
  g.setDefaultEdgeLabel(() => ({}));
  for (const n of nodes) g.setNode(n.id, { width: NODE_W, height: NODE_H });
  edges.forEach((e, i) => {
    if (g.hasNode(e.source) && g.hasNode(e.target)) g.setEdge(e.source, e.target, {}, `e${i}`);
  });
  dagre.layout(g);
  return new Map(
    nodes.map(n => {
      const p = g.node(n.id);
      return [n.id, { x: Math.round(p.x - NODE_W / 2), y: Math.round(p.y - NODE_H / 2) }];
    })
  );
}

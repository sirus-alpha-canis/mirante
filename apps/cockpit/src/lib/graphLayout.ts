// A small deterministic force layout (Fruchterman–Reingold) for the memory graph: no
// randomness (nodes start on a circle sorted by id), no dependency. O(n²) per iteration;
// past 100 nodes the iteration count shrinks so 300 nodes stay well under 50 ms. The Brain
// shows a list instead of a graph past `MAX_GRAPH_NODES`.

export const MAX_GRAPH_NODES = 400;

export interface LayoutGraph {
  nodes: { id: string }[];
  links: { src: string; dst: string }[];
}

export type Point = { x: number; y: number };
export type Positions = Record<string, Point>;

export function layout(g: LayoutGraph, w: number, h: number, pad = 20): Positions {
  const ids = [...new Set(g.nodes.map((n) => n.id))].sort();
  const n = ids.length;
  const cx = w / 2;
  const cy = h / 2;
  if (n === 0) return {};
  if (n === 1) return { [ids[0]]: { x: cx, y: cy } };
  const index = new Map(ids.map((id, i) => [id, i]));
  const xs = new Float64Array(n);
  const ys = new Float64Array(n);
  const r = Math.max(1, Math.min(w, h) / 2 - pad);
  for (let i = 0; i < n; i++) {
    const a = (2 * Math.PI * i) / n;
    xs[i] = cx + r * Math.cos(a);
    ys[i] = cy + r * Math.sin(a);
  }
  const seen = new Set<string>();
  const edges: [number, number][] = [];
  for (const l of g.links) {
    const i = index.get(l.src);
    const j = index.get(l.dst);
    if (i === undefined || j === undefined || i === j) continue;
    const key = i < j ? `${i}:${j}` : `${j}:${i}`;
    if (seen.has(key)) continue;
    seen.add(key);
    edges.push([i, j]);
  }
  const area = Math.max(1, (w - 2 * pad) * (h - 2 * pad));
  const k = Math.sqrt(area / n);
  const k2 = k * k;
  const iterations = n <= 100 ? 200 : Math.max(30, Math.floor((200 * 100 * 100) / (n * n)));
  let temp = w / 10;
  const cool = temp / (iterations + 1);
  const dx = new Float64Array(n);
  const dy = new Float64Array(n);
  for (let it = 0; it < iterations; it++) {
    dx.fill(0);
    dy.fill(0);
    for (let i = 0; i < n; i++) {
      for (let j = i + 1; j < n; j++) {
        let ex = xs[i] - xs[j];
        let ey = ys[i] - ys[j];
        let d2 = ex * ex + ey * ey;
        if (d2 < 1e-4) {
          // Coincident: push apart along a fixed, index-derived direction.
          ex = ((i - j) % 2 === 0 ? 1 : -1) * 0.01;
          ey = 0.01;
          d2 = ex * ex + ey * ey;
        }
        const f = k2 / d2; // (k²/d) / d: the unit vector folded in
        dx[i] += ex * f;
        dy[i] += ey * f;
        dx[j] -= ex * f;
        dy[j] -= ey * f;
      }
    }
    for (const [i, j] of edges) {
      const ex = xs[i] - xs[j];
      const ey = ys[i] - ys[j];
      const d = Math.sqrt(ex * ex + ey * ey) || 0.01;
      const f = d / k; // (d²/k) / d
      dx[i] -= ex * f;
      dy[i] -= ey * f;
      dx[j] += ex * f;
      dy[j] += ey * f;
    }
    for (let i = 0; i < n; i++) {
      // A light pull to the centre keeps unlinked groups from pinning to the walls.
      dx[i] += (cx - xs[i]) * 0.02;
      dy[i] += (cy - ys[i]) * 0.02;
      const len = Math.sqrt(dx[i] * dx[i] + dy[i] * dy[i]);
      if (len > 0) {
        const step = Math.min(len, temp);
        xs[i] += (dx[i] / len) * step;
        ys[i] += (dy[i] / len) * step;
      }
      xs[i] = Math.min(w - pad, Math.max(pad, xs[i]));
      ys[i] = Math.min(h - pad, Math.max(pad, ys[i]));
    }
    temp = Math.max(temp - cool, 0.5);
  }
  const out: Positions = {};
  for (let i = 0; i < n; i++) out[ids[i]] = { x: Math.round(xs[i] * 10) / 10, y: Math.round(ys[i] * 10) / 10 };
  return out;
}

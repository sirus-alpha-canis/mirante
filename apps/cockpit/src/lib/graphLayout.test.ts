import { describe, expect, it } from "vitest";
import { layout } from "./graphLayout";

const dist = (a: { x: number; y: number }, b: { x: number; y: number }) => Math.hypot(a.x - b.x, a.y - b.y);

describe("layout", () => {
  it("is deterministic and keeps nodes inside the box", () => {
    const g = { nodes: ["a", "b", "c"].map((n) => ({ id: n })), links: [{ src: "a", dst: "b" }] };
    const p1 = layout(g, 600, 400);
    const p2 = layout(g, 600, 400);
    expect(p1).toEqual(p2);
    for (const p of Object.values(p1)) expect(p.x >= 20 && p.x <= 580 && p.y >= 20 && p.y <= 380).toBe(true);
    // Input order does not matter.
    expect(layout({ nodes: [...g.nodes].reverse(), links: g.links }, 600, 400)).toEqual(p1);
  });

  it("linked nodes end closer than unlinked ones", () => {
    const g = { nodes: ["a", "b", "c", "d"].map((n) => ({ id: n })), links: [{ src: "a", dst: "b" }, { src: "c", dst: "d" }] };
    const p = layout(g, 600, 400);
    expect(dist(p.a, p.b)).toBeLessThan(dist(p.a, p.c));
    expect(dist(p.c, p.d)).toBeLessThan(dist(p.b, p.d));
  });

  it("handles 0, 1 and 300 nodes within budget, and ignores links to unknown nodes", () => {
    expect(layout({ nodes: [], links: [] }, 600, 400)).toEqual({});
    expect(layout({ nodes: [{ id: "a" }], links: [{ src: "a", dst: "zz" }] }, 600, 400)).toEqual({ a: { x: 300, y: 200 } });
    const nodes = Array.from({ length: 300 }, (_, i) => ({ id: `n${i}` }));
    const links = nodes.slice(1).map((n, i) => ({ src: nodes[i].id, dst: n.id }));
    // Warm the JIT first (a cold first call also compiles the function, ~30 ms here); the
    // budget is for the layout itself, measured under a full parallel test run.
    layout({ nodes: nodes.slice(0, 120), links }, 900, 600);
    const t0 = performance.now();
    const p = layout({ nodes, links }, 900, 600);
    expect(performance.now() - t0).toBeLessThan(50);
    expect(Object.keys(p)).toHaveLength(300);
    for (const q of Object.values(p)) expect(Number.isFinite(q.x) && Number.isFinite(q.y)).toBe(true);
  });
});

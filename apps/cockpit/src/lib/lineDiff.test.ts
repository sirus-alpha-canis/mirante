import { describe, expect, it } from "vitest";
import { DIFF_MAX_LINES, lineDiff, splitLines } from "./lineDiff";

describe("lineDiff", () => {
  it("keeps common lines and puts each change's deletions before its additions", () => {
    expect(lineDiff("a\nb\nc", "a\nB\nc")).toEqual([{ same: "a" }, { del: "b" }, { add: "B" }, { same: "c" }]);
  });

  it("an appended line is one addition; identical text has no change", () => {
    expect(lineDiff("a\nb\n", "a\nb\nc\n")).toEqual([{ same: "a" }, { same: "b" }, { add: "c" }]);
    expect(lineDiff("x\ny", "x\ny")).toEqual([{ same: "x" }, { same: "y" }]);
    expect(lineDiff("", "")).toEqual([]);
    expect(lineDiff("", "n")).toEqual([{ add: "n" }]);
  });

  it("finds the longest common run inside a changed middle", () => {
    const ops = lineDiff("h\n1\n2\n3\nt", "h\n0\n1\n3\n4\nt");
    expect(ops).toEqual([{ same: "h" }, { add: "0" }, { same: "1" }, { del: "2" }, { same: "3" }, { add: "4" }, { same: "t" }]);
  });

  it("splits lines without a phantom last line", () => {
    expect(splitLines("a\nb\n")).toEqual(["a", "b"]);
    expect(splitLines("a\r\nb")).toEqual(["a", "b"]);
    expect(splitLines("")).toEqual([]);
  });

  it("past the cap it still shows every line, as one replaced block, quickly", () => {
    const n = DIFF_MAX_LINES + 500;
    const a = Array.from({ length: n }, (_, i) => `a${i}`).join("\n");
    const b = Array.from({ length: n }, (_, i) => `b${i}`).join("\n");
    const t0 = performance.now();
    const ops = lineDiff(a, b);
    expect(performance.now() - t0).toBeLessThan(500);
    expect(ops.filter((o) => "del" in o)).toHaveLength(n);
    expect(ops.filter((o) => "add" in o)).toHaveLength(n);
    expect(ops.findIndex((o) => "add" in o)).toBe(n);
  });
});

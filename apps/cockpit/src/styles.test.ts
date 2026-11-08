// Contrast and wrapping rules the stylesheet must keep.
import { describe, expect, it } from "vitest";
import css from "./styles.css?raw";

/** WCAG relative luminance of #rrggbb / #rgb. */
function luminance(hex: string): number {
  const h = hex.replace("#", "");
  const full = h.length === 3 ? [...h].map((c) => c + c).join("") : h;
  const [r, g, b] = [0, 2, 4].map((i) => parseInt(full.slice(i, i + 2), 16) / 255).map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

const contrast = (a: string, b: string) => {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x + 0.05) / (y + 0.05);
};

/** Every value `name` takes in the stylesheet's token blocks, in order (light, dark, dark). */
const tokens = (name: string) => [...css.matchAll(new RegExp(`${name}: (#[0-9a-fA-F]{3,6});`, "g"))].map((m) => m[1]);

describe("styles", () => {
  it("the danger tag's text is readable (4.5:1) in light and both dark themes", () => {
    expect(css).toMatch(/\.tag-danger \{[^}]*color: var\(--on-danger\)/);
    const bg = tokens("--danger");
    const fg = tokens("--on-danger");
    expect(bg).toHaveLength(3);
    expect(fg).toHaveLength(3);
    bg.forEach((b, i) => expect(contrast(b, fg[i]), `${fg[i]} on ${b}`).toBeGreaterThanOrEqual(4.5));
  });

  it("links and branch names wrap anywhere instead of overflowing", () => {
    expect(css).toMatch(/^\.link \{[^}]*overflow-wrap: anywhere/m);
    expect(css).toMatch(/^\.branch \{[^}]*overflow-wrap: anywhere/m);
  });
});

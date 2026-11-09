// Contrast and wrapping rules the stylesheet must keep.
import { describe, expect, it } from "vitest";
import css from "./styles.css?raw";
import page from "../index.html?raw";

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

  it("every text token is readable (4.5:1) on the surfaces it sits on, in light and both dark themes", () => {
    const surfaces = ["--bg", "--panel", "--panel-2"].map(tokens);
    const texts = ["--text", "--muted", "--faint", "--needs", "--work", "--review", "--danger", "--add", "--del"];
    for (const name of texts) {
      const fg = tokens(name);
      expect(fg, name).toHaveLength(3);
      for (const s of surfaces) {
        expect(s).toHaveLength(3);
        fg.forEach((f, i) => expect(contrast(f, s[i]), `${name} ${f} on ${s[i]}`).toBeGreaterThanOrEqual(4.5));
      }
    }
    // A state's text on its own tint (a needs-you action, a stale note, the banner).
    for (const state of ["--needs", "--work", "--review"]) {
      const fg = tokens(state);
      const bg = tokens(`${state}-bg`);
      fg.forEach((f, i) => expect(contrast(f, bg[i]), `${state} ${f} on ${bg[i]}`).toBeGreaterThanOrEqual(4.5));
    }
    // Text on the quiet button surface, and the primary button's ink.
    const lift = tokens("--panel-3");
    tokens("--text").forEach((f, i) => expect(contrast(f, lift[i])).toBeGreaterThanOrEqual(4.5));
    const accent = tokens("--accent");
    tokens("--accent-text").forEach((f, i) => expect(contrast(f, accent[i])).toBeGreaterThanOrEqual(4.5));
  });

  it("the focus ring stands out (3:1) from every surface", () => {
    const focus = tokens("--focus");
    expect(focus).toHaveLength(3);
    for (const s of ["--bg", "--panel", "--panel-2", "--panel-3"].map(tokens)) {
      focus.forEach((f, i) => expect(contrast(f, s[i]), `${f} on ${s[i]}`).toBeGreaterThanOrEqual(3));
    }
    expect(css).toMatch(/^:focus-visible \{ outline: 2px solid var\(--focus\)/m);
  });

  it("no decorative accent: the four state colors are the only hues (besides project dots)", () => {
    // The old teal/blue accents are gone from every block.
    expect(css).not.toMatch(/#0f766e|#2dd4bf|#1d4ed8|#93b4ff/i);
    // Light and dark keep the same token set.
    const block = (re: RegExp) => [...(re.exec(css)?.[1] ?? "").matchAll(/(--[\w-]+):/g)].map((m) => m[1]).sort();
    const light = block(/^:root \{([^}]*)\}/m).filter((n) => !/^--(mono|sans|fs|s\d|r|row|ease|t)\b/.test(n));
    const dark = block(/^:root\[data-theme="dark"\] \{([^}]*)\}/m);
    expect(dark).toEqual(light);
  });

  it("motion only where reduced motion is not asked for, and none where it is", () => {
    expect(css).toMatch(/@media \(prefers-reduced-motion: no-preference\) \{[\s\S]*?transition: border-color/);
    expect(css).toMatch(/@media \(prefers-reduced-motion: reduce\) \{[^}]*transition: none !important/);
    // Durations stay within 120–180ms.
    for (const m of css.matchAll(/(\d+)ms/g)) {
      expect(Number(m[1])).toBeGreaterThanOrEqual(120);
      expect(Number(m[1])).toBeLessThanOrEqual(180);
    }
  });

  it("the launch splash paints the app's reading surface, text and muted, in each theme", () => {
    const splash = (name: string) => [...page.matchAll(new RegExp(`--splash-${name}: (#[0-9a-fA-F]{6});`, "g"))].map((m) => m[1]);
    expect(splash("bg")).toEqual(tokens("--panel"));
    expect(splash("text")).toEqual(tokens("--text"));
    expect(splash("muted")).toEqual(tokens("--muted"));
  });

  it("links and branch names wrap anywhere instead of overflowing", () => {
    expect(css).toMatch(/^\.link \{[^}]*overflow-wrap: anywhere/m);
    expect(css).toMatch(/^\.branch \{[^}]*overflow-wrap: anywhere/m);
  });
});

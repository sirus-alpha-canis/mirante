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

  it("takes Vibely's palette (docs/plans/2026-11-25-vibely-visual-identity.md): light, then both dark blocks", () => {
    const map: Record<string, [string, string]> = {
      "--bg": ["#efefef", "#111111"],
      "--panel": ["#fafafa", "#1a1a1a"],
      "--panel-2": ["#ffffff", "#1e1e1e"],
      "--panel-3": ["#e5e5e5", "#2a2a2a"],
      "--line": ["#e5e5e5", "#2a2a2a"],
      "--text": ["#1e1e1e", "#fafafa"],
      "--muted": ["#3b3b3b", "#bbbbbb"],
      "--faint": ["#666666", "#919191"],
      "--accent": ["#1e1e1e", "#fafafa"],
      "--accent-text": ["#ffffff", "#111111"],
      "--needs": ["#8f4e19", "#fbbb3c"],
      "--needs-bg": ["#feecc8", "#3d2e10"],
      "--work": ["#2659be", "#7aa3f4"],
      "--work-bg": ["#eaf1fd", "#18264a"],
      "--review": ["#1c6e53", "#36d39f"],
      "--review-bg": ["#e8fbf5", "#123a2d"],
      "--add": ["#1c6e53", "#36d39f"],
      "--danger": ["#a8141b", "#d9787c"],
      "--del": ["#a8141b", "#d9787c"],
      "--on-danger": ["#fafafa", "#111111"],
      "--focus": ["#3b3b3b", "#bbbbbb"],
    };
    for (const [name, [light, dark]] of Object.entries(map)) expect(tokens(name).map((v) => v.toLowerCase()), name).toEqual([light, dark, dark]);
    expect(css).toMatch(/--r-sm: 4px;\s*--r: 8px;\s*--r-lg: 12px;/);
    // Working is blue now; the old cyan is gone.
    expect(css).not.toMatch(/#1c6b7a|#67bccb/i);
  });

  describe("components in Vibely's language, at Mirante's density (Task 6)", () => {
    /** The declarations of the first rule whose selector list is exactly `sel`. */
    const rule = (sel: string) => {
      const esc = sel.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      return new RegExp(`^${esc} \\{([^}]*)\\}`, "m").exec(css)?.[1] ?? "";
    };

    it("cards and dialogs are 12, everything else 8 or 4: no radius outside the tokens", () => {
      expect(rule(".card, .tile")).toContain("border-radius: var(--r-lg)");
      expect(rule(".palette")).toContain("border-radius: var(--r-lg)");
      // Outside the token block a radius is a token, a circle or nothing (no pill radii).
      const body = css.slice(css.indexOf("* { box-sizing"));
      for (const m of body.matchAll(/border-radius: ([^;]+);/g)) expect(m[1], m[0]).toMatch(/^(var\(--r(-sm|-lg)?\)|50%|0)$/);
    });

    it("fields have a 1px border in the field line (Vibely's input), and the focus colour on focus", () => {
      expect(tokens("--field-line").map((v) => v.toLowerCase())).toEqual(["#e5e5e5", "#3b3b3b", "#3b3b3b"]);
      const fields = /^([^{}]*\.rail-filter[^{}]*) \{([^}]*)\}/m.exec(css);
      expect(fields?.[2]).toContain("border: 1px solid var(--field-line)");
      for (const sel of [".rail-filter", ".add-repo input", ".popover-search", ".brain-bar input", ".tasklist-tools input", ".dna-input", ".rules-pick select", ".dispatch-row select", ".queue-edit textarea", ".child-steer input"])
        expect(fields?.[1], sel).toContain(sel);
      expect(css).toMatch(/^\.rail-filter:focus,[^{]*\{ border-color: var\(--focus\)/m);
    });

    it("buttons: primary is neutral ink, secondary outlined, destructive the danger red", () => {
      expect(rule(".btn-primary")).toMatch(/background: var\(--accent\); color: var\(--accent-text\)/);
      expect(rule(".btn")).toContain("box-shadow: inset 0 0 0 1px var(--btn-line)");
      expect(tokens("--btn-line").map((v) => v.toLowerCase())).toEqual(["#bbbbbb", "#3b3b3b", "#3b3b3b"]);
      expect(rule(".btn-danger")).toMatch(/background: var\(--danger\); color: var\(--on-danger\)/);
    });

    it("state chips are tinted pairs: the state's text on its own tint", () => {
      expect(rule(".pill-needs_you")).toMatch(/background: var\(--needs-bg\); color: var\(--needs\)/);
      expect(rule(".pill-working")).toMatch(/background: var\(--work-bg\); color: var\(--work\)/);
      expect(rule(".pill-review")).toMatch(/background: var\(--review-bg\); color: var\(--review\)/);
      expect(rule(".tag-warn")).toMatch(/background: var\(--needs-bg\); color: var\(--needs\)/);
      expect(rule(".tag-on")).toMatch(/background: var\(--review-bg\); color: var\(--review\)/);
      // A row's pill is its dot alone: no tint behind it.
      expect(rule(".row .pill")).toContain("background: none");
    });

    it("a floating note (toast) has the amber left stripe; an error toast the danger one", () => {
      expect(css).toMatch(/\.detail-main \.composer \[role="status"\][^{]*\{[^}]*border-left: 3px solid var\(--needs\)/);
      expect(css).toMatch(/\.detail-main \.composer \.error[^{]*\{[^}]*border-left: 3px solid var\(--danger\)/);
    });

    it("a dialog's overlay dims to 60% (Vibely's 80% is too heavy for a desktop tool)", () => {
      expect(rule(".overlay")).toContain("background: rgb(0 0 0 / 0.6)");
    });

    it("a thread card's stripe is its thread's state, like any card", () => {
      expect(rule(".thread-card-needs_you")).toContain("--stripe: var(--needs)");
      expect(rule(".thread-card-working")).toContain("--stripe: var(--work)");
      expect(rule(".thread-card-review")).toContain("--stripe: var(--review)");
      expect(css).not.toMatch(/\.thread-card \{[^}]*border-left: 3px solid var\(--work\)/);
    });

    it("no gradients anywhere", () => {
      expect(css).not.toMatch(/gradient\(/);
    });
  });

  it("muted and faint text stay readable (4.5:1) on the lift surface (keys, quiet chips, the send button)", () => {
    const lift = tokens("--panel-3");
    for (const name of ["--muted", "--faint"]) {
      tokens(name).forEach((f, i) => expect(contrast(f, lift[i]), `${name} ${f} on ${lift[i]}`).toBeGreaterThanOrEqual(4.5));
    }
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

  it("a rail disclosure never animates its height, and its fade only without reduced motion", () => {
    const rules = [...css.matchAll(/[^{}]*disclosure[^{}]*\{[^}]*\}/g)].map((m) => m[0]);
    expect(rules.length).toBeGreaterThan(0);
    for (const r of rules) expect(r).not.toMatch(/height[^;]*(transition|animation)|(transition|animation)[^;]*height/);
    const calm = /@media \(prefers-reduced-motion: no-preference\) \{([\s\S]*?)\n\}/.exec(css)?.[1] ?? "";
    expect(calm).toMatch(/\.disclosure-body:not\(\[hidden\]\) \{ animation: disclosure-in/);
    // Outside that block, no disclosure rule moves.
    expect(css.replace(calm, "")).not.toMatch(/\.disclosure[^{]*\{[^}]*(animation|transition):/);
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

  it("a folded rail summary that does not fit ends in an ellipsis, not a clipped letter", () => {
    // text-overflow needs a block box: on a flex container the text is cut mid-glyph.
    const rule = /^\.disclosure-summary \{([^}]*)\}/m.exec(css)?.[1] ?? "";
    expect(rule).toMatch(/display: block/);
    for (const d of ["min-width: 0", "overflow: hidden", "text-overflow: ellipsis", "white-space: nowrap"]) expect(rule).toContain(d);
  });
});

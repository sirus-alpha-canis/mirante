// @vitest-environment node
// The bundled fonts: every @font-face in styles.css names a file that is there, with its
// OFL license next to it, and a production build carries the woff2 files (fingerprinted)
// and points the stylesheet at them.
import { describe, expect, it } from "vitest";
import css from "./styles.css?raw";

// What is in assets/fonts: each woff2 as a data: URL (its bytes), each license as text.
const woff2 = import.meta.glob<string>("./assets/fonts/*.woff2", { query: "?inline", import: "default", eager: true });
const licenses = import.meta.glob<string>("./assets/fonts/*.txt", { query: "?raw", import: "default", eager: true });
const faces = [...css.matchAll(/@font-face \{([^}]*)\}/g)].map((m) => m[1]);
const field = (face: string, name: string) => new RegExp(`${name}: ([^;]+);`).exec(face)?.[1];
const files = faces.map((f) => /url\("\.\/([^"]+)"\)/.exec(f)?.[1] ?? "");

describe("bundled fonts", () => {
  it("declares Inter (one variable file) and JetBrains Mono (400 and 700), swapping in from the system stack", () => {
    expect(faces.map((f) => [field(f, "font-family"), field(f, "font-weight"), field(f, "font-style"), field(f, "font-display")])).toEqual([
      ["Inter", "100 900", "normal", "swap"],
      ['"JetBrains Mono"', "400", "normal", "swap"],
      ['"JetBrains Mono"', "700", "normal", "swap"],
    ]);
    expect(files).toEqual(["assets/fonts/InterVariable.woff2", "assets/fonts/JetBrainsMono-Regular.woff2", "assets/fonts/JetBrainsMono-Bold.woff2"]);
    // The fallbacks stay.
    expect(css).toContain('--mono: "JetBrains Mono", ui-monospace, "SF Mono", Menlo, monospace;');
    expect(css).toContain('--sans: Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;');
  });

  it("each file is there and is woff2, its OFL next to it", () => {
    for (const f of files) {
      const data = woff2[`./${f}`];
      expect(data, f).toBeDefined();
      const bytes = atob(data.slice(data.indexOf(",") + 1));
      expect(bytes.slice(0, 4), f).toBe("wOF2");
      expect(bytes.length, f).toBeLessThan(400 * 1024);
    }
    // Nothing bundled that no @font-face uses.
    expect(Object.keys(woff2).sort()).toEqual(files.map((f) => `./${f}`).sort());
    for (const [license, owner] of [
      ["Inter-OFL.txt", "The Inter Project Authors"],
      ["JetBrainsMono-OFL.txt", "The JetBrains Mono Project Authors"],
    ]) {
      const text = licenses[`./assets/fonts/${license}`];
      expect(text, license).toContain(owner);
      expect(text, license).toContain("SIL OPEN FONT LICENSE Version 1.1");
    }
  });

  it("a production build emits the woff2 files and its stylesheet points at them", async () => {
    const { build } = await import("vite");
    const out = await build({ root: new URL("..", import.meta.url).pathname, logLevel: "silent", build: { write: false, emptyOutDir: false } });
    const bundle = (Array.isArray(out) ? out[0] : out) as { output: { fileName: string; type: string; source?: string | Uint8Array }[] };
    const names = bundle.output.map((o) => o.fileName);
    const emitted = ["InterVariable", "JetBrainsMono-Regular", "JetBrainsMono-Bold"].map((n) => names.find((f) => new RegExp(`^assets/${n}-[\\w-]+\\.woff2$`).test(f)));
    for (const f of emitted) expect(f).toBeDefined();
    const sheet = bundle.output.find((o) => o.fileName.endsWith(".css"));
    const text = String(sheet?.source ?? "");
    for (const f of emitted) expect(text).toContain(`url(/${f})`);
    expect(text).not.toMatch(/url\((https?:)?\/\//);
  }, 60_000);
});

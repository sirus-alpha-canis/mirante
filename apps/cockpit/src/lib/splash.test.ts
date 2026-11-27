// The launch splash: inline in index.html (first paint, before any script), then the app's.
import { render } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RibbonMark, Wordmark } from "../components/Splash";
import ribbonSvg from "../assets/brand/ribbon.svg?raw";
import ribbonMonoSvg from "../assets/brand/ribbon-mono.svg?raw";
import iconSourceSvg from "../../src-tauri/icons/source/ribbon.svg?raw";
import html from "../../index.html?raw";
import boot from "../../public/theme-boot.js?raw";
import { dismissSplash, SPLASH_FADE_MS } from "./splash";
import { THEME_KEY } from "./theme";
import css from "../styles.css?raw";
import conf from "../../src-tauri/tauri.conf.json";
import shell from "../../src-tauri/src/lib.rs?raw";

function mountSplash() {
  const el = document.createElement("div");
  el.id = "splash";
  document.body.append(el);
  return el;
}

afterEach(() => {
  vi.useRealTimers();
  document.getElementById("splash")?.remove();
});

describe("dismissSplash", () => {
  it("fades the launch splash out, then removes it", () => {
    vi.useFakeTimers();
    const el = mountSplash();
    dismissSplash(document, false);
    expect(el).toHaveClass("splash-out");
    expect(document.getElementById("splash")).not.toBeNull();
    vi.advanceTimersByTime(SPLASH_FADE_MS);
    expect(document.getElementById("splash")).toBeNull();
  });

  it("with reduced motion it goes at once, no fade", () => {
    mountSplash();
    dismissSplash(document, true);
    expect(document.getElementById("splash")).toBeNull();
  });

  it("no splash (tests, a second mount) is nothing to do", () => {
    expect(() => dismissSplash(document, false)).not.toThrow();
  });
});

describe("index.html's splash", () => {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const style = [...doc.querySelectorAll("style")].map((s) => s.textContent).join("\n");

  it("is there from the first paint, announced politely, before the app's root", () => {
    const splash = doc.getElementById("splash")!;
    expect(splash).not.toBeNull();
    expect(splash.getAttribute("role")).toBe("status");
    expect(splash.getAttribute("aria-live")).toBe("polite");
    expect(splash.getAttribute("aria-label")).toBeTruthy();
    expect(splash.compareDocumentPosition(doc.getElementById("root")!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(splash.getAttribute("aria-label")).toBe("Starting Mirante");
    // The ribbon mark (back band, then the front over it) and the wordmark, both decorative.
    const [ribbon, word] = [...splash.querySelectorAll("svg")];
    expect(ribbon.getAttribute("class")).toBe("ribbon");
    expect([...ribbon.querySelectorAll("path")].map((p) => p.getAttribute("class"))).toEqual(["ribbon-back", "ribbon-front"]);
    expect(word.getAttribute("class")).toBe("wordmark");
    for (const svg of [ribbon, word]) expect(svg.getAttribute("aria-hidden")).toBe("true");
  });

  it("draws the wordmark as outlines: no text, no font needed before the app's load", () => {
    const word = doc.querySelector("#splash svg.wordmark")!;
    expect(word.querySelectorAll("text")).toHaveLength(0);
    expect(word.querySelector("path")?.getAttribute("d")).toMatch(/^M[\d\s.MLHVQCZ-]+$/);
    expect(doc.getElementById("splash")!.textContent!.trim()).toBe("");
  });

  it("animates once, our own draw-in: 120 to 600ms in all, nothing that loops", () => {
    const anims = [...style.matchAll(/animation:\s*([^;]+);/g)].map((m) => m[1].trim()).filter((a) => a !== "none");
    expect(anims.length).toBeGreaterThan(0);
    const ms = (t: string) => (t.endsWith("ms") ? parseFloat(t) : parseFloat(t) * 1000);
    for (const a of anims) {
      expect(a, a).not.toMatch(/infinite/);
      const [duration, delay = "0ms"] = a.match(/[\d.]+m?s/g)!;
      expect(ms(duration), a).toBeGreaterThanOrEqual(120);
      expect(ms(duration) + ms(delay), a).toBeLessThanOrEqual(600);
    }
  });

  it("paints the theme's background (no white flash) and follows a pinned theme", () => {
    expect(style).toMatch(/prefers-color-scheme:\s*dark/);
    expect(style).toContain('[data-theme="dark"]');
    expect(style).toContain('[data-theme="light"]');
    expect(style).toMatch(/html\s*\{[^}]*background/);
    expect(doc.querySelector('meta[name="color-scheme"]')?.getAttribute("content")).toBe("light dark");
  });

  it("is still with reduced motion", () => {
    expect(style).toMatch(/@media \(prefers-reduced-motion: reduce\)\s*\{[^}]*animation:\s*none/);
  });

  it("loads nothing remote and runs no inline script (the CSP allows neither)", () => {
    expect(html).not.toMatch(/https?:\/\//);
    for (const s of doc.querySelectorAll("script")) expect(s.getAttribute("src"), s.outerHTML).toBeTruthy();
    // The pinned theme is read by a file of the app's own, before the body paints.
    const first = doc.querySelector("head script");
    expect(first?.getAttribute("src")).toBe("/theme-boot.js");
    expect(first?.hasAttribute("type")).toBe(false);
  });

  it("theme-boot.js reads the same key the app saves the theme under", () => {
    expect(boot).toContain(JSON.stringify(THEME_KEY));
    expect(boot).toContain("try");
  });
});

describe("the app's waiting mark", () => {
  const shapes = (svg: Element) =>
    [...svg.querySelectorAll("*")].map((e) => [e.tagName.toLowerCase(), ...[...e.attributes].map((a) => `${a.name}=${a.value}`).sort()].join(" "));
  const launch = (cls: string) => new DOMParser().parseFromString(html, "text/html").querySelector(`#splash svg.${cls}`)!;
  const paths = (svg: Element) => [...svg.querySelectorAll("path")].map((p) => [p.getAttribute("d"), p.getAttribute("fill")]);

  it("is the launch splash's, shape for shape: the ribbon and the wordmark", () => {
    for (const [cls, el] of [["ribbon", createElement(RibbonMark)], ["wordmark", createElement(Wordmark)]] as const) {
      const { container } = render(el);
      const app = container.querySelector("svg")!;
      expect(shapes(app), cls).toEqual(shapes(launch(cls)));
      expect(app.getAttribute("viewBox"), cls).toBe(launch(cls).getAttribute("viewBox"));
      expect(app.getAttribute("class"), cls).toBe(cls);
    }
  });

  it("is the brand master's (assets/brand/ribbon.svg), in its two tones", () => {
    const master = new DOMParser().parseFromString(ribbonSvg, "image/svg+xml").documentElement;
    const { container } = render(createElement(RibbonMark));
    const app = container.querySelector("svg")!;
    expect(paths(app)).toEqual(paths(master));
    expect(paths(app).map(([, fill]) => fill)).toEqual(["#650C10", "#A8141B"]);
    expect(app.getAttribute("viewBox")).toBe(master.getAttribute("viewBox"));
    // The mono master is the same geometry.
    const mono = new DOMParser().parseFromString(ribbonMonoSvg, "image/svg+xml").documentElement;
    expect(paths(mono).map(([d]) => d)).toEqual(paths(master).map(([d]) => d));
    // The app icon is rendered from the same master.
    expect(iconSourceSvg).toBe(ribbonSvg);
  });

  it("still: no draw-in; mono: one colour (currentColor) for dark or tinted surfaces", () => {
    const { container } = render(createElement(RibbonMark, { still: true, tone: "mono" }));
    expect(container.querySelector("svg")).toHaveClass("ribbon", "ribbon-still", "ribbon-mono");
    expect(css).toMatch(/\.ribbon-mono path \{[^}]*fill: currentColor/);
  });
});

describe("the window", () => {
  it("starts hidden and shows once its page has loaded (at the latest after a few seconds): never a white rectangle", () => {
    expect(conf.app.windows.find((w) => w.label === "main")).toMatchObject({ visible: false });
    expect(shell).toMatch(/PageLoadEvent::Finished[\s\S]*w\.show\(\)/);
    expect(shell).toMatch(/SHOW_AT_THE_LATEST[\s\S]*window\.show\(\)/);
  });
});

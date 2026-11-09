// The launch splash: inline in index.html (first paint, before any script), then the app's.
import { render } from "@testing-library/react";
import { createElement } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { OrbMark } from "../components/Splash";
import html from "../../index.html?raw";
import boot from "../../public/theme-boot.js?raw";
import { dismissSplash, SPLASH_FADE_MS } from "./splash";
import { THEME_KEY } from "./theme";
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
    // The orbiting dots (the Orb-Dot mark) and the wordmark.
    expect(splash.querySelector("svg .orb-sats")).not.toBeNull();
    expect(splash.textContent).toContain("Mirante");
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
  it("is the launch splash's, shape for shape", () => {
    const shapes = (svg: Element) =>
      [...svg.querySelectorAll("*")].map((e) => [e.tagName.toLowerCase(), ...[...e.attributes].map((a) => `${a.name}=${a.value}`).sort()].join(" "));
    const launch = new DOMParser().parseFromString(html, "text/html").querySelector("#splash svg")!;
    const { container } = render(createElement(OrbMark));
    const app = container.querySelector("svg")!;
    expect(shapes(app)).toEqual(shapes(launch));
    expect(app.getAttribute("viewBox")).toBe(launch.getAttribute("viewBox"));
  });
});

describe("the window", () => {
  it("starts hidden and shows once its page has loaded (at the latest after a few seconds): never a white rectangle", () => {
    expect(conf.app.windows.find((w) => w.label === "main")).toMatchObject({ visible: false });
    expect(shell).toMatch(/PageLoadEvent::Finished[\s\S]*w\.show\(\)/);
    expect(shell).toMatch(/SHOW_AT_THE_LATEST[\s\S]*window\.show\(\)/);
  });
});

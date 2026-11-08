import { describe, expect, it } from "vitest";
import { applyTheme, loadTheme, nextTheme, THEME_KEY } from "./theme";

describe("theme", () => {
  it("cycles system -> light -> dark -> system and pins via data-theme", () => {
    expect([nextTheme("system"), nextTheme("light"), nextTheme("dark")]).toEqual(["light", "dark", "system"]);
    const el = document.createElement("html");
    applyTheme("dark", el);
    expect(el.dataset.theme).toBe("dark");
    applyTheme("system", el);
    expect(el.dataset.theme).toBeUndefined();
  });

  it("loads only known values", () => {
    const store = (v: string | null) => ({ getItem: (k: string) => (k === THEME_KEY ? v : null), setItem: () => {} });
    expect(loadTheme(store("dark"))).toBe("dark");
    expect(loadTheme(store("neon"))).toBe("system");
  });
});

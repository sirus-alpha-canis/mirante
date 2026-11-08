import { describe, expect, it } from "vitest";
import { hintsFor, isMacPlatform, isTypingTarget, shortcutFor } from "./shortcuts";

const CODE: Record<string, string> = { k: "KeyK", g: "KeyG", b: "KeyB", "1": "Digit1", j: "KeyJ", J: "KeyJ", Escape: "Escape" };
const k = (key: string, o: Partial<KeyboardEvent> = {}) => ({
  key,
  code: CODE[key] ?? "",
  metaKey: false,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  repeat: false,
  ...o,
});

describe("shortcutFor", () => {
  it("uses ⌘ on macOS and Ctrl elsewhere", () => {
    expect(shortcutFor(k("k", { metaKey: true }), true)).toBe("palette");
    expect(shortcutFor(k("k", { ctrlKey: true }), true)).toBeNull();
    expect(shortcutFor(k("g", { ctrlKey: true }), false)).toBe("grid");
    expect(shortcutFor(k("1", { metaKey: true }), true)).toBe("home");
    expect(shortcutFor(k("J", { metaKey: true, shiftKey: true }), true)).toBe("theme");
  });

  it("mod+J is the next task that needs you; with Shift it stays the theme", () => {
    expect(shortcutFor(k("j", { metaKey: true }), true)).toBe("next");
    expect(shortcutFor(k("j", { ctrlKey: true }), false)).toBe("next");
    expect(shortcutFor(k("J", { metaKey: true, shiftKey: true }), true)).toBe("theme");
    expect(shortcutFor(k("j", { metaKey: true, repeat: true }), true)).toBeNull();
  });

  it("mod+B is brain", () => {
    expect(shortcutFor(k("b", { metaKey: true }), true)).toBe("brain");
    expect(shortcutFor(k("b", { ctrlKey: true }), false)).toBe("brain");
    expect(shortcutFor(k("b"), true)).toBeNull();
  });

  it("plain keys and Alt combos are left to inputs; Escape always counts", () => {
    expect(shortcutFor(k("g"), true)).toBeNull();
    expect(shortcutFor(k("k", { metaKey: true, altKey: true }), true)).toBeNull();
    expect(shortcutFor(k("Escape"), true)).toBe("escape");
  });

  it("matches the physical key, so other layouts and Shift-shifted keys still work", () => {
    // AZERTY: the key at the "1" position types "&" without Shift.
    expect(shortcutFor(k("&", { code: "Digit1", metaKey: true }), true)).toBe("home");
    // A synthetic event without a code falls back to the key.
    expect(shortcutFor(k("g", { code: "", metaKey: true }), true)).toBe("grid");
  });

  it("Shift is part of the chord, and auto-repeat never fires one", () => {
    expect(shortcutFor(k("K", { code: "KeyK", metaKey: true, shiftKey: true }), true)).toBeNull();
    expect(shortcutFor(k("g", { metaKey: true, shiftKey: true }), true)).toBeNull();
    expect(shortcutFor(k("1", { metaKey: true, shiftKey: true }), true)).toBeNull();
    expect(shortcutFor(k("k", { metaKey: true, repeat: true }), true)).toBeNull();
    expect(shortcutFor(k("Escape", { repeat: true }), true)).toBeNull();
  });
});

describe("platform", () => {
  it("prefers userAgentData and falls back to navigator.platform", () => {
    expect(isMacPlatform({ platform: "MacIntel" })).toBe(true);
    expect(isMacPlatform({ platform: "Win32" })).toBe(false);
    expect(isMacPlatform({ platform: "", userAgentData: { platform: "macOS" } })).toBe(true);
    expect(isMacPlatform({ platform: "MacIntel", userAgentData: { platform: "Windows" } })).toBe(false);
  });

  it("hints name the keys of the platform", () => {
    expect(hintsFor(true)).toEqual({ palette: "⌘K", grid: "⌘G", brain: "⌘B", home: "⌘1", theme: "⇧⌘J", next: "⌘J", skills: "⌘L", send: "⌘↵" });
    expect(hintsFor(false)).toEqual({
      palette: "Ctrl+K",
      grid: "Ctrl+G",
      brain: "Ctrl+B",
      home: "Ctrl+1",
      theme: "Ctrl+Shift+J",
      next: "Ctrl+J",
      skills: "Ctrl+L",
      send: "Ctrl+↵",
    });
  });

  it("⌘L (Ctrl+L elsewhere) is the Skills chord", () => {
    expect(shortcutFor({ key: "l", code: "KeyL", metaKey: true, ctrlKey: false, shiftKey: false, altKey: false }, true)).toBe("skills");
    expect(shortcutFor({ key: "l", code: "KeyL", metaKey: false, ctrlKey: true, shiftKey: false, altKey: false }, false)).toBe("skills");
  });
});

describe("isTypingTarget", () => {
  it("inputs, textareas, editable content and the terminal keep their keys", () => {
    const input = document.createElement("input");
    const area = document.createElement("textarea");
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    const term = document.createElement("div");
    term.className = "xterm";
    const inner = document.createElement("textarea");
    term.appendChild(inner);
    for (const el of [input, area, editable, inner]) expect(isTypingTarget(el), el.tagName).toBe(true);
    expect(isTypingTarget(document.createElement("button"))).toBe(false);
    expect(isTypingTarget(window)).toBe(false);
    expect(isTypingTarget(null)).toBe(false);
  });
});

import { describe, expect, it } from "vitest";
import { isTypingTarget, shortcutFor } from "./shortcuts";

const k = (key: string, o: Partial<KeyboardEvent> = {}) => ({ key, metaKey: false, ctrlKey: false, shiftKey: false, altKey: false, ...o });

describe("shortcutFor", () => {
  it("uses ⌘ on macOS and Ctrl elsewhere", () => {
    expect(shortcutFor(k("k", { metaKey: true }), true)).toBe("palette");
    expect(shortcutFor(k("k", { ctrlKey: true }), true)).toBeNull();
    expect(shortcutFor(k("g", { ctrlKey: true }), false)).toBe("grid");
    expect(shortcutFor(k("1", { metaKey: true }), true)).toBe("home");
    expect(shortcutFor(k("J", { metaKey: true, shiftKey: true }), true)).toBe("theme");
  });

  it("plain keys and Alt combos are left to inputs; Escape always counts", () => {
    expect(shortcutFor(k("g"), true)).toBeNull();
    expect(shortcutFor(k("k", { metaKey: true, altKey: true }), true)).toBeNull();
    expect(shortcutFor(k("Escape"), true)).toBe("escape");
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

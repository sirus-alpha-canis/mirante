// App-wide keys. "mod" is ⌘ on macOS and Ctrl elsewhere. Only mod combos (and Escape) are
// shortcuts, so plain typing never triggers one; the caller also leaves Escape to whatever
// is being typed in (`isTypingTarget`): the terminal and the composers need it.
//
// Keys are matched by their physical position (`code`), so a chord works on any layout
// (AZERTY's "1" key types "&"); a synthetic event without a code falls back to `key`. Shift
// is part of the chord (⌘K is not ⇧⌘K), and a held key's auto-repeat never fires one.
//
// Inside the terminal: off macOS the mod key is Ctrl, which the terminal needs (Ctrl+K kills
// a line, Ctrl+G…), so the caller leaves mod chords there to the terminal. On macOS ⌘ is not
// a terminal key, so ⌘K opens the command bar even with the terminal focused.
export type ShortcutAction = "palette" | "grid" | "brain" | "home" | "escape" | "theme" | "next" | "skills";

type KeyLike = Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey"> & Partial<Pick<KeyboardEvent, "code" | "repeat">>;

const CHORDS: { code: string; action: Exclude<ShortcutAction, "escape">; shift: boolean }[] = [
  { code: "KeyK", action: "palette", shift: false },
  { code: "KeyG", action: "grid", shift: false },
  { code: "KeyB", action: "brain", shift: false },
  { code: "Digit1", action: "home", shift: false },
  { code: "KeyJ", action: "theme", shift: true },
  // Focus mode: the next task that needs you.
  { code: "KeyJ", action: "next", shift: false },
  // The skills library ("L" for library).
  { code: "KeyL", action: "skills", shift: false },
];

const CODE_OF_KEY: Record<string, string> = { k: "KeyK", g: "KeyG", b: "KeyB", "1": "Digit1", j: "KeyJ", l: "KeyL" };

export function shortcutFor(e: KeyLike, mac: boolean): ShortcutAction | null {
  if (e.repeat) return null;
  if (e.key === "Escape") return "escape";
  const mod = mac ? e.metaKey : e.ctrlKey;
  if (!mod || e.altKey) return null;
  // The other modifier is not part of any chord (Ctrl+⌘K is not ⌘K).
  if (mac ? e.ctrlKey : e.metaKey) return null;
  const code = e.code || CODE_OF_KEY[e.key.toLowerCase()] || "";
  return CHORDS.find((c) => c.code === code && c.shift === e.shiftKey)?.action ?? null;
}

/** Whether a key event's target is something the user types into (an input, a textarea,
 * editable content, or the xterm.js terminal, whose keys belong to the agent). */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof Element)) return false;
  if (target.closest(".xterm")) return true;
  if (target instanceof HTMLElement && target.isContentEditable) return true;
  if (target.closest("[contenteditable=\"\"], [contenteditable=\"true\"]")) return true;
  return target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement || target instanceof HTMLSelectElement;
}

type NavLike = { platform?: string; userAgentData?: { platform?: string } };

/** macOS or not: `navigator.userAgentData.platform` where the engine has it, else the
 * (deprecated, still universal) `navigator.platform`. */
export function isMacPlatform(nav: NavLike | undefined): boolean {
  if (!nav) return false;
  const p = nav.userAgentData?.platform || nav.platform || "";
  return /mac/i.test(p);
}

/** This machine, as the webview reports it. */
export function onMac(): boolean {
  return isMacPlatform(typeof navigator === "undefined" ? undefined : (navigator as NavLike));
}

/** Each chord's label, plus `send`: the key that sends a composer or the dispatch form. */
export type ShortcutHints = Record<Exclude<ShortcutAction, "escape"> | "send", string>;

export function hintsFor(mac: boolean): ShortcutHints {
  return mac
    ? { palette: "⌘K", grid: "⌘G", brain: "⌘B", home: "⌘1", theme: "⇧⌘J", next: "⌘J", skills: "⌘L", send: "⌘↵" }
    : { palette: "Ctrl+K", grid: "Ctrl+G", brain: "Ctrl+B", home: "Ctrl+1", theme: "Ctrl+Shift+J", next: "Ctrl+J", skills: "Ctrl+L", send: "Ctrl+↵" };
}

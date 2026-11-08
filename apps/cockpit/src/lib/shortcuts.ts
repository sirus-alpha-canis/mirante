// App-wide keys. "mod" is ⌘ on macOS and Ctrl elsewhere. Only mod combos (and Escape) are
// shortcuts, so plain typing never triggers one; the caller also leaves Escape to whatever
// is being typed in (`isTypingTarget`): the terminal and the composers need it.
export type ShortcutAction = "palette" | "grid" | "home" | "escape" | "theme";

export function shortcutFor(e: Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey" | "shiftKey" | "altKey">, mac: boolean): ShortcutAction | null {
  const mod = mac ? e.metaKey : e.ctrlKey;
  if (e.key === "Escape") return "escape";
  if (!mod || e.altKey) return null;
  switch (e.key.toLowerCase()) {
    case "k":
      return "palette";
    case "g":
      return "grid";
    case "1":
      return "home";
    case "j":
      return e.shiftKey ? "theme" : null;
    default:
      return null;
  }
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

export const SHORTCUT_HINTS: Record<Exclude<ShortcutAction, "escape">, string> = {
  palette: "⌘K",
  grid: "⌘G",
  home: "⌘1",
  theme: "⇧⌘J",
};

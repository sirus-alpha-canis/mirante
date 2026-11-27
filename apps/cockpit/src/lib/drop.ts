// Files dropped from Finder. Tauri's own drop handler is on (the webview never gets the
// files), so its drag-drop event, with absolute paths and the cursor in physical pixels, is
// routed here to one target: the one under the cursor, else the one holding the focus, else
// the one last focused, else the only one mounted. The target under the cursor (or the one
// that would take the drop) is highlighted while a drag hovers.

/** Tauri's `DragDropEvent` (`@tauri-apps/api/webview`), as the shell forwards it. */
export type DragDrop =
  | { type: "enter"; paths: string[]; position: { x: number; y: number } }
  | { type: "over"; position: { x: number; y: number } }
  | { type: "drop"; paths: string[]; position: { x: number; y: number } }
  | { type: "leave" };

export interface DropTarget {
  el(): HTMLElement | null;
  drop(paths: string[]): void;
  hover(on: boolean): void;
}

const targets: DropTarget[] = [];
let lastFocused: DropTarget | null = null;
let hovered: DropTarget | null = null;

export function addDropTarget(t: DropTarget): () => void {
  targets.push(t);
  return () => {
    const i = targets.indexOf(t);
    if (i >= 0) targets.splice(i, 1);
    if (lastFocused === t) lastFocused = null;
    if (hovered === t) hovered = null;
  };
}

/** The person put the focus in `t` (it takes a drop that lands on no target). */
export function focusedTarget(t: DropTarget): void {
  lastFocused = t;
}

const mounted = (t: DropTarget) => !!t.el()?.isConnected;

/** Where a drop at physical `(x, y)` goes. */
export function targetAt(pos: { x: number; y: number } | null, doc: Document = document): DropTarget | null {
  const live = targets.filter(mounted);
  if (pos && typeof doc.elementFromPoint === "function") {
    const ratio = doc.defaultView?.devicePixelRatio || 1;
    const hit = doc.elementFromPoint(pos.x / ratio, pos.y / ratio);
    // The innermost target holding the point (a terminal inside a panel with a composer).
    const under = hit ? live.filter((t) => t.el()!.contains(hit)) : [];
    if (under.length) return under.reduce((a, b) => (a.el()!.contains(b.el()!) ? b : a));
  }
  const focus = doc.activeElement;
  const holding = focus ? live.find((t) => t.el()!.contains(focus)) : undefined;
  if (holding) return holding;
  if (lastFocused && live.includes(lastFocused)) return lastFocused;
  return live.length === 1 ? live[0] : null;
}

function hover(next: DropTarget | null) {
  if (hovered === next) return;
  hovered?.hover(false);
  hovered = next;
  next?.hover(true);
}

export function handleDragDrop(e: DragDrop, doc: Document = document): void {
  switch (e.type) {
    case "enter":
    case "over":
      hover(targetAt(e.position, doc));
      return;
    case "leave":
      hover(null);
      return;
    case "drop": {
      const t = targetAt(e.position, doc);
      hover(null);
      if (t && e.paths.length) t.drop(e.paths);
      return;
    }
  }
}

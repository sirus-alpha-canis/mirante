// Where the person was when the app closed: the view, the project, the open task and its
// tab, and Home's scroll. Per viewer, in this machine's storage; storage that refuses (a
// private window, blocked site data) just means starting on Home.
export type View = "home" | "detail" | "grid" | "brain" | "skills" | "rules";
export type DetailTab = "Steps" | "Terminal" | "Diff" | "Memory";

export interface UiState {
  mode: View;
  project: string | null;
  taskId: string | null;
  tab: DetailTab | null;
  /** Home's scroll offset, in pixels. */
  scroll: number;
}

export const UI_KEY = "mirante.ui";

const VIEWS: View[] = ["home", "detail", "grid", "brain", "skills", "rules"];
const TABS: DetailTab[] = ["Steps", "Terminal", "Diff", "Memory"];

export const DEFAULT_UI: UiState = { mode: "home", project: null, taskId: null, tab: null, scroll: 0 };

type KV = Pick<Storage, "getItem" | "setItem">;

/** The saved state, each field checked (a bad or missing one falls back to its default). */
export function loadUi(store: KV): UiState {
  let raw: unknown;
  try {
    raw = JSON.parse(store.getItem(UI_KEY) ?? "null");
  } catch {
    return DEFAULT_UI;
  }
  if (!raw || typeof raw !== "object") return DEFAULT_UI;
  const o = raw as Record<string, unknown>;
  const str = (v: unknown) => (typeof v === "string" && v.length > 0 ? v : null);
  const taskId = str(o.taskId);
  const mode = VIEWS.includes(o.mode as View) ? (o.mode as View) : "home";
  return {
    // A detail with no task to show is Home.
    mode: mode === "detail" && !taskId ? "home" : mode,
    project: str(o.project),
    taskId,
    tab: TABS.includes(o.tab as DetailTab) ? (o.tab as DetailTab) : null,
    scroll: typeof o.scroll === "number" && Number.isFinite(o.scroll) && o.scroll > 0 ? o.scroll : 0,
  };
}

export function saveUi(store: KV, ui: UiState): void {
  try {
    store.setItem(UI_KEY, JSON.stringify(ui));
  } catch {
    // Not remembered: the next start is on Home.
  }
}

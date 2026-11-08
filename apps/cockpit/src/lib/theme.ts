// "system" follows the OS (prefers-color-scheme); light / dark pin it. Per machine.
export type Theme = "system" | "light" | "dark";
export const THEME_KEY = "mirante.theme";

type KV = Pick<Storage, "getItem" | "setItem">;

export function loadTheme(store: KV): Theme {
  const v = store.getItem(THEME_KEY);
  return v === "light" || v === "dark" ? v : "system";
}

export function nextTheme(t: Theme): Theme {
  return t === "system" ? "light" : t === "light" ? "dark" : "system";
}

export function applyTheme(t: Theme, root: HTMLElement = document.documentElement): void {
  if (t === "system") delete root.dataset.theme;
  else root.dataset.theme = t;
}

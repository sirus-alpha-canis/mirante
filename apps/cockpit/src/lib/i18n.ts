// Every word the cockpit shows a person, in one place: `en` (complete, the fallback) and
// `pt`. `t(key, params)` reads the current language; `{name}` in a string is a parameter.
// Text sent to agents (dispatch context, propose / refine requests) is not here: agents read
// English whatever the interface's language.
import { createContext, useContext } from "react";

export type Lang = "en" | "pt";
export const LANGS: Lang[] = ["en", "pt"];
/** Where the chosen language is remembered (per viewer, this machine). */
export const LANG_KEY = "mirante.lang";

const en = {
  // Common
  "common.retry": "Retry",
  // Terminal
  "term.region": "Terminal of {branch}",
  "term.noLive": "No live session. Resume the task to see its terminal.",
  "term.ended": "stream ended",
  "term.reconnecting": "stream ended, reconnecting…",
  "term.lost": "the terminal stream is gone",
  "term.reconnect": "Reconnect",
  "term.readOnly": "read-only stream (this agent takes messages from the composer)",
  "term.inputTimedOut": "input not delivered (the daemon timed out)",
  "term.inputFailed": "input not delivered",
  "term.inputDropped": "input dropped (the daemon is not keeping up)",
  "term.linkHint": "Link to {url} (⌘/Ctrl-click to open)",
  "term.linkBlocked": "Mirante opens only github.com and localhost links — copy it instead",
  "term.linkFailed": "could not open the link",
} satisfies Record<string, string>;

export type Key = keyof typeof en;
export type Params = Record<string, string | number>;

const pt: Partial<Record<Key, string>> = {
  "common.retry": "Tentar de novo",
  "term.region": "Terminal de {branch}",
  "term.noLive": "Nenhuma sessão ativa. Retome a tarefa para ver o terminal.",
  "term.ended": "stream encerrado",
  "term.reconnecting": "stream encerrado, reconectando…",
  "term.lost": "o stream do terminal caiu",
  "term.reconnect": "Reconectar",
  "term.readOnly": "stream só de leitura (este agente recebe mensagens pelo compositor)",
  "term.inputTimedOut": "entrada não entregue (o daemon não respondeu a tempo)",
  "term.inputFailed": "entrada não entregue",
  "term.inputDropped": "entrada descartada (o daemon não está dando conta)",
  "term.linkHint": "Link para {url} (⌘/Ctrl-clique para abrir)",
  "term.linkBlocked": "O Mirante só abre links do github.com e do localhost — copie este",
  "term.linkFailed": "não foi possível abrir o link",
};

export const DICTS: Record<Lang, Partial<Record<Key, string>>> = { en, pt };

let current: Lang = "en";

/** The language `t` speaks (set by the app; tests leave it at English). */
export function setLang(l: Lang): void {
  current = l;
}

export function getLang(): Lang {
  return current;
}

/** `key` in the current language (English when it has no translation), `{name}` filled. */
export function t(key: Key, params?: Params): string {
  const s = (current === "en" ? undefined : DICTS[current][key]) ?? en[key];
  if (!params) return s;
  return s.replace(/\{(\w+)\}/g, (m, k: string) => (k in params ? String(params[k]) : m));
}

/** `one` when `n` is 1, else `many`; `{n}` is `n`. */
export function tn(n: number, one: Key, many: Key, params?: Params): string {
  return t(n === 1 ? one : many, { n, ...params });
}

type KV = Pick<Storage, "getItem" | "setItem">;

/** The remembered language; else Portuguese for a Portuguese system, English otherwise. */
export function loadLang(store: KV, system: string | undefined = typeof navigator === "undefined" ? undefined : navigator.language): Lang {
  try {
    const v = store.getItem(LANG_KEY);
    if (v === "en" || v === "pt") return v;
  } catch {
    // Storage refused: the system's language decides.
  }
  return system?.toLowerCase().startsWith("pt") ? "pt" : "en";
}

export function saveLang(store: KV, l: Lang): void {
  try {
    store.setItem(LANG_KEY, l);
  } catch {
    // Not remembered: the next start follows the system.
  }
}

/** The language shown; components that call `useT` render again when it changes. */
export const LangContext = createContext<Lang>("en");

/** `t`, re-rendering the caller when the language changes. */
export function useT(): typeof t {
  useContext(LangContext);
  return t;
}

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
  // Badges and labels
  "badge.manualHint": "In the task's Terminal tab, press Shift+Tab until the footer says \"manual mode on\" (Mirante does not switch it for you).",
  "badge.editsTitle": "Claude edits files without asking you.",
  "badge.autoTitle": "Claude approves its own commands.",
  "badge.editsOn": "accept-edits on",
  "badge.autoOn": "auto mode on",
  "badge.hibernated": "hibernated",
  "badge.queued": "{n} queued",
  "label.autoEdits": "auto-edits",
  "label.autoEditsTitle": "Codex edits files inside this worktree without asking. Commands outside the sandbox, network and other folders still ask you.",
  "label.sessionGrant": "session-wide grant",
  "label.sessionGrantTitle": "Allowing lets the agent write under this folder until the session ends, not just once.",
  "label.allowSession": "Allow for this session",
  "label.once": "once",
  "label.onceTitle": "Allowing applies to this request only.",
  "label.allowOnce": "Allow once",
  // Columns and Home's sections
  "column.needs_you": "Needs you",
  "column.working": "Working",
  "column.review": "Ready for review",
  "column.idle": "Idle",
  "column.archived": "Archived",
  "section.needs_you": "Needs you",
  "section.review": "Ready for review",
  "section.working": "Working",
  "section.idle": "Idle",
  "nudge.one": "1 agent has waited on you for over 10 min",
  "nudge.many": "{n} agents have waited on you for over 10 min",
  "home.inbox": "Inbox",
  "home.empty": "No agents yet. Dispatch one below.",
  "home.noProject": "Add a repository on the left to start.",
  "home.updates": "Agent updates",
  // App frame
  "nav.views": "Views",
  "nav.home": "Home",
  "nav.detail": "Detail",
  "nav.grid": "Grid",
  "nav.brain": "Brain",
  "nav.skills": "Skills",
  "next.label": "Next",
  "next.position": "{at} of {n}",
  "next.none": "Nothing needs you",
  "next.title": "Open the next task that needs you ({key}; not while typing in a field)",
  "next.aria": "Next, {at} of {n}",
  "next.ariaNone": "Next: nothing needs you",
  "top.commands": "Commands",
  "theme.label": "Theme: {theme}",
  "theme.system": "system",
  "theme.light": "light",
  "theme.dark": "dark",
  "lang.label": "Language",
  "lang.en": "English",
  "lang.pt": "Portuguese",
  "conn.aria": "Daemon connection",
  "conn.connecting": "Connecting to the Mirante daemon…",
  "conn.down": "Daemon not reachable ({error}). Retrying in {s}s.",
  "conn.start": "Start daemon",
  "cmd.home": "Go to Home",
  "cmd.next": "Next task that needs you",
  "cmd.grid": "Toggle agent grid",
  "cmd.brain": "Go to Brain",
  "cmd.review": "Review memory",
  "cmd.skills": "Go to Skills",
  "cmd.skillsReview": "Review skills",
  "cmd.lang": "Language: {lang}",
  "cmd.project": "Project {name}",
  "cmd.answer": "Answer in terminal: {title} (@claude {branch})",
  "cmd.open": "Open {title} (@{agent} {branch})",
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
  "badge.manualHint": "Na aba Terminal da tarefa, aperte Shift+Tab até o rodapé dizer \"manual mode on\" (o Mirante não muda isso por você).",
  "badge.editsTitle": "O Claude edita arquivos sem perguntar.",
  "badge.autoTitle": "O Claude aprova os próprios comandos.",
  "badge.editsOn": "accept-edits ligado",
  "badge.autoOn": "modo automático ligado",
  "badge.hibernated": "hibernado",
  "badge.queued": "{n} na fila",
  "label.autoEdits": "edições automáticas",
  "label.autoEditsTitle": "O Codex edita arquivos dentro desta worktree sem perguntar. Comandos fora do sandbox, rede e outras pastas ainda pedem a você.",
  "label.sessionGrant": "permissão para a sessão",
  "label.sessionGrantTitle": "Permitir deixa o agente escrever nesta pasta até a sessão acabar, não só uma vez.",
  "label.allowSession": "Permitir nesta sessão",
  "label.once": "uma vez",
  "label.onceTitle": "Permitir vale só para este pedido.",
  "label.allowOnce": "Permitir uma vez",
  "column.needs_you": "Precisa de você",
  "column.working": "Trabalhando",
  "column.review": "Pronto para revisar",
  "column.idle": "Parados",
  "column.archived": "Arquivados",
  "section.needs_you": "Precisa de você",
  "section.review": "Pronto para revisar",
  "section.working": "Trabalhando",
  "section.idle": "Parados",
  "nudge.one": "1 agente espera você há mais de 10 min",
  "nudge.many": "{n} agentes esperam você há mais de 10 min",
  "home.inbox": "Caixa de entrada",
  "home.empty": "Nenhum agente ainda. Despache um abaixo.",
  "home.noProject": "Adicione um repositório à esquerda para começar.",
  "home.updates": "Atualizações dos agentes",
  "nav.views": "Visões",
  "nav.home": "Início",
  "nav.detail": "Detalhe",
  "nav.grid": "Grade",
  "nav.brain": "Brain",
  "nav.skills": "Skills",
  "next.label": "Próximo",
  "next.position": "{at} de {n}",
  "next.none": "Nada precisa de você",
  "next.title": "Abrir a próxima tarefa que precisa de você ({key}; não enquanto você digita num campo)",
  "next.aria": "Próximo, {at} de {n}",
  "next.ariaNone": "Próximo: nada precisa de você",
  "top.commands": "Comandos",
  "theme.label": "Tema: {theme}",
  "theme.system": "sistema",
  "theme.light": "claro",
  "theme.dark": "escuro",
  "lang.label": "Idioma",
  "lang.en": "Inglês",
  "lang.pt": "Português",
  "conn.aria": "Conexão com o daemon",
  "conn.connecting": "Conectando ao daemon do Mirante…",
  "conn.down": "Daemon inacessível ({error}). Nova tentativa em {s}s.",
  "conn.start": "Iniciar daemon",
  "cmd.home": "Ir para o Início",
  "cmd.next": "Próxima tarefa que precisa de você",
  "cmd.grid": "Alternar a grade de agentes",
  "cmd.brain": "Ir para o Brain",
  "cmd.review": "Revisar a memória do repositório",
  "cmd.skills": "Ir para Skills",
  "cmd.skillsReview": "Revisar skills",
  "cmd.lang": "Idioma: {lang}",
  "cmd.project": "Projeto {name}",
  "cmd.answer": "Responder no terminal: {title} (@claude {branch})",
  "cmd.open": "Abrir {title} (@{agent} {branch})",
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

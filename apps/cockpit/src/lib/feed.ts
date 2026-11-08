// How a task's feed reads to a person: repeated lines folded ("×N"), runs of tool steps
// summed up in plain words ("read 14 files, ran 3 commands"), the current step said
// without raw commands, and the agent's last answer. Pure functions over feed items; the
// text they return is shown as text nodes only.
import type { FeedItem } from "../store/state";
import { t, tn } from "./i18n";

export interface Folded {
  /** The newest line of the run (its seq keys the row). */
  item: FeedItem;
  count: number;
}

const sameLine = (a: FeedItem, b: FeedItem) => a.kind === b.kind && a.text === b.text && a.ok === b.ok && a.mode === b.mode;

/** Consecutive identical lines (same kind, text and outcome) as one line with a count. */
export function collapseRepeats(items: FeedItem[]): Folded[] {
  const out: Folded[] = [];
  for (const item of items) {
    const last = out[out.length - 1];
    if (last && sameLine(last.item, item)) out[out.length - 1] = { item, count: last.count + 1 };
    else out.push({ item, count: 1 });
  }
  return out;
}

/** A tool step's tool name and what it worked on (`"Read src/a.ts"` → Read, src/a.ts). */
function toolOf(i: FeedItem): { tool: string; what: string } {
  if (i.kind === "command") return { tool: "Bash", what: i.text.replace(/^\$ /, "") };
  if (i.tool) return { tool: i.tool, what: i.text.slice(i.tool.length).trim() };
  const at = i.text.indexOf(" ");
  return at < 0 ? { tool: i.text, what: "" } : { tool: i.text.slice(0, at), what: i.text.slice(at + 1) };
}

const READ = new Set(["Read", "NotebookRead", "LS"]);
const EDIT = new Set(["Edit", "Write", "MultiEdit", "NotebookEdit"]);
const SEARCH = new Set(["Grep", "Glob", "WebSearch", "ToolSearch"]);
const RUN = new Set(["Bash", "BashOutput", "command"]);

type Bucket = "read" | "edit" | "search" | "run" | "other";

function bucket(i: FeedItem): Bucket {
  if (i.kind === "command") return "run";
  const { tool } = toolOf(i);
  if (READ.has(tool)) return "read";
  if (EDIT.has(tool)) return "edit";
  if (SEARCH.has(tool)) return "search";
  if (RUN.has(tool)) return "run";
  return "other";
}

/** "read 14 files, ran 3 commands": what a run of tool steps did. Files are counted once
 * each; searches, commands and other tools per call. */
export function describeRun(items: FeedItem[]): string {
  const files = { read: new Set<string>(), edit: new Set<string>() };
  let search = 0;
  let run = 0;
  let other = 0;
  let failed = 0;
  for (const i of items) {
    if (i.ok === false) failed++;
    const b = bucket(i);
    if (b === "read" || b === "edit") files[b].add(toolOf(i).what);
    else if (b === "search") search++;
    else if (b === "run") run++;
    else other++;
  }
  const parts = [
    files.read.size > 0 && tn(files.read.size, "run.readOne", "run.readMany"),
    files.edit.size > 0 && tn(files.edit.size, "run.editOne", "run.editMany"),
    search > 0 && tn(search, "run.searchOne", "run.searchMany"),
    run > 0 && tn(run, "run.runOne", "run.runMany"),
    other > 0 && tn(other, "run.otherOne", "run.otherMany"),
  ].filter(Boolean);
  return parts.join(", ") + (failed > 0 ? tn(failed, "run.failedOne", "run.failedMany") : "");
}

export type StepEntry = { type: "line"; item: FeedItem; count: number } | { type: "run"; items: FeedItem[]; text: string };

const isToolStep = (i: FeedItem) => i.kind === "tool" || i.kind === "command";

/** The Steps tab's entries: two or more tool steps in a row become one run (expandable to
 * its lines); every other line stays, identical consecutive ones folded with a count. */
export function summarizeSteps(items: FeedItem[]): StepEntry[] {
  const out: StepEntry[] = [];
  let run: FeedItem[] = [];
  const flush = () => {
    if (run.length === 1) out.push({ type: "line", item: run[0], count: 1 });
    else if (run.length > 1) out.push({ type: "run", items: run, text: describeRun(run) });
    run = [];
  };
  let others: FeedItem[] = [];
  const flushOthers = () => {
    for (const f of collapseRepeats(others)) out.push({ type: "line", ...f });
    others = [];
  };
  for (const i of items) {
    if (isToolStep(i)) {
      flushOthers();
      run.push(i);
    } else {
      flush();
      others.push(i);
    }
  }
  flush();
  flushOthers();
  return out;
}

const baseName = (p: string) => p.replace(/\/+$/, "").split("/").pop() || p;

/** What the agent is doing, in a few plain words (never the raw command). */
export function plainStep(i: FeedItem | undefined): string {
  if (!i) return t("step.starting");
  switch (i.kind) {
    case "command":
      return t("step.command");
    case "agent_message":
      return t("step.writing");
    case "turn":
      return t(i.phase === "ended" ? "step.turnEnded" : "step.turnStarted");
    case "message":
      return t("step.message");
    case "permission":
      return t("step.permission");
    case "tool": {
      const { tool, what } = toolOf(i);
      switch (bucket(i)) {
        case "read":
          return what ? t("step.reading", { file: baseName(what) }) : t("step.readingFiles");
        case "edit":
          return what ? t("step.editing", { file: baseName(what) }) : t("step.editingFiles");
        case "search":
          return t("step.searching");
        case "run":
          return t("step.command");
        default:
          return t("step.using", { tool });
      }
    }
    default:
      return t("step.working");
  }
}

/** The agent's newest message (its answer once a turn ends), or null. */
export function lastAnswer(feed: FeedItem[] | undefined): string | null {
  if (!feed) return null;
  for (let k = feed.length - 1; k >= 0; k--) if (feed[k].kind === "agent_message" && feed[k].text.trim()) return feed[k].text;
  return null;
}

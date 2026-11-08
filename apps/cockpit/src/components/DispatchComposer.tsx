import {
  type KeyboardEvent,
  type Ref,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { useEnv, useSelect } from "../env";
import {
  applySuggestion,
  branchFromGoal,
  branchProblem,
  GOAL_HELP,
  isPowerSyntax,
  matchProjects,
  nextFreeBranch,
  parseDispatch,
  resolveRepo,
  suggest,
  type SuggestionItem,
} from "../lib/dispatch";
import { type AgentKind, type CreatedOrigin, M, type StartResult, type TaskView } from "../lib/protocol";
import type { Project } from "../store/select";

/** What `task.create` fixed for the new task's skills, in a person's words. */
export function originText(branch: string, o: CreatedOrigin): { text: string; warn: boolean } {
  if (o.observed) {
    return {
      text: `${branch} gets no skills until you confirm its origin: the checkout is pinned to ${o.origin}, but its remote now says ${o.observed}. Confirm it from the task's Skills chip.`,
      warn: true,
    };
  }
  const pinned = o.pinned ? ", pinned for this checkout now" : "";
  return { text: `Skills: ${branch} follows ${o.origin} (client ${o.tenant})${pinned}.`, warn: false };
}

/** Where the last agent picked in the form is remembered (per viewer, this machine). */
export const AGENT_KEY = "mirante.dispatch.agent";

type KV = Pick<Storage, "getItem" | "setItem">;

/** The remembered agent; Claude when none, unknown, or storage refuses. */
export function loadAgent(store: KV): AgentKind {
  try {
    return store.getItem(AGENT_KEY) === "codex" ? "codex" : "claude";
  } catch {
    return "claude";
  }
}

export function saveAgent(store: KV, agent: AgentKind): void {
  try {
    store.setItem(AGENT_KEY, agent);
  } catch {
    // A private window or blocked storage: the choice just is not remembered.
  }
}

// How many auto-named branches to try when git already has one (-2, -3…).
const MAX_RENAMES = 50;

const AGENT_LABEL: Record<AgentKind, string> = { claude: "Claude", codex: "Codex" };

/** The dispatch form: agent, repo, goal (⌘↵), and a branch named after the goal. Text that
 * starts with "@claude" / "@codex" is the one-line syntax ("@claude shop/feat/x goal"). */
export function DispatchComposer(props: {
  projects: Project[];
  /** The project selected in the rail: the repo the form starts on. */
  project: string | null;
  value: string;
  onChange(v: string): void;
  agent: AgentKind;
  onAgent(a: AgentKind): void;
  onDispatched?(taskId: string): void;
  /** The goal textarea, for callers that move the keyboard to it. */
  inputRef?: Ref<HTMLTextAreaElement>;
}) {
  const { rpc, cockpit } = useEnv();
  const tasks = useSelect((s) => s.tasks);
  const [error, setError] = useState<string | null>(null);
  const [originNote, setOriginNote] = useState<{ text: string; warn: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  // A ref, not `busy`: a second ⌘↵ (or key auto-repeat) arrives before React re-renders.
  const inFlight = useRef(false);
  // The draft as it is now, for the moment the create returns (the user may keep typing).
  const latest = useRef(props.value);
  latest.current = props.value;

  // The repo: picked here, else the rail's selection, else the first project.
  const [picked, setPicked] = useState<string | null>(null);
  useEffect(() => setPicked(null), [props.project]);
  const has = (p: string | null) => !!p && props.projects.some((x) => x.path === p);
  const repo = has(picked) ? picked : has(props.project) ? props.project : (props.projects[0]?.path ?? null);

  // The branch: named after the goal, past the names this repo's tasks already use, unless edited.
  const [edited, setEdited] = useState<string | null>(null);
  const power = isPowerSyntax(props.value);
  const takenIn = useCallback(
    (path: string | null) => new Set(Object.values(tasks).filter((t) => t.repo_path === path).map((t) => t.branch)),
    [tasks],
  );
  const taken = useMemo(() => takenIn(repo), [takenIn, repo]);
  const auto = nextFreeBranch(branchFromGoal(props.value), taken);
  const branch = edited ?? auto;

  const pickAgent = (a: AgentKind) => props.onAgent(a);

  /** task.create; an auto-named branch git already has moves on to -2, -3… */
  const create = async (target: { repo: string; agent: AgentKind; branch: string; goal: string }, renames: boolean) => {
    if (!renames) return { v: await rpc<TaskView>(M.taskCreate, target), branch: target.branch };
    const tried = takenIn(target.repo);
    let b = target.branch;
    for (let n = 0; ; n++) {
      try {
        // A base makes the daemon refuse an existing branch instead of checking it out.
        return { v: await rpc<TaskView>(M.taskCreate, { ...target, branch: b, base: "HEAD" }), branch: b };
      } catch (e) {
        if (n >= MAX_RENAMES || !(e instanceof Error) || !/already exists/.test(e.message)) throw e;
        tried.add(b);
        b = nextFreeBranch(branchFromGoal(target.goal), tried);
      }
    }
  };

  const submit = async () => {
    if (inFlight.current) return;
    const sent = props.value;
    let target: { repo: string; agent: AgentKind; branch: string; goal: string };
    let renames = false;
    if (power) {
      const p = parseDispatch(sent);
      if (!p.ok) return setError(p.error);
      const r = resolveRepo(p.value.repo, props.projects);
      if (!r.ok) return setError(r.error);
      // "shop/" with no branch: named from the goal, like the form's.
      renames = p.value.branch === "";
      const b = renames ? nextFreeBranch(branchFromGoal(p.value.goal), takenIn(r.path)) : p.value.branch;
      target = { repo: r.path, agent: p.value.agent, branch: b, goal: p.value.goal };
    } else {
      const goal = sent.trim();
      if (!goal) return setError("say what the agent should do");
      if (!repo) return setError("Add a repository under Projects first.");
      const bad = branchProblem(branch);
      if (bad) return setError(bad);
      target = { repo, agent: props.agent, branch, goal };
      renames = edited === null;
    }
    inFlight.current = true;
    setBusy(true);
    setError(null);
    setOriginNote(null);
    try {
      const { v, branch: made } = await create(target, renames);
      cockpit.dispatch({ type: "task", view: v });
      if (v.origin) setOriginNote(originText(made, v.origin));
      // Clear only what was dispatched: text typed meanwhile is the user's next message.
      if (latest.current === sent) {
        props.onChange("");
        setEdited(null);
      }
      props.onDispatched?.(v.id);
      // Boot can take up to 45 s; progress arrives as events, so do not wait here.
      rpc<StartResult>(M.sessionStart, { task: v.id, prompt: target.goal }).catch((e) =>
        setError(`${made} was created but the agent did not start: ${e instanceof Error ? e.message : e}`),
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  // Suggestions for the @ line: agents, then projects; never inside the goal.
  const box = useRef<HTMLTextAreaElement | null>(null);
  const setBox = useCallback(
    (el: HTMLTextAreaElement | null) => {
      box.current = el;
      const r = props.inputRef;
      if (typeof r === "function") r(el);
      else if (r) (r as { current: HTMLTextAreaElement | null }).current = el;
    },
    [props.inputRef],
  );
  const [caret, setCaret] = useState(0);
  const [focused, setFocused] = useState(false);
  // The text Escape closed the list on: it stays closed until the text changes.
  const [dismissed, setDismissed] = useState<string | null>(null);
  const sugg = focused && dismissed !== props.value ? suggest(props.value, caret, props.projects) : null;
  const suggKey = sugg ? `${sugg.kind}:${sugg.from}:${sugg.items.map((i) => i.label).join(",")}` : "";
  const [activeAt, setActiveAt] = useState({ key: "", i: 0 });
  const active = sugg && activeAt.key === suggKey ? Math.min(activeAt.i, sugg.items.length - 1) : 0;
  const listId = useId();
  const optionId = (i: number) => `${listId}-opt-${i}`;
  // Where the caret goes once a picked suggestion is in the text.
  const placeCaret = useRef<number | null>(null);
  useLayoutEffect(() => {
    const at = placeCaret.current;
    if (at === null || !box.current) return;
    placeCaret.current = null;
    box.current.setSelectionRange(at, at);
  });
  const pick = (i: number) => {
    if (!sugg) return;
    const next = applySuggestion(props.value, sugg, sugg.items[i]);
    placeCaret.current = next.caret;
    setCaret(next.caret);
    props.onChange(next.text);
    box.current?.focus();
  };

  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter that confirms an IME composition (Japanese, Chinese…) is not a send nor a pick.
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    if (sugg && !e.metaKey && !e.ctrlKey && !e.altKey) {
      const n = sugg.items.length;
      if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        e.preventDefault();
        setActiveAt({ key: suggKey, i: (active + (e.key === "ArrowDown" ? 1 : n - 1)) % n });
        return;
      }
      if ((e.key === "Enter" && !e.shiftKey) || (e.key === "Tab" && !e.shiftKey)) {
        e.preventDefault();
        pick(active);
        return;
      }
      if (e.key === "Escape") {
        e.preventDefault();
        setDismissed(props.value);
        return;
      }
    }
    // ⌘↵ / Ctrl↵ sends; a plain Enter is a new line of the goal.
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void submit();
    }
  };

  return (
    <form
      className="composer dispatch"
      aria-label="Dispatch an agent"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className="dispatch-row">
        <div className="seg" role="group" aria-label="Agent">
          {(["claude", "codex"] as const).map((a) => (
            <button
              key={a}
              type="button"
              className="btn btn-small"
              aria-pressed={!power && props.agent === a}
              disabled={power}
              onClick={() => pickAgent(a)}
            >
              {AGENT_LABEL[a]}
            </button>
          ))}
        </div>
        <RepoBox projects={props.projects} value={repo} disabled={power} onPick={setPicked} />
      </div>
      <label htmlFor="dispatch" className="sr-only">
        Goal
      </label>
      <textarea
        id="dispatch"
        ref={setBox}
        rows={3}
        placeholder={GOAL_HELP}
        value={props.value}
        onChange={(e) => {
          setCaret(e.target.selectionStart ?? e.target.value.length);
          props.onChange(e.target.value);
        }}
        onSelect={(e) => setCaret(e.currentTarget.selectionStart ?? 0)}
        onFocus={() => setFocused(true)}
        onBlur={() => setFocused(false)}
        onKeyDown={onKey}
        aria-describedby={error ? "dispatch-error" : "dispatch-branch"}
        aria-autocomplete="list"
        aria-controls={sugg ? listId : undefined}
        aria-activedescendant={sugg ? optionId(active) : undefined}
      />
      {sugg && (
        <OptionList
          id={listId}
          label="Suggestions"
          items={sugg.items}
          active={active}
          optionId={optionId}
          onPick={pick}
        />
      )}
      <button className="btn btn-primary" type="submit" disabled={busy || !props.value.trim()}>
        Dispatch
      </button>
      <p id="dispatch-branch" className="small muted dispatch-branch">
        {power ? (
          <>Agent, repo and branch come from the @ line.</>
        ) : edited === null ? (
          <>
            Branch <code>{auto}</code>{" "}
            <button type="button" className="btn btn-small btn-ghost" aria-label="Edit branch" onClick={() => setEdited(auto)}>
              edit
            </button>
          </>
        ) : (
          <>
            <label htmlFor="dispatch-branch-name">Branch</label>{" "}
            <input id="dispatch-branch-name" value={edited} onChange={(e) => setEdited(e.target.value)} spellCheck={false} />{" "}
            <button type="button" className="btn btn-small btn-ghost" onClick={() => setEdited(null)}>
              use {auto}
            </button>
          </>
        )}
      </p>
      {error && (
        <p id="dispatch-error" className="error" role="alert">
          {error}
        </p>
      )}
      {originNote && (
        <p className={`small ${originNote.warn ? "warn" : "muted"}`} role="status">
          {originNote.text}
        </p>
      )}
    </form>
  );
}

/** A listbox under a field that keeps the keyboard (`aria-activedescendant` on the field).
 * Options are picked with a click; mousedown keeps the field's focus. */
function OptionList(props: {
  id: string;
  label: string;
  items: SuggestionItem[];
  active: number;
  optionId(i: number): string;
  onPick(i: number): void;
}) {
  return (
    <ul className="suggest" id={props.id} role="listbox" aria-label={props.label}>
      {props.items.map((it, i) => (
        <li
          key={it.label + i}
          id={props.optionId(i)}
          role="option"
          aria-selected={i === props.active}
          className={i === props.active ? "active" : undefined}
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => props.onPick(i)}
        >
          <span>{it.label}</span>
          {it.detail && <span className="muted small">{it.detail}</span>}
        </li>
      ))}
    </ul>
  );
}

/** The form's repository: a combobox over the Projects, filtered by folder name as typed. */
function RepoBox(props: { projects: Project[]; value: string | null; disabled: boolean; onPick(path: string): void }) {
  // null: not searching (the box shows the chosen project's name).
  const [query, setQuery] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  const id = useId();
  const listId = `${id}-list`;
  const optionId = (i: number) => `${id}-opt-${i}`;
  const chosen = props.projects.find((p) => p.path === props.value);
  const items = query === null ? [] : matchProjects(query, props.projects);
  const open = items.length > 0;
  const at = Math.min(active, Math.max(items.length - 1, 0));
  const pick = (i: number) => {
    props.onPick(items[i].path);
    setQuery(null);
  };
  const onKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      if (query === null) {
        setQuery("");
        setActive(0);
        return;
      }
      const n = items.length || 1;
      setActive((at + (e.key === "ArrowDown" ? 1 : n - 1)) % n);
    } else if (e.key === "Enter") {
      // Never submits the form from here.
      e.preventDefault();
      if (open) pick(at);
    } else if (e.key === "Tab" && open && !e.shiftKey) {
      pick(at);
    } else if (e.key === "Escape" && query !== null) {
      e.preventDefault();
      setQuery(null);
    }
  };
  return (
    <span className="repo-box">
      <label htmlFor={id} className="small muted">
        Repository
      </label>
      <input
        id={id}
        role="combobox"
        aria-expanded={open}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={open ? optionId(at) : undefined}
        title={chosen?.path}
        placeholder={props.projects.length === 0 ? "No projects yet" : "Search projects"}
        disabled={props.disabled || props.projects.length === 0}
        value={query ?? chosen?.name ?? ""}
        spellCheck={false}
        autoComplete="off"
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(0);
        }}
        onKeyDown={onKey}
        onBlur={() => setQuery(null)}
      />
      {open && <OptionList id={listId} label="Projects" items={items.map((p) => ({ label: p.name, detail: p.path, insert: p.path }))} active={at} optionId={optionId} onPick={pick} />}
    </span>
  );
}

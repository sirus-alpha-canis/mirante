import {
  type KeyboardEvent,
  type ReactNode,
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
  AGENT_OPTIONS,
  applySuggestion,
  branchFromGoal,
  branchProblem,
  type ContextRef,
  contextKey,
  GOAL_HELP,
  isPowerSyntax,
  matchProjects,
  mayHoldSecrets,
  nextFreeBranch,
  parseDispatch,
  resolveRepo,
  suggest,
  type SuggestionItem,
  withContext,
} from "../lib/dispatch";
import { type AgentKind, type CreatedOrigin, M, type NoteSummary, type SkillSummary, type StartResult, type TaskView } from "../lib/protocol";
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
    const v = store.getItem(AGENT_KEY);
    return AGENT_OPTIONS.find((a) => a.id === v)?.id ?? "claude";
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

const agentLabel = (a: AgentKind) => AGENT_OPTIONS.find((o) => o.id === a)?.label ?? a;

/**
 * The dispatch form, one rounded box: the goal on top, a toolbar below it with the agent
 * picker, the repository chip (searchable), the branch chip (once there is a goal: named
 * after it, editable), "+ contexto" (attach repo files, memory notes and skills: they go
 * after the goal as labeled references), a memory hint while new sessions wait, and send
 * (⌘↵). Text that starts with "@claude" / "@codex" is the one-line syntax
 * ("@claude shop/feat/x goal"), with suggestions as it is typed.
 */
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
  /** The memory guard pauses new sessions (`daemon.status.memory.paused`). */
  paused?: boolean;
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

  // "+ contexto": what is attached to this goal (cleared with it once dispatched).
  const [refs, setRefs] = useState<ContextRef[]>([]);
  const attach = (r: ContextRef) => setRefs((cur) => (cur.some((x) => contextKey(x) === contextKey(r)) ? cur : [...cur, r]));
  const detach = (key: string) => setRefs((cur) => cur.filter((x) => contextKey(x) !== key));

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
    const sentRefs = refs;
    let target: { repo: string; agent: AgentKind; branch: string; goal: string };
    let renames = false;
    if (power) {
      const p = parseDispatch(sent);
      if (!p.ok) return setError(p.error);
      const r = resolveRepo(p.value.repo, props.projects);
      if (!r.ok) return setError(r.error);
      // Files and notes were picked in the form's repo: elsewhere they name nothing (or
      // something else).
      const local = sentRefs.filter((x) => x.kind !== "skill");
      if (local.length > 0 && r.path !== repo) {
        const from = props.projects.find((x) => x.path === repo)?.name ?? repo ?? "another repo";
        return setError(`The attached files and notes are from ${from}, not ${p.value.repo}: remove them, or dispatch to ${from}.`);
      }
      // "shop/" with no branch: named from the goal, like the form's.
      renames = p.value.branch === "";
      const b = renames ? nextFreeBranch(branchFromGoal(p.value.goal), takenIn(r.path)) : p.value.branch;
      target = { repo: r.path, agent: p.value.agent, branch: b, goal: withContext(p.value.goal, sentRefs) };
    } else {
      const goal = sent.trim();
      if (!goal) return setError("say what the agent should do");
      if (!repo) return setError("Add a repository under Projects first.");
      const bad = branchProblem(branch);
      if (bad) return setError(bad);
      target = { repo, agent: props.agent, branch, goal: withContext(goal, sentRefs) };
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
        setRefs([]);
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

  const hasGoal = props.value.trim().length > 0;
  return (
    <form
      className="composer dispatch"
      aria-label="Dispatch an agent"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className="composer-box">
        {refs.length > 0 && (
          <ul className="attached" aria-label="Attached context">
            {refs.map((r) => (
              <li key={contextKey(r)} className="attached-chip">
                <span className="muted small">{r.kind === "file" ? "file" : r.kind === "note" ? "note" : "skill"}</span>{" "}
                <span>{r.kind === "file" ? r.path : r.kind === "note" ? r.title : r.name}</span>
                <button
                  type="button"
                  className="btn btn-small btn-ghost"
                  aria-label={`Remove ${r.kind === "file" ? r.path : r.kind === "note" ? r.title : r.name}`}
                  onClick={() => detach(contextKey(r))}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
        <label htmlFor="dispatch" className="sr-only">
          Goal
        </label>
        <textarea
          id="dispatch"
          ref={setBox}
          rows={2}
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
        {sugg && <OptionList id={listId} label="Suggestions" items={sugg.items} active={active} optionId={optionId} onPick={pick} />}
        <div className="composer-toolbar" role="group" aria-label="Dispatch options">
          <AgentPicker value={props.agent} disabled={power} onPick={props.onAgent} />
          <RepoBox projects={props.projects} value={repo} disabled={power} onPick={setPicked} />
          <span id="dispatch-branch" className="small muted dispatch-branch">
            {power ? (
              <>Agent, repo and branch come from the @ line.</>
            ) : !hasGoal ? null : edited === null ? (
              <span className="tchip tchip-static">
                <span aria-hidden="true">⎇</span> <code>{auto}</code>{" "}
                <button type="button" className="btn btn-small btn-ghost" aria-label="Edit branch" onClick={() => setEdited(auto)}>
                  edit
                </button>
              </span>
            ) : (
              <span className="tchip tchip-static">
                <label htmlFor="dispatch-branch-name">Branch</label>{" "}
                <input id="dispatch-branch-name" value={edited} onChange={(e) => setEdited(e.target.value)} spellCheck={false} />{" "}
                <button type="button" className="btn btn-small btn-ghost" onClick={() => setEdited(null)}>
                  use {auto}
                </button>
              </span>
            )}
          </span>
          <ContextPicker repo={power ? null : repo} onAttach={attach} attached={refs} />
          <span className="toolbar-spacer" />
          {props.paused && (
            <span className="small warn" role="status">
              Memory is tight: new sessions wait until it eases
            </span>
          )}
          <button className="btn btn-primary" type="submit" disabled={busy || !hasGoal} title="Dispatch (⌘↵)">
            Dispatch
          </button>
        </div>
      </div>
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

/** A popover opened by a toolbar button: Escape (or Tab out) closes it and the keyboard goes
 * back to the button. */
function Popover(props: { label: string; open: boolean; onClose(): void; trigger: ReactNode; children: ReactNode }) {
  const wrap = useRef<HTMLSpanElement>(null);
  return (
    <span
      className="popover-wrap"
      ref={wrap}
      onKeyDown={(e) => {
        if (e.key === "Escape" && props.open) {
          e.preventDefault();
          e.stopPropagation();
          props.onClose();
          wrap.current?.querySelector<HTMLButtonElement>("button")?.focus();
        }
      }}
      onBlur={(e) => {
        if (props.open && !wrap.current?.contains(e.relatedTarget as Node | null)) props.onClose();
      }}
    >
      {props.trigger}
      {props.open && (
        <div className="popover" role="dialog" aria-label={props.label}>
          {props.children}
        </div>
      )}
    </span>
  );
}

/** Which agent runs the task: a menu (Claude, Codex; more agents join `AGENT_OPTIONS`). */
function AgentPicker({ value, disabled, onPick }: { value: AgentKind; disabled: boolean; onPick(a: AgentKind): void }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const choose = (a: AgentKind) => {
    onPick(a);
    setOpen(false);
  };
  return (
    <Popover
      label="Agent"
      open={open}
      onClose={() => setOpen(false)}
      trigger={
        <button
          type="button"
          className="tchip"
          aria-haspopup="dialog"
          aria-expanded={open}
          aria-controls={open ? id : undefined}
          disabled={disabled}
          onClick={() => setOpen((o) => !o)}
          title="Agent and model"
        >
          Agent: {agentLabel(value)} <span aria-hidden="true">▾</span>
        </button>
      }
    >
      <ul id={id} className="menu" role="group" aria-label="Agents">
        {AGENT_OPTIONS.map((o) => (
          <li key={o.id}>
            <button type="button" className="menu-item" aria-pressed={o.id === value} onClick={() => choose(o.id)}>
              <span>{o.label}</span> <span className="muted small">{o.model}</span>
            </button>
          </li>
        ))}
      </ul>
    </Popover>
  );
}

type ContextTab = "files" | "notes" | "skills";
const CONTEXT_TABS: { id: ContextTab; label: string }[] = [
  { id: "files", label: "Files" },
  { id: "notes", label: "Memory notes" },
  { id: "skills", label: "Skills" },
];

/** "+ contexto": search the repo's files (`repo.files`), its memory notes (`dna.search`) or
 * the skills (`skill.list`) and attach one. Names only: nothing is read here. */
function ContextPicker({ repo, onAttach, attached }: { repo: string | null; onAttach(r: ContextRef): void; attached: ContextRef[] }) {
  const { rpc } = useEnv();
  const [open, setOpen] = useState(false);
  const [tab, setTab] = useState<ContextTab>("files");
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<{ key: string; ref: ContextRef; label: string; detail?: string }[]>([]);
  const [note, setNote] = useState<string | null>(null);
  const searchId = useId();
  const search = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!open) return;
    let live = true;
    const q = query.trim();
    const run = async () => {
      if (tab === "files") {
        if (!repo) return { list: [], note: "Pick a repository first." };
        const files = (await rpc<string[]>(M.repoFiles, { repo, query: q, limit: 20 })) ?? [];
        return {
          list: files.map((path) => ({
            key: `file:${path}`,
            ref: { kind: "file", path } as ContextRef,
            label: path,
            detail: mayHoldSecrets(path) ? "⚠ may hold secrets" : undefined,
          })),
          note: files.length ? null : "No matching file.",
        };
      }
      if (tab === "notes") {
        if (!repo) return { list: [], note: "Pick a repository first." };
        if (!q) return { list: [], note: "Type to search the repo's memory." };
        const notes = (await rpc<NoteSummary[]>(M.dnaSearch, { repo, query: q, limit: 10 })) ?? [];
        return {
          list: notes.map((n) => ({ key: `note:${n.slug}`, ref: { kind: "note", slug: n.slug, title: n.title } as ContextRef, label: n.title, detail: n.slug })),
          note: notes.length ? null : "No matching note.",
        };
      }
      const skills = ((await rpc<SkillSummary[]>(M.skillList, { status: "active" })) ?? []).filter(
        (s) => !q || s.name.toLowerCase().includes(q.toLowerCase()) || s.description.toLowerCase().includes(q.toLowerCase()),
      );
      return {
        list: skills.slice(0, 20).map((s) => ({ key: `skill:${s.name}`, ref: { kind: "skill", name: s.name, description: s.description } as ContextRef, label: s.name, detail: s.description })),
        note: skills.length ? null : "No matching skill.",
      };
    };
    run()
      .then((r) => {
        if (!live) return;
        setHits(r.list);
        setNote(r.note);
      })
      .catch((e) => {
        if (!live) return;
        setHits([]);
        setNote(e instanceof Error ? e.message : String(e));
      });
    return () => {
      live = false;
    };
  }, [open, tab, query, repo, rpc]);
  const taken = new Set(attached.map(contextKey));
  return (
    <Popover
      label="Add context"
      open={open}
      onClose={() => setOpen(false)}
      trigger={
        <button type="button" className="tchip" aria-haspopup="dialog" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
          + contexto
        </button>
      }
    >
      <div className="seg" role="group" aria-label="Kind of context">
        {CONTEXT_TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            className="btn btn-small"
            aria-pressed={tab === t.id}
            onClick={() => {
              setTab(t.id);
              setQuery("");
            }}
          >
            {t.label}
          </button>
        ))}
      </div>
      <label htmlFor={searchId} className="sr-only">
        Search {CONTEXT_TABS.find((t) => t.id === tab)?.label}
      </label>
      <input
        id={searchId}
        ref={search}
        className="popover-search"
        placeholder={tab === "files" ? "Search files by path" : tab === "notes" ? "Search memory notes" : "Search skills"}
        value={query}
        autoFocus
        spellCheck={false}
        onChange={(e) => setQuery(e.target.value)}
      />
      {note && <p className="small muted">{note}</p>}
      <ul className="menu" aria-label="Results">
        {hits.map((h) => (
          <li key={h.key}>
            <button
              type="button"
              className="menu-item"
              disabled={taken.has(h.key)}
              onClick={() => {
                onAttach(h.ref);
                // The button turns off once attached: the keyboard stays in the search.
                search.current?.focus();
              }}
            >
              <span>{h.label}</span>
              {h.detail && <span className="muted small">{h.detail}</span>}
              {taken.has(h.key) && <span className="muted small">attached</span>}
            </button>
          </li>
        ))}
      </ul>
    </Popover>
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
    <span className="repo-box tchip tchip-static">
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
      {open && (
        <OptionList
          id={listId}
          label="Projects"
          items={items.map((p) => ({ label: p.name, detail: p.path, insert: p.path }))}
          active={at}
          optionId={optionId}
          onPick={pick}
        />
      )}
    </span>
  );
}

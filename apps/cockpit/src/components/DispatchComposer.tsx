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
import { shallow, useEnv, useLive, useSelect } from "../env";
import { hintsFor, onMac } from "../lib/shortcuts";
import { useDismiss } from "../lib/useDismiss";
import { type Key, t, useT } from "../lib/i18n";
import { DaemonError } from "../lib/daemon";
import {
  AGENT_OPTIONS,
  applySuggestion,
  branchFromGoal,
  branchProblem,
  type ContextRef,
  contextKey,
  foreignRepo,
  goalHelp,
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
import { type AgentKind, type CreatedOrigin, M, type NoteSummary, RpcCode, type SkillSummary, type StartResult, type TaskView } from "../lib/protocol";
import type { Project } from "../store/select";
import type { State } from "../store/state";

/** What the form holds besides the goal: attached context, an edited branch, a repo picked
 * here. The app keeps it (with the goal), so leaving Home and coming back loses nothing. */
export interface ComposerForm {
  refs: ContextRef[];
  /** The branch as typed (null: named after the goal). */
  edited: string | null;
  /** The repo picked in the form (null: the rail's selection). */
  picked: string | null;
}

export const EMPTY_FORM: ComposerForm = { refs: [], edited: null, picked: null };

/** The branches `repo`'s tasks use, sorted (a list compared shallowly: a task event that
 * changes no branch renders nothing). */
function branchesIn(s: State, repo: string | null): string[] {
  if (!repo) return [];
  const out: string[] = [];
  for (const t of Object.values(s.tasks)) if (t.repo_path === repo) out.push(t.branch);
  return out.sort();
}

/** git refused the branch because it exists (`task.create` with a base). */
function branchExists(e: unknown): boolean {
  return e instanceof DaemonError && e.code === RpcCode.INVALID_PARAMS && /branch already exists/.test(e.message);
}

/** What `task.create` fixed for the new task's skills, in a person's words. */
export function originText(branch: string, o: CreatedOrigin): { text: string; warn: boolean } {
  if (o.observed) {
    return { text: t("dispatch.originUnconfirmed", { branch, origin: o.origin, observed: o.observed }), warn: true };
  }
  return { text: t(o.pinned ? "dispatch.originPinned" : "dispatch.origin", { branch, origin: o.origin, client: o.tenant }), warn: false };
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
  /** The rest of the form, when the caller keeps it (else the form keeps its own). */
  form?: ComposerForm;
  onForm?(update: (cur: ComposerForm) => ComposerForm): void;
}) {
  const { rpc, cockpit } = useEnv();
  const t = useT();
  const live = useLive();
  const hints = useMemo(() => hintsFor(onMac()), []);
  const [ownForm, setOwnForm] = useState<ComposerForm>(EMPTY_FORM);
  const form = props.form ?? ownForm;
  const setForm = props.onForm ?? setOwnForm;
  const [error, setError] = useState<string | null>(null);
  const [originNote, setOriginNote] = useState<{ text: string; warn: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  // A ref, not `busy`: a second ⌘↵ (or key auto-repeat) arrives before React re-renders.
  const inFlight = useRef(false);
  // The draft as it is now, for the moment the create returns (the user may keep typing).
  const latest = useRef(props.value);
  latest.current = props.value;

  // The repo: picked here, else the rail's selection, else the first project. A new rail
  // selection takes over from what was picked here.
  const picked = form.picked;
  const setPicked = (p: string | null) => setForm((f) => ({ ...f, picked: p }));
  const railWas = useRef(props.project);
  useEffect(() => {
    if (railWas.current === props.project) return;
    railWas.current = props.project;
    setForm((f) => (f.picked === null ? f : { ...f, picked: null }));
  }, [props.project, setForm]);
  const has = (p: string | null) => !!p && props.projects.some((x) => x.path === p);
  const repo = has(picked) ? picked : has(props.project) ? props.project : (props.projects[0]?.path ?? null);

  // The branch: named after the goal, past the names this repo's tasks already use, unless edited.
  const edited = form.edited;
  const setEdited = (b: string | null) => setForm((f) => ({ ...f, edited: b }));
  const power = isPowerSyntax(props.value);
  const takenIn = (path: string | null) => new Set(branchesIn(cockpit.getState(), path));
  const selectBranches = useCallback((s: State) => branchesIn(s, repo), [repo]);
  const branches = useSelect(selectBranches, shallow);
  const taken = useMemo(() => new Set(branches), [branches]);
  const auto = nextFreeBranch(branchFromGoal(props.value), taken);
  const branch = edited ?? auto;

  // "+ contexto": what is attached to this goal (cleared with it once dispatched).
  const refs = form.refs;
  const attach = (r: ContextRef) => setForm((f) => (f.refs.some((x) => contextKey(x) === contextKey(r)) ? f : { ...f, refs: [...f.refs, r] }));
  const detach = (key: string) => setForm((f) => ({ ...f, refs: f.refs.filter((x) => contextKey(x) !== key) }));
  const repoLabel = (path: string | null) => props.projects.find((x) => x.path === path)?.name ?? path ?? t("dispatch.anotherRepo");

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
        if (n >= MAX_RENAMES || !branchExists(e)) throw e;
        tried.add(b);
        b = nextFreeBranch(branchFromGoal(target.goal), tried);
      }
    }
  };

  const submit = async () => {
    if (inFlight.current || !live) return;
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
      const other = foreignRepo(sentRefs, r.path);
      if (other) {
        const from = repoLabel(other);
        return setError(t("dispatch.foreignRefs", { from, to: p.value.repo }));
      }
      // "shop/" with no branch: named from the goal, like the form's.
      renames = p.value.branch === "";
      const b = renames ? nextFreeBranch(branchFromGoal(p.value.goal), takenIn(r.path)) : p.value.branch;
      target = { repo: r.path, agent: p.value.agent, branch: b, goal: withContext(p.value.goal, sentRefs) };
    } else {
      const goal = sent.trim();
      if (!goal) return setError(t("dispatch.noGoal"));
      if (!repo) return setError(t("dispatch.noRepo"));
      const bad = branchProblem(branch);
      if (bad) return setError(bad);
      const other = foreignRepo(sentRefs, repo);
      if (other) {
        const from = repoLabel(other);
        return setError(t("dispatch.foreignRefs", { from, to: repoLabel(repo) }));
      }
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
        setForm((f) => ({ ...f, edited: null, refs: [] }));
      }
      props.onDispatched?.(v.id);
      // Boot can take up to 45 s; progress arrives as events, so do not wait here.
      rpc<StartResult>(M.sessionStart, { task: v.id, prompt: target.goal }).catch((e) =>
        setError(t("dispatch.notStarted", { branch: made, error: e instanceof Error ? e.message : String(e) })),
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
      aria-label={t("dispatch.form")}
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className="composer-box">
        {refs.length > 0 && (
          <ul className="attached" aria-label={t("dispatch.attached")}>
            {refs.map((r) => (
              <li key={contextKey(r)} className="attached-chip">
                <span className="muted small">{t(r.kind === "file" ? "context.kindFile" : r.kind === "note" ? "context.kindNote" : "context.kindSkill")}</span>{" "}
                <span>{r.kind === "file" ? r.path : r.kind === "note" ? r.title : r.name}</span>
                <button
                  type="button"
                  className="btn btn-small btn-ghost"
                  aria-label={t("dispatch.remove", { name: r.kind === "file" ? r.path : r.kind === "note" ? r.title : r.name })}
                  onClick={() => detach(contextKey(r))}
                >
                  ×
                </button>
              </li>
            ))}
          </ul>
        )}
        <label htmlFor="dispatch" className="sr-only">
          {t("dispatch.goal")}
        </label>
        <textarea
          id="dispatch"
          ref={setBox}
          rows={2}
          placeholder={goalHelp(hints.send)}
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
        {sugg && <OptionList id={listId} label={t("dispatch.suggestions")} items={sugg.items} active={active} optionId={optionId} onPick={pick} />}
        <div className="composer-toolbar" role="group" aria-label={t("dispatch.options")}>
          <AgentPicker value={props.agent} disabled={power} onPick={props.onAgent} />
          <RepoBox projects={props.projects} value={repo} disabled={power} onPick={setPicked} />
          <span id="dispatch-branch" className="small muted dispatch-branch">
            {power ? (
              <>{t("dispatch.fromAtLine")}</>
            ) : !hasGoal ? null : edited === null ? (
              <span className="tchip tchip-static">
                <span aria-hidden="true">⎇</span> <code>{auto}</code>{" "}
                <button type="button" className="btn btn-small btn-ghost" aria-label={t("dispatch.editBranch")} onClick={() => setEdited(auto)}>
                  {t("dispatch.edit")}
                </button>
              </span>
            ) : (
              <span className="tchip tchip-static">
                <label htmlFor="dispatch-branch-name">{t("dispatch.branch")}</label>{" "}
                <input id="dispatch-branch-name" value={edited} onChange={(e) => setEdited(e.target.value)} spellCheck={false} />{" "}
                <button type="button" className="btn btn-small btn-ghost" onClick={() => setEdited(null)}>
                  {t("dispatch.useAuto", { branch: auto })}
                </button>
              </span>
            )}
          </span>
          <ContextPicker repo={power ? null : repo} onAttach={attach} attached={refs} />
          <span className="toolbar-spacer" />
          {props.paused && (
            <span className="small warn" role="status">
              {t("dispatch.paused")}
            </span>
          )}
          <button
            className="btn btn-primary"
            type="submit"
            disabled={busy || !hasGoal || !live}
            title={live ? t("dispatch.sendTitle", { key: hints.send }) : t("dispatch.offline")}
          >
            {t("dispatch.send")}
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

/** A popover opened by a toolbar button. It closes like every menu here (`useDismiss`): a
 * press outside, Escape (the keyboard goes back to the button), focus leaving it, or the window
 * losing focus. */
function Popover(props: { label: string; open: boolean; onClose(): void; trigger: ReactNode; children: ReactNode }) {
  const wrap = useRef<HTMLSpanElement>(null);
  const { onClose } = props;
  useDismiss(
    wrap,
    (why) => {
      onClose();
      if (why === "escape") wrap.current?.querySelector<HTMLButtonElement>("button")?.focus();
    },
    { enabled: props.open },
  );
  return (
    <span className="popover-wrap" ref={wrap}>
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
  const t = useT();
  const [open, setOpen] = useState(false);
  const id = useId();
  const choose = (a: AgentKind) => {
    onPick(a);
    setOpen(false);
  };
  return (
    <Popover
      label={t("dispatch.agent")}
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
          title={t("dispatch.agentTitle")}
        >
          {t("dispatch.agentChip", { agent: agentLabel(value) })} <span aria-hidden="true">▾</span>
        </button>
      }
    >
      <ul id={id} className="menu" role="group" aria-label={t("dispatch.agents")}>
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
/** A file or note search goes out once typing has paused this long. */
export const CONTEXT_DEBOUNCE_MS = 150;
const CONTEXT_TABS: { id: ContextTab; label: Key }[] = [
  { id: "files", label: "context.files" },
  { id: "notes", label: "context.notes" },
  { id: "skills", label: "context.skills" },
];

/** "+ contexto": search the repo's files (`repo.files`), its memory notes (`dna.search`) or
 * the skills (`skill.list`) and attach one. Names only: nothing is read here. */
function ContextPicker({ repo, onAttach, attached }: { repo: string | null; onAttach(r: ContextRef): void; attached: ContextRef[] }) {
  const { rpc } = useEnv();
  const t = useT();
  const [open, setOpen] = useState(false);
  const [tab, setTab] = useState<ContextTab>("files");
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<{ key: string; ref: ContextRef; label: string; detail?: string }[]>([]);
  const [note, setNote] = useState<string | null>(null);
  // The active skills, asked once per opening (filtered here as the person types).
  const skills = useRef<Promise<SkillSummary[]> | null>(null);
  useEffect(() => {
    if (!open) skills.current = null;
  }, [open]);
  // What was last searched: a new query waits for typing to settle; opening, a tab or a
  // repo search at once.
  const searched = useRef<{ tab: ContextTab; repo: string | null; open: boolean } | null>(null);
  const searchId = useId();
  const search = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!open) {
      searched.current = null;
      return;
    }
    let live = true;
    const q = query.trim();
    const run = async () => {
      if (tab === "files") {
        if (!repo) return { list: [], note: t("context.pickRepo") };
        const files = (await rpc<string[]>(M.repoFiles, { repo, query: q, limit: 20 })) ?? [];
        return {
          list: files.map((path) => ({
            key: `file:${path}`,
            ref: { kind: "file", path, repo } as ContextRef,
            label: path,
            detail: mayHoldSecrets(path) ? t("context.secrets") : undefined,
          })),
          note: files.length ? null : t("context.noFile"),
        };
      }
      if (tab === "notes") {
        if (!repo) return { list: [], note: t("context.pickRepo") };
        if (!q) return { list: [], note: t("context.typeNotes") };
        const notes = (await rpc<NoteSummary[]>(M.dnaSearch, { repo, query: q, limit: 10 })) ?? [];
        return {
          list: notes.map((n) => ({ key: `note:${n.slug}`, ref: { kind: "note", slug: n.slug, title: n.title, repo } as ContextRef, label: n.title, detail: n.slug })),
          note: notes.length ? null : t("context.noNote"),
        };
      }
      if (!skills.current) {
        skills.current = rpc<SkillSummary[]>(M.skillList, { status: "active" }).then((l) => l ?? []);
        // A failed list is asked again next time.
        skills.current.catch(() => {
          skills.current = null;
        });
      }
      const needle = q.toLowerCase();
      const found = (await skills.current).filter((x) => !needle || x.name.toLowerCase().includes(needle) || x.description.toLowerCase().includes(needle));
      return {
        list: found.slice(0, 20).map((x) => ({ key: `skill:${x.name}`, ref: { kind: "skill", name: x.name, description: x.description } as ContextRef, label: x.name, detail: x.description })),
        note: found.length ? null : t("context.noSkill"),
      };
    };
    const go = () =>
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
    const prev = searched.current;
    searched.current = { tab, repo, open };
    // Only the daemon's searches wait (skills filter here).
    const typed = prev !== null && prev.tab === tab && prev.repo === repo && tab !== "skills";
    let timer: ReturnType<typeof setTimeout> | null = null;
    if (typed) timer = setTimeout(go, CONTEXT_DEBOUNCE_MS);
    else void go();
    return () => {
      live = false;
      if (timer !== null) clearTimeout(timer);
    };
  }, [open, tab, query, repo, rpc]);
  const taken = new Set(attached.map(contextKey));
  return (
    <Popover
      label={t("context.dialog")}
      open={open}
      onClose={() => setOpen(false)}
      trigger={
        <button type="button" className="tchip" aria-haspopup="dialog" aria-expanded={open} onClick={() => setOpen((o) => !o)}>
          {t("context.button")}
        </button>
      }
    >
      <div className="seg" role="group" aria-label={t("context.kind")}>
        {CONTEXT_TABS.map((c) => (
          <button
            key={c.id}
            type="button"
            className="btn btn-small"
            aria-pressed={tab === c.id}
            onClick={() => {
              setTab(c.id);
              setQuery("");
            }}
          >
            {t(c.label)}
          </button>
        ))}
      </div>
      <label htmlFor={searchId} className="sr-only">
        {t("context.search", { what: t(CONTEXT_TABS.find((c) => c.id === tab)!.label) })}
      </label>
      <input
        id={searchId}
        ref={search}
        className="popover-search"
        placeholder={t(tab === "files" ? "context.searchFiles" : tab === "notes" ? "context.searchNotes" : "context.searchSkills")}
        value={query}
        autoFocus
        spellCheck={false}
        onChange={(e) => setQuery(e.target.value)}
      />
      {note && <p className="small muted">{note}</p>}
      <ul className="menu" aria-label={t("context.results")}>
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
              {taken.has(h.key) && <span className="muted small">{t("context.attachedMark")}</span>}
            </button>
          </li>
        ))}
      </ul>
    </Popover>
  );
}

/** The form's repository: a combobox over the Projects, filtered by folder name as typed. */
function RepoBox(props: { projects: Project[]; value: string | null; disabled: boolean; onPick(path: string): void }) {
  const t = useT();
  // null: not searching (the box shows the chosen project's name).
  const [query, setQuery] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  const id = useId();
  const listId = `${id}-list`;
  const optionId = (i: number) => `${id}-opt-${i}`;
  const chosen = props.projects.find((p) => p.path === props.value);
  const items = query === null ? [] : matchProjects(query, props.projects);
  const open = items.length > 0;
  const wrap = useRef<HTMLSpanElement>(null);
  // The list closes like every menu here; the box shows the chosen project again.
  useDismiss(wrap, () => setQuery(null), { enabled: open });
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
    <span className="repo-box tchip tchip-static" ref={wrap}>
      <label htmlFor={id} className="small muted">
        {t("dispatch.repository")}
      </label>
      <input
        id={id}
        role="combobox"
        aria-expanded={open}
        aria-controls={listId}
        aria-autocomplete="list"
        aria-activedescendant={open ? optionId(at) : undefined}
        title={chosen?.path}
        placeholder={t(props.projects.length === 0 ? "dispatch.noProjects" : "dispatch.searchProjects")}
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
          label={t("dispatch.projects")}
          items={items.map((p) => ({ label: p.name, detail: p.path, insert: p.path }))}
          active={at}
          optionId={optionId}
          onPick={pick}
        />
      )}
    </span>
  );
}

import { type KeyboardEvent, type Ref, useEffect, useMemo, useRef, useState } from "react";
import { useEnv, useStore } from "../env";
import {
  branchFromGoal,
  branchProblem,
  GOAL_HELP,
  isPowerSyntax,
  nextFreeBranch,
  parseDispatch,
  resolveRepo,
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
  const s = useStore();
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
  const taken = useMemo(
    () => new Set(Object.values(s.tasks).filter((t) => t.repo_path === repo).map((t) => t.branch)),
    [s.tasks, repo],
  );
  const auto = nextFreeBranch(branchFromGoal(props.value), taken);
  const branch = edited ?? auto;

  const pickAgent = (a: AgentKind) => props.onAgent(a);

  /** task.create; an auto-named branch git already has moves on to -2, -3… */
  const create = async (target: { repo: string; agent: AgentKind; branch: string; goal: string }, renames: boolean) => {
    if (!renames) return { v: await rpc<TaskView>(M.taskCreate, target), branch: target.branch };
    const tried = new Set(taken);
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
      target = { repo: r.path, agent: p.value.agent, branch: p.value.branch, goal: p.value.goal };
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

  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    // Enter that confirms an IME composition (Japanese, Chinese…) is not a send.
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
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
        <label htmlFor="dispatch-repo" className="small muted">
          Repository
        </label>
        <select
          id="dispatch-repo"
          value={repo ?? ""}
          disabled={power || props.projects.length === 0}
          onChange={(e) => setPicked(e.target.value)}
        >
          {props.projects.length === 0 && <option value="">No projects yet</option>}
          {props.projects.map((p) => (
            <option key={p.path} value={p.path} title={p.path}>
              {p.name}
            </option>
          ))}
        </select>
      </div>
      <label htmlFor="dispatch" className="sr-only">
        Goal
      </label>
      <textarea
        id="dispatch"
        ref={props.inputRef}
        rows={3}
        placeholder={GOAL_HELP}
        value={props.value}
        onChange={(e) => props.onChange(e.target.value)}
        onKeyDown={onKey}
        aria-describedby={error ? "dispatch-error" : "dispatch-branch"}
      />
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

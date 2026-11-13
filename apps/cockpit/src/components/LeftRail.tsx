import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useDismiss } from "../lib/useDismiss";
import { shallow, useEnv, useSelect } from "../env";
import { AGENT_OPTIONS } from "../lib/dispatch";
import { same, type State } from "../store/state";
import { dnaStatus } from "../lib/dna";
import { type DaemonSettings, M, type PausePolicy, type SummariesSetting as SummariesSettingValue } from "../lib/protocol";
import { usePendingSkills } from "../lib/useSkills";
import type { AgentKind, DaemonStatus } from "../lib/protocol";
import { addRepo, addRepos, expandHome, loadRegistry, normaliseRepoPath, type Registry, showRepos } from "../lib/repos";
import type { RepoScan } from "../lib/protocol";
import { columnLabel, needsYou, type Project, repoName } from "../store/select";
import { ProjectList } from "./ProjectList";
import { ScanPicker } from "./ScanPicker";
import { inboxOf } from "../lib/inbox";
import { learnDnaKey, useDnaRev } from "../lib/useDna";
import { MemoryPanel } from "./MemoryPanel";
import { Disclosure } from "./Disclosure";
import { type Key, useT } from "../lib/i18n";

const selectColumns = (s: State) => inboxOf(s).columns;

/** What the rail shows of a live session. */
interface LiveSession {
  id: string;
  agent: AgentKind;
  branch: string;
  repo_path: string;
  title: string;
  needsYou: boolean;
}

/** Live sessions, oldest first: only what the rail shows (compared by value, so a task's
 * other changes render nothing here). */
function selectLive(s: State): LiveSession[] {
  return Object.values(s.tasks)
    .filter((t) => t.live)
    .sort((x, y) => x.created_at - y.created_at)
    .map((t) => ({ id: t.id, agent: t.agent, branch: t.branch, repo_path: t.repo_path, title: t.title, needsYou: needsYou(t) }));
}

export function LeftRail(props: {
  /** Every project, hidden ones too (marked). */
  projects: Project[];
  /** What the rail remembers (read from storage when not given). */
  registry?: Registry;
  selected: string | null;
  onSelect(path: string): void;
  onReposChanged(registry: Registry): void;
  /** Selects the agent in the dispatch form and focuses its goal. */
  onMention(agent: AgentKind): void;
  /** Opens a task's detail (`"Terminal"` when it needs the person). */
  onOpen(taskId: string, tab?: "Terminal"): void;
  /** Opens the memory review queue of a project. */
  onReview?(path: string): void;
  /** Opens the Skills view. */
  onSkills?(): void;
  status: DaemonStatus | null;
}) {
  const { storage, rpc, cockpit, pickFolder, homeDir, pathKind, scanRepos } = useEnv();
  const tr = useT();
  // Live sessions per agent (the task objects: kept while unchanged, so a step elsewhere
  // renders nothing here).
  const liveTasks = useSelect(selectLive, same);
  const columns = useSelect(selectColumns, shallow);
  // The selected project's memory proposals waiting for a person (none when its memory is
  // off, the daemon is older, or the path is no repo).
  const sel = props.selected;
  const version = useDnaRev(sel);
  const [pending, setPending] = useState<{ path: string; n: number } | null>(null);
  useEffect(() => {
    if (!sel) return;
    let live = true;
    dnaStatus(rpc, sel)
      .then((r) => {
        if (r.available) learnDnaKey(cockpit, sel, r.status);
        if (live) setPending({ path: sel, n: r.available && r.status?.enabled ? (r.status.pending ?? 0) : 0 });
      })
      .catch(() => live && setPending({ path: sel, n: 0 }));
    return () => {
      live = false;
    };
  }, [rpc, cockpit, sel, version]);
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);
  const agents = useMemo(
    () => AGENT_OPTIONS.map((o) => ({ agent: o.id, live: liveTasks.filter((t) => t.agent === o.id) })),
    [liveTasks],
  );
  // The agent whose live sessions are listed to pick from (it has several).
  const [picking, setPicking] = useState<AgentKind | null>(null);
  // The open list (with its agent's button) closes like every menu here; after Escape the
  // keyboard goes back to the agent.
  const pickWrap = useRef<HTMLLIElement>(null);
  useDismiss(
    pickWrap,
    (why) => {
      setPicking(null);
      if (why === "escape") pickWrap.current?.querySelector<HTMLButtonElement>("button")?.focus();
    },
    { enabled: picking !== null },
  );
  const openTask = (t: LiveSession) => {
    setPicking(null);
    props.onOpen(t.id, t.needsYou ? "Terminal" : undefined);
  };
  /** One live session: open it. Several: list them. None: write it a new task. */
  const agentClick = (a: { agent: AgentKind; live: LiveSession[] }) => {
    if (a.live.length === 1) return openTask(a.live[0]);
    if (a.live.length > 1) return setPicking((p) => (p === a.agent ? null : a.agent));
    setPicking(null);
    props.onMention(a.agent);
  };

  const registry = props.registry ?? loadRegistry(storage);
  // The repositories found in a folder given to Add project, to pick from.
  const [picker, setPicker] = useState<{ folder: string; scan: RepoScan } | null>(null);
  const [scanning, setScanning] = useState<string | null>(null);
  /** Looks for the repositories in `folder` and lists them to pick from. */
  const scan = async (folder: string) => {
    setScanning(folder);
    try {
      const found = await scanRepos(folder);
      setPicker({ folder, scan: found });
      return found;
    } finally {
      setScanning(null);
    }
  };

  /** Adds a typed or picked folder: "~/…" expanded with the app's home, then it must be a
   * folder. A git checkout is added; another folder is looked into, and the repositories
   * found in it are listed to pick from. */
  const add = async (raw: string, typed: boolean) => {
    try {
      const text = raw.trim();
      const expanded = text === "~" || text.startsWith("~/") ? expandHome(text, await homeDir()) : text;
      const n = normaliseRepoPath(expanded);
      if (!n.ok) return setError(n.error);
      const kind = await pathKind(n.path);
      if (kind === "missing") return setError(tr("rail.missing", { path: n.path }));
      if (kind === "file") return setError(tr("rail.isFile", { path: n.path }));
      setPicker(null);
      setScanning(n.path);
      let found: RepoScan;
      try {
        found = await scanRepos(n.path);
      } finally {
        setScanning(null);
      }
      setError(null);
      if (found.root === null) {
        setPicker({ folder: n.path, scan: found });
        return;
      }
      const r = addRepo(storage, n.path);
      if (!r.ok) return setError(r.error);
      if (typed) setPath("");
      props.onReposChanged(showRepos(storage, [n.path]));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  const addPicked = (paths: string[]) => {
    if (!picker) return;
    props.onReposChanged(addRepos(storage, paths, picker.folder));
    setPicker(null);
    setPath("");
  };
  const choose = async () => {
    try {
      const picked = await pickFolder();
      if (picked) await add(picked, false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };
  return (
    <nav className="rail" aria-label={tr("rail.aria")}>
      <ProjectList
        projects={props.projects}
        registry={registry}
        selected={props.selected}
        onSelect={props.onSelect}
        onReposChanged={props.onReposChanged}
        onOpen={(id) => props.onOpen(id)}
        onScan={(folder) => void scan(folder).catch((e) => setError(e instanceof Error ? e.message : String(e)))}
        pending={pending}
        onReview={props.onReview}
      />
      <form
        className="add-repo"
        onSubmit={(e) => {
          e.preventDefault();
          void add(path, true);
        }}
      >
        <label htmlFor="repo-path" className="sr-only">
          {tr("rail.repoPath")}
        </label>
        <input id="repo-path" placeholder={tr("rail.repoPlaceholder")} value={path} onChange={(e) => setPath(e.target.value)} />
        <button className="btn btn-small" type="button" onClick={() => void choose()} title={tr("rail.chooseTitle")}>
          {tr("rail.choose")}
        </button>
        <button className="btn btn-small" type="submit">
          {tr("rail.add")}
        </button>
        {error && (
          <p className="error" role="alert">
            {error}
          </p>
        )}
      </form>
      {scanning && (
        <p className="small muted" role="status">
          {tr("rail.scanning", { name: repoName(scanning) })}
        </p>
      )}
      {picker && (
        <ScanPicker
          key={picker.folder}
          folder={picker.folder}
          scan={picker.scan}
          inProjects={new Set(registry.repos)}
          onAdd={addPicked}
          onCancel={() => setPicker(null)}
        />
      )}
      <h2>{tr("rail.agents")}</h2>
      <ul>
        {agents.map((a) => (
          <li key={a.agent} ref={picking === a.agent ? pickWrap : undefined}>
            <button
              className="rail-item"
              onClick={() => agentClick(a)}
              aria-expanded={a.live.length > 1 ? picking === a.agent : undefined}
              title={
                a.live.length === 0
                  ? tr("rail.agentWrite", { agent: a.agent })
                  : a.live.length === 1
                    ? tr("rail.agentOpen", { branch: a.live[0].branch })
                    : tr("rail.agentPick", { n: a.live.length, agent: a.agent })
              }
            >
              <span>@{a.agent}</span>
              <span className="muted small">{tr("rail.live", { n: a.live.length })}</span>
            </button>
            {picking === a.agent && a.live.length > 1 && (
              <ul className="rail-pick" aria-label={tr("rail.liveList", { agent: a.agent })}>
                {a.live.map((t) => (
                  <li key={t.id}>
                    <button className="rail-item" onClick={() => openTask(t)} title={t.title}>
                      <span>
                        {t.branch} · {repoName(t.repo_path)}
                      </span>
                      <span className={`small ${t.needsYou ? "warn" : "muted"}`}>{columns[t.id] ? columnLabel(columns[t.id]) : ""}</span>
                    </button>
                  </li>
                ))}
                <li>
                  <button
                    className="btn btn-small btn-ghost"
                    onClick={() => {
                      setPicking(null);
                      props.onMention(a.agent);
                    }}
                  >
                    {tr("rail.newTask", { agent: a.agent })}
                  </button>
                </li>
              </ul>
            )}
          </li>
        ))}
      </ul>
      {props.onSkills && <SkillsEntry onOpen={props.onSkills} />}
      <div className="rail-foot">
        <MemoryPanel status={props.status} onOpen={(id) => props.onOpen(id)} />
        <SessionSettings status={props.status} />
      </div>
    </nav>
  );
}

/** Sessions outlive the app (the daemon owns them); the daemon's auto-resume setting
 * (`daemon.settings`) says whether they come back after the daemon itself restarts. */
export function SessionSettings({ status }: { status: DaemonStatus | null }) {
  const { rpc } = useEnv();
  const t = useT();
  // The answer of the last change, until the next status poll says the same.
  const [mine, setMine] = useState<boolean | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const known = typeof status?.auto_resume === "boolean" ? status.auto_resume : null;
  // The daemon's environment turned it off: nothing to switch here.
  const forced = status?.auto_resume_forced_off === true;
  const on = mine ?? known;
  useEffect(() => setMine(null), [known]);
  // The pause policy, held here so the folded line says what the last change chose.
  const pauseKnown = status?.pause_new_sessions_under_pressure ?? null;
  const [pauseMine, setPauseMine] = useState<PausePolicy | null>(null);
  useEffect(() => setPauseMine(null), [pauseKnown]);
  const pause = pauseMine ?? pauseKnown;
  const summary = [
    status && on !== null ? t(!forced && on ? "sessions.sum.resumeOn" : "sessions.sum.resumeOff") : null,
    pause ? t(PAUSE_SUMMARY[pause]) : null,
  ]
    .filter(Boolean)
    .join(" · ");
  const change = async (want: boolean) => {
    setBusy(true);
    setError(null);
    try {
      const r = await rpc<DaemonSettings>(M.daemonSettings, { auto_resume: want });
      setMine(r?.auto_resume ?? want);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Disclosure id="sessions" className="settings" title={t("sessions.title")} summary={summary}>
      <p className="small muted">{t("sessions.keep")}</p>
      {status && on === null ? (
        <p className="small muted">{t("sessions.noResume")}</p>
      ) : (
        <label className="small check">
          <input type="checkbox" checked={!forced && (on ?? false)} disabled={!status || busy || forced} onChange={(e) => void change(e.target.checked)} />{" "}
          {t("sessions.resume")}
        </label>
      )}
      {forced && <p className="small muted">{t("sessions.forced")}</p>}
      <PauseSetting status={status} value={pause} onChosen={setPauseMine} />
      <SummariesSetting status={status} />
      {error && (
        <p className="error small" role="alert">
          {error}
        </p>
      )}
    </Disclosure>
  );
}

/** Thread summaries by the person's own CLI (`daemon.settings`'s `summaries`): off by
 * default; Claude only (Codex cannot run without tools). Hidden for an older daemon. */
function SummariesSetting({ status }: { status: DaemonStatus | null }) {
  const { rpc } = useEnv();
  const t = useT();
  const id = useId();
  const known = status?.summaries ?? null;
  // The answer of the last change, until the next status poll says the same.
  const [mine, setMine] = useState<SummariesSettingValue | null>(null);
  useEffect(() => setMine(null), [known]);
  const value = mine ?? known;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!value) return null;
  const change = async (want: SummariesSettingValue) => {
    setBusy(true);
    setError(null);
    try {
      const r = await rpc<DaemonSettings>(M.daemonSettings, { summaries: want });
      setMine(r?.summaries ?? want);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="pause-setting">
      <label htmlFor={id} className="small">
        {t("sessions.summaries")}
      </label>{" "}
      <select id={id} value={value} disabled={busy} onChange={(e) => void change(e.target.value as SummariesSettingValue)}>
        <option value="none">{t("sessions.summaries.none")}</option>
        <option value="claude">{t("sessions.summaries.claude")}</option>
      </select>
      <p className="small muted">{t("sessions.summariesHelp")}</p>
      {error && (
        <p className="error small" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

const PAUSE_OPTIONS: { id: PausePolicy; label: Key }[] = [
  { id: "off", label: "sessions.pause.off" },
  { id: "critical", label: "sessions.pause.critical" },
  { id: "elevated", label: "sessions.pause.elevated" },
];

const PAUSE_SUMMARY: Record<PausePolicy, Key> = {
  off: "sessions.sum.pause.off",
  critical: "sessions.sum.pause.critical",
  elevated: "sessions.sum.pause.elevated",
};

/** When the memory guard pauses new sessions (`daemon.settings`'s
 * `pause_new_sessions_under_pressure`): off by default, the guard only advises. Hidden for
 * an older daemon that has no such setting; read-only when `MIRANTE_MEMGUARD_PAUSE` decides. */
function PauseSetting({ status, value, onChosen }: { status: DaemonStatus | null; value: PausePolicy | null; onChosen(p: PausePolicy): void }) {
  const { rpc } = useEnv();
  const t = useT();
  const id = useId();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!value) return null;
  const forced = status?.pause_forced_by_env === true;
  const change = async (want: PausePolicy) => {
    setBusy(true);
    setError(null);
    try {
      const r = await rpc<DaemonSettings>(M.daemonSettings, { pause_new_sessions_under_pressure: want });
      onChosen(r?.pause_new_sessions_under_pressure ?? want);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="pause-setting">
      <label htmlFor={id} className="small">
        {t("sessions.pause")}
      </label>{" "}
      <select id={id} value={value} disabled={busy || forced} onChange={(e) => void change(e.target.value as PausePolicy)}>
        {PAUSE_OPTIONS.map((o) => (
          <option key={o.id} value={o.id}>
            {t(o.label)}
          </option>
        ))}
      </select>
      <p className="small muted">{t(forced ? "sessions.pauseForced" : "sessions.pauseHelp")}</p>
      {error && (
        <p className="error small" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** "Skills (n to review)": skill proposals waiting for a person, across all clients (a
 * count, shared with the Skills view; an older daemon shows no count). */
function SkillsEntry({ onOpen }: { onOpen(): void }) {
  const t = useT();
  const pending = usePendingSkills() ?? 0;
  return (
    <>
      <h2>{t("skillsEntry.title")}</h2>
      <button className="rail-item" onClick={onOpen} title={t("skillsEntry.tip")}>
        {pending > 0 ? t("skillsEntry.pending", { n: pending }) : t("skillsEntry.title")}
      </button>
    </>
  );
}

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Brain } from "./components/Brain";
import { CommandBar, type Command } from "./components/CommandBar";
import { Detail } from "./components/Detail";
import { Grid } from "./components/Grid";
import { Home } from "./components/Home";
import { Skills } from "./components/Skills";
import { type Env, EnvContext } from "./env";
import { notificationsFor, shouldNotify } from "./lib/notify";
import { type DaemonStatus, M, type TaskView } from "./lib/protocol";
import { loadRepos } from "./lib/repos";
import { hintsFor, isTypingTarget, onMac, shortcutFor } from "./lib/shortcuts";
import { applyTheme, loadTheme, nextTheme, THEME_KEY, type Theme } from "./lib/theme";
import { useCockpitState } from "./store/cockpit";
import { gridTasks, needsYou, newlyNeedingYou, projects } from "./store/select";
import type { State } from "./store/state";

export type Mode = "home" | "detail" | "grid" | "brain" | "skills";

export const STATUS_POLL_MS = 5000;

/** A form field with what the person is typing (not the terminal: on macOS ⌘ chords pass
 * through it, and leaving it loses nothing). View chords wait there: switching views would
 * unmount the field and lose the edit. */
function isFormField(target: EventTarget | null): boolean {
  return isTypingTarget(target) && !(target instanceof Element && target.closest(".xterm"));
}
/** Tasks that start needing the user within this window share one decision: past two, a
 * single summary (a reconnect replays missed events one by one, a restart sends them all). */
export const NOTIFY_BATCH_MS = 250;

/** The task ids whose card, tile or detail is on screen in this mode. */
export function visibleTasks(s: State, mode: Mode, project: string | null, taskId: string | null): Set<string> {
  // The brain shows notes, no task: every task that starts needing you is off screen.
  if (mode === "brain" || mode === "skills") return new Set();
  if (mode === "detail") return new Set(taskId ? [taskId] : []);
  if (mode === "grid") return new Set(gridTasks(s).map((t) => t.id));
  return new Set(Object.values(s.tasks).filter((t) => t.repo_path === project && t.lifecycle !== "archived").map((t) => t.id));
}

function ConnectionBanner({ s, env }: { s: State; env: Env }) {
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  if (s.connection.state === "live") return null;
  const start = () => {
    setStarting(true);
    setError(null);
    env
      .startDaemon()
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setStarting(false));
  };
  return (
    <div className={`banner banner-${s.connection.state}`} role="status" aria-label="Daemon connection">
      {s.connection.state === "connecting" ? (
        "Connecting to the Mirante daemon…"
      ) : (
        <>
          Daemon not reachable ({s.connection.error}). Retrying in {Math.round(s.connection.retryMs / 100) / 10}s.{" "}
          <button className="btn btn-small" disabled={starting} onClick={start}>
            Start daemon
          </button>
          {error && <span className="error"> {error}</span>}
        </>
      )}
    </div>
  );
}

export function App({ env }: { env: Env }) {
  const mac = useMemo(onMac, []);
  const hints = useMemo(() => hintsFor(mac), [mac]);
  const s = useCockpitState(env.cockpit);
  const [mode, setMode] = useState<Mode>("home");
  const [beforeGrid, setBeforeGrid] = useState<Mode>("home");
  const [beforeBrain, setBeforeBrain] = useState<Mode>("home");
  // Bumped by every "Review memory": the brain (open or not) switches to its review queue.
  const [reviewNonce, setReviewNonce] = useState(0);
  // Bumped by every "Review skills": the Skills view shows its review queue.
  const [skillsReviewNonce, setSkillsReviewNonce] = useState(0);
  const [taskId, setTaskId] = useState<string | null>(null);
  const [detailTab, setDetailTab] = useState<"Terminal" | undefined>(undefined);
  // Bumped by every "Answer in terminal": Detail re-selects its Terminal tab on each new value.
  const [terminalNonce, setTerminalNonce] = useState(0);
  const [repos, setRepos] = useState(() => loadRepos(env.storage));
  const [project, setProject] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [palette, setPalette] = useState(false);
  const [theme, setTheme] = useState<Theme>(() => loadTheme(env.storage));
  const [status, setStatus] = useState<DaemonStatus | null>(null);

  const projs = useMemo(() => projects(s, repos), [s, repos]);
  const current = project ?? projs[0]?.path ?? null;

  // What is on screen as of the last render: read when an event arrives, before re-render.
  const visible = useRef<Set<string>>(new Set());
  visible.current = visibleTasks(s, mode, current, taskId);

  useEffect(() => {
    env.cockpit.connect().catch(() => {});
  }, [env]);

  useEffect(() => {
    // Tasks due a notification, collected for NOTIFY_BATCH_MS. Whether one is on screen is
    // decided when it starts needing the user (what was shown then), not at the flush.
    let due = new Map<string, TaskView>();
    let timer: ReturnType<typeof setTimeout> | null = null;
    const flush = () => {
      timer = null;
      const tasks = [...due.values()];
      due = new Map();
      for (const n of notificationsFor(tasks)) env.notify(n);
    };
    const off = env.cockpit.onChange((prev, next) => {
      // The first snapshot is what the user left, not news.
      if (prev.bootId === null) return;
      for (const t of newlyNeedingYou(prev, next)) {
        if (shouldNotify(t.id, visible.current, document.hasFocus())) due.set(t.id, t);
      }
      if (due.size > 0 && !timer) timer = setTimeout(flush, NOTIFY_BATCH_MS);
    });
    return () => {
      off();
      if (timer) clearTimeout(timer);
    };
  }, [env]);

  const live = s.connection.state === "live";
  useEffect(() => {
    // Away from the daemon nothing is known: no stale memory numbers, no stale inline-answers flag.
    if (!live) {
      setStatus(null);
      return;
    }
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    // One request at a time: the next poll is scheduled when this one settles.
    // An unknown status is null (never a guessed one): the memory panel then says so.
    const poll = () =>
      env
        .rpc<DaemonStatus>(M.daemonStatus)
        .then(
          (v) => !stopped && setStatus(v ?? null),
          () => !stopped && setStatus(null),
        )
        .finally(() => {
          if (!stopped) timer = setTimeout(poll, STATUS_POLL_MS);
        });
    void poll();
    return () => {
      stopped = true;
      clearTimeout(timer);
      setStatus(null);
    };
  }, [env, live]);

  useEffect(() => {
    applyTheme(theme);
    env.storage.setItem(THEME_KEY, theme);
  }, [env, theme]);

  const open = useCallback((id: string, tab?: "Terminal") => {
    setTaskId(id);
    setDetailTab(tab);
    if (tab === "Terminal") setTerminalNonce((n) => n + 1);
    setMode("detail");
  }, []);
  const toggleGrid = useCallback(() => {
    if (mode === "grid") {
      setMode(beforeGrid);
    } else {
      setBeforeGrid(mode);
      setMode("grid");
    }
  }, [mode, beforeGrid]);
  // From a task's detail, the brain shows that task's repo (its project).
  const detailRepo = mode === "detail" && taskId ? s.tasks[taskId]?.repo_path : undefined;
  const toggleBrain = useCallback(() => {
    if (mode === "brain") {
      setMode(beforeBrain);
    } else {
      if (detailRepo) setProject(detailRepo);
      setBeforeBrain(mode);
      setReviewNonce(0);
      setMode("brain");
    }
  }, [mode, beforeBrain, detailRepo]);
  const openReview = useCallback(
    (path?: string) => {
      const repo = path ?? detailRepo;
      if (repo) setProject(repo);
      if (mode !== "brain") setBeforeBrain(mode);
      setReviewNonce((n) => n + 1);
      setMode("brain");
    },
    [mode, detailRepo],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const a = shortcutFor(e, mac);
      if (!a || e.defaultPrevented) return;
      // The command bar is modal: only its own toggle (and its Escape) act while it is open.
      if (palette && a !== "palette" && a !== "escape") return;
      if (a === "escape") {
        // Escape belongs to whatever is being typed in (the terminal, a composer).
        if (!palette && mode === "detail" && !isTypingTarget(e.target)) setMode("home");
        return;
      }
      // Off macOS the mod key is Ctrl, which the terminal needs (Ctrl+K, Ctrl+G…). On macOS
      // ⌘ is no terminal key, so ⌘K opens the command bar even from the terminal.
      if (!mac && e.target instanceof Element && e.target.closest(".xterm")) return;
      if ((a === "brain" || a === "grid" || a === "home") && isFormField(e.target)) return;
      e.preventDefault();
      if (a === "palette") setPalette((p) => !p);
      if (a === "grid") toggleGrid();
      if (a === "brain") toggleBrain();
      if (a === "home") setMode("home");
      if (a === "theme") setTheme(nextTheme);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette, mode, toggleGrid, toggleBrain, mac]);

  const inlineAnswers = status?.claude_inline_answers ?? false;
  const commands: Command[] = useMemo(
    () => [
      { id: "home", label: "Go to Home", hint: hints.home, run: () => setMode("home") },
      { id: "grid", label: "Toggle agent grid", hint: hints.grid, run: toggleGrid },
      {
        id: "brain",
        label: "Go to Brain",
        hint: hints.brain,
        run: () => {
          if (mode !== "brain") toggleBrain();
        },
      },
      { id: "review", label: "Review memory", run: () => openReview() },
      { id: "skills", label: "Go to Skills", run: () => setMode("skills") },
      {
        id: "skills-review",
        label: "Review skills",
        run: () => {
          setSkillsReviewNonce((n) => n + 1);
          setMode("skills");
        },
      },
      { id: "theme", label: `Theme: ${nextTheme(theme)}`, hint: hints.theme, run: () => setTheme(nextTheme) },
      ...projs.map((p) => ({
        id: `p-${p.path}`,
        label: `Project ${p.name}`,
        run: () => {
          setProject(p.path);
          setMode("home");
        },
      })),
      ...Object.values(s.tasks)
        .filter((t) => t.lifecycle !== "archived")
        .map((t) =>
          // A Claude request is answered in its own TUI unless the daemon types answers (a
          // trust prompt is not: its card answers it here).
          needsYou(t) && t.boot !== "trust_prompt" && t.agent === "claude" && !inlineAnswers
            ? { id: `t-${t.id}`, label: `Answer in terminal: ${t.title} (@claude ${t.branch})`, run: () => open(t.id, "Terminal") }
            : { id: `t-${t.id}`, label: `Open ${t.title} (@${t.agent} ${t.branch})`, run: () => open(t.id) },
        ),
    ],
    [projs, s.tasks, theme, open, toggleGrid, toggleBrain, openReview, mode, hints, inlineAnswers],
  );

  return (
    <EnvContext.Provider value={env}>
      <div className="app">
        <header className="topbar">
          <span className="brand">Mirante</span>
          <nav aria-label="Views" className="modes">
            <button aria-pressed={mode === "home"} onClick={() => setMode("home")}>
              Home <kbd>{hints.home}</kbd>
            </button>
            <button aria-pressed={mode === "detail"} disabled={!taskId} onClick={() => setMode("detail")}>
              Detail
            </button>
            <button aria-pressed={mode === "grid"} onClick={toggleGrid}>
              Grid <kbd>{hints.grid}</kbd>
            </button>
            <button aria-pressed={mode === "brain"} onClick={toggleBrain}>
              Brain <kbd>{hints.brain}</kbd>
            </button>
            <button aria-pressed={mode === "skills"} onClick={() => setMode("skills")}>
              Skills
            </button>
          </nav>
          <button className="btn btn-small" onClick={() => setPalette(true)}>
            Commands <kbd>{hints.palette}</kbd>
          </button>
          <button className="btn btn-small" onClick={() => setTheme(nextTheme)} aria-label={`Theme: ${theme}`} title={`Theme: ${theme} (${hints.theme})`}>
            <span aria-hidden="true">{theme === "system" ? "◐" : theme === "light" ? "☀" : "☾"}</span>
          </button>
        </header>
        <ConnectionBanner s={s} env={env} />
        {mode === "home" && (
          <Home
            projects={projs}
            project={current}
            onProject={setProject}
            onReposChanged={setRepos}
            onOpen={open}
            onReview={openReview}
            onSkills={() => {
              setSkillsReviewNonce((n) => n + 1);
              setMode("skills");
            }}
            status={status}
            draft={draft}
            onDraft={setDraft}
          />
        )}
        {mode === "detail" && taskId && (
          <Detail
            taskId={taskId}
            onSelect={setTaskId}
            initialTab={detailTab}
            focusTerminalNonce={terminalNonce}
            inlineAnswers={inlineAnswers}
          />
        )}
        {mode === "grid" && <Grid onOpen={open} status={status} />}
        {mode === "brain" && <Brain key={current ?? ""} repo={current} reviewNonce={reviewNonce} />}
        {mode === "skills" && <Skills reviewNonce={skillsReviewNonce} />}
        {palette && (
          <CommandBar
            commands={commands}
            onClose={() => setPalette(false)}
            onDispatch={(text) => {
              setDraft(text);
              setMode("home");
            }}
          />
        )}
      </div>
    </EnvContext.Provider>
  );
}

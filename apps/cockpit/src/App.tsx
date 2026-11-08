import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { CommandBar, type Command } from "./components/CommandBar";
import { Detail } from "./components/Detail";
import { Grid } from "./components/Grid";
import { Home } from "./components/Home";
import { type Env, EnvContext } from "./env";
import { shouldNotify } from "./lib/notify";
import { type DaemonStatus, M } from "./lib/protocol";
import { loadRepos } from "./lib/repos";
import { isTypingTarget, SHORTCUT_HINTS, shortcutFor } from "./lib/shortcuts";
import { applyTheme, loadTheme, nextTheme, THEME_KEY, type Theme } from "./lib/theme";
import { useCockpitState } from "./store/cockpit";
import { gridTasks, newlyNeedingYou, projects } from "./store/select";
import type { State } from "./store/state";

export type Mode = "home" | "detail" | "grid";

const MAC = typeof navigator !== "undefined" && /Mac/.test(navigator.platform);
export const STATUS_POLL_MS = 5000;

/** The task ids whose card, tile or detail is on screen in this mode. */
export function visibleTasks(s: State, mode: Mode, project: string | null, taskId: string | null): Set<string> {
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
  const s = useCockpitState(env.cockpit);
  const [mode, setMode] = useState<Mode>("home");
  const [beforeGrid, setBeforeGrid] = useState<Mode>("home");
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

  useEffect(
    () =>
      env.cockpit.onChange((prev, next) => {
        // The first snapshot is what the user left, not news.
        if (prev.bootId === null) return;
        for (const t of newlyNeedingYou(prev, next)) {
          if (shouldNotify(t.id, visible.current, document.hasFocus())) env.notify(t);
        }
      }),
    [env],
  );

  const live = s.connection.state === "live";
  useEffect(() => {
    if (!live) return;
    let stopped = false;
    // An unknown status is null (never a guessed one): the memory panel then says so.
    const poll = () =>
      env.rpc<DaemonStatus>(M.daemonStatus).then(
        (v) => !stopped && setStatus(v ?? null),
        () => !stopped && setStatus(null),
      );
    void poll();
    const h = setInterval(poll, STATUS_POLL_MS);
    return () => {
      stopped = true;
      clearInterval(h);
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

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const a = shortcutFor(e, MAC);
      if (!a || e.defaultPrevented) return;
      if (a === "escape") {
        // Escape belongs to whatever is being typed in (the terminal, a composer).
        if (!palette && mode === "detail" && !isTypingTarget(e.target)) setMode("home");
        return;
      }
      // Off macOS the mod key is Ctrl, which the terminal needs (Ctrl+K, Ctrl+G…).
      if (!MAC && e.target instanceof Element && e.target.closest(".xterm")) return;
      e.preventDefault();
      if (a === "palette") setPalette((p) => !p);
      if (a === "grid") toggleGrid();
      if (a === "home") setMode("home");
      if (a === "theme") setTheme(nextTheme);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette, mode, toggleGrid]);

  const commands: Command[] = useMemo(
    () => [
      { id: "home", label: "Go to Home", hint: SHORTCUT_HINTS.home, run: () => setMode("home") },
      { id: "grid", label: "Toggle agent grid", hint: SHORTCUT_HINTS.grid, run: toggleGrid },
      { id: "theme", label: `Theme: ${nextTheme(theme)}`, hint: SHORTCUT_HINTS.theme, run: () => setTheme(nextTheme) },
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
        .map((t) => ({ id: `t-${t.id}`, label: `Open ${t.title} (@${t.agent} ${t.branch})`, run: () => open(t.id) })),
    ],
    [projs, s.tasks, theme, open, toggleGrid],
  );

  return (
    <EnvContext.Provider value={env}>
      <div className="app">
        <header className="topbar">
          <span className="brand">Mirante</span>
          <nav aria-label="Views" className="modes">
            <button aria-pressed={mode === "home"} onClick={() => setMode("home")}>
              Home <kbd>{SHORTCUT_HINTS.home}</kbd>
            </button>
            <button aria-pressed={mode === "detail"} disabled={!taskId} onClick={() => setMode("detail")}>
              Detail
            </button>
            <button aria-pressed={mode === "grid"} onClick={toggleGrid}>
              Grid <kbd>{SHORTCUT_HINTS.grid}</kbd>
            </button>
          </nav>
          <button className="btn btn-small" onClick={() => setPalette(true)}>
            Commands <kbd>{SHORTCUT_HINTS.palette}</kbd>
          </button>
          <button className="btn btn-small" onClick={() => setTheme(nextTheme)} aria-label={`Theme: ${theme}`} title={`Theme: ${theme} (${SHORTCUT_HINTS.theme})`}>
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
            inlineAnswers={status?.claude_inline_answers ?? false}
          />
        )}
        {mode === "grid" && <Grid onOpen={open} status={status} />}
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

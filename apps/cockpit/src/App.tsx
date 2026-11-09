import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Brain } from "./components/Brain";
import { CommandBar, type Command } from "./components/CommandBar";
import { Detail } from "./components/Detail";
import { Grid } from "./components/Grid";
import { Home } from "./components/Home";
import { Skills } from "./components/Skills";
import { Splash } from "./components/Splash";
import { dismissSplash } from "./lib/splash";
import { type Env, EnvContext } from "./env";
import { type ComposerForm, EMPTY_FORM } from "./components/DispatchComposer";
import { type Lang, LANGS, LangContext, loadLang, saveLang, setLang, t as tr, useT } from "./lib/i18n";
import { notificationsFor, shouldNotify } from "./lib/notify";
import { useNow } from "./lib/useNow";
import { needingOrder, nextNeeding } from "./lib/inbox";
import { type DaemonStatus, M, type TaskView } from "./lib/protocol";
import { loadRepos } from "./lib/repos";
import { hintsFor, isTypingTarget, onMac, shortcutFor } from "./lib/shortcuts";
import { applyTheme, loadTheme, nextTheme, saveTheme, type Theme } from "./lib/theme";
import { type DetailTab, loadUi, saveUi } from "./lib/uistate";
import { useCockpitSelect } from "./store/cockpit";
import { gridTasks, needsYou, newlyNeedingYou, type Project, projects } from "./store/select";
import { safeStorage, shallow, useSelect } from "./env";
import { type Connection, same, type State } from "./store/state";

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

/** On Home: the task's card or row is in the page, not folded away (a closed "Parados"),
 * and not scrolled out of the window (when the layout says where it is). */
export function cardOnScreen(id: string): boolean {
  const el = document.querySelector(`[data-task="${CSS.escape(id)}"]`);
  if (!el || el.closest("details:not([open])")) return false;
  const r = el.getBoundingClientRect();
  // No layout (zero box): nothing says it is off screen.
  if (r.width === 0 && r.height === 0) return true;
  return r.bottom > 0 && r.top < window.innerHeight;
}

/** The person is looking at this window: focused and not hidden (another space, minimized). */
export function looking(): boolean {
  return document.hasFocus() && !document.hidden;
}

/** The task ids whose card, tile or detail is on screen in this mode. `onScreen`: on Home,
 * whether a task's card is (the DOM by default). */
export function visibleTasks(
  s: State,
  mode: Mode,
  project: string | null,
  taskId: string | null,
  onScreen: (id: string) => boolean = cardOnScreen,
): Set<string> {
  // The brain shows notes, no task: every task that starts needing you is off screen.
  if (mode === "brain" || mode === "skills") return new Set();
  if (mode === "detail") return new Set(taskId ? [taskId] : []);
  if (mode === "grid") return new Set(gridTasks(s).map((t) => t.id));
  // Home is an inbox of every project: the tasks whose card or row is on screen.
  void project;
  return new Set(Object.values(s.tasks).filter((t) => t.lifecycle !== "archived" && onScreen(t.id)).map((t) => t.id));
}

const selectTasks = (s: State) => s.tasks;
const selectNeeding = (s: State) => needingOrder(s);
const selectConnection = (s: State) => s.connection;
const selectBoot = (s: State) => s.bootId;

/** How long to wait between saves of Home's scroll (it is kept in memory at once). */
export const SCROLL_SAVE_MS = 250;

function ConnectionBanner({ connection, env }: { connection: Connection; env: Env }) {
  const t = useT();
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);
  // When the next try is due (a new value per disconnect report), counted down each second.
  const due = useMemo(() => (connection.state === "down" ? Date.now() + connection.retryMs : 0), [connection]);
  const now = useNow(1000);
  if (connection.state === "live") return null;
  const start = () => {
    setStarting(true);
    setError(null);
    env
      .startDaemon()
      .catch((e) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setStarting(false));
  };
  return (
    <div className={`banner banner-${connection.state}`} role="status" aria-label={t("conn.aria")}>
      {connection.state === "connecting" ? (
        t("conn.connecting")
      ) : (
        <>
          {t("conn.down", { error: connection.error, s: Math.max(0, Math.ceil((due - now) / 1000)) })}{" "}
          <button className="btn btn-small" disabled={starting} onClick={start}>
            {t("conn.start")}
          </button>
          {error && <span className="error"> {error}</span>}
        </>
      )}
    </div>
  );
}

/** The command bar with its task commands: mounted (and reading every task) only while open. */
function Palette(props: {
  base: Command[];
  inlineAnswers: boolean;
  onOpen(id: string, tab?: "Terminal"): void;
  onClose(): void;
  onDispatch(text: string): void;
}) {
  const t = useT();
  const tasks = useSelect(selectTasks);
  const { base, inlineAnswers, onOpen } = props;
  const commands = useMemo(
    () => [
      ...base,
      ...Object.values(tasks)
        .filter((x) => x.lifecycle !== "archived")
        .map((x) =>
          // A Claude request is answered in its own TUI unless the daemon types answers (a
          // trust prompt is not: its card answers it here).
          needsYou(x) && x.boot !== "trust_prompt" && x.agent === "claude" && !inlineAnswers
            ? { id: `t-${x.id}`, label: t("cmd.answer", { title: x.title, branch: x.branch }), run: () => onOpen(x.id, "Terminal") }
            : { id: `t-${x.id}`, label: t("cmd.open", { title: x.title, agent: x.agent, branch: x.branch }), run: () => onOpen(x.id) },
        ),
    ],
    [base, tasks, inlineAnswers, onOpen, t],
  );
  return <CommandBar commands={commands} onClose={props.onClose} onDispatch={props.onDispatch} />;
}

/** Whether `t` is answered in its terminal (a Claude request, unless the daemon types answers). */
function answeredInTerminal(t: TaskView | undefined, inlineAnswers: boolean): boolean {
  return !!t && t.agent === "claude" && t.boot !== "trust_prompt" && !inlineAnswers;
}

export function App({ env: given }: { env: Env }) {
  // Storage that throws (a private window, blocked site data) is storage that forgets.
  const env = useMemo(() => ({ ...given, storage: safeStorage(given.storage) }), [given]);
  const mac = useMemo(onMac, []);
  const hints = useMemo(() => hintsFor(mac), [mac]);
  const [lang, setLangState] = useState<Lang>(() => loadLang(env.storage));
  // Before anything below renders: `t` speaks the chosen language.
  setLang(lang);
  const t = tr;
  useEffect(() => {
    document.documentElement.lang = lang;
  }, [lang]);
  const changeLang = useCallback(
    (l: Lang) => {
      setLangState(l);
      saveLang(env.storage, l);
    },
    [env],
  );
  // Only what App itself shows: the views below select their own slices.
  const connection = useCockpitSelect(env.cockpit, selectConnection);
  const needing = useCockpitSelect(env.cockpit, selectNeeding, shallow);
  const bootId = useCockpitSelect(env.cockpit, selectBoot);
  // Where the person was when the app last closed (per viewer, this machine).
  const [saved] = useState(() => loadUi(env.storage));
  const [mode, setMode] = useState<Mode>(saved.mode);
  const [beforeGrid, setBeforeGrid] = useState<Mode>("home");
  const [beforeBrain, setBeforeBrain] = useState<Mode>("home");
  // The view a detail was opened from (Escape goes back there).
  const [beforeDetail, setBeforeDetail] = useState<Mode>("home");
  // The task whose detail was left for Home: its card takes the keyboard there.
  const [returnTo, setReturnTo] = useState<string | null>(null);
  // Bumped by every "Review memory": the brain (open or not) switches to its review queue.
  const [reviewNonce, setReviewNonce] = useState(0);
  // Bumped by every "Review skills": the Skills view shows its review queue.
  const [skillsReviewNonce, setSkillsReviewNonce] = useState(0);
  const [taskId, setTaskId] = useState<string | null>(saved.taskId);
  const [detailTab, setDetailTab] = useState<DetailTab | undefined>(saved.tab ?? undefined);
  // The tab the detail shows now (remembered with the rest).
  const [shownTab, setShownTab] = useState<DetailTab | null>(saved.tab);
  // Bumped by every "Answer in terminal": Detail re-selects its Terminal tab on each new value.
  const [terminalNonce, setTerminalNonce] = useState(0);
  const [repos, setRepos] = useState(() => loadRepos(env.storage));
  const [project, setProject] = useState<string | null>(saved.project);
  // The dispatch form (goal and the rest) lives here: Home unmounts on every other view.
  const [draft, setDraft] = useState("");
  const [form, setForm] = useState<ComposerForm>(EMPTY_FORM);
  const [palette, setPalette] = useState(false);
  const [theme, setTheme] = useState<Theme>(() => loadTheme(env.storage));
  const [status, setStatus] = useState<DaemonStatus | null>(null);

  // The projects, kept while equal: a task refresh that changes no project leaves Home's
  // props alone (and is worked out once per change of the tasks).
  const projCache = useRef<{ tasks: State["tasks"] | null; repos: string[]; value: Project[] }>({ tasks: null, repos, value: [] });
  const selectProjects = useCallback(
    (s: State) => {
      const c = projCache.current;
      if (c.tasks === s.tasks && c.repos === repos) return c.value;
      const next = projects(s, repos);
      projCache.current = { tasks: s.tasks, repos, value: same(c.value, next) ? c.value : next };
      return projCache.current.value;
    },
    [repos],
  );
  const projs = useCockpitSelect(env.cockpit, selectProjects);
  // A remembered project that is no longer listed (removed, its tasks gone) is not shown:
  // the first one is, once the daemon's tasks are known.
  const listed = bootId === null || (project !== null && projs.some((p) => p.path === project));
  const current = (listed ? project : null) ?? projs[0]?.path ?? null;

  // The view as of the last render; what it shows is worked out from the state before a
  // change (App does not render for every event).
  const view = useRef({ mode, current, taskId });
  view.current = { mode, current, taskId };

  useEffect(() => {
    env.cockpit.connect().catch(() => {});
  }, [env]);
  // The launch splash (index.html) has done its job once the app is on screen.
  useEffect(() => dismissSplash(), []);

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
      const fresh = newlyNeedingYou(prev, next);
      if (fresh.length === 0) return;
      const { mode, current, taskId } = view.current;
      const visible = visibleTasks(prev, mode, current, taskId);
      for (const t of fresh) {
        if (shouldNotify(t.id, visible, looking())) due.set(t.id, t);
      }
      if (due.size > 0 && !timer) timer = setTimeout(flush, NOTIFY_BATCH_MS);
    });
    return () => {
      off();
      if (timer) clearTimeout(timer);
    };
  }, [env]);

  const live = connection.state === "live";
  // Never connected yet (no snapshot): nothing to show but that it is connecting.
  const waiting = connection.state === "connecting" && bootId === null;
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
          // A poll that says the same keeps the object: nothing below renders again.
          (v) => !stopped && setStatus((cur) => (same(cur, v ?? null) ? cur : (v ?? null))),
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

  // Home's scroll is kept here at once and saved a moment after scrolling stops.
  const ui = useRef({ ...saved });
  const scrollTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    ui.current = { ...ui.current, mode, project, taskId, tab: shownTab };
    saveUi(env.storage, ui.current);
  }, [env, mode, project, taskId, shownTab]);
  const onHomeScroll = useCallback(
    (top: number) => {
      ui.current = { ...ui.current, scroll: top };
      if (scrollTimer.current !== null) clearTimeout(scrollTimer.current);
      scrollTimer.current = setTimeout(() => {
        scrollTimer.current = null;
        saveUi(env.storage, ui.current);
      }, SCROLL_SAVE_MS);
    },
    [env],
  );
  useEffect(
    () => () => {
      if (scrollTimer.current !== null) clearTimeout(scrollTimer.current);
    },
    [],
  );

  useEffect(() => {
    applyTheme(theme);
    saveTheme(env.storage, theme);
  }, [env, theme]);

  // A new rail selection takes over from a repo picked in the form.
  const formProject = useRef(current);
  useEffect(() => {
    if (formProject.current === current) return;
    formProject.current = current;
    setForm((f) => (f.picked === null ? f : { ...f, picked: null }));
  }, [current]);

  // A remembered (or open) detail whose task is gone, once the daemon's tasks are known:
  // back to Home (the one behaviour for a task that disappears).
  const gone = useCockpitSelect(env.cockpit, useCallback((s: State) => s.bootId !== null && !!taskId && !s.tasks[taskId], [taskId]));
  useEffect(() => {
    if (!gone) return;
    setTaskId(null);
    setMode((m) => (m === "detail" ? "home" : m));
  }, [gone]);

  const open = useCallback((id: string, tab?: "Terminal") => {
    setTaskId(id);
    setDetailTab(tab);
    if (tab === "Terminal") setTerminalNonce((n) => n + 1);
    setMode((m) => {
      if (m !== "detail") setBeforeDetail(m);
      return "detail";
    });
  }, []);
  const inlineAnswers = status?.claude_inline_answers ?? false;
  const inlineRef = useRef(inlineAnswers);
  inlineRef.current = inlineAnswers;
  /** Opens a task where it is answered (Claude's request on its Terminal tab). */
  const openWhereAnswered = useCallback(
    (id: string) => {
      const t = env.cockpit.getState().tasks[id];
      if (!t) return;
      open(id, needsYou(t) && answeredInTerminal(t, inlineRef.current) ? "Terminal" : undefined);
    },
    [env, open],
  );
  // A click on a task's notification opens it.
  useEffect(() => env.onNoteClick?.(openWhereAnswered), [env, openWhereAnswered]);
  // Focus mode: the next task that needs the person, on the tab where it is answered.
  const next = nextNeeding(needing, mode === "detail" ? taskId : null);
  const goNext = useCallback(() => {
    if (!next.id) return;
    const t = env.cockpit.getState().tasks[next.id];
    open(next.id, answeredInTerminal(t, inlineRef.current) ? "Terminal" : undefined);
  }, [env, next.id, open]);
  /** Leaves the detail for `to` (Home focuses the card of the task it showed). */
  const leave = useCallback(
    (to: Mode) => {
      if (mode === "detail" && to !== "detail") setReturnTo(taskId);
      setMode(to);
    },
    [mode, taskId],
  );
  const goHome = useCallback(() => leave("home"), [leave]);
  const goSkills = useCallback(() => leave("skills"), [leave]);
  const toggleGrid = useCallback(() => {
    if (mode === "grid") {
      setMode(beforeGrid);
    } else {
      setBeforeGrid(mode);
      leave("grid");
    }
  }, [mode, beforeGrid, leave]);
  // From a task's detail, the brain shows that task's repo (its project).
  const detailRepo = useCockpitSelect(
    env.cockpit,
    useCallback((s: State) => (mode === "detail" && taskId ? s.tasks[taskId]?.repo_path : undefined), [mode, taskId]),
  );
  const toggleBrain = useCallback(() => {
    if (mode === "brain") {
      setMode(beforeBrain);
    } else {
      if (detailRepo) setProject(detailRepo);
      setBeforeBrain(mode);
      setReviewNonce(0);
      leave("brain");
    }
  }, [mode, beforeBrain, detailRepo, leave]);
  const openReview = useCallback(
    (path?: string) => {
      const repo = path ?? detailRepo;
      if (repo) setProject(repo);
      if (mode !== "brain") setBeforeBrain(mode);
      setReviewNonce((n) => n + 1);
      leave("brain");
    },
    [mode, detailRepo, leave],
  );
  const reviewSkills = useCallback(() => {
    setSkillsReviewNonce((n) => n + 1);
    leave("skills");
  }, [leave]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const a = shortcutFor(e, mac);
      if (!a || e.defaultPrevented) return;
      // The command bar is modal: only its own toggle (and its Escape) act while it is open.
      if (palette && a !== "palette" && a !== "escape") return;
      if (a === "escape") {
        // Escape belongs to whatever is being typed in (the terminal, a composer).
        if (!palette && mode === "detail" && !isTypingTarget(e.target)) leave(beforeDetail === "detail" ? "home" : beforeDetail);
        return;
      }
      // Off macOS the mod key is Ctrl, which the terminal needs (Ctrl+K, Ctrl+G…). On macOS
      // ⌘ is no terminal key, so ⌘K opens the command bar even from the terminal.
      if (!mac && e.target instanceof Element && e.target.closest(".xterm")) return;
      if ((a === "brain" || a === "grid" || a === "home" || a === "next" || a === "skills") && isFormField(e.target)) return;
      e.preventDefault();
      if (a === "next") goNext();
      if (a === "palette") setPalette((p) => !p);
      if (a === "grid") toggleGrid();
      if (a === "brain") toggleBrain();
      if (a === "home") goHome();
      if (a === "skills") goSkills();
      if (a === "theme") setTheme(nextTheme);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [palette, mode, beforeDetail, toggleGrid, toggleBrain, goHome, goSkills, leave, mac, goNext]);

  const themeName = (x: Theme) => t(x === "system" ? "theme.system" : x === "light" ? "theme.light" : "theme.dark");
  // Built only while the command bar is open.
  const baseCommands = useMemo<Command[]>(
    () =>
      !palette
        ? []
        : [
            { id: "home", label: t("cmd.home"), hint: hints.home, run: goHome },
            { id: "next", label: t("cmd.next"), hint: hints.next, run: goNext },
            { id: "grid", label: t("cmd.grid"), hint: hints.grid, run: toggleGrid },
            {
              id: "brain",
              label: t("cmd.brain"),
              hint: hints.brain,
              run: () => {
                if (mode !== "brain") toggleBrain();
              },
            },
            { id: "review", label: t("cmd.review"), run: () => openReview() },
            { id: "skills", label: t("cmd.skills"), hint: hints.skills, run: goSkills },
            { id: "skills-review", label: t("cmd.skillsReview"), run: reviewSkills },
            { id: "theme", label: t("theme.label", { theme: themeName(nextTheme(theme)) }), hint: hints.theme, run: () => setTheme(nextTheme) },
            {
              id: "lang",
              label: t("cmd.lang", { lang: t(lang === "en" ? "lang.pt" : "lang.en") }),
              run: () => changeLang(lang === "en" ? "pt" : "en"),
            },
            ...projs.map((p) => ({
              id: `p-${p.path}`,
              label: t("cmd.project", { name: p.name }),
              run: () => {
                setProject(p.path);
                leave("home");
              },
            })),
          ],
    // `t` follows `lang`.
    [palette, projs, theme, lang, toggleGrid, toggleBrain, openReview, mode, hints, goNext, goHome, goSkills, reviewSkills, leave, changeLang],
  );
  const onDispatchText = useCallback(
    (text: string) => {
      setDraft(text);
      leave("home");
    },
    [leave],
  );
  const closePalette = useCallback(() => setPalette(false), []);
  const consumedReturn = useCallback(() => setReturnTo(null), []);

  return (
    <EnvContext.Provider value={env}>
      <LangContext.Provider value={lang}>
        <div className="app">
          <header className="topbar">
            <span className="brand">Mirante</span>
            <nav aria-label={t("nav.views")} className="modes">
              <button aria-pressed={mode === "home"} onClick={goHome}>
                {t("nav.home")} <kbd>{hints.home}</kbd>
              </button>
              <button aria-pressed={mode === "detail"} disabled={!taskId} onClick={() => setMode("detail")}>
                {t("nav.detail")}
              </button>
              <button aria-pressed={mode === "grid"} onClick={toggleGrid}>
                {t("nav.grid")} <kbd>{hints.grid}</kbd>
              </button>
              <button aria-pressed={mode === "brain"} onClick={toggleBrain}>
                {t("nav.brain")} <kbd>{hints.brain}</kbd>
              </button>
              <button aria-pressed={mode === "skills"} onClick={goSkills}>
                {t("nav.skills")} <kbd>{hints.skills}</kbd>
              </button>
            </nav>
            <button
              className="btn btn-small next"
              disabled={needing.length === 0}
              onClick={goNext}
              title={needing.length === 0 ? t("next.none") : t("next.title", { key: hints.next })}
              aria-label={needing.length === 0 ? t("next.ariaNone") : t("next.aria", { at: Math.max(next.at, 1), n: needing.length })}
            >
              {t("next.label")} <kbd>{hints.next}</kbd>
              {needing.length > 0 && (
                <span className="muted small">
                  {" "}
                  · {t("next.position", { at: next.at > 0 ? next.at : "–", n: needing.length })}
                </span>
              )}
            </button>
            <button className="btn btn-small" onClick={() => setPalette(true)}>
              {t("top.commands")} <kbd>{hints.palette}</kbd>
            </button>
            <button
              className="btn btn-small"
              onClick={() => setTheme(nextTheme)}
              aria-label={t("theme.label", { theme: themeName(theme) })}
              title={`${t("theme.label", { theme: themeName(theme) })} (${hints.theme})`}
            >
              <span aria-hidden="true">{theme === "system" ? "◐" : theme === "light" ? "☀" : "☾"}</span>
            </button>
            <label className="sr-only" htmlFor="lang">
              {t("lang.label")}
            </label>
            <select id="lang" className="lang" value={lang} onChange={(e) => changeLang(e.target.value as Lang)} title={t("lang.label")}>
              {LANGS.map((l) => (
                <option key={l} value={l}>
                  {l === "en" ? "EN · English" : "PT · Português"}
                </option>
              ))}
            </select>
          </header>
          {/* Until the daemon first answers, the launch splash's mark says so (not an empty view). */}
          {waiting ? <Splash text={t("conn.connecting")} /> : <ConnectionBanner connection={connection} env={env} />}
          {!waiting && mode === "home" && (
            <Home
              projects={projs}
              project={current}
              onProject={setProject}
              onReposChanged={setRepos}
              onOpen={open}
              onReview={openReview}
              onSkills={reviewSkills}
              status={status}
              draft={draft}
              onDraft={setDraft}
              form={form}
              onForm={setForm}
              scroll={ui.current.scroll}
              onScroll={onHomeScroll}
              focusTask={returnTo}
              onFocused={consumedReturn}
            />
          )}
          {!waiting && mode === "detail" && taskId && (
            <Detail
              taskId={taskId}
              onSelect={setTaskId}
              initialTab={detailTab}
              focusTerminalNonce={terminalNonce}
              inlineAnswers={inlineAnswers}
              onTab={setShownTab}
            />
          )}
          {!waiting && mode === "grid" && <Grid onOpen={open} status={status} />}
          {!waiting && mode === "brain" && <Brain key={current ?? ""} repo={current} reviewNonce={reviewNonce} />}
          {!waiting && mode === "skills" && <Skills reviewNonce={skillsReviewNonce} />}
          {palette && <Palette base={baseCommands} inlineAnswers={inlineAnswers} onOpen={open} onClose={closePalette} onDispatch={onDispatchText} />}
        </div>
      </LangContext.Provider>
    </EnvContext.Provider>
  );
}

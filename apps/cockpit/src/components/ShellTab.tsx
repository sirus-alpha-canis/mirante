// A side pane Terminal tab: the person's own shell number `n` beside a task (`shell.*`),
// in its worktree. A tab added with "+" starts its shell; a tab that comes back (another
// task and back, the app restarted) attaches to the shell if it still runs, else says
// "Shell ended" with Restart (shells do not survive a daemon restart). Closing the tab ends
// the shell. Nothing is ever typed into it but what the person types.
import { useCallback, useEffect, useRef, useState } from "react";
import { useEnv } from "../env";
import { useT } from "../lib/i18n";
import { M, type ShellView, type TaskView } from "../lib/protocol";
import { TerminalTab } from "./TerminalTab";

type Rpc = <T>(method: string, params?: object) => Promise<T>;

/** Shells the person asked for just now ("+"): their tab starts one when it first shows. */
const fresh = new Set<string>();
const key = (task: string, n: number) => `${task}:${n}`;

/** A tab for shell `n` of `task` was just added: it starts the shell. */
export function markFresh(task: string, n: number): void {
  fresh.add(key(task, n));
}

/** The tab of shell `n` of `task` closed: the shell ends (if it still runs). */
export function closeShell(rpc: Rpc, task: string, n: number): void {
  fresh.delete(key(task, n));
  rpc<ShellView[]>(M.shellList, { task })
    .then((all) => {
      const s = Array.isArray(all) ? all.find((x) => x.n === n) : undefined;
      if (s) return rpc(M.shellClose, { shell: s.shell });
    })
    .catch(() => {});
}

type Phase = { k: "starting" } | { k: "on"; shell: string } | { k: "ended" } | { k: "failed"; error: string };

export function ShellTab({ task, n }: { task: TaskView; n: number }) {
  const { rpc } = useEnv();
  const t = useT();
  const [phase, setPhase] = useState<Phase>({ k: "starting" });
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);
  const start = useCallback(() => {
    setPhase({ k: "starting" });
    fresh.delete(key(task.id, n));
    rpc<ShellView>(M.shellOpen, { task: task.id, n, rows: 24, cols: 80 }).then(
      (s) => alive.current && setPhase(s?.shell ? { k: "on", shell: s.shell } : { k: "failed", error: "the daemon named no shell" }),
      (e: unknown) => alive.current && setPhase({ k: "failed", error: e instanceof Error ? e.message : String(e) }),
    );
  }, [rpc, task.id, n]);
  useEffect(() => {
    rpc<ShellView[]>(M.shellList, { task: task.id }).then(
      (all) => {
        if (!alive.current) return;
        const s = Array.isArray(all) ? all.find((x) => x.n === n) : undefined;
        if (s) setPhase({ k: "on", shell: s.shell });
        else if (fresh.has(key(task.id, n))) start();
        else setPhase({ k: "ended" });
      },
      (e: unknown) => alive.current && setPhase({ k: "failed", error: e instanceof Error ? e.message : String(e) }),
    );
  }, [rpc, task.id, n, start]);
  // Stable for the terminal (it is memoized on the shell's id).
  const onEnd = useCallback(() => {
    if (alive.current) setPhase({ k: "ended" });
  }, []);
  return (
    <div className="pane-shell">
      <p className="muted small pane-shell-help">{t("shell.help")}</p>
      {phase.k === "starting" && <p className="muted small">{t("shell.starting")}</p>}
      {phase.k === "on" && (
        <div className="pane-terminal">
          <TerminalTab key={phase.shell} t={task} active source={{ kind: "shell", shell: phase.shell, n, onEnd }} />
        </div>
      )}
      {(phase.k === "ended" || phase.k === "failed") && (
        <p className={phase.k === "failed" ? "error small" : "muted small"} role={phase.k === "failed" ? "alert" : "status"}>
          {phase.k === "failed" ? t("shell.failed", { error: phase.error }) : t("shell.ended")}{" "}
          <button type="button" className="btn btn-small" onClick={start}>
            {t("shell.restart")}
          </button>
        </p>
      )}
    </div>
  );
}

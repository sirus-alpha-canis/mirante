import { type KeyboardEvent, useState } from "react";
import { useEnv } from "../env";
import { DISPATCH_HELP, parseDispatch, resolveRepo } from "../lib/dispatch";
import { M, type StartResult, type TaskView } from "../lib/protocol";
import type { Project } from "../store/select";

/** "@claude shop/feat/login add a login page" + Enter: new worktree, task and session. */
export function DispatchComposer(props: {
  projects: Project[];
  value: string;
  onChange(v: string): void;
  onDispatched?(taskId: string): void;
}) {
  const { rpc, cockpit } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    const p = parseDispatch(props.value);
    if (!p.ok) return setError(p.error);
    const repo = resolveRepo(p.value.repo, props.projects);
    if (!repo.ok) return setError(repo.error);
    const { agent, branch, goal } = p.value;
    setBusy(true);
    setError(null);
    try {
      const v = await rpc<TaskView>(M.taskCreate, { repo: repo.path, agent, branch, goal });
      cockpit.dispatch({ type: "task", view: v });
      props.onChange("");
      props.onDispatched?.(v.id);
      // Boot can take up to 45 s; progress arrives as events, so do not wait here.
      rpc<StartResult>(M.sessionStart, { task: v.id, prompt: goal }).catch((e) =>
        setError(`${branch} was created but the agent did not start: ${e instanceof Error ? e.message : e}`),
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void submit();
    }
  };

  return (
    <form
      className="composer"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <label htmlFor="dispatch" className="sr-only">
        Dispatch an agent
      </label>
      <textarea
        id="dispatch"
        rows={2}
        placeholder={DISPATCH_HELP}
        value={props.value}
        onChange={(e) => props.onChange(e.target.value)}
        onKeyDown={onKey}
        aria-describedby={error ? "dispatch-error" : undefined}
      />
      <button className="btn btn-primary" type="submit" disabled={busy || !props.value.trim()}>
        Dispatch
      </button>
      {error && (
        <p id="dispatch-error" className="error" role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

import { type KeyboardEvent, type Ref, useRef, useState } from "react";
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
  /** The textarea, for callers that move the keyboard to it. */
  inputRef?: Ref<HTMLTextAreaElement>;
}) {
  const { rpc, cockpit } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // A ref, not `busy`: a second Enter (or key auto-repeat) arrives before React re-renders.
  const inFlight = useRef(false);
  // The draft as it is now, for the moment the create returns (the user may keep typing).
  const latest = useRef(props.value);
  latest.current = props.value;

  const submit = async () => {
    if (inFlight.current) return;
    const sent = props.value;
    const p = parseDispatch(sent);
    if (!p.ok) return setError(p.error);
    const repo = resolveRepo(p.value.repo, props.projects);
    if (!repo.ok) return setError(repo.error);
    const { agent, branch, goal } = p.value;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      const v = await rpc<TaskView>(M.taskCreate, { repo: repo.path, agent, branch, goal });
      cockpit.dispatch({ type: "task", view: v });
      // Clear only what was dispatched: text typed meanwhile is the user's next message.
      if (latest.current === sent) props.onChange("");
      props.onDispatched?.(v.id);
      // Boot can take up to 45 s; progress arrives as events, so do not wait here.
      rpc<StartResult>(M.sessionStart, { task: v.id, prompt: goal }).catch((e) =>
        setError(`${branch} was created but the agent did not start: ${e instanceof Error ? e.message : e}`),
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
        ref={props.inputRef}
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

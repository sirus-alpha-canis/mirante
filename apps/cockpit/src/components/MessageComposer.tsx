import { useState } from "react";
import { useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import { M, RpcCode, type SendMode, type SendResult, type TaskView } from "../lib/protocol";

/**
 * Mid-run messages. "Steer now" reaches the agent at its next step (Claude: next tool
 * boundary; Codex: turn/steer); "Queue" is held by the daemon until the turn ends. When the
 * agent cannot be steered the daemon says so (UNSUPPORTED) and the fallback is offered:
 * interrupt, then send as a new prompt.
 */
export function MessageComposer({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const [text, setText] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const [fallback, setFallback] = useState(false);

  const send = async (mode: SendMode) => {
    if (!text.trim()) return;
    setNote(null);
    setFallback(false);
    try {
      const r = await rpc<SendResult>(M.messageSend, { task: t.id, text, mode });
      setText("");
      setNote(r.delivered === "steer" ? "sent now" : `queued (#${r.queued}, delivered when the turn ends)`);
    } catch (e) {
      const err = e instanceof DaemonError ? e : null;
      if (err?.code === RpcCode.UNSUPPORTED) setFallback(true);
      setNote(err?.code === RpcCode.NEEDS_YOU ? "answer the open request first" : String(err?.message ?? e));
    }
  };

  const interruptThenSend = async () => {
    try {
      await rpc(M.sessionInterrupt, { task: t.id });
      await send("queue");
    } catch (e) {
      setNote(e instanceof Error ? e.message : String(e));
    }
  };

  if (!t.live) return <p className="muted small composer-off">No live session: resume it to talk to the agent.</p>;
  return (
    <div className="composer">
      <label htmlFor={`msg-${t.id}`} className="sr-only">
        Message to the agent
      </label>
      <textarea
        id={`msg-${t.id}`}
        rows={2}
        value={text}
        placeholder="Message the agent (⌘↵ steer now, ⌥↵ queue)"
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
            e.preventDefault();
            void send("steer");
          } else if (e.key === "Enter" && e.altKey) {
            e.preventDefault();
            void send("queue");
          }
        }}
      />
      <div className="composer-actions">
        <button className="btn btn-primary" onClick={() => send("steer")} disabled={!text.trim()}>
          Steer now
        </button>
        <button className="btn" onClick={() => send("queue")} disabled={!text.trim()}>
          Queue
        </button>
        {fallback && (
          <button className="btn btn-warn" onClick={interruptThenSend}>
            Interrupt and send
          </button>
        )}
      </div>
      {note && (
        <p className="small muted" role="status">
          {note}
        </p>
      )}
    </div>
  );
}

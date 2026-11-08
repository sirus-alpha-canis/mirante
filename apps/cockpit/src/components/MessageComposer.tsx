import { useRef, useState } from "react";
import { useEnv, useLive } from "../env";
import { onMac } from "../lib/shortcuts";
import { useT } from "../lib/i18n";
import { DaemonError } from "../lib/daemon";
import { M, RpcCode, type SendMode, type SendResult, type TaskView } from "../lib/protocol";

/**
 * Mid-run messages. "Steer now" reaches the agent at its next step (Claude: next tool
 * boundary; Codex: turn/steer); "Queue" is held by the daemon until the turn ends. When the
 * agent cannot be steered the daemon says so (UNSUPPORTED, or an older daemon without the
 * method) and the fallback is offered: interrupt, then send as a new prompt.
 *
 * One send at a time: a double click, key auto-repeat or a second shortcut while a send is
 * in flight does nothing. Mount with `key={t.id}` so a draft never follows another task.
 */
export function MessageComposer({ t }: { t: TaskView }) {
  const { rpc } = useEnv();
  const live = useLive();
  const tt = useT();
  const [mac] = useState(onMac);
  const [text, setText] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const [fallback, setFallback] = useState(false);
  // A ref, not only `busy`: a second click arrives before React re-renders.
  const inFlight = useRef(false);
  const [busy, setBusy] = useState(false);

  const guarded = async (work: () => Promise<void>) => {
    if (inFlight.current) return;
    inFlight.current = true;
    setBusy(true);
    try {
      await work();
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  /** Sends `msg` (captured when the user acted); clears the box only if it still holds it. */
  const deliver = async (msg: string, mode: SendMode) => {
    try {
      const r = await rpc<SendResult>(M.messageSend, { task: t.id, text: msg, mode });
      setText((cur) => (cur === msg ? "" : cur));
      setNote(r.delivered === "steer" ? tt("composer.sentNow") : tt("composer.queued", { n: r.queued ?? "?" }));
    } catch (e) {
      const err = e instanceof DaemonError ? e : null;
      if (err?.unsupported) setFallback(true);
      setNote(err?.code === RpcCode.NEEDS_YOU ? tt("composer.answerFirst") : String(err?.message ?? e));
    }
  };

  const send = (mode: SendMode) => {
    const msg = text;
    if (!msg.trim() || !live) return;
    return guarded(async () => {
      setNote(null);
      setFallback(false);
      await deliver(msg, mode);
    });
  };

  const interruptThenSend = () => {
    const msg = text;
    if (!msg.trim()) return;
    return guarded(async () => {
      setFallback(false);
      setNote(null);
      try {
        await rpc(M.sessionInterrupt, { task: t.id });
      } catch (e) {
        setNote(e instanceof Error ? e.message : String(e));
        return;
      }
      await deliver(msg, "queue");
    });
  };

  if (!t.live) return <p className="muted small composer-off">{tt("composer.noLive")}</p>;
  // Nothing can be sent while the daemon is away.
  const empty = !text.trim() || !live;
  return (
    <div className="composer">
      <label htmlFor={`msg-${t.id}`} className="sr-only">
        {tt("composer.label")}
      </label>
      <textarea
        id={`msg-${t.id}`}
        rows={2}
        value={text}
        placeholder={tt("composer.placeholder", { steer: mac ? "⌘↵" : "Ctrl+↵", queue: mac ? "⌥↵" : "Alt+↵" })}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          // Enter that confirms an IME composition (Japanese, Chinese…) is not a send.
          if (e.nativeEvent.isComposing || e.keyCode === 229) return;
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
        <button className="btn btn-primary" onClick={() => send("steer")} disabled={empty || busy}>
          {tt("composer.steer")}
        </button>
        <button className="btn" onClick={() => send("queue")} disabled={empty || busy}>
          {tt("composer.queue")}
        </button>
        {fallback && (
          <button className="btn btn-warn" onClick={interruptThenSend} disabled={empty || busy}>
            {tt("composer.interruptSend")}
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

import { useMemo, useRef, useState } from "react";
import { useEnv, useStore } from "../env";
import { DaemonError } from "../lib/daemon";
import { type DaemonStatus, M, RpcCode, type SendMode, type SendResult, type TaskView } from "../lib/protocol";
import { columnOf, gridTasks, lastLines, pendingPerms } from "../store/select";
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { HibernateButton } from "./MemoryPanel";
import { PermissionCard } from "./PermissionCard";
import { TrustCard } from "./TrustCard";

function Tile({ t, onOpen, inlineAnswers }: { t: TaskView; onOpen(id: string, tab?: "Terminal"): void; inlineAnswers: boolean }) {
  const s = useStore();
  const { rpc } = useEnv();
  const [text, setText] = useState("");
  const [note, setNote] = useState<string | null>(null);
  // One send at a time (key auto-repeat must not send twice).
  const inFlight = useRef(false);
  const perms = useMemo(() => pendingPerms(s.perms, t.id), [s.perms, t.id]);
  const perm = perms[0];
  const col = columnOf(t, s.feed[t.id]);
  const unexplained = t.status === "needs_you" && perms.length === 0 && t.boot !== "trust_prompt";
  const send = async (mode: SendMode) => {
    const msg = text;
    if (!msg.trim() || inFlight.current) return;
    inFlight.current = true;
    setNote(null);
    try {
      const r = await rpc<SendResult>(M.messageSend, { task: t.id, text: msg, mode });
      setText((cur) => (cur === msg ? "" : cur));
      setNote(r?.delivered === "steer" ? "sent" : "queued");
    } catch (e) {
      const err = e instanceof DaemonError ? e : null;
      setNote(err?.code === RpcCode.NEEDS_YOU ? "answer the open request first" : String(err?.message ?? (e instanceof Error ? e.message : e)));
    } finally {
      inFlight.current = false;
    }
  };
  return (
    <article className={`tile card-${col}`} aria-label={`${t.agent} on ${t.branch}`}>
      <header className="card-head">
        <button className="link" onClick={() => onOpen(t.id)}>
          {t.title}
        </button>
        <StatusPill column={col} />
      </header>
      <TaskBadges t={t} />
      <ol className="lines">
        {lastLines(s.feed[t.id], 4).map((i) => (
          <FeedLine key={i.seq} item={i} />
        ))}
      </ol>
      {t.boot === "trust_prompt" && <TrustCard taskId={t.id} />}
      {unexplained && (
        <p className="perm perm-compact">
          <span className="muted">Waiting for you in the agent's terminal.</span>{" "}
          <button className="btn btn-small" onClick={() => onOpen(t.id, "Terminal")}>
            Open terminal
          </button>
        </p>
      )}
      {perm && (
        <PermissionCard
          key={perm.id}
          perm={perm}
          agent={t.agent}
          inlineAnswers={inlineAnswers}
          onAnswerInTerminal={() => onOpen(t.id, "Terminal")}
          compact
        />
      )}
      {t.live && (
        <input
          className="mini"
          aria-label={`Message ${t.branch} (Enter steer, Alt+Enter queue)`}
          placeholder="Enter steer · ⌥↵ queue"
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            // Enter that confirms an IME composition is not a send.
            if (e.nativeEvent.isComposing || e.keyCode === 229) return;
            if (e.key === "Enter") {
              e.preventDefault();
              void send(e.altKey ? "queue" : "steer");
            }
          }}
        />
      )}
      <footer className="tile-foot">
        {note && (
          <span className="small muted" role="status">
            {note}
          </span>
        )}
        <HibernateButton t={t} />
      </footer>
    </article>
  );
}

/** V2, the agent grid: up to six tiles, needs-you first, then working, then recently finished. */
export function Grid({ onOpen, status }: { onOpen(id: string, tab?: "Terminal"): void; status: DaemonStatus | null }) {
  const s = useStore();
  const tiles = useMemo(() => gridTasks(s), [s]);
  const inlineAnswers = status?.claude_inline_answers ?? false;
  if (tiles.length === 0) return <main className="grid-empty muted">No active agents. Dispatch one from Home (⌘1).</main>;
  return (
    <main className={`grid grid-${Math.max(2, tiles.length)}`} aria-label="Agent grid">
      {tiles.map((t) => (
        <Tile key={t.id} t={t} onOpen={onOpen} inlineAnswers={inlineAnswers} />
      ))}
    </main>
  );
}

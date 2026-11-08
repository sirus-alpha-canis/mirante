import { memo, useRef, useState } from "react";
import { shallow, useEnv, useSelect } from "../env";
import { DaemonError } from "../lib/daemon";
import { collapseRepeats } from "../lib/feed";
import { type DaemonStatus, M, RpcCode, type SendMode, type SendResult, type TaskView } from "../lib/protocol";
import { hintsFor, onMac } from "../lib/shortcuts";
import { columnOf, gridTasks, lastLines, pendingPerms } from "../store/select";
import { FeedLine } from "./AgentCard";
import { StatusPill, TaskBadges } from "./Badges";
import { HibernateButton } from "./MemoryPanel";
import { PermissionCard } from "./PermissionCard";
import { TrustCard } from "./TrustCard";
import { useT } from "../lib/i18n";

const Tile = memo(function Tile({ t, onOpen, inlineAnswers }: { t: TaskView; onOpen(id: string, tab?: "Terminal"): void; inlineAnswers: boolean }) {
  const { rpc } = useEnv();
  const tt = useT();
  const feed = useSelect((s) => s.feed[t.id]);
  const [text, setText] = useState("");
  const [note, setNote] = useState<string | null>(null);
  // One send at a time (key auto-repeat must not send twice).
  const inFlight = useRef(false);
  const perms = useSelect((s) => pendingPerms(s.perms, t.id), shallow);
  const perm = perms[0];
  const col = columnOf(t, feed);
  const unexplained = t.status === "needs_you" && perms.length === 0 && t.boot !== "trust_prompt";
  const send = async (mode: SendMode) => {
    const msg = text;
    if (!msg.trim() || inFlight.current) return;
    inFlight.current = true;
    setNote(null);
    try {
      const r = await rpc<SendResult>(M.messageSend, { task: t.id, text: msg, mode });
      setText((cur) => (cur === msg ? "" : cur));
      setNote(tt(r?.delivered === "steer" ? "grid.sent" : "grid.queued"));
    } catch (e) {
      const err = e instanceof DaemonError ? e : null;
      setNote(err?.code === RpcCode.NEEDS_YOU ? tt("grid.answerFirst") : String(err?.message ?? (e instanceof Error ? e.message : e)));
    } finally {
      inFlight.current = false;
    }
  };
  return (
    <article className={`tile card-${col}`} aria-label={tt("card.label", { agent: t.agent, branch: t.branch })}>
      <header className="card-head">
        <button className="link" onClick={() => onOpen(t.id)}>
          {t.title}
        </button>
        <StatusPill column={col} />
      </header>
      <TaskBadges t={t} />
      <ol className="lines">
        {collapseRepeats(lastLines(feed, 12))
          .slice(-4)
          .map((f) => (
            <FeedLine key={f.item.seq} item={f.item} count={f.count} />
          ))}
      </ol>
      {t.boot === "trust_prompt" && <TrustCard taskId={t.id} />}
      {unexplained && (
        <p className="perm perm-compact">
          <span className="muted">{tt("card.waitingTerminal")}</span>{" "}
          <button className="btn btn-small" onClick={() => onOpen(t.id, "Terminal")}>
            {tt("card.openTerminal")}
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
          aria-label={tt("grid.messageLabel", { branch: t.branch })}
          placeholder={tt("grid.messagePlaceholder")}
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
});

/** V2, the agent grid: up to six tiles, needs-you first, then working, then recently finished. */
export function Grid({ onOpen, status }: { onOpen(id: string, tab?: "Terminal"): void; status: DaemonStatus | null }) {
  const t = useT();
  const tiles = useSelect(gridTasks, shallow);
  const inlineAnswers = status?.claude_inline_answers ?? false;
  if (tiles.length === 0) return <main className="grid-empty muted">{t("grid.empty", { key: hintsFor(onMac()).home })}</main>;
  return (
    <main className={`grid grid-${Math.max(2, tiles.length)}`} aria-label={t("grid.label")}>
      {tiles.map((t) => (
        <Tile key={t.id} t={t} onOpen={onOpen} inlineAnswers={inlineAnswers} />
      ))}
    </main>
  );
}

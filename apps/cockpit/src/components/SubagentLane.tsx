// The "Inside Claude" lane (Plan threads, Task 17): the agents Claude itself started inside
// this task (its subagents), read-only. Each one shows its type, what it was asked to do,
// running or done, how long, its tool calls (grouped from the task's steps by `agent_id`),
// its latest text and any permission it asks for. Mirante never steers, stops or answers
// them from here: there is no control but "Open beside". Their texts are the agents'
// (redacted and capped by the daemon): shown as text nodes only.
import { memo, useCallback, useEffect, useMemo, useState } from "react";
import { shallow, useBoot, useEnv, useSelect } from "../env";
import { describeRun } from "../lib/feed";
import { elapsed } from "../lib/inbox";
import { tn, useT } from "../lib/i18n";
import { M, type SubagentView } from "../lib/protocol";
import { useNow } from "../lib/useNow";
import type { FeedItem, State } from "../store/state";
import { pendingPerms } from "../store/select";
import { FeedLine } from "./AgentCard";
import { usePane } from "./paneContext";

/** How often a running agent's latest text is read again. */
export const SUBAGENT_POLL_MS = 4000;
const NO_LANES: SubagentView[] = [];
const NO_STEPS: FeedItem[] = [];

/** `task.subagents` for `taskId`, fetched again on its `subagent.*` events, on a reconnect
 * and, while one still runs, every few seconds (its side file grows). */
function useSubagents(taskId: string): { lanes: SubagentView[]; loaded: boolean } {
  const { rpc } = useEnv();
  const boot = useBoot();
  const rev = useSelect(useCallback((s: State) => s.subagentRev[taskId] ?? 0, [taskId]));
  const live = useSelect(useCallback((s: State) => !!s.tasks[taskId]?.live, [taskId]));
  const [state, setState] = useState<{ lanes: SubagentView[]; loaded: boolean }>({ lanes: NO_LANES, loaded: false });
  const [tick, setTick] = useState(0);
  const running = state.lanes.some((l) => l.status === "running");
  useEffect(() => {
    let cancel = false;
    rpc<SubagentView[]>(M.taskSubagents, { task: taskId }).then(
      (lanes) => !cancel && setState({ lanes: Array.isArray(lanes) && lanes.length ? lanes : NO_LANES, loaded: true }),
      () => !cancel && setState((s) => ({ ...s, loaded: true })),
    );
    return () => {
      cancel = true;
    };
  }, [rpc, taskId, rev, live, boot, tick]);
  useEffect(() => {
    if (!running) return;
    const id = setInterval(() => setTick((n) => n + 1), SUBAGENT_POLL_MS);
    return () => clearInterval(id);
  }, [running]);
  return state;
}

/** The task's steps one agent ran (its `agent_id`). */
function useSteps(taskId: string, id: string): FeedItem[] {
  const feed = useSelect(useCallback((s: State) => s.feed[taskId] ?? NO_STEPS, [taskId]));
  return useMemo(() => {
    const mine = feed.filter((i) => i.agent === id);
    return mine.length ? mine : NO_STEPS;
  }, [feed, id]);
}

const Lane = memo(function Lane({ taskId, lane }: { taskId: string; lane: SubagentView }) {
  const t = useT();
  const pane = usePane();
  const now = useNow(30_000);
  const steps = useSteps(taskId, lane.id);
  const asks = useSelect(
    useCallback((s: State) => pendingPerms(s.perms, taskId).filter((p) => lane.permissions.includes(p.id)), [taskId, lane.permissions]),
    shallow,
  );
  const type = lane.agent_type || "agent";
  const description = lane.description || t("subagents.noDescription");
  const took = (lane.ended_ms ?? now) - lane.started_ms;
  return (
    <li className={`subagent subagent-${lane.status}`} aria-label={t("subagents.agentLabel", { type, description })}>
      <div className="subagent-head">
        <span className="tag">{type}</span>
        <span className="subagent-desc">{description}</span>
        <span className={`pill pill-${lane.status === "running" ? "working" : "idle"}`}>{t(lane.status === "running" ? "subagents.running" : "subagents.done")}</span>
        <span className="small muted">{elapsed(took)}</span>
        <span className="small muted">{tn(lane.tools, "subagents.toolsOne", "subagents.toolsMany")}</span>
        {pane && (
          <button type="button" className="btn btn-small btn-ghost" title={t("subagents.openTitle")} onClick={() => pane.openTarget({ k: "subagent", id: lane.id })}>
            {t("subagents.open")}
          </button>
        )}
      </div>
      {asks.map((p) => (
        <p key={p.id} className="small warn" role="status">
          {t("subagents.asks", { what: `${p.tool} ${p.detail}`.trim() })}
        </p>
      ))}
      {steps.length > 0 && <p className="small muted subagent-steps">{describeRun(steps)}</p>}
      {/* The agent's text: a text node, clamped to three lines. */}
      {lane.last_text && <p className="answer-text answer-clamp3">{lane.last_text}</p>}
    </li>
  );
});

/** The lane, inside a Claude task's Detail; nothing while Claude started no agent. */
export const SubagentLane = memo(function SubagentLane({ taskId }: { taskId: string }) {
  const t = useT();
  const { lanes } = useSubagents(taskId);
  if (lanes.length === 0) return null;
  return (
    <section className="subagent-lane" aria-label={t("subagents.title")}>
      <header className="subagent-lane-head">
        <h2>{t("subagents.title")}</h2>
        <span className="small muted">{t("subagents.hint")}</span>
      </header>
      <ul className="subagents">
        {lanes.map((l) => (
          <Lane key={l.id} taskId={taskId} lane={l} />
        ))}
      </ul>
    </section>
  );
});

/** The side pane's "subagent" target: one agent's latest text and its steps. */
export function SubagentPane({ taskId, id }: { taskId: string; id: string }) {
  const t = useT();
  const { lanes, loaded } = useSubagents(taskId);
  const steps = useSteps(taskId, id);
  const lane = lanes.find((l) => l.id === id);
  if (!lane) return loaded ? <p className="muted small">{t("subagents.gone")}</p> : null;
  return (
    <div className="subagent-pane">
      <p className="small muted">
        <span className="tag">{lane.agent_type || "agent"}</span> {lane.description || t("subagents.noDescription")} ·{" "}
        {t(lane.status === "running" ? "subagents.running" : "subagents.done")}
      </p>
      <h3>{t("subagents.lastText")}</h3>
      {lane.last_text ? <p className="answer-text subagent-text">{lane.last_text}</p> : <p className="muted small">{t("subagents.noText")}</p>}
      {steps.length > 0 && (
        <>
          <h3>{t("subagents.steps")}</h3>
          <ol className="lines">
            {steps.map((i) => (
              <FeedLine key={i.seq} item={i} />
            ))}
          </ol>
        </>
      )}
    </div>
  );
}

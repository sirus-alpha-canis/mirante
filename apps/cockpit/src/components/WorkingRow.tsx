import { memo } from "react";
import { useSelect } from "../env";
import { plainStep } from "../lib/feed";
import { elapsed } from "../lib/inbox";
import type { TaskView } from "../lib/protocol";
import { useNow } from "../lib/useNow";
import { columnOf } from "../store/select";
import type { FeedItem } from "../store/state";
import { AutoModeBadge, StatusPill } from "./Badges";

/** The newest line that says what the agent does (not a turn marker). */
function lastStep(feed: FeedItem[] | undefined): FeedItem | undefined {
  if (!feed) return undefined;
  for (let i = feed.length - 1; i >= 0; i--) if (feed[i].kind !== "turn" && feed[i].kind !== "system") return feed[i];
  return undefined;
}

/** When the running turn started (else undefined). */
function turnStart(feed: FeedItem[] | undefined): number | undefined {
  if (!feed) return undefined;
  for (let i = feed.length - 1; i >= 0; i--) {
    const f = feed[i];
    if (f.kind === "turn") return f.phase === "started" ? f.ts : undefined;
  }
  return undefined;
}

/** A working task in one line: title · agent · elapsed · what it is doing, in plain words. */
export const WorkingRow = memo(function WorkingRow({ t, onOpen }: { t: TaskView; onOpen(id: string): void }) {
  const step = useSelect((s) => lastStep(s.feed[t.id]));
  const since = useSelect((s) => turnStart(s.feed[t.id])) ?? t.updated_at;
  const col = useSelect((s) => columnOf(t, s.feed[t.id]));
  const now = useNow(30_000);
  return (
    <article className="row" aria-label={`${t.agent} on ${t.branch}`}>
      <button className="link row-title" onClick={() => onOpen(t.id)}>
        {t.title}
      </button>
      <span className="muted small">
        @{t.agent} · {elapsed(now - since)} · {plainStep(step)}
      </span>
      <AutoModeBadge t={t} />
      <StatusPill column={col} />
    </article>
  );
});

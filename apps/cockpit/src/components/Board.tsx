import { useMemo } from "react";
import { useStore } from "../env";
import { BOARD, COLUMN_LABEL, tasksIn } from "../store/select";

/** Right rail: every project's tasks by what they need. */
export function Board({ onOpen }: { onOpen(id: string): void }) {
  const s = useStore();
  const cols = useMemo(() => BOARD.map((c) => ({ c, tasks: tasksIn(s, c) })), [s]);
  return (
    <aside className="board" aria-label="Board">
      {cols.map(({ c, tasks }) => (
        <section key={c} aria-label={COLUMN_LABEL[c]}>
          <h2>
            {COLUMN_LABEL[c]} <span className="count">{tasks.length}</span>
          </h2>
          <ul>
            {tasks.map((t) => (
              <li key={t.id}>
                <button className={`chip chip-${c}`} onClick={() => onOpen(t.id)}>
                  <span className="chip-title">{t.title}</span>
                  <span className="muted">
                    @{t.agent} · {t.branch}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </aside>
  );
}

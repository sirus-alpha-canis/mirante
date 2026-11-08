import { shallow, useSelect } from "../env";
import { BOARD, COLUMN_LABEL, tasksIn } from "../store/select";
import type { State } from "../store/state";
import type { TaskView } from "../lib/protocol";

const selectColumns = (s: State) => BOARD.map((c) => ({ c, tasks: tasksIn(s, c) }));
const sameColumns = (a: { tasks: TaskView[] }[], b: { tasks: TaskView[] }[]) => a.every((x, i) => shallow(x.tasks, b[i].tasks));

/** Right rail: every project's tasks by what they need. */
export function Board({ onOpen }: { onOpen(id: string): void }) {
  const cols = useSelect(selectColumns, sameColumns);
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

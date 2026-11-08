import { type KeyboardEvent, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useEnv, useStore } from "../env";
import { DaemonError } from "../lib/daemon";
import { M, type QueuedMessage, RpcCode, type TaskView } from "../lib/protocol";

type Act = "edit" | "up" | "down";

/**
 * The task's queue (spec §2.4: delivered when the turn ends; editable and reorderable).
 * Each change is one daemon call that returns the queue after it; a message that left the
 * queue meanwhile (delivered, or being delivered) is refused with CONFLICT and the list is
 * reloaded. One change at a time.
 */
export function QueueList({ t }: { t: TaskView }) {
  const { rpc, cockpit } = useEnv();
  const list = useStore().queues[t.id];
  const [editing, setEditing] = useState<{ id: string; text: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inFlight = useRef(false);
  const root = useRef<HTMLElement>(null);
  // Where the keyboard goes once the list re-renders (the moved or edited message).
  const focusNext = useRef<{ id: string; act: Act } | null>(null);

  const loaded = list !== undefined;
  useEffect(() => {
    // Later changes arrive as events (the store refetches on them).
    if (!loaded && t.queued > 0) cockpit.refreshQueue(t.id);
  }, [cockpit, loaded, t.id, t.queued]);

  useLayoutEffect(() => {
    const want = focusNext.current;
    if (!want || !root.current) return;
    const row = [...root.current.querySelectorAll<HTMLElement>("[data-id]")].find((el) => el.dataset.id === want.id);
    const button = (act: Act) => row?.querySelector<HTMLButtonElement>(`[data-act="${act}"]`);
    // At an edge its Up/Down is off: the other one keeps the keyboard on the message.
    const pick = [button(want.act), button(want.act === "up" ? "down" : "up"), button("edit")].find((b) => b && !b.disabled);
    // Not rendered yet (the edit box is still closing): try again on the next render.
    if (!pick) return;
    focusNext.current = null;
    pick.focus();
  });

  const change = async (method: string, params: object, focus?: { id: string; act: Act }) => {
    if (inFlight.current) return false;
    inFlight.current = true;
    setBusy(true);
    setError(null);
    try {
      const after = await rpc<QueuedMessage[]>(method, params);
      focusNext.current = focus ?? null;
      cockpit.dispatch({ type: "queue", taskId: t.id, list: after });
      return true;
    } catch (e) {
      const conflict = e instanceof DaemonError && e.code === RpcCode.CONFLICT;
      setError(conflict ? "That message is already being delivered (or was): it can no longer be changed." : e instanceof Error ? e.message : String(e));
      if (conflict) cockpit.refreshQueue(t.id);
      return false;
    } finally {
      inFlight.current = false;
      setBusy(false);
    }
  };

  if (!list || list.length === 0) return null;

  const move = (i: number, by: -1 | 1) => {
    const ids = list.map((m) => m.id);
    [ids[i], ids[i + by]] = [ids[i + by], ids[i]];
    void change(M.messageReorder, { task_id: t.id, ids }, { id: list[i].id, act: by < 0 ? "up" : "down" });
  };
  const save = async () => {
    if (!editing || !editing.text.trim()) return;
    const id = editing.id;
    if (await change(M.messageEdit, { id, text: editing.text }, { id, act: "edit" })) setEditing(null);
  };
  const cancel = (id: string) => {
    focusNext.current = { id, act: "edit" };
    setEditing(null);
  };
  const onEditKey = (e: KeyboardEvent<HTMLTextAreaElement>, id: string) => {
    if (e.nativeEvent.isComposing || e.keyCode === 229) return;
    if (e.key === "Escape") {
      e.preventDefault();
      cancel(id);
    } else if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void save();
    }
  };

  return (
    <section className="queue" aria-label="Queued messages" ref={root}>
      <h2 className="small muted">Queued · delivered when the turn ends, in this order</h2>
      <ol>
        {list.map((m, i) => {
          const n = i + 1;
          const isEditing = editing?.id === m.id;
          return (
            <li key={m.id} data-id={m.id} className="queue-item">
              {isEditing ? (
                <form
                  className="queue-edit"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void save();
                  }}
                >
                  <textarea
                    aria-label={`Edit queued message ${n}`}
                    rows={2}
                    autoFocus
                    value={editing.text}
                    onChange={(e) => setEditing({ id: m.id, text: e.target.value })}
                    onKeyDown={(e) => onEditKey(e, m.id)}
                  />
                  <button className="btn btn-small btn-primary" type="submit" disabled={busy || !editing.text.trim()}>
                    Save
                  </button>
                  <button className="btn btn-small" type="button" onClick={() => cancel(m.id)}>
                    Cancel
                  </button>
                </form>
              ) : (
                <>
                  <span className="text">{m.text}</span>
                  <span className="queue-actions">
                    <button
                      className="btn btn-small btn-ghost"
                      data-act="up"
                      aria-label={`Move queued message ${n} up`}
                      disabled={busy || i === 0}
                      onClick={() => move(i, -1)}
                    >
                      ↑
                    </button>
                    <button
                      className="btn btn-small btn-ghost"
                      data-act="down"
                      aria-label={`Move queued message ${n} down`}
                      disabled={busy || i === list.length - 1}
                      onClick={() => move(i, 1)}
                    >
                      ↓
                    </button>
                    <button
                      className="btn btn-small"
                      data-act="edit"
                      aria-label={`Edit queued message ${n}`}
                      disabled={busy || editing !== null}
                      onClick={() => setEditing({ id: m.id, text: m.text })}
                    >
                      Edit
                    </button>
                    <button
                      className="btn btn-small"
                      aria-label={`Remove queued message ${n}`}
                      disabled={busy}
                      onClick={() => void change(M.messageRemove, { id: m.id })}
                    >
                      Remove
                    </button>
                  </span>
                </>
              )}
            </li>
          );
        })}
      </ol>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

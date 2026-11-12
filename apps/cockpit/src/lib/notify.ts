// Native notification when a task starts needing the user, unless that task is already on
// screen in the focused window (the decision is the client's: only it knows what is shown).
import { isPermissionGranted, onAction, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import { t as tr } from "./i18n";
import type { TaskView } from "./protocol";

export interface Note {
  title: string;
  body: string;
  /** The task a click on it opens (none for a summary). */
  taskId?: string;
}

/** Past this many tasks at once (a reconnect, a restarted daemon), one summary instead. */
export const SUMMARY_OVER = 2;
/** Longest notification line, in characters (an agent-chosen title can be anything). */
export const NOTE_MAX = 120;

export function shouldNotify(taskId: string, visible: ReadonlySet<string>, focused: boolean): boolean {
  return !(focused && visible.has(taskId));
}

/** One line of text for the OS: no control characters, whitespace collapsed, truncated. */
export function clean(text: string): string {
  const flat = text
    .replace(/\s+/gu, " ")
    .replace(/\p{Cc}/gu, "")
    .trim();
  return flat.length > NOTE_MAX ? `${flat.slice(0, NOTE_MAX - 1)}…` : flat;
}

export function notificationFor(t: TaskView): Note {
  const title = tr(t.boot === "trust_prompt" ? "notify.trust" : "notify.needs", { branch: t.branch });
  return { title: clean(title), body: clean(t.title), taskId: t.id };
}

/** What to show for the tasks that just started needing the user, all at once. */
export function notificationsFor(tasks: TaskView[]): Note[] {
  // Children of one fan-out group count as one item: "<group>: 2 children need you".
  const items: (TaskView | TaskView[])[] = [];
  const byGroup = new Map<string, TaskView[]>();
  for (const t of tasks) {
    if (!t.group_id) {
      items.push(t);
      continue;
    }
    const list = byGroup.get(t.group_id);
    if (list) list.push(t);
    else {
      const fresh = [t];
      byGroup.set(t.group_id, fresh);
      items.push(fresh);
    }
  }
  const one = (i: TaskView | TaskView[]): Note => {
    if (!Array.isArray(i)) return notificationFor(i);
    if (i.length === 1) return notificationFor(i[0]);
    const title = tr("notify.group", { title: i[0].group?.title ?? i[0].branch, n: i.length });
    return { title: clean(title), body: clean(i.map((t) => t.branch).join(", ")), taskId: i[0].id };
  };
  if (items.length <= SUMMARY_OVER) return items.map(one);
  return [{ title: tr("notify.many", { n: tasks.length }), body: clean(tasks.map((t) => t.branch).join(", ")) }];
}

export interface NotificationApi {
  isPermissionGranted(): Promise<boolean>;
  requestPermission(): Promise<string>;
  sendNotification(n: Note): void;
}

/**
 * A sender that asks the OS at most once per run. Concurrent first calls share that one
 * check. After a refusal it never prompts again, but it re-checks the current permission
 * (no prompt) on each later call, so a grant given in System Settings is honoured.
 */
export function createNotifier(api: NotificationApi): (n: Note) => Promise<void> {
  let first: Promise<boolean> | null = null;
  let granted = false;
  const allowed = async (): Promise<boolean> => {
    if (granted) return true;
    if (!first) {
      first = (async () => (await api.isPermissionGranted()) || (await api.requestPermission()) === "granted")();
      granted = await first;
      return granted;
    }
    // A check already ran (or is running): wait for it; if it said no, look again quietly.
    if (await first) return true;
    granted = await api.isPermissionGranted();
    return granted;
  };
  return async (n) => {
    if (await allowed()) api.sendNotification(n);
  };
}

/** The task a clicked notification is about: its `extra.task` when the platform reports it,
 * else the task of the last notification sent with that title. */
export function clickedTask(
  performed: { actionId: string; notification: { title?: string; extra?: Record<string, unknown> } | null },
  sentTitles: ReadonlyMap<string, string>,
): string | null {
  if (performed.actionId !== "tap" || !performed.notification) return null;
  const extra = performed.notification.extra?.task;
  if (typeof extra === "string") return extra;
  return (performed.notification.title && sentTitles.get(performed.notification.title)) || null;
}

// Titles of notifications sent this run, to their task (the newest 50).
const sent = new Map<string, string>();

export const notify = createNotifier({
  isPermissionGranted,
  requestPermission,
  sendNotification: (n) => {
    if (n.taskId) {
      sent.delete(n.title);
      sent.set(n.title, n.taskId);
      while (sent.size > 50) sent.delete(sent.keys().next().value!);
    }
    sendNotification({ title: n.title, body: n.body, ...(n.taskId ? { extra: { task: n.taskId } } : {}) });
  },
});

/** Calls `fn` with the task of each notification the person clicks (the app is brought
 * forward by the system). */
export function onNotificationClick(fn: (taskId: string) => void): () => void {
  let stop: (() => void) | null = null;
  let stopped = false;
  onAction((e) => {
    const id = clickedTask(e, sent);
    if (id) fn(id);
  })
    .then((l) => {
      if (stopped) void l.unregister();
      else stop = () => void l.unregister();
    })
    .catch(() => {
      // No listener (an older plugin, or refused): a click just brings the app forward.
    });
  return () => {
    stopped = true;
    stop?.();
  };
}

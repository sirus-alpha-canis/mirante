// Native notification when a task starts needing the user, unless that task is already on
// screen in the focused window (the decision is the client's: only it knows what is shown).
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { TaskView } from "./protocol";

export interface Note {
  title: string;
  body: string;
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
  const why = t.boot === "trust_prompt" ? "asks to trust its folder" : "needs you";
  return { title: clean(`${t.branch} ${why}`), body: clean(t.title) };
}

/** What to show for the tasks that just started needing the user, all at once. */
export function notificationsFor(tasks: TaskView[]): Note[] {
  if (tasks.length <= SUMMARY_OVER) return tasks.map(notificationFor);
  return [{ title: `${tasks.length} agents need you`, body: clean(tasks.map((t) => t.branch).join(", ")) }];
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

export const notify = createNotifier({ isPermissionGranted, requestPermission, sendNotification });

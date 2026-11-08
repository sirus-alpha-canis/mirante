// Native notification when a task starts needing the user, unless that task is already on
// screen in the focused window (the decision is the client's: only it knows what is shown).
import { isPermissionGranted, requestPermission, sendNotification } from "@tauri-apps/plugin-notification";
import type { TaskView } from "./protocol";

export function shouldNotify(taskId: string, visible: ReadonlySet<string>, focused: boolean): boolean {
  return !(focused && visible.has(taskId));
}

export function notificationFor(t: TaskView): { title: string; body: string } {
  const why = t.boot === "trust_prompt" ? "asks to trust its folder" : "needs you";
  return { title: `${t.branch} ${why}`, body: t.title };
}

let granted: boolean | null = null;

/** Asks the OS once per run; a refusal is respected (no notification, no new prompt). */
export async function notify(t: TaskView): Promise<void> {
  if (granted === null) {
    granted = await isPermissionGranted();
    if (!granted) granted = (await requestPermission()) === "granted";
  }
  if (granted) sendNotification(notificationFor(t));
}

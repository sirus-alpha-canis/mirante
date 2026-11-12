import { describe, expect, it, vi } from "vitest";
import { task } from "../test/fixtures";
import { clean, clickedTask, createNotifier, notificationFor, notificationsFor, SUMMARY_OVER, shouldNotify } from "./notify";

describe("shouldNotify", () => {
  it("stays quiet only when the task is on screen in the focused window", () => {
    const visible = new Set(["a"]);
    expect(shouldNotify("a", visible, true)).toBe(false);
    expect(shouldNotify("a", visible, false)).toBe(true);
    expect(shouldNotify("b", visible, true)).toBe(true);
  });

  it("says why", () => {
    expect(notificationFor(task("a", { boot: "trust_prompt" })).title).toBe("feat/a asks to trust its folder");
    expect(notificationFor(task("a", { status: "needs_you" }))).toEqual({ title: "feat/a needs you", body: "goal of a", taskId: "a" });
  });
});

describe("notification text", () => {
  it("strips control characters, collapses whitespace and truncates", () => {
    expect(clean("a\u0007b\u001b[31m  c\n\t d")).toBe("ab[31m c d");
    const long = clean("x".repeat(500));
    expect(long).toHaveLength(120);
    expect(long.endsWith("…")).toBe(true);
    const n = notificationFor(task("a", { title: "fix\nthe   cart\u0000" }));
    expect(n.body).toBe("fix the cart");
  });

  it(`more than ${SUMMARY_OVER} tasks at once become one summary`, () => {
    const two = notificationsFor([task("a"), task("b")]);
    expect(two.map((n) => n.title)).toEqual(["feat/a needs you", "feat/b needs you"]);
    const many = notificationsFor([task("a"), task("b"), task("c")]);
    expect(many).toEqual([{ title: "3 agents need you", body: "feat/a, feat/b, feat/c" }]);
    expect(notificationsFor([])).toEqual([]);
  });
});

describe("fan-out groups", () => {
  it("children of one group turning needs-you together give one notification", () => {
    const g = { id: "g1", kind: "compare" as const, title: "fix login", goal: "x", branch: "mirante/x", created_at: 1 };
    const c = (id: string) => task(id, { status: "needs_you", group_id: "g1", group: g });
    expect(notificationsFor([c("a"), c("b"), task("solo")])).toEqual([
      { title: "fix login: 2 children need you", body: "feat/a, feat/b", taskId: "a" },
      { title: "feat/solo needs you", body: "goal of solo", taskId: "solo" },
    ]);
    // One child alone is an ordinary notification.
    expect(notificationsFor([c("a")])[0].title).toBe("feat/a needs you");
  });
});

describe("createNotifier", () => {
  function api(granted: boolean, answer: "granted" | "denied" = "denied") {
    return {
      isPermissionGranted: vi.fn(async () => granted),
      requestPermission: vi.fn(async () => answer),
      sendNotification: vi.fn(),
    };
  }

  it("checks permission once for a burst, asking at most once", async () => {
    const a = api(false, "granted");
    const notify = createNotifier(a);
    await Promise.all([notify({ title: "1", body: "" }), notify({ title: "2", body: "" }), notify({ title: "3", body: "" })]);
    expect(a.isPermissionGranted).toHaveBeenCalledTimes(1);
    expect(a.requestPermission).toHaveBeenCalledTimes(1);
    expect(a.sendNotification).toHaveBeenCalledTimes(3);
  });

  it("a refusal is never asked again, but a later grant in Settings is honoured", async () => {
    const a = api(false, "denied");
    const notify = createNotifier(a);
    await notify({ title: "1", body: "" });
    expect(a.sendNotification).not.toHaveBeenCalled();
    a.isPermissionGranted.mockResolvedValue(true);
    await notify({ title: "2", body: "" });
    expect(a.requestPermission).toHaveBeenCalledTimes(1);
    expect(a.sendNotification).toHaveBeenCalledWith({ title: "2", body: "" });
    await notify({ title: "3", body: "" });
    expect(a.isPermissionGranted).toHaveBeenCalledTimes(2);
  });
});

describe("clickedTask", () => {
  it("a click on a task's notification names its task (extra first, else by title); actions and summaries none", () => {
    const titles = new Map([["feat/a needs you", "a"]]);
    expect(clickedTask({ actionId: "tap", notification: { title: "x", extra: { task: "b" } } }, titles)).toBe("b");
    expect(clickedTask({ actionId: "tap", notification: { title: "feat/a needs you", extra: {} } }, titles)).toBe("a");
    expect(clickedTask({ actionId: "tap", notification: { title: "3 agents need you" } }, titles)).toBeNull();
    expect(clickedTask({ actionId: "other", notification: { title: "feat/a needs you" } }, titles)).toBeNull();
    expect(clickedTask({ actionId: "tap", notification: null }, titles)).toBeNull();
  });
});

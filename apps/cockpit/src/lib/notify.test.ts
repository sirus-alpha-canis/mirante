import { describe, expect, it } from "vitest";
import { task } from "../test/fixtures";
import { notificationFor, shouldNotify } from "./notify";

describe("shouldNotify", () => {
  it("stays quiet only when the task is on screen in the focused window", () => {
    const visible = new Set(["a"]);
    expect(shouldNotify("a", visible, true)).toBe(false);
    expect(shouldNotify("a", visible, false)).toBe(true);
    expect(shouldNotify("b", visible, true)).toBe(true);
  });

  it("says why", () => {
    expect(notificationFor(task("a", { boot: "trust_prompt" })).title).toBe("feat/a asks to trust its folder");
    expect(notificationFor(task("a", { status: "needs_you" }))).toEqual({ title: "feat/a needs you", body: "goal of a" });
  });
});

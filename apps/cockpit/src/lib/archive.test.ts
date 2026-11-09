import { describe, expect, it, vi } from "vitest";
import { makeEnv, rpcError } from "../test/env";
import { task } from "../test/fixtures";
import { M, RpcCode } from "./protocol";
import { archiveTasks } from "./archive";

describe("archiveTasks", () => {
  it("stops a live session first, archives each task and puts the archived view in the store", async () => {
    const h = makeEnv({
      [M.sessionStop]: (p: Record<string, unknown>) => task(String(p.task), { live: false }),
      [M.taskArchive]: (p: Record<string, unknown>) => task(String(p.task), { live: false, lifecycle: "archived" }),
    });
    const tasks = [task("a"), task("b", { live: false })];
    h.load(tasks);
    const failed = await archiveTasks(h.env, tasks);
    expect(failed).toEqual([]);
    expect(h.calls.map(([m, p]) => `${m} ${p.task}`)).toEqual([`${M.sessionStop} a`, `${M.taskArchive} a`, `${M.taskArchive} b`]);
    expect(h.env.cockpit.getState().tasks.a.lifecycle).toBe("archived");
  });

  it("says which tasks the daemon refused and why, and goes on with the rest", async () => {
    const h = makeEnv({
      [M.taskArchive]: (p: Record<string, unknown>) =>
        p.task === "dirty"
          ? rpcError(RpcCode.CONFLICT, "the worktree /wt/dirty has uncommitted changes:\n?? wip.txt")
          : task(String(p.task), { lifecycle: "archived", live: false }),
    });
    const tasks = [task("dirty", { live: false }), task("ok", { live: false })];
    h.load(tasks);
    const failed = await archiveTasks(h.env, tasks);
    expect(failed).toEqual([{ id: "dirty", title: "goal of dirty", branch: "feat/dirty", reason: expect.stringContaining("uncommitted changes") }]);
    expect(h.env.cockpit.getState().tasks.ok.lifecycle).toBe("archived");
  });

  it("a session that ended meanwhile is no reason to stop; another stop failure is", async () => {
    const stop = vi.fn((p: Record<string, unknown>) => (p.task === "gone" ? rpcError(RpcCode.CONFLICT, "no live session") : rpcError(RpcCode.DRIVER, "kill failed")));
    const h = makeEnv({ [M.sessionStop]: stop, [M.taskArchive]: (p: Record<string, unknown>) => task(String(p.task), { lifecycle: "archived" }) });
    const failed = await archiveTasks(h.env, [task("gone"), task("stuck")]);
    expect(failed.map((f) => [f.id, f.reason])).toEqual([["stuck", "kill failed"]]);
    expect(h.calls.filter(([m]) => m === M.taskArchive).map(([, p]) => p.task)).toEqual(["gone"]);
  });
});

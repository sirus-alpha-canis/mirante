import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { RpcCode } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { MemoryPanel } from "./MemoryPanel";

const status = { pid: 1, boot_id: "b1", version: "x", live_sessions: 1 };
const GB = 2 ** 30;

describe("MemoryPanel", () => {
  it("says when the daemon has no memory guard", () => {
    const h = makeEnv();
    h.load([task("a")]);
    renderWith(h.env, <MemoryPanel status={status} />);
    expect(screen.getByText(/Memory guard not available/)).toBeInTheDocument();
  });

  it("shows pressure, per-session memory, notices, and hibernates on request", async () => {
    const h = makeEnv({ "session.hibernate": {} });
    const memory = { bytes: 3 * GB, peak: 3 * GB, budget: 4 * GB, state: "soft" };
    h.load([task("a", { memory })], [ev(1, "a", "memory.offer_hibernate", { reason: "memory pressure elevated" })]);
    const sys = { level: "elevated", total: 16 * GB, free: 512 * 2 ** 20, swap_used: 0, swap_total: 0, paused: true };
    renderWith(h.env, <MemoryPanel status={{ ...status, memory: sys }} />);
    expect(screen.getByText("elevated")).toBeInTheDocument();
    expect(screen.getByText(/512 MB free/)).toBeInTheDocument();
    expect(screen.getByText(/new sessions paused/)).toBeInTheDocument();
    expect(screen.getByText("3072 MB / 4096 MB")).toBeInTheDocument();
    expect(screen.getByText("feat/a: hibernate suggested: memory pressure elevated")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    expect(h.calls).toContainEqual(["session.hibernate", { task: "a" }]);
  });

  it("a daemon without hibernate answers METHOD_NOT_FOUND and the UI says so", async () => {
    const h = makeEnv({ "session.hibernate": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method") });
    h.load([task("a", { memory: { bytes: 1, peak: 1, budget: 2, state: "ok" } })]);
    renderWith(h.env, <MemoryPanel status={status} />);
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("memory guard not available in this daemon");
  });

  it("lists hibernated sessions with Wake, heaviest first, at most five", async () => {
    const h = makeEnv({ "session.wake": {} });
    const m = (mb: number, state = "ok") => ({ bytes: mb * 2 ** 20, peak: mb * 2 ** 20, budget: 1024 * 2 ** 20, state });
    h.load(
      [
        task("s", { live: false, boot: null, hibernated: true }),
        ...[100, 900, 300, 1100, 200, 50].map((mb, i) => task(`t${i}`, { memory: m(mb, mb > 1024 ? "hard" : "ok") })),
      ],
      [ev(1, "s", "memory.auto_hibernated", { reason: "idle under critical pressure" })],
    );
    renderWith(h.env, <MemoryPanel status={null} />);
    const rows = within(screen.getByRole("region", { name: "Memory guard" })).getAllByRole("listitem");
    const sessions = rows.filter((r) => r.closest(".mem-list"));
    expect(sessions.map((r) => r.querySelector(".mem-name")?.textContent)).toEqual(["feat/t3", "feat/t1", "feat/t2", "feat/t4", "feat/t0"]);
    expect(sessions[0]).toHaveClass("mem-hard");
    expect(screen.getByText("feat/s: hibernated automatically: idle under critical pressure")).toBeInTheDocument();
  });

  it("Wake resumes a hibernated session, one request at a time", async () => {
    let release: (v: unknown) => void = () => {};
    const h = makeEnv({ "session.wake": () => new Promise((r) => (release = r)) });
    h.load([task("s", { live: false, boot: null, hibernated: true })]);
    renderWith(h.env, <MemoryPanel status={null} />);
    expect(screen.getByText("hibernated")).toBeInTheDocument();
    const wake = screen.getByRole("button", { name: "Wake" });
    await userEvent.click(wake);
    await userEvent.click(wake);
    expect(h.calls.filter(([m]) => m === "session.wake")).toEqual([["session.wake", { task: "s" }]]);
    expect(wake).toBeDisabled();
    release({});
  });
});

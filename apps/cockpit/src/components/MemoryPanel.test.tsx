import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { RpcCode } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { MemoryPanel } from "./MemoryPanel";
import { EnvContext } from "../env";
import { setLang } from "../lib/i18n";
import { disclosureKey } from "./Disclosure";

const status = { pid: 1, boot_id: "b1", version: "x", live_sessions: 1 };
const GB = 2 ** 30;

/** The panel as a viewer who left it open sees it. */
const opened = (h: ReturnType<typeof makeEnv>) => {
  h.env.storage.setItem(disclosureKey("memory"), "1");
  return h.env;
};

describe("MemoryPanel", () => {
  it("says when the daemon has no memory guard", () => {
    const h = makeEnv();
    h.load([task("a")]);
    renderWith(opened(h), <MemoryPanel status={status} />);
    expect(screen.getByText(/Memory guard not available/)).toBeInTheDocument();
  });

  it("an unknown status (not loaded, or the daemon is away) says unknown, not unavailable", () => {
    const h = makeEnv();
    h.load([task("a")]);
    renderWith(opened(h), <MemoryPanel status={null} />);
    expect(screen.getByText("System memory status unknown.")).toBeInTheDocument();
    expect(screen.queryByText(/not available/)).toBeNull();
  });

  it("Hibernate asks first, and warns when the agent is mid-turn", async () => {
    const h = makeEnv({ "session.hibernate": {} });
    const memory = { bytes: 1, peak: 1, budget: 2, state: "ok" };
    h.load([task("a", { memory, status: "working" })]);
    renderWith(opened(h), <MemoryPanel status={null} />);
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    const confirm = screen.getByRole("group", { name: "Confirm hibernate" });
    expect(confirm).toHaveTextContent("The agent is mid-turn; hibernating ends this turn.");
    expect(h.calls.filter(([m]) => m === "session.hibernate")).toEqual([]);
    await userEvent.click(within(confirm).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group", { name: "Confirm hibernate" })).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    await userEvent.click(screen.getByRole("button", { name: "Hibernate now" }));
    expect(h.calls).toContainEqual(["session.hibernate", { task: "a" }]);
  });

  it("an idle session's confirm has no mid-turn warning", async () => {
    const h = makeEnv();
    h.load([task("a", { memory: { bytes: 1, peak: 1, budget: 2, state: "ok" } })]);
    renderWith(opened(h), <MemoryPanel status={null} />);
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    expect(screen.getByRole("group", { name: "Confirm hibernate" })).not.toHaveTextContent(/mid-turn/);
  });

  it("shows pressure, per-session memory, notices, and hibernates on request", async () => {
    const h = makeEnv({ "session.hibernate": {} });
    const memory = { bytes: 3 * GB, peak: 3 * GB, budget: 4 * GB, state: "soft" };
    h.load([task("a", { memory })], [ev(1, "a", "memory.offer_hibernate", { reason: "memory pressure elevated" })]);
    const sys = { level: "elevated", total: 16 * GB, free: 512 * 2 ** 20, swap_used: 0, swap_total: 0, paused: true };
    renderWith(opened(h), <MemoryPanel status={{ ...status, memory: sys }} />);
    expect(screen.getByText("System memory tight (little free memory) · new sessions paused until it eases")).toBeInTheDocument();
    expect(screen.getByText("512 MB free of 16.0 GB")).toBeInTheDocument();
    expect(screen.getByText("3072 MB / 4096 MB")).toBeInTheDocument();
    expect(screen.getByText("feat/a: hibernate suggested: memory pressure elevated")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    await userEvent.click(screen.getByRole("button", { name: "Hibernate now" }));
    expect(h.calls).toContainEqual(["session.hibernate", { task: "a" }]);
  });

  it("a daemon without hibernate answers METHOD_NOT_FOUND and the UI says so", async () => {
    const h = makeEnv({ "session.hibernate": rpcError(RpcCode.METHOD_NOT_FOUND, "unknown method") });
    h.load([task("a", { memory: { bytes: 1, peak: 1, budget: 2, state: "ok" } })]);
    renderWith(opened(h), <MemoryPanel status={status} />);
    await userEvent.click(screen.getByRole("button", { name: "Hibernate" }));
    await userEvent.click(screen.getByRole("button", { name: "Hibernate now" }));
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
    renderWith(opened(h), <MemoryPanel status={null} />);
    const rows = within(screen.getByRole("region", { name: "Memory guard" })).getAllByRole("listitem");
    const sessions = rows.filter((r) => r.closest(".mem-list"));
    expect(sessions.map((r) => r.querySelector(".mem-name")?.textContent)).toEqual(["feat/t3", "feat/t1", "feat/t2", "feat/t4", "feat/t0"]);
    expect(sessions[0]).toHaveClass("mem-hard");
    expect(screen.getByText("feat/s: hibernated automatically: idle under critical pressure")).toBeInTheDocument();
  });

  describe("notices", () => {
    const at = (e: ReturnType<typeof ev>, agoMs: number) => ({ ...e, ts_ms: Date.now() - agoMs });
    const MIN = 60_000;
    const normal = { level: "normal", total: 16 * GB, free: 8 * GB, swap_used: 0, swap_total: 0, paused: false };
    const elevated = { ...normal, level: "elevated", free: 512 * 2 ** 20, paused: true };
    const latest = () => document.querySelector(".mem-latest");

    it("shows only the latest notice of the last 10 minutes, one muted line; the rest under History", async () => {
      const h = makeEnv();
      h.load(
        [task("a")],
        [
          at(ev(1, null, "memory.level", { to: "elevated" }), 30 * MIN),
          at(ev(2, null, "memory.paused"), 29 * MIN),
          at(ev(3, null, "memory.level", { to: "normal" }), 3 * MIN),
          at(ev(4, null, "memory.resumed"), 2 * MIN),
        ],
      );
      renderWith(opened(h), <MemoryPanel status={{ ...status, memory: normal }} />);
      expect(latest()).toHaveTextContent("new sessions and queued deliveries resumed");
      expect(latest()).toHaveClass("muted");
      // One line, not a list of every notice.
      expect(screen.getAllByText(/new sessions and queued deliveries/).filter((e) => e.closest(".mem-latest"))).toHaveLength(1);
      const history = screen.getByText("History (4)").closest("details")!;
      expect(history).not.toHaveAttribute("open");
      await userEvent.click(screen.getByText("History (4)"));
      const items = within(within(history).getByRole("list", { name: "Memory notices" })).getAllByRole("listitem");
      expect(items.map((i) => i.textContent)).toEqual([
        "new sessions and queued deliveries resumed",
        "system memory pressure normal",
        "new sessions and queued deliveries paused (system memory pressure)",
        "system memory pressure elevated",
      ]);
    });

    it("nothing in the last 10 minutes: no latest line, only the History disclosure", () => {
      const h = makeEnv();
      h.load([task("a")], [at(ev(1, null, "memory.resumed"), 11 * MIN)]);
      renderWith(opened(h), <MemoryPanel status={{ ...status, memory: normal }} />);
      expect(latest()).toBeNull();
      expect(screen.getByText("History (1)")).toBeInTheDocument();
    });

    it("a hibernate suggestion shows only while pressure is still elevated", () => {
      const h = makeEnv();
      h.load([task("a")], [at(ev(1, "a", "memory.offer_hibernate", { reason: "memory pressure elevated" }), MIN)]);
      const { rerender } = renderWith(opened(h), <MemoryPanel status={{ ...status, memory: elevated }} />);
      expect(latest()).toHaveTextContent("feat/a: hibernate suggested: memory pressure elevated");
      rerender(
        <EnvContext.Provider value={h.env}>
          <MemoryPanel status={{ ...status, memory: normal }} />
        </EnvContext.Provider>,
      );
      expect(latest()).toBeNull();
    });

    it("no notices: no History", () => {
      const h = makeEnv();
      h.load([task("a")]);
      renderWith(opened(h), <MemoryPanel status={{ ...status, memory: normal }} />);
      expect(screen.queryByText(/^History/)).toBeNull();
    });
  });

  it("Wake resumes a hibernated session, one request at a time", async () => {
    let release: (v: unknown) => void = () => {};
    const h = makeEnv({ "session.wake": () => new Promise((r) => (release = r)) });
    h.load([task("s", { live: false, boot: null, hibernated: true })]);
    renderWith(opened(h), <MemoryPanel status={null} />);
    expect(screen.getByText("hibernated")).toBeInTheDocument();
    const wake = screen.getByRole("button", { name: "Wake" });
    await userEvent.click(wake);
    await userEvent.click(wake);
    expect(h.calls.filter(([m]) => m === "session.wake")).toEqual([["session.wake", { task: "s" }]]);
    expect(wake).toBeDisabled();
    release({});
  });

  describe("folded (the default)", () => {
    const sys = (level: string, over: Record<string, unknown> = {}) => ({ level, total: 16 * GB, free: 2 * GB, available: 7 * GB, swap_used: 0, swap_total: 0, paused: false, ...over });
    const head = () => screen.getByRole("button", { name: /^System memory/ });

    it("one line: the title and a summary chip; the details hidden until opened", async () => {
      const h = makeEnv();
      h.load([task("a", { memory: { bytes: 1, peak: 1, budget: 2, state: "ok" } })], [ev(1, "a", "memory.resumed")]);
      renderWith(h.env, <MemoryPanel status={{ ...status, memory: sys("normal") }} />);
      expect(head()).toHaveAttribute("aria-expanded", "false");
      expect(head()).toHaveAccessibleName("System memory normal · 7.0 GB free");
      expect(screen.queryByRole("button", { name: "Hibernate" })).toBeNull();
      expect(screen.getByText(/^History/)).not.toBeVisible();
      await userEvent.click(head());
      expect(head()).toHaveAttribute("aria-expanded", "true");
      expect(screen.getByText("7.0 GB available of 16.0 GB")).toBeVisible();
      expect(screen.getByRole("button", { name: "Hibernate" })).toBeVisible();
      expect(screen.getByText("History (1)")).toBeVisible();
      await userEvent.click(head());
      expect(screen.queryByRole("button", { name: "Hibernate" })).toBeNull();
    });

    it.each([
      ["normal", "dot-ready", "normal"],
      ["elevated", "dot-needs", "elevated"],
      ["critical", "dot-danger", "critical"],
      ["surprising", "dot-unknown", "unknown"],
    ])("level %s: a %s dot and its word", (level, dot, word) => {
      const h = makeEnv();
      h.load([]);
      renderWith(h.env, <MemoryPanel status={{ ...status, memory: sys(level) }} />);
      expect(head()).toHaveAccessibleName(`System memory ${word} · 7.0 GB free`);
      expect(head().querySelector(".dot")).toHaveClass(dot);
    });

    it("an older daemon's free memory, no status, no guard: each says so", () => {
      const h = makeEnv();
      h.load([]);
      const { rerender } = renderWith(h.env, <MemoryPanel status={{ ...status, memory: sys("normal", { available: undefined }) }} />);
      expect(head()).toHaveAccessibleName("System memory normal · 2.0 GB free");
      const again = (s: Parameters<typeof MemoryPanel>[0]["status"]) =>
        rerender(
          <EnvContext.Provider value={h.env}>
            <MemoryPanel status={s} />
          </EnvContext.Provider>,
        );
      again(null);
      expect(head()).toHaveAccessibleName("System memory unknown");
      again(status);
      expect(head()).toHaveAccessibleName("System memory not available");
    });

    it("pressure changes update the summary live, never open the panel", () => {
      const h = makeEnv();
      h.load([]);
      const { rerender } = renderWith(h.env, <MemoryPanel status={{ ...status, memory: sys("normal") }} />);
      rerender(
        <EnvContext.Provider value={h.env}>
          <MemoryPanel status={{ ...status, memory: sys("critical", { available: 300 * 2 ** 20, paused: true }) }} />
        </EnvContext.Provider>,
      );
      expect(head()).toHaveAttribute("aria-expanded", "false");
      expect(head()).toHaveAccessibleName("System memory critical · 300 MB free");
      expect(head().querySelector(".dot")).toHaveClass("dot-danger");
    });

    it("remembers being opened", async () => {
      const h = makeEnv();
      h.load([]);
      const { unmount } = renderWith(h.env, <MemoryPanel status={{ ...status, memory: sys("normal") }} />);
      await userEvent.click(head());
      unmount();
      renderWith(h.env, <MemoryPanel status={{ ...status, memory: sys("normal") }} />);
      expect(head()).toHaveAttribute("aria-expanded", "true");
    });

    it("says it in Portuguese", () => {
      const h = makeEnv();
      h.load([]);
      setLang("pt");
      try {
        renderWith(h.env, <MemoryPanel status={{ ...status, memory: sys("elevated") }} />);
        expect(screen.getByRole("button", { name: /^Memória do sistema/ })).toHaveAccessibleName("Memória do sistema elevada · 7.0 GB livres");
      } finally {
        setLang("en");
      }
    });
  });
});

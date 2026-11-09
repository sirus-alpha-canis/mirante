import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { setLang } from "../lib/i18n";
import type { DaemonStatus } from "../lib/protocol";
import { hintsFor } from "../lib/shortcuts";
import { daemonState, StatusLine, viewHints } from "./StatusLine";

const GB = 2 ** 30;
const status = (memory: DaemonStatus["memory"]): DaemonStatus => ({ pid: 1, boot_id: "b", version: "0", live_sessions: 0, memory });
const mem = { level: "normal", total: 16 * GB, free: 2 * GB, available: 9 * GB, swap_used: GB, swap_total: 4 * GB, paused: false };

afterEach(() => setLang("en"));

describe("StatusLine", () => {
  it("says the daemon's state, the machine's memory, what needs you and this view's keys", () => {
    render(<StatusLine connection={{ state: "live" }} status={status(mem)} needing={2} view="home" hints={hintsFor(true)} mac />);
    const line = screen.getByRole("contentinfo", { name: "Status" });
    expect(line).toHaveTextContent("daemon running");
    expect(line).toHaveTextContent("memory normal · swap 25% · 9.0 GB available");
    expect(line).toHaveTextContent("2 need you");
    expect(line).toHaveTextContent("⌘J next · ⌘K commands · ⌘↵ dispatch");
    // Never a live region: it changes every few seconds.
    expect(line.closest("[aria-live], [role=status]")).toBeNull();
  });

  it("an unknown memory says so; pressure and pausing are flagged; nothing needing you says nothing", () => {
    const { rerender } = render(<StatusLine connection={{ state: "connecting" }} status={null} needing={0} view="detail" hints={hintsFor(false)} mac={false} />);
    const line = screen.getByRole("contentinfo");
    expect(line).toHaveTextContent("connecting");
    expect(line).toHaveTextContent("memory unknown");
    expect(line).not.toHaveTextContent("need");
    expect(line).toHaveTextContent("Ctrl+↵ steer · Alt+↵ queue · Esc back · Ctrl+K commands");
    rerender(<StatusLine connection={{ state: "down", error: "x", retryMs: 1 }} status={status({ ...mem, level: "critical", paused: true })} needing={1} view="grid" hints={hintsFor(true)} mac />);
    expect(line).toHaveTextContent("daemon not reachable");
    expect(screen.getByText(/memory critical/)).toHaveClass("status-needs");
    expect(line).toHaveTextContent("new sessions paused");
    expect(line).toHaveTextContent("1 needs you");
  });

  it("speaks Portuguese", () => {
    setLang("pt");
    render(<StatusLine connection={{ state: "live" }} status={status(mem)} needing={3} view="home" hints={hintsFor(true)} mac />);
    const line = screen.getByRole("contentinfo");
    expect(line).toHaveTextContent("daemon rodando");
    expect(line).toHaveTextContent("memória normal");
    expect(line).toHaveTextContent("3 precisam de você");
    expect(line).toHaveTextContent("⌘↵ despachar");
  });

  it("maps the daemon to a state color: ready, working, needs you", () => {
    expect(daemonState({ state: "live" }).dot).toBe("dot-ready");
    expect(daemonState({ state: "connecting" }).dot).toBe("dot-work");
    expect(daemonState({ state: "down", error: "", retryMs: 0 }).dot).toBe("dot-needs");
    expect(viewHints("skills", hintsFor(true), true).map(([k]) => k)).toEqual(["⌘1", "⌘J", "⌘K"]);
  });
});

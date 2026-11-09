import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { daemonOutdated, NO_UPDATE, UpdateContext } from "../lib/update";
import { Unsupported } from "./Restart";

describe("daemonOutdated", () => {
  const st = (build?: string) => ({ pid: 1, boot_id: "b", version: "0.1.0", live_sessions: 0, ...(build ? { build } : {}) });
  it("is outdated only when the binary on disk names another build", () => {
    expect(daemonOutdated(st("a.1"), { version: "0.1.0", build: "a.1" })).toBe(false);
    expect(daemonOutdated(st("a.1"), { version: "0.1.0", build: "b.2" })).toBe(true);
    // A daemon from before build ids, with a binary that has one.
    expect(daemonOutdated(st(), { version: "0.1.0", build: "b.2" })).toBe(true);
    // Nothing known: never a guess.
    expect(daemonOutdated(st("a.1"), { version: "0.1.0" })).toBe(false);
    expect(daemonOutdated(null, { version: "0.1.0", build: "b.2" })).toBe(false);
    expect(daemonOutdated(st("a.1"), null)).toBe(false);
  });
});

describe("Unsupported", () => {
  it("says the feature's own text when the daemon is not known to be outdated", () => {
    render(<Unsupported text="This daemon cannot list rules. Update Mirante to use it." />);
    expect(screen.getByText("This daemon cannot list rules. Update Mirante to use it.")).toBeInTheDocument();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("offers the restart when the update is installed, and says when it is under way", async () => {
    const restart = vi.fn();
    const { rerender } = render(
      <UpdateContext.Provider value={{ ...NO_UPDATE, outdated: true, restart }}>
        <Unsupported text="old text" />
      </UpdateContext.Provider>,
    );
    expect(screen.queryByText("old text")).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "Restart daemon" }));
    expect(restart).toHaveBeenCalled();
    rerender(
      <UpdateContext.Provider value={{ ...NO_UPDATE, outdated: true, restarting: true, restart }}>
        <Unsupported text="old text" />
      </UpdateContext.Provider>,
    );
    expect(screen.getByRole("button", { name: "Restarting…" })).toBeDisabled();
  });
});

import { act, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import type { TaskDiff } from "../lib/protocol";
import { EnvContext } from "../env";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { snapshot, task } from "../test/fixtures";
import { DiffTab } from "./DiffTab";
import { DoneCard } from "./DoneCard";

const diff: TaskDiff = {
  base: "main",
  merge_base: "abc12345ff",
  files: [{ path: "src/a.ts", added: 3, removed: 1 }],
  untracked: [],
  patch: "+x",
  truncated: false,
  compare_url: null,
  pushed: false,
};

const diffCalls = (calls: [string, unknown][]) => calls.filter(([m]) => m === "task.diff").length;

describe("DiffTab and DoneCard", () => {
  it("a card mounted again for the same turn shows the diff it fetched, without a new patch", async () => {
    const h = makeEnv({ "task.diff": diff });
    h.load([task("a")]);
    const r = renderWith(h.env, <DoneCard taskId="a" version={1} onOpen={() => {}} />);
    expect(await screen.findByText(/1 file changed/)).toBeInTheDocument();
    r.unmount();
    renderWith(h.env, <DoneCard taskId="a" version={1} onOpen={() => {}} />);
    expect(screen.getByText(/1 file changed/)).toBeInTheDocument();
    await act(async () => {});
    expect(diffCalls(h.calls)).toBe(1);
  });

  it("a new turn fetches again; the Diff tab revalidates what a card cached", async () => {
    const h = makeEnv({ "task.diff": diff });
    h.load([task("a")]);
    const r = renderWith(h.env, <DoneCard taskId="a" version={1} onOpen={() => {}} />);
    await screen.findByText(/1 file changed/);
    r.rerender(
      <EnvContext.Provider value={h.env}>
        <DoneCard taskId="a" version={2} onOpen={() => {}} />
      </EnvContext.Provider>,
    );
    await act(async () => {});
    expect(diffCalls(h.calls)).toBe(2);
    r.unmount();
    renderWith(h.env, <DiffTab taskId="a" version={2} />);
    expect(screen.getByLabelText("Patch")).toBeInTheDocument();
    await act(async () => {});
    expect(diffCalls(h.calls)).toBe(3);
  });

  it("a failed diff offers Retry, which fetches it again", async () => {
    let fail = true;
    const h = makeEnv({ "task.diff": () => (fail ? rpcError(4, "git broke") : diff) });
    h.load([task("a")]);
    renderWith(h.env, <DiffTab taskId="a" version={0} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Diff unavailable: git broke");
    fail = false;
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByLabelText("Patch")).toBeInTheDocument();
  });

  it("the card's error has Retry too", async () => {
    let fail = true;
    const h = makeEnv({ "task.diff": () => (fail ? rpcError(4, "git broke") : diff) });
    h.load([task("a")]);
    renderWith(h.env, <DoneCard taskId="a" version={1} onOpen={() => {}} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("git broke");
    fail = false;
    await userEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText(/1 file changed/)).toBeInTheDocument();
  });

  it("a restarted daemon (new boot) fetches the diff again", async () => {
    const h = makeEnv({ "task.diff": diff });
    h.load([task("a")]);
    renderWith(h.env, <DoneCard taskId="a" version={1} onOpen={() => {}} />);
    await screen.findByText(/1 file changed/);
    act(() => h.env.cockpit.dispatch({ type: "connected", snapshot: snapshot([task("a")], 0, { boot_id: "b2" }) }));
    await act(async () => {});
    expect(diffCalls(h.calls)).toBe(2);
  });
});

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

  it("a task across repos: one section per repo in the Diff tab and one line per repo on the card, each with its Open PR", async () => {
    const section = (name: string, files: TaskDiff["files"], pushed: boolean) => ({
      ...diff,
      name,
      repo: `/w/gol/${name}`,
      files,
      pushed,
      compare_url: `https://github.com/gol/${name}/compare/main...mirante/x?expand=1`,
    });
    const across: TaskDiff = {
      ...diff,
      repos: [
        section("Site-B2C", [{ path: "a.ts", added: 40, removed: 2 }, { path: "b.ts", added: 0, removed: 0 }, { path: "c.ts", added: 0, removed: 0 }], true),
        section("bff-flight", [{ path: "x.ts", added: 1, removed: 0 }], false),
      ],
    };
    const h = makeEnv({ "task.diff": across });
    h.load([task("a")]);
    const r = renderWith(h.env, <DoneCard taskId="a" version={1} onOpen={() => {}} />);
    const site = await screen.findByRole("listitem", { name: "Site-B2C" });
    expect(site).toHaveTextContent("Site-B2C: 3 files changed, +40 −2 vs main");
    expect(screen.getByRole("listitem", { name: "bff-flight" })).toHaveTextContent("bff-flight: 1 file changed, +1 −0");
    const pr = screen.getByRole("button", { name: "Open PR for Site-B2C" });
    expect(pr).toBeEnabled();
    expect(screen.getByRole("button", { name: "Open PR for bff-flight" })).toBeDisabled();
    await userEvent.click(pr);
    expect(h.env.openUrl).toHaveBeenCalledWith("https://github.com/gol/Site-B2C/compare/main...mirante/x?expand=1");
    r.unmount();
    renderWith(h.env, <DiffTab taskId="a" version={1} />);
    const tab = await screen.findByRole("region", { name: "bff-flight" });
    expect(tab).toHaveTextContent("x.ts");
    expect(screen.getByRole("region", { name: "Site-B2C" })).toHaveTextContent("a.ts");
    expect(screen.getAllByLabelText("Patch")).toHaveLength(2);
    expect(screen.getByRole("navigation", { name: "Repositories" })).toHaveTextContent("Site-B2Cbff-flight");
  });
});

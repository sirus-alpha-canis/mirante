import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import type { ProposalView } from "../lib/protocol";
import { RpcCode } from "../lib/protocol";
import { ev, task } from "../test/fixtures";
import { makeEnv, renderWith, rpcError } from "../test/env";
import { LeftRail } from "./LeftRail";
import { ReviewQueue } from "./ReviewQueue";

const HEAD = "50ea8cc255941e739b13cdbe2c937abca0319834";

const proposal = (id: string, over: Partial<ProposalView> = {}): ProposalView => ({
  id,
  repo: "/r/app",
  task_id: "t1",
  origin: "agent",
  title: `Proposal ${id}`,
  kind: "gotcha",
  body: "The spool <b>keeps</b> events. See [[hook-spool]].",
  anchors: [{ path: "src/spool.rs", symbol: "spool", start: 3, end: 9, commit: HEAD, fingerprint: "0123456789abcdef" }],
  redactions: 0,
  status: "pending",
  slug: null,
  decision_note: "",
  created_at: 1,
  decided_at: null,
  ...over,
});

function setup(answers: Record<string, unknown>) {
  const h = makeEnv(answers);
  h.load([task("t1", { repo_path: "/r/app", branch: "feat/spool" })]);
  renderWith(h.env, <ReviewQueue repo="/r/app" />);
  return h;
}

const card = (title: string) => screen.findByRole("article", { name: title });

describe("ReviewQueue", () => {
  it("lists pending proposals with citations, redaction warning and task origin", async () => {
    const h = setup({ "dna.proposals": [proposal("p1", { redactions: 1 }), proposal("p2", { task_id: null, origin: "cli" })] });
    const c1 = await card("Proposal p1");
    expect(c1).toHaveTextContent("1 secret was redacted");
    expect(c1).toHaveTextContent("src/spool.rs#spool:3-9");
    expect(c1).toHaveTextContent("50ea8cc");
    expect(c1).toHaveTextContent("from task feat/spool");
    expect(within(c1).getByLabelText("Body")).toHaveValue("The spool <b>keeps</b> events. See [[hook-spool]].");
    expect(c1.querySelector("b")).toBeNull();
    const c2 = await card("Proposal p2");
    expect(c2).toHaveTextContent("from the CLI");
    expect(c2).not.toHaveTextContent("redacted");
    expect(h.calls).toContainEqual(["dna.proposals", { repo: "/r/app", limit: 50 }]);
  });

  it("accept sends the edited title/body once and disables the buttons while in flight", async () => {
    let release: (v: unknown) => void = () => {};
    const h = setup({
      "dna.proposals": [proposal("p1")],
      "dna.review": () => new Promise((r) => (release = r)),
    });
    const c = await card("Proposal p1");
    const title = within(c).getByLabelText("Title");
    await userEvent.clear(title);
    await userEvent.type(title, "Edited");
    const accept = within(c).getByRole("button", { name: "Accept" });
    await userEvent.dblClick(accept);
    expect(accept).toBeDisabled();
    expect(within(c).getByRole("button", { name: "Reject" })).toBeDisabled();
    release(proposal("p1", { status: "accepted", slug: "edited", title: "Edited" }));
    await waitFor(() => expect(h.calls.filter((x) => x[0] === "dna.review")).toEqual([["dna.review", { id: "p1", accept: true, title: "Edited" }]]));
  });

  it("reject asks for an optional note", async () => {
    const h = setup({ "dna.proposals": [proposal("p1")], "dna.review": proposal("p1", { status: "rejected" }) });
    const c = await card("Proposal p1");
    await userEvent.click(within(c).getByRole("button", { name: "Reject" }));
    expect(h.calls.filter((x) => x[0] === "dna.review")).toEqual([]);
    await userEvent.type(within(c).getByLabelText("Why (optional)"), "too task-specific");
    await userEvent.click(within(c).getByRole("button", { name: "Reject proposal" }));
    await waitFor(() =>
      expect(h.calls.filter((x) => x[0] === "dna.review")).toEqual([["dna.review", { id: "p1", accept: false, note: "too task-specific" }]]),
    );
  });

  it("a CONFLICT (already decided) refreshes instead of erroring", async () => {
    let list = [proposal("p1")];
    const h = setup({
      "dna.proposals": () => list,
      "dna.review": () => {
        list = [];
        return rpcError(RpcCode.CONFLICT, "this proposal was already decided");
      },
    });
    const c = await card("Proposal p1");
    await userEvent.click(within(c).getByRole("button", { name: "Accept" }));
    expect(await screen.findByText(/already decided elsewhere/)).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("article", { name: "Proposal p1" })).toBeNull());
    expect(h.calls.filter((x) => x[0] === "dna.proposals").length).toBeGreaterThanOrEqual(2);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("FORBIDDEN explains that review cannot come from an agent session", async () => {
    setup({ "dna.proposals": [proposal("p1")], "dna.review": rpcError(RpcCode.FORBIDDEN, "Mirante-launched agents are refused") });
    const c = await card("Proposal p1");
    await userEvent.click(within(c).getByRole("button", { name: "Accept" }));
    expect(await within(c).findByRole("alert")).toHaveTextContent(/cannot be made from an agent session/);
  });

  it("pages with after when a page is full, and refetches on dna events", async () => {
    const page1 = Array.from({ length: 50 }, (_, i) => proposal(`a${i}`));
    const h = setup({ "dna.proposals": (p: Record<string, unknown>) => (p.after ? [proposal("z")] : page1) });
    await card("Proposal a0");
    await userEvent.click(screen.getByRole("button", { name: "Load more" }));
    expect(await card("Proposal z")).toBeInTheDocument();
    expect(h.calls).toContainEqual(["dna.proposals", { repo: "/r/app", limit: 50, after: "a49" }]);
    const n = h.calls.filter((x) => x[0] === "dna.proposals").length;
    h.env.cockpit.dispatch({ type: "event", event: ev(9, "t1", "dna.proposed", { id: "q", title: "Q" }) });
    await waitFor(() => expect(h.calls.filter((x) => x[0] === "dna.proposals").length).toBe(n + 1));
  });

  it("says how many secrets were redacted from the accepted edits", async () => {
    setup({ "dna.proposals": [proposal("p1")], "dna.review": { ...proposal("p1"), status: "accepted", slug: "x", redacted: 2 } });
    const c = await card("Proposal p1");
    await userEvent.type(within(c).getByLabelText("Body"), " token ghp_x");
    await userEvent.click(within(c).getByRole("button", { name: "Accept" }));
    expect(await screen.findByText(/Accepted as x\. 2 secrets were redacted from your edits\./)).toBeInTheDocument();
  });

  it("a decision removes only that card: loaded pages stay, and an event refetch keeps them", async () => {
    const page1 = Array.from({ length: 50 }, (_, i) => proposal(`a${i}`));
    let all = [...page1, proposal("z")];
    const h = setup({
      "dna.proposals": (p: Record<string, unknown>) => {
        const from = p.after ? all.findIndex((x) => x.id === p.after) + 1 : 0;
        return all.slice(from, from + Number(p.limit));
      },
      "dna.review": (p: Record<string, unknown>) => {
        all = all.filter((x) => x.id !== p.id);
        return { ...proposal(String(p.id)), status: "rejected", redacted: 0 };
      },
    });
    await card("Proposal a0");
    await userEvent.click(screen.getByRole("button", { name: "Load more" }));
    expect(await card("Proposal z")).toBeInTheDocument();
    const lists = () => h.calls.filter((x) => x[0] === "dna.proposals").length;
    const n = lists();
    const c = await card("Proposal a3");
    await userEvent.click(within(c).getByRole("button", { name: "Reject" }));
    await userEvent.click(within(c).getByRole("button", { name: "Reject proposal" }));
    await waitFor(() => expect(screen.queryByRole("article", { name: "Proposal a3" })).toBeNull());
    expect(lists()).toBe(n);
    expect(screen.getByRole("article", { name: "Proposal z" })).toBeInTheDocument();
    // The daemon's dna.decided arrives: the refetch covers what was loaded, not one page.
    h.env.cockpit.dispatch({ type: "event", event: ev(9, "t1", "dna.decided", { id: "a3", accepted: false, slug: null, repo: "/r/app" }) });
    await waitFor(() => expect(lists()).toBe(n + 1));
    expect(h.calls[h.calls.length - 1]).toEqual(["dna.proposals", { repo: "/r/app", limit: 100 }]);
    expect(await card("Proposal z")).toBeInTheDocument();
  });

  it("an empty queue says nothing waits", async () => {
    setup({ "dna.proposals": [] });
    expect(await screen.findByText("Nothing waits for review.")).toBeInTheDocument();
  });

  it("left rail shows the project's pending count", async () => {
    const h = makeEnv({ "dna.status": { repo: "/r/app", enabled: true, dir: true, notes: 3, stale: 0, unreviewed: 0, pending: 2, invalid: 0, head: null } });
    h.load([task("t1", { repo_path: "/r/app" })]);
    const onReview = vi.fn();
    renderWith(
      h.env,
      <LeftRail
        projects={[{ path: "/r/app", name: "app", tasks: 1, needsYou: 0 }]}
        selected="/r/app"
        onSelect={() => {}}
        onReposChanged={() => {}}
        onMention={() => {}}
        onOpen={() => {}}
        onReview={onReview}
        status={null}
      />,
    );
    await userEvent.click(await screen.findByRole("button", { name: "2 to review" }));
    expect(onReview).toHaveBeenCalledWith("/r/app");
    expect(h.calls).toContainEqual(["dna.status", { repo: "/r/app" }]);
  });
});

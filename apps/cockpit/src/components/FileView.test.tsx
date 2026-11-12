import { fireEvent, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { makeEnv, renderWith } from "../test/env";
import { resetEditorsCache } from "./EditorButton";
import { FileView } from "./FileView";

beforeEach(() => resetEditorsCache());

function mount(answer: unknown, path = "src/x.rs", line?: number) {
  const h = makeEnv({ "task.file": answer });
  const onOpenPath = vi.fn();
  const r = renderWith(h.env, <FileView taskId="t1" path={path} line={line} onOpenPath={onOpenPath} />);
  return { ...h, ...r, onOpenPath };
}

describe("FileView", () => {
  it("text is highlighted in a line-numbered listing of text nodes", async () => {
    const { container } = mount({ path: "src/x.rs", abs: "/w/src/x.rs", size: 20, kind: "text", text: 'fn main() {\n  "<b>"\n}', lang: "rust" });
    await waitFor(() => expect(container.querySelector(".code-line")).not.toBeNull());
    expect(container.querySelectorAll(".code-line")).toHaveLength(3);
    expect(container.querySelector(".tk-kw")?.textContent).toBe("fn");
    expect(container.querySelector(".tk-str")?.textContent).toBe('"<b>"');
    expect(container.querySelector("b")).toBeNull();
  });

  it("binary shows size and Open in editor", async () => {
    const { env } = mount({ path: "a.bin", abs: "/w/a.bin", size: 2048, kind: "binary" }, "a.bin");
    expect(await screen.findByText(/Binary file \(2\.0 KiB\)/)).toBeInTheDocument();
    const open = await screen.findByRole("button", { name: /Open in VS Code/ });
    fireEvent.click(open);
    expect(env.openInEditor).toHaveBeenCalledWith("t1", "a.bin", null, "vscode");
  });

  it("secret_hidden shows Reveal which re-asks with reveal:true", async () => {
    const h = makeEnv({
      "task.file": (p: Record<string, unknown>) =>
        p.reveal ? { path: ".env", abs: "/w/.env", size: 9, kind: "text", text: "TOKEN=abc" } : { path: ".env", abs: "/w/.env", size: 9, kind: "secret_hidden" },
    });
    renderWith(h.env, <FileView taskId="t1" path=".env" onOpenPath={() => {}} />);
    expect(await screen.findByText(/may hold secrets/)).toBeInTheDocument();
    expect(screen.queryByText("TOKEN=abc")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Reveal" }));
    expect(await screen.findByText("TOKEN=abc")).toBeInTheDocument();
    expect(h.calls.map(([, p]) => p)).toEqual([
      { task: "t1", path: ".env" },
      { task: "t1", path: ".env", reveal: true },
    ]);
  });

  it(".md defaults to Preview with a Source toggle", async () => {
    const { container } = mount({ path: "README.md", abs: "/w/README.md", size: 12, kind: "text", text: "# Hello\n\nbody", lang: "markdown" }, "README.md");
    expect(await screen.findByRole("heading", { name: "Hello" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Source" }));
    expect(screen.queryByRole("heading", { name: "Hello" })).toBeNull();
    expect(container.querySelectorAll(".code-line")).toHaveLength(3);
    fireEvent.click(screen.getByRole("button", { name: "Preview" }));
    expect(screen.getByRole("heading", { name: "Hello" })).toBeInTheDocument();
  });

  it("line anchor scrolls and marks line N", async () => {
    const scroll = vi.fn();
    Element.prototype.scrollIntoView = scroll;
    const { container } = mount({ path: "a.ts", abs: "/w/a.ts", size: 9, kind: "text", text: "a\nb\nc\nd", lang: "ts" }, "a.ts", 3);
    await waitFor(() => expect(container.querySelector(".code-line-mark")).not.toBeNull());
    expect(container.querySelector(".code-line-mark")?.getAttribute("data-line")).toBe("3");
    expect(scroll).toHaveBeenCalled();
  });

  it("an image shows from its data URL; too large and errors say so", async () => {
    const img = mount({ path: "a.png", abs: "/w/a.png", size: 3, kind: "image", image: { mime: "image/png", b64: "iVBO" } }, "a.png");
    await waitFor(() => expect(img.container.querySelector("img")).not.toBeNull());
    expect(img.container.querySelector("img")!.getAttribute("src")).toBe("data:image/png;base64,iVBO");
    img.unmount();
    mount({ path: "big", abs: "/w/big", size: 3 << 20, kind: "too_large" }, "big");
    expect(await screen.findByText(/Too large to show here \(3\.0 MiB\)/)).toBeInTheDocument();
  });

  it("a read the daemon refuses says why", async () => {
    mount({ kind: "rpc", code: 2, message: "hosts is a symlink: not followed" }, "hosts");
    expect(await screen.findByText(/Could not read hosts: hosts is a symlink/)).toBeInTheDocument();
  });
});

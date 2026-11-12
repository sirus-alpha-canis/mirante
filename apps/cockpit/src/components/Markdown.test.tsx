import { fireEvent, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { makeEnv, renderWith } from "../test/env";
import { Markdown } from "./Markdown";

const PNG = { path: "img/a.png", abs: "/w/img/a.png", size: 3, kind: "image", image: { mime: "image/png", b64: "iVBO" } };

function mount(src: string, answers: Record<string, unknown> = {}, path = "docs/README.md") {
  const h = makeEnv(answers);
  const onOpenPath = vi.fn();
  const r = renderWith(h.env, <Markdown src={src} path={path} taskId="t1" onOpenPath={onOpenPath} />);
  return { ...h, ...r, onOpenPath };
}

describe("Markdown", () => {
  it("raw HTML is text, never markup", () => {
    const { container } = mount('<img src=x onerror=alert(1)><script>x</script>\n\n<a href="javascript:alert(1)">x</a>');
    expect(container.querySelector("img,script,a[href]")).toBeNull();
    expect(screen.getByText(/<script>/)).toBeInTheDocument();
  });

  it("javascript:, data:, file: and vbscript: links render as text with copy", () => {
    const { container, env } = mount("[a](javascript:alert(1)) [b](data:text/html,x) [c](file:///etc/passwd) [d](vbscript:x)");
    expect(container.querySelector("a")).toBeNull();
    expect(screen.getAllByRole("button", { name: /copy link/i })).toHaveLength(4);
    for (const b of screen.getAllByRole("button", { name: /copy link/i })) fireEvent.click(b);
    expect(env.openUrl).not.toHaveBeenCalled();
  });

  it("github and localhost links go through the opener; relative links open in the side pane", async () => {
    const { env, onOpenPath } = mount("[gh](https://github.com/o/r) [dev](http://localhost:5173/) [g](guide.md#L4)");
    fireEvent.click(screen.getByRole("link", { name: "gh" }));
    fireEvent.click(screen.getByRole("link", { name: "dev" }));
    expect(vi.mocked(env.openUrl).mock.calls).toEqual([["https://github.com/o/r"], ["http://localhost:5173/"]]);
    fireEvent.click(screen.getByRole("link", { name: "g" }));
    expect(onOpenPath).toHaveBeenCalledWith("docs/guide.md", 4);
  });

  it("remote images are alt text; worktree images load through task.file", async () => {
    const { container, calls } = mount("![remote](https://evil.example/x.png) ![logo](../img/a.png)", { "task.file": PNG });
    expect(screen.getByText(/remote image not loaded/)).toBeInTheDocument();
    await waitFor(() => expect(container.querySelector("img")).not.toBeNull());
    const img = container.querySelector("img")!;
    expect(img.getAttribute("src")).toBe("data:image/png;base64,iVBO");
    expect(img.getAttribute("alt")).toBe("logo");
    expect(calls).toEqual([["task.file", { task: "t1", path: "img/a.png" }]]);
    expect(container.querySelectorAll("img")).toHaveLength(1);
  });

  it("a worktree image the daemon says is not an image stays alt text", async () => {
    const { container, calls } = mount("![x](a.svg)", { "task.file": { path: "a.svg", abs: "/w/a.svg", size: 9, kind: "text", text: "<svg/>" } }, "README.md");
    await waitFor(() => expect(calls.length).toBe(1));
    expect(container.querySelector("img")).toBeNull();
    expect(screen.getByText("x")).toBeInTheDocument();
  });

  it("caps: over 256 KiB renders as plain text with a note", () => {
    const { container } = mount("# big\n" + "a".repeat(300 * 1024));
    expect(container.querySelector("h1")).toBeNull();
    expect(screen.getByText(/too large to preview/i)).toBeInTheDocument();
  });

  it("tables, nested lists (2 levels), blockquote, hr, inline code", () => {
    const { container } = mount("| a | b |\n|---|---|\n| 1 | `2` |\n\n- x\n  - y\n\n> q\n\n---\n\n## H");
    expect(container.querySelector("table th")?.textContent).toBe("a");
    expect(container.querySelector("td code")?.textContent).toBe("2");
    expect(container.querySelector("ul ul li")?.textContent).toBe("y");
    expect(container.querySelector("blockquote")?.textContent).toBe("q");
    expect(container.querySelector("hr")).not.toBeNull();
    expect(container.querySelector("h2")?.textContent).toBe("H");
  });
});

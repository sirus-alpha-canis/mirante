import { fireEvent, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { makeEnv, renderWith } from "../test/env";
import { BrowserTab } from "./BrowserTab";

// jsdom runs at http://localhost:3000; the app runs at tauri://localhost.
function mount(url = "", self = "tauri://localhost") {
  const h = makeEnv({});
  const onNavigate = vi.fn();
  const r = renderWith(h.env, <BrowserTab url={url} onNavigate={onNavigate} self={self} />);
  return { ...h, ...r, onNavigate };
}
const frame = (c: HTMLElement) => c.querySelector("iframe");

/** The frame loaded the dev server's page: cross-origin, so its document is not readable. */
const loadedCrossOrigin = (f: HTMLIFrameElement) => {
  Object.defineProperty(f, "contentDocument", { configurable: true, get: () => null });
  fireEvent.load(f);
};

describe("BrowserTab", () => {
  it("with no address yet, asks for one and shows no frame", () => {
    const { container } = mount();
    expect(frame(container)).toBeNull();
    expect(screen.getByRole("textbox", { name: "Address" })).toHaveAttribute("placeholder", "http://localhost:3000");
    expect(screen.getByText(/a dev server on this machine/)).toBeInTheDocument();
  });

  it("a localhost address loads in a sandboxed frame: exact flags, no referrer, nothing delegated", async () => {
    const { container, onNavigate } = mount();
    await userEvent.type(screen.getByRole("textbox", { name: "Address" }), "localhost:5173{Enter}");
    expect(onNavigate).toHaveBeenCalledWith("http://localhost:5173/");
    const f = frame(container)!;
    expect(f).toHaveAttribute("src", "http://localhost:5173/");
    expect(f.getAttribute("sandbox")).toBe("allow-scripts allow-forms allow-same-origin");
    expect(f).toHaveAttribute("referrerpolicy", "no-referrer");
    expect(f).toHaveAttribute("allow", "");
    expect(f).toHaveAttribute("title", "Preview of localhost:5173");
  });

  it.each([
    ["https://localhost:3000", "Only http:// pages on this machine show here."],
    ["http://example.com:3000", "Only localhost and 127.0.0.1 show here."],
    ["http://user:pw@localhost:3000", "An address with a user name or password is not shown."],
    ["http://localhost", "Name the port (localhost:3000)."],
    ["file:///etc/passwd", "Only http:// pages on this machine show here."],
  ])("refuses %s and says why; the frame stays as it was", async (typed, why) => {
    const { container, onNavigate } = mount("http://localhost:3000/");
    const bar = screen.getByRole("textbox", { name: "Address" });
    await userEvent.clear(bar);
    await userEvent.type(bar, `${typed}{Enter}`);
    expect(screen.getByRole("alert")).toHaveTextContent(why);
    expect(onNavigate).not.toHaveBeenCalled();
    expect(frame(container)).toHaveAttribute("src", "http://localhost:3000/");
  });

  it("refuses the app's own origin", async () => {
    const { onNavigate } = mount("", "http://127.0.0.1:1420");
    await userEvent.type(screen.getByRole("textbox", { name: "Address" }), "http://127.0.0.1:1420{Enter}");
    expect(screen.getByRole("alert")).toHaveTextContent("That is Mirante itself.");
    expect(onNavigate).not.toHaveBeenCalled();
  });

  it("an address kept from before is checked again; a bad one shows no frame", () => {
    const { container } = mount("https://example.com/");
    expect(frame(container)).toBeNull();
    expect(screen.getByRole("textbox", { name: "Address" })).toHaveValue("");
  });

  it("Back goes to the address before; Reload loads the frame again; Open in browser uses the opener", async () => {
    const { container, onNavigate, env } = mount("http://localhost:3000/");
    const back = screen.getByRole("button", { name: "Back" });
    expect(back).toBeDisabled();
    const bar = screen.getByRole("textbox", { name: "Address" });
    await userEvent.clear(bar);
    await userEvent.type(bar, "localhost:4000/docs{Enter}");
    expect(onNavigate).toHaveBeenLastCalledWith("http://localhost:4000/docs");
    expect(frame(container)).toHaveAttribute("src", "http://localhost:4000/docs");
    expect(back).toBeEnabled();
    await userEvent.click(back);
    expect(onNavigate).toHaveBeenLastCalledWith("http://localhost:3000/");
    expect(frame(container)).toHaveAttribute("src", "http://localhost:3000/");
    expect(back).toBeDisabled();
    const before = frame(container);
    await userEvent.click(screen.getByRole("button", { name: "Reload" }));
    expect(frame(container)).not.toBe(before);
    await userEvent.click(screen.getByRole("button", { name: "Open in browser" }));
    expect(env.openUrl).toHaveBeenCalledWith("http://localhost:3000/");
  });

  it("a server that refuses to be framed: says so and offers Open in browser", async () => {
    const { container, env } = mount("http://localhost:3000/");
    const f = frame(container)!;
    // A refused frame stays an empty document of the app's own origin (readable here).
    fireEvent.load(f);
    expect(await screen.findByText("This server can't be shown here.")).toBeInTheDocument();
    expect(frame(container)).toBeNull();
    const open = screen.getAllByRole("button", { name: "Open in browser" });
    await userEvent.click(open[open.length - 1]);
    expect(env.openUrl).toHaveBeenCalledWith("http://localhost:3000/");
    // Reload tries again.
    await userEvent.click(screen.getByRole("button", { name: "Reload" }));
    expect(frame(container)).not.toBeNull();
  });

  it("a page that loaded (cross-origin, unreadable) shows normally", () => {
    const { container } = mount("http://localhost:3000/");
    loadedCrossOrigin(frame(container)!);
    expect(screen.queryByText("This server can't be shown here.")).toBeNull();
    expect(frame(container)).not.toBeNull();
  });

  it("Open in browser that fails says so", async () => {
    const { env } = mount("http://localhost:3000/");
    (env.openUrl as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error("denied"));
    await userEvent.click(screen.getByRole("button", { name: "Open in browser" }));
    expect(await screen.findByText(/could not open the browser/)).toBeInTheDocument();
  });
});

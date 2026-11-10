import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { makeEnv, renderWith } from "../test/env";
import { Disclosure, disclosureKey } from "./Disclosure";

const mount = (env = makeEnv().env) => {
  const r = renderWith(
    env,
    <Disclosure id="box" title="Box" summary="3 things">
      <button>Inside</button>
    </Disclosure>,
  );
  return { env, ...r };
};

describe("Disclosure", () => {
  it("is closed by default: the title and the summary on one header button, the body hidden", () => {
    mount();
    const head = screen.getByRole("button", { name: "Box 3 things" });
    expect(head).toHaveAttribute("aria-expanded", "false");
    const body = document.getElementById(head.getAttribute("aria-controls")!)!;
    expect(body).not.toBeVisible();
    expect(screen.queryByRole("button", { name: "Inside" })).toBeNull();
    expect(screen.getByRole("region", { name: "Box" })).toBeInTheDocument();
  });

  it("opens and closes from the header, by click or keyboard; the summary shows only while closed", async () => {
    mount();
    const head = screen.getByRole("button", { name: /^Box/ });
    await userEvent.click(head);
    expect(head).toHaveAttribute("aria-expanded", "true");
    expect(head).toHaveAccessibleName("Box");
    expect(screen.getByRole("button", { name: "Inside" })).toBeVisible();
    head.focus();
    await userEvent.keyboard("{Enter}");
    expect(head).toHaveAttribute("aria-expanded", "false");
    await userEvent.keyboard(" ");
    expect(head).toHaveAttribute("aria-expanded", "true");
  });

  it("remembers open or closed per viewer", async () => {
    const { env, unmount } = mount();
    await userEvent.click(screen.getByRole("button", { name: /^Box/ }));
    expect(env.storage.getItem(disclosureKey("box"))).toBe("1");
    unmount();
    mount(env);
    expect(screen.getByRole("button", { name: /^Box/ })).toHaveAttribute("aria-expanded", "true");
    await userEvent.click(screen.getByRole("button", { name: /^Box/ }));
    expect(env.storage.getItem(disclosureKey("box"))).toBe("0");
  });

  it("storage that throws means closed, and toggling still works", async () => {
    const env = makeEnv().env;
    env.storage = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
    };
    mount(env);
    const head = screen.getByRole("button", { name: /^Box/ });
    expect(head).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(head);
    expect(head).toHaveAttribute("aria-expanded", "true");
  });
});

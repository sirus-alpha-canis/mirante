import { describe, expect, it } from "vitest";
import { type Block, type Inline, inlines, linkTarget, parse, resolveRel } from "./markdown";

const texts = (inl: Inline[]): string => inl.map((i) => ("v" in i ? i.v : "c" in i ? texts(i.c) : i.t === "img" ? i.alt : "")).join("");

describe("markdown parser", () => {
  it("parses the whitelist into an AST", () => {
    expect(parse("# T\n\n- a\n- **b**\n\n```rs\nfn x(){}\n```").map((n) => n.type)).toEqual(["heading", "list", "code"]);
  });

  it("headings, paragraphs, code fences keep their text", () => {
    const [h, p, c] = parse("## Title ##\n\nline one\nline two\n\n~~~\n<b>raw</b>\n~~~") as [
      Extract<Block, { type: "heading" }>,
      Extract<Block, { type: "para" }>,
      Extract<Block, { type: "code" }>,
    ];
    expect(h.level).toBe(2);
    expect(texts(h.inl)).toBe("Title");
    expect(texts(p.inl)).toBe("line one\nline two");
    expect(c).toEqual({ type: "code", lang: null, text: "<b>raw</b>" });
  });

  it("an unterminated fence runs to the end", () => {
    expect(parse("```py\nprint(1)\nmore")).toEqual([{ type: "code", lang: "py", text: "print(1)\nmore" }]);
  });

  it("tables, nested lists (2 levels), blockquote, hr, inline code", () => {
    const blocks = parse(
      ["| a | b |", "|---|:-:|", "| 1 | `2` |", "", "1. one", "   - inner", "2. two", "", "> quoted", "> # head", "", "---"].join("\n"),
    );
    expect(blocks.map((b) => b.type)).toEqual(["table", "list", "quote", "hr"]);
    const table = blocks[0] as Extract<Block, { type: "table" }>;
    expect(table.head.map(texts)).toEqual(["a", "b"]);
    expect(table.rows[0][1]).toEqual([{ t: "code", v: "2" }]);
    const list = blocks[1] as Extract<Block, { type: "list" }>;
    expect(list.ordered).toBe(true);
    expect(list.items).toHaveLength(2);
    expect(list.items[0].map((b) => b.type)).toEqual(["para", "list"]);
    const quote = blocks[2] as Extract<Block, { type: "quote" }>;
    expect(quote.blocks.map((b) => b.type)).toEqual(["para", "heading"]);
  });

  it("inlines: strong, em, code, links, images, escapes; raw HTML stays text", () => {
    expect(inlines("a **b** *c* `d`")).toEqual([
      { t: "text", v: "a " },
      { t: "strong", c: [{ t: "text", v: "b" }] },
      { t: "text", v: " " },
      { t: "em", c: [{ t: "text", v: "c" }] },
      { t: "text", v: " " },
      { t: "code", v: "d" },
    ]);
    expect(inlines("[x](https://github.com/o) ![logo](img/l.png)")).toEqual([
      { t: "link", href: "https://github.com/o", c: [{ t: "text", v: "x" }] },
      { t: "text", v: " " },
      { t: "img", src: "img/l.png", alt: "logo" },
    ]);
    expect(texts(inlines("<script>alert(1)</script> \\*no\\*"))).toBe("<script>alert(1)</script> *no*");
    expect(inlines("snake_case_name")).toEqual([{ t: "text", v: "snake_case_name" }]);
  });

  it("never throws on hostile or degenerate input", () => {
    for (const s of ["", "[", "![", "**", "`", "[a](", "> > > > > > > > > > x", "|", "- ".repeat(200), "*".repeat(5000), "[".repeat(3000)]) {
      expect(() => parse(s)).not.toThrow();
    }
  });
});

describe("linkTarget", () => {
  it("github and localhost links go through the opener", () => {
    expect(linkTarget("https://github.com/o/r", "README.md")).toEqual({ kind: "open", url: "https://github.com/o/r" });
    expect(linkTarget("http://localhost:5173/x", "README.md")).toEqual({ kind: "open", url: "http://localhost:5173/x" });
  });

  it("javascript:, data:, file:, vbscript: and other hosts are copy-only", () => {
    for (const h of ["javascript:alert(1)", "JaVaScRiPt:alert(1)", "data:text/html,x", "file:///etc/passwd", "vbscript:x", "https://evil.example/", "//evil.example/x", "java\nscript:alert(1)"]) {
      expect(linkTarget(h, "README.md").kind, h).toBe("copy");
    }
  });

  it("relative links open the file in the side pane, relative to the file", () => {
    expect(linkTarget("guide.md", "docs/README.md")).toEqual({ kind: "pane", path: "docs/guide.md" });
    expect(linkTarget("../src/x.rs#L12", "docs/README.md")).toEqual({ kind: "pane", path: "src/x.rs", line: 12 });
    expect(linkTarget("./a%20b.md", "README.md")).toEqual({ kind: "pane", path: "a b.md" });
    expect(linkTarget("/abs.md", "docs/README.md")).toEqual({ kind: "pane", path: "abs.md" });
    expect(linkTarget("../../etc/passwd", "docs/README.md").kind).toBe("copy");
    expect(linkTarget("#section", "README.md").kind).toBe("copy");
  });

  it("resolveRel refuses climbing out of the root", () => {
    expect(resolveRel("a/b.md", "../c.md")).toBe("c.md");
    expect(resolveRel("b.md", "../c.md")).toBeNull();
  });
});

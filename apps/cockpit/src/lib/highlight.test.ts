import { describe, expect, it } from "vitest";
import { highlight, type Tok } from "./highlight";

const kinds = (lines: Tok[][], k: Tok["k"]) => lines.flat().filter((t) => t.k === k).map((t) => t.v);
const joined = (lines: Tok[][]) => lines.map((l) => l.map((t) => t.v).join("")).join("\n");

describe("highlight", () => {
  it("tokenizes rust/ts/py/sh/json/toml/yaml keywords strings comments numbers", () => {
    const rs = highlight('fn main() { let x = 42; // hi\n  "s" }', "rust");
    expect(kinds(rs, "kw")).toEqual(["fn", "let"]);
    expect(kinds(rs, "num")).toEqual(["42"]);
    expect(kinds(rs, "com")).toEqual(["// hi"]);
    expect(kinds(rs, "str")).toEqual(['"s"']);
    const ts = highlight("const a = `t${1}`; /* c */ export function f() {}", "ts");
    expect(kinds(ts, "kw")).toEqual(["const", "export", "function"]);
    expect(kinds(ts, "com")).toEqual(["/* c */"]);
    const py = highlight('def f():\n    return "x"  # note\n"""doc\nmore"""', "python");
    expect(kinds(py, "kw")).toEqual(["def", "return"]);
    expect(kinds(py, "com")).toEqual(["# note"]);
    expect(kinds(py, "str")).toEqual(['"x"', '"""doc', 'more"""']);
    const sh = highlight("if [ -f x ]; then echo 'a'; fi # done", "sh");
    expect(kinds(sh, "kw")).toEqual(["if", "then", "echo", "fi"]);
    expect(kinds(sh, "com")).toEqual(["# done"]);
    const json = highlight('{"a": 1.5, "b": true, "c": null}', "json");
    expect(kinds(json, "str")).toEqual(['"a"', '"b"', '"c"']);
    expect(kinds(json, "num")).toEqual(["1.5"]);
    expect(kinds(json, "kw")).toEqual(["true", "null"]);
    const toml = highlight('[package]\nname = "x" # n\nv = 3', "toml");
    expect(kinds(toml, "str")).toEqual(['"x"']);
    expect(kinds(toml, "com")).toEqual(["# n"]);
    expect(kinds(toml, "num")).toEqual(["3"]);
    const yaml = highlight("on: push # trigger\nn: 2\nok: true", "yaml");
    expect(kinds(yaml, "com")).toEqual(["# trigger"]);
    expect(kinds(yaml, "kw")).toEqual(["true"]);
  });

  it("keeps every character, line by line", () => {
    const src = 'let s = "a\\"b"; // c\n\n/* x\ny */ 1';
    expect(joined(highlight(src, "rust"))).toBe(src);
    expect(highlight(src, "rust")).toHaveLength(4);
  });

  it("unknown lang is one plain token", () => {
    expect(highlight("fn x() {}", null)).toEqual([[{ k: "plain", v: "fn x() {}" }]]);
    expect(highlight("a\nb", "cobol")).toEqual([[{ k: "plain", v: "a" }], [{ k: "plain", v: "b" }]]);
  });

  it("unterminated string or block comment never throws and covers the rest", () => {
    const s = highlight('let a = "open\nstill', "rust");
    expect(kinds(s, "str")).toEqual(['"open', "still"]);
    const c = highlight("/* open\nmore\nend", "ts");
    expect(kinds(c, "com")).toEqual(["/* open", "more", "end"]);
  });

  it("over 5,000 lines or 512 KiB returns plain", () => {
    const many = "let x = 1;\n".repeat(5001);
    expect(highlight(many, "rust").flat().every((t) => t.k === "plain")).toBe(true);
    const wide = "let x = 1; ".repeat(50_000);
    expect(highlight(wide, "rust").flat().every((t) => t.k === "plain")).toBe(true);
  });
});

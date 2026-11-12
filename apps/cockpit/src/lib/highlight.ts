// A small syntax highlighter for the side pane: keywords, strings, comments and numbers by
// per-family rule tables (C-like, Python/Ruby, shell-like, data, SQL). Its own code, no
// dependency. Output is plain tokens per line that `FileView` renders as text nodes, so a
// file's content can never become markup. Unknown languages, and files past the limits,
// are plain text.

export type TokKind = "kw" | "str" | "com" | "num" | "plain";
export interface Tok {
  k: TokKind;
  v: string;
}

/** Past either, the file is shown plain (highlighting stays cheap). */
export const HIGHLIGHT_MAX_LINES = 5000;
export const HIGHLIGHT_MAX_BYTES = 512 * 1024;

interface Family {
  line: string[];
  block?: [string, string];
  quotes: string[];
  /** `"""` / `'''` strings (Python). */
  triple?: boolean;
  /** `'` starts a string only as a short char literal (Rust lifetimes, C chars). */
  charQuote?: boolean;
  kw: ReadonlySet<string>;
  /** Keywords match in any case (SQL). */
  ci?: boolean;
}

const words = (s: string) => new Set(s.split(/\s+/).filter(Boolean));

const C_LIKE_BASE = { line: ["//"], block: ["/*", "*/"] as [string, string] };
const JS_KW = words(`
  abstract as async await break case catch class const continue debugger declare default delete do else enum export
  extends false finally for from function get if implements import in instanceof interface is keyof let new null
  of private protected public readonly return satisfies set static super switch this throw true try type typeof
  undefined var void while with yield`);

const FAMILIES: Record<string, Family> = {
  rust: {
    ...C_LIKE_BASE,
    quotes: ['"', "'"],
    charQuote: true,
    kw: words(`as async await break const continue crate dyn else enum extern false fn for if impl in let loop match mod
      move mut pub ref return self Self static struct super trait true type unsafe use where while`),
  },
  ts: { ...C_LIKE_BASE, quotes: ['"', "'", "`"], kw: JS_KW },
  go: {
    ...C_LIKE_BASE,
    quotes: ['"', "'", "`"],
    kw: words(`break case chan const continue default defer else fallthrough false for func go goto if import interface
      map nil package range return select struct switch true type var`),
  },
  java: {
    ...C_LIKE_BASE,
    quotes: ['"', "'"],
    charQuote: true,
    kw: words(`abstract break case catch class const continue default do else enum extends false final finally for fun
      if implements import in interface is let new null object override package private protected public return static
      super switch this throw true try val var void when while func guard struct protocol extension self nil`),
  },
  c: {
    ...C_LIKE_BASE,
    quotes: ['"', "'"],
    charQuote: true,
    kw: words(`auto break case char class const continue default delete do double else enum extern false float for goto if
      include inline int long namespace new nullptr private protected public return short signed sizeof static struct
      switch template this true typedef union unsigned using virtual void volatile while define`),
  },
  css: { line: [], block: ["/*", "*/"], quotes: ['"', "'"], kw: words("important media import from to") },
  python: {
    line: ["#"],
    quotes: ['"', "'"],
    triple: true,
    kw: words(`and as assert async await break class continue def del elif else except False finally for from global if
      import in is lambda None nonlocal not or pass raise return True try while with yield self`),
  },
  ruby: {
    line: ["#"],
    quotes: ['"', "'"],
    kw: words(`alias and begin break case class def do else elsif end ensure false for if in module next nil not or redo
      rescue retry return self super then true undef unless until when while yield require`),
  },
  sh: {
    line: ["#"],
    quotes: ['"', "'"],
    kw: words(`if then else elif fi for while until do done case esac in function return local export echo exit set unset
      source shift test true false readonly`),
  },
  data: { line: ["#"], quotes: ['"', "'"], kw: words("true false null") },
  json: { line: [], quotes: ['"'], kw: words("true false null") },
  sql: {
    line: ["--"],
    block: ["/*", "*/"],
    quotes: ["'", '"'],
    ci: true,
    kw: words(`select from where and or not insert into values update set delete create table index view drop alter add
      primary key foreign references join left right inner outer on group by order having limit offset as distinct
      union all null is in exists case when then else end begin commit rollback trigger default integer text`),
  },
};

/** The highlighter's language (`task.file`'s `lang`) to its family. */
const FAMILY_OF: Record<string, string> = {
  rust: "rust",
  ts: "ts",
  tsx: "ts",
  js: "ts",
  jsx: "ts",
  go: "go",
  java: "java",
  kotlin: "java",
  swift: "java",
  c: "c",
  cpp: "c",
  csharp: "c",
  php: "c",
  css: "css",
  python: "python",
  ruby: "ruby",
  sh: "sh",
  make: "sh",
  dockerfile: "sh",
  toml: "data",
  yaml: "data",
  json: "json",
  sql: "sql",
};

const plainLines = (text: string): Tok[][] => text.split("\n").map((l) => [{ k: "plain", v: l }]);

const WORD_START = /[A-Za-z_$]/;
const WORD = /[\w$]/;

function scan(text: string, f: Family): Tok[] {
  const out: Tok[] = [];
  let plain = "";
  const push = (k: TokKind, v: string) => {
    if (k === "plain") {
      plain += v;
      return;
    }
    if (plain) out.push({ k: "plain", v: plain });
    plain = "";
    out.push({ k, v });
  };
  const n = text.length;
  let i = 0;
  while (i < n) {
    const c = text[i];
    // Comments.
    const lc = f.line.find((m) => text.startsWith(m, i));
    if (lc !== undefined) {
      const end = text.indexOf("\n", i);
      const stop = end < 0 ? n : end;
      push("com", text.slice(i, stop));
      i = stop;
      continue;
    }
    if (f.block && text.startsWith(f.block[0], i)) {
      const end = text.indexOf(f.block[1], i + f.block[0].length);
      const stop = end < 0 ? n : end + f.block[1].length;
      push("com", text.slice(i, stop));
      i = stop;
      continue;
    }
    // Strings.
    if (f.quotes.includes(c)) {
      if (f.triple && text.startsWith(c.repeat(3), i)) {
        const end = text.indexOf(c.repeat(3), i + 3);
        const stop = end < 0 ? n : end + 3;
        push("str", text.slice(i, stop));
        i = stop;
        continue;
      }
      if (c === "'" && f.charQuote) {
        const m = /^'(?:\\.[^']{0,8}|[^'\\\n])'/.exec(text.slice(i, i + 12));
        if (!m) {
          push("plain", c);
          i++;
          continue;
        }
        push("str", m[0]);
        i += m[0].length;
        continue;
      }
      let j = i + 1;
      while (j < n && text[j] !== c) j += text[j] === "\\" ? 2 : 1;
      const stop = Math.min(n, j + 1);
      push("str", text.slice(i, stop));
      i = stop;
      continue;
    }
    // Numbers (not inside a word).
    if (/\d/.test(c) && !WORD.test(text[i - 1] ?? "")) {
      const m = /^(?:0[xX][\da-fA-F_]+|\d[\d_]*(?:\.\d+)?(?:[eE][+-]?\d+)?)/.exec(text.slice(i, i + 64));
      if (m) {
        push("num", m[0]);
        i += m[0].length;
        continue;
      }
    }
    // Words.
    if (WORD_START.test(c)) {
      let j = i + 1;
      while (j < n && WORD.test(text[j])) j++;
      const w = text.slice(i, j);
      push(f.kw.has(f.ci ? w.toLowerCase() : w) ? "kw" : "plain", w);
      i = j;
      continue;
    }
    push("plain", c);
    i++;
  }
  if (plain) out.push({ k: "plain", v: plain });
  return out;
}

/** `text` as lines of tokens for `lang` (from `task.file`); plain when unknown or too big. */
export function highlight(text: string, lang: string | null | undefined): Tok[][] {
  const fam = lang ? FAMILIES[FAMILY_OF[lang] ?? ""] : undefined;
  if (!fam || text.length > HIGHLIGHT_MAX_BYTES) return plainLines(text);
  let lines = 1;
  for (let i = text.indexOf("\n"); i >= 0; i = text.indexOf("\n", i + 1)) if (++lines > HIGHLIGHT_MAX_LINES) return plainLines(text);
  const out: Tok[][] = [[]];
  for (const tok of scan(text, fam)) {
    const parts = tok.v.split("\n");
    parts.forEach((p, n) => {
      if (n > 0) out.push([]);
      if (p) out[out.length - 1].push({ k: tok.k, v: p });
    });
  }
  return out;
}

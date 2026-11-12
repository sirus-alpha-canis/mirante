// A small Markdown parser for the side pane's preview: a whitelisted AST, never HTML.
//
// Files in a worktree are untrusted (an agent wrote them, or a stranger's PR did), so the
// output is a closed set of node types that `components/Markdown.tsx` maps to fixed React
// elements. Raw HTML is never recognised: `<script>` stays the text "<script>". Odd
// constructs fall back to text, which is the safe direction. Covers CommonMark's common
// subset: headings, paragraphs, lists (nested), fenced code, block quotes, rules, pipe
// tables; inline strong, emphasis, code, links, images and backslash escapes.
import { openableLink } from "./links";

export type Inline =
  | { t: "text"; v: string }
  | { t: "strong" | "em"; c: Inline[] }
  | { t: "code"; v: string }
  | { t: "link"; href: string; c: Inline[] }
  | { t: "img"; src: string; alt: string };

export type Block =
  | { type: "heading"; level: 1 | 2 | 3 | 4 | 5 | 6; inl: Inline[] }
  | { type: "para"; inl: Inline[] }
  | { type: "list"; ordered: boolean; items: Block[][] }
  | { type: "code"; lang: string | null; text: string }
  | { type: "quote"; blocks: Block[] }
  | { type: "hr" }
  | { type: "table"; head: Inline[][]; rows: Inline[][][] };

/** Past this, the preview shows the source as plain text (with a note). */
export const MARKDOWN_MAX = 256 * 1024;
/** How deep quotes and lists nest before the rest is a paragraph of text. */
const MAX_DEPTH = 6;
/** How deep emphasis nests before the rest is text. */
const MAX_INLINE_DEPTH = 4;

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([\w+#.-]{0,32})?.*$/;
const HEADING = /^ {0,3}(#{1,6})(?:\s+(.*?))?\s*$/;
const HR = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/;
const QUOTE = /^ {0,3}> ?(.*)$/;
const ITEM = /^( *)([-*+]|\d{1,9}[.)])(?:\s+(.*))?$/;
const TABLE_SEP = /^\s*\|?\s*:?-+:?\s*(\|\s*:?-+:?\s*)*\|?\s*$/;

const indentOf = (l: string) => l.length - l.trimStart().length;
const blank = (l: string) => l.trim() === "";

function startsBlock(l: string): boolean {
  return FENCE.test(l) || HEADING.test(l) || HR.test(l) || QUOTE.test(l) || ITEM.test(l);
}

/** The cells of a pipe-table row. */
function cells(l: string): string[] {
  let s = l.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|") && !s.endsWith("\\|")) s = s.slice(0, -1);
  const out: string[] = [];
  let cur = "";
  for (let i = 0; i < s.length; i++) {
    if (s[i] === "\\" && s[i + 1] === "|") {
      cur += "|";
      i++;
    } else if (s[i] === "|") {
      out.push(cur.trim());
      cur = "";
    } else cur += s[i];
  }
  out.push(cur.trim());
  return out;
}

function parseBlocks(lines: string[], depth: number): Block[] {
  const out: Block[] = [];
  if (depth > MAX_DEPTH) {
    const text = lines.join("\n").trim();
    return text ? [{ type: "para", inl: [{ t: "text", v: text }] }] : [];
  }
  let i = 0;
  while (i < lines.length) {
    const l = lines[i];
    if (blank(l)) {
      i++;
      continue;
    }
    const fence = FENCE.exec(l);
    if (fence) {
      const mark = fence[1];
      const body: string[] = [];
      i++;
      while (i < lines.length && !(lines[i].trim().startsWith(mark[0].repeat(mark.length)) && lines[i].trim().replace(new RegExp(`^\\${mark[0]}+`), "").trim() === "")) {
        body.push(lines[i]);
        i++;
      }
      i++; // the closing fence (or past the end)
      out.push({ type: "code", lang: fence[2] || null, text: body.join("\n") });
      continue;
    }
    const h = HEADING.exec(l);
    if (h) {
      const text = (h[2] ?? "").replace(/\s+#+\s*$/, "").replace(/^#+$/, "");
      out.push({ type: "heading", level: h[1].length as 1 | 2 | 3 | 4 | 5 | 6, inl: inlines(text) });
      i++;
      continue;
    }
    if (HR.test(l)) {
      out.push({ type: "hr" });
      i++;
      continue;
    }
    if (QUOTE.test(l)) {
      const body: string[] = [];
      while (i < lines.length && !blank(lines[i])) {
        const q = QUOTE.exec(lines[i]);
        body.push(q ? q[1] : lines[i]);
        i++;
      }
      out.push({ type: "quote", blocks: parseBlocks(body, depth + 1) });
      continue;
    }
    const item = ITEM.exec(l);
    if (item) {
      const base = item[1].length;
      const ordered = /\d/.test(item[2]);
      const items: string[][] = [];
      // Where an item's content starts: continuation lines are cut there.
      let offset = base + item[2].length + 1;
      while (i < lines.length) {
        const cur = lines[i];
        const m = ITEM.exec(cur);
        if (m && m[1].length === base && /\d/.test(m[2]) === ordered) {
          items.push([m[3] ?? ""]);
          offset = base + m[2].length + 1;
          i++;
          continue;
        }
        if (blank(cur)) {
          // A blank line continues the list only when indented content follows.
          const next = lines[i + 1];
          if (next !== undefined && !blank(next) && (indentOf(next) > base || (ITEM.exec(next)?.[1].length === base))) {
            items[items.length - 1].push("");
            i++;
            continue;
          }
          break;
        }
        if (indentOf(cur) > base) {
          items[items.length - 1].push(cur.slice(Math.min(indentOf(cur), offset)));
          i++;
          continue;
        }
        // A lazy continuation line of the item's paragraph.
        if (!startsBlock(cur)) {
          items[items.length - 1].push(cur);
          i++;
          continue;
        }
        break;
      }
      out.push({ type: "list", ordered, items: items.map((body) => parseBlocks(body, depth + 1)) });
      continue;
    }
    if (l.includes("|") && i + 1 < lines.length && TABLE_SEP.test(lines[i + 1]) && lines[i + 1].includes("-")) {
      const head = cells(l).map(inlines);
      i += 2;
      const rows: Inline[][][] = [];
      while (i < lines.length && !blank(lines[i]) && lines[i].includes("|")) {
        const r = cells(lines[i]);
        rows.push(head.map((_, n) => inlines(r[n] ?? "")));
        i++;
      }
      out.push({ type: "table", head, rows });
      continue;
    }
    const para: string[] = [];
    while (i < lines.length && !blank(lines[i]) && (para.length === 0 || !startsBlock(lines[i]))) {
      para.push(lines[i].trim());
      i++;
    }
    out.push({ type: "para", inl: inlines(para.join("\n")) });
  }
  return out;
}

/** The blocks of a Markdown document. */
export function parse(src: string): Block[] {
  return parseBlocks(src.replace(/\r\n?/g, "\n").split("\n"), 0);
}

const ESCAPABLE = "\\`*_{}[]()#+-.!|<>~";
const isWord = (c: string | undefined) => !!c && /[\p{L}\p{N}]/u.test(c);

/** `[text](href)` starting at `s[i] === "["`: the text, the href and where it ends. */
function linkAt(s: string, i: number): { text: string; href: string; end: number } | null {
  let depth = 0;
  let j = i;
  for (; j < s.length; j++) {
    if (s[j] === "\\") {
      j++;
      continue;
    }
    if (s[j] === "[") depth++;
    else if (s[j] === "]" && --depth === 0) break;
  }
  if (j >= s.length || s[j + 1] !== "(") return null;
  const close = s.indexOf(")", j + 2);
  if (close < 0) return null;
  let target = s.slice(j + 2, close).trim();
  // An optional title after the destination is dropped.
  const sp = target.search(/\s/);
  if (sp >= 0) target = target.slice(0, sp);
  if (target.startsWith("<") && target.endsWith(">")) target = target.slice(1, -1);
  return { text: s.slice(i + 1, j), href: target, end: close + 1 };
}

/** The inline nodes of a paragraph's text. */
export function inlines(s: string, depth = 0): Inline[] {
  const out: Inline[] = [];
  let buf = "";
  const flush = () => {
    if (buf) out.push({ t: "text", v: buf });
    buf = "";
  };
  if (depth > MAX_INLINE_DEPTH) return s ? [{ t: "text", v: s }] : [];
  let i = 0;
  while (i < s.length) {
    const c = s[i];
    if (c === "\\" && i + 1 < s.length && ESCAPABLE.includes(s[i + 1])) {
      buf += s[i + 1];
      i += 2;
      continue;
    }
    if (c === "`") {
      let n = 1;
      while (s[i + n] === "`") n++;
      const fence = "`".repeat(n);
      const close = s.indexOf(fence, i + n);
      if (close > i) {
        flush();
        const v = s.slice(i + n, close);
        out.push({ t: "code", v: v.length > 1 && v.startsWith(" ") && v.endsWith(" ") ? v.slice(1, -1) : v });
        i = close + n;
        continue;
      }
      buf += fence;
      i += n;
      continue;
    }
    if (c === "!" && s[i + 1] === "[") {
      const l = linkAt(s, i + 1);
      if (l) {
        flush();
        out.push({ t: "img", src: l.href, alt: l.text });
        i = l.end;
        continue;
      }
    }
    if (c === "[") {
      const l = linkAt(s, i);
      if (l) {
        flush();
        out.push({ t: "link", href: l.href, c: inlines(l.text, depth + 1) });
        i = l.end;
        continue;
      }
    }
    if (c === "<") {
      const m = /^<(https?:\/\/[^\s<>]{1,2000})>/.exec(s.slice(i, i + 2010));
      if (m) {
        flush();
        out.push({ t: "link", href: m[1], c: [{ t: "text", v: m[1] }] });
        i += m[0].length;
        continue;
      }
    }
    if ((c === "*" || c === "_") && s[i + 1] === c) {
      const close = s.indexOf(c + c, i + 2);
      const okOpen = c === "*" || !isWord(s[i - 1]);
      if (okOpen && close > i + 2 && !/\s/.test(s[i + 2]) && (c === "*" || !isWord(s[close + 2]))) {
        flush();
        out.push({ t: "strong", c: inlines(s.slice(i + 2, close), depth + 1) });
        i = close + 2;
        continue;
      }
      buf += c + c;
      i += 2;
      continue;
    }
    if (c === "*" || c === "_") {
      const close = s.indexOf(c, i + 1);
      const okOpen = c === "*" || !isWord(s[i - 1]);
      if (okOpen && close > i + 1 && !/\s/.test(s[i + 1]) && !/\s/.test(s[close - 1]) && (c === "*" || !isWord(s[close + 1]))) {
        flush();
        out.push({ t: "em", c: inlines(s.slice(i + 1, close), depth + 1) });
        i = close + 1;
        continue;
      }
    }
    buf += c;
    i++;
  }
  flush();
  return out;
}

/** `rel` resolved against the folder of `file` (both worktree-relative); null when it climbs
 * out of the worktree. A leading "/" is the worktree's root. */
export function resolveRel(file: string, rel: string): string | null {
  const parts = rel.startsWith("/") ? [] : file.split("/").slice(0, -1);
  for (const seg of rel.split("/")) {
    if (seg === "" || seg === ".") continue;
    if (seg === "..") {
      if (parts.length === 0) return null;
      parts.pop();
    } else parts.push(seg);
  }
  return parts.length ? parts.join("/") : null;
}

export type LinkTarget = { kind: "open"; url: string } | { kind: "pane"; path: string; line?: number } | { kind: "copy"; href: string };

/** Where a link in a worktree file goes: the opener (github.com and localhost, as the
 * terminal's links), the side pane (a relative path, resolved against the file), or nowhere
 * (shown with "copy link"). */
export function linkTarget(href: string, filePath: string): LinkTarget {
  const copy: LinkTarget = { kind: "copy", href };
  // Control characters and whitespace inside a scheme are a classic filter bypass.
  if (/[\u0000-\u001f\u007f]/.test(href)) return copy;
  const h = href.trim();
  if (!h || h.startsWith("#") || h.startsWith("//")) return copy;
  if (/^[a-z][a-z0-9+.-]*:/i.test(h)) {
    let url: URL;
    try {
      url = new URL(h);
    } catch {
      return copy;
    }
    return openableLink(url) ? { kind: "open", url: url.href } : copy;
  }
  const [pathPart, frag = ""] = h.split("#", 2);
  const clean = pathPart.split("?")[0];
  let decoded: string;
  try {
    decoded = decodeURIComponent(clean);
  } catch {
    return copy;
  }
  const path = resolveRel(filePath, decoded);
  if (!path) return copy;
  const line = /^L(\d{1,7})/.exec(frag);
  return line ? { kind: "pane", path, line: Number(line[1]) } : { kind: "pane", path };
}

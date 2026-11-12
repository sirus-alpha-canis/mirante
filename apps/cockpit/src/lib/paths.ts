// File paths the person can click: in a Steps line, in the terminal's text (`src/a.rs:12:3`,
// `/abs/path.ts#L7`), or in an OSC 8 `file://` link. Each is mapped to a path relative to
// where the agent runs (the worktree, or a workspace task's folder: `<repo>/…`); anything
// outside it is refused here, and the daemon checks again when the file is read.

export interface PathTask {
  worktree_path: string;
  workspace_path?: string | null;
  repos?: { name: string; worktree_path: string }[] | null;
}

/** macOS: /tmp and /var are links into /private; agents print either form. */
const canon = (p: string) => p.replace(/^\/private(?=\/(?:tmp|var)(?:\/|$))/, "");

/** `rel`'s levels, checked: no empty level, ".", ".." or NUL. */
function cleanRel(rel: string): string | null {
  const segs = rel.split("/");
  if (segs.some((s) => s === "" || s === "." || s === ".." || s.includes("\u0000"))) return null;
  return segs.join("/");
}

/** `raw` (absolute, or relative to the agent's folder) as a path relative to the task's
 * folder; null when it is outside it. */
export function toWorktreeRel(raw: string, task: PathTask): string | null {
  let p = raw.trim();
  if (!p || p.startsWith("~") || p.includes("\\")) return null;
  if (p.startsWith("/")) {
    p = canon(p);
    const root = canon(task.workspace_path || task.worktree_path).replace(/\/+$/, "");
    if (!root || !p.startsWith(`${root}/`)) return null;
    p = p.slice(root.length + 1);
  } else {
    p = p.replace(/^(?:\.\/)+/, "");
  }
  return cleanRel(p);
}

export interface FoundPath {
  /** Where it is in the line (string offsets, end exclusive). */
  start: number;
  end: number;
  text: string;
  path: string;
  line?: number;
  col?: number;
}

/** Longest line scanned and longest path taken (the terminal hands over each line). */
const LINE_MAX = 2000;
const PATH_MAX = 200;

// A path: `a/b/c.ext`, `/abs/c`, `./x`, or a bare `name.ext`; then `:line[:col]` or `#Lline`.
// It never starts right after a word character, "/", ":", "." or "-", so the parts of a URL
// (`https://host/a.js`) are never taken for one.
const PATH = /(?<![\w/:.\-@~+%])((?:\.{0,2}\/)?[\w@+~-][\w.@+~-]*(?:\/[\w.@+~-]+)*)(?::(\d{1,7})(?::(\d{1,5}))?|#L(\d{1,7}))?/g;

/** The path-looking tokens of one terminal (or Steps) line. */
export function detectPaths(text: string): FoundPath[] {
  const s = text.length > LINE_MAX ? text.slice(0, LINE_MAX) : text;
  const out: FoundPath[] = [];
  for (const m of s.matchAll(PATH)) {
    let path = m[1];
    // Sentence punctuation after a path is not part of it.
    path = path.replace(/[.]+$/, "");
    if (path.length > PATH_MAX || path.length === 0) continue;
    const slash = path.includes("/");
    const dot = path.lastIndexOf(".");
    const base = path.slice(path.lastIndexOf("/") + 1);
    const ext = dot > path.lastIndexOf("/") ? path.slice(dot + 1) : "";
    if (!slash) {
      // A bare name needs a real extension (`README.md`, not `3.50`, `1.2.3` or `e.g`).
      if (!/^[A-Za-z][A-Za-z0-9]{0,9}$/.test(ext) || base.length - ext.length - 1 < 2 && ext.length < 2) continue;
      if (/^\d/.test(base) && /\d/.test(base.slice(0, dot))) continue;
    } else if (!/[A-Za-z]/.test(path)) continue;
    const start = m.index ?? 0;
    const trimmed = m[1].length - path.length;
    const suffix = trimmed > 0 ? "" : m[0].slice(m[1].length);
    const lineNo = m[2] ?? m[4];
    out.push({
      start,
      end: start + path.length + suffix.length,
      text: path + suffix,
      path,
      line: lineNo ? Number(lineNo) : undefined,
      col: m[3] ? Number(m[3]) : undefined,
    });
  }
  return out;
}

/** An OSC 8 `file://` link: its absolute path and line; "other-host" when it names another
 * machine; null when it is no file URL. */
export function fileUrlTarget(uri: string): { path: string; line?: number } | "other-host" | null {
  let url: URL;
  try {
    url = new URL(uri);
  } catch {
    return null;
  }
  if (url.protocol !== "file:") return null;
  if (url.hostname !== "" && url.hostname !== "localhost") return "other-host";
  let path: string;
  try {
    path = decodeURIComponent(url.pathname);
  } catch {
    return null;
  }
  let line: number | undefined;
  const hash = /^#L(\d{1,7})/.exec(url.hash);
  if (hash) line = Number(hash[1]);
  const colon = /:(\d{1,7})(?::\d{1,5})?$/.exec(path);
  if (colon) {
    line ??= Number(colon[1]);
    path = path.slice(0, colon.index);
  }
  return line ? { path, line } : { path };
}

import { FitAddon } from "@xterm/addon-fit";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { WebglAddon } from "@xterm/addon-webgl";
import { type ILinkHandler, Terminal } from "@xterm/xterm";
import { memo, useEffect, useRef, useState } from "react";
import { useEnv, useSelect } from "../env";
import { t, useT } from "../lib/i18n";
import type { State } from "../store/state";
import { attachAck, attachClose, attachOpen, attachWrite, DaemonError, shellAttachOpen } from "../lib/daemon";
import { openableLink } from "../lib/links";
import { detectPaths, fileUrlTarget, toWorktreeRel } from "../lib/paths";
import type { FileView } from "../lib/protocol";
import { usePane } from "./paneContext";
import { M, type TaskView } from "../lib/protocol";
import { type AttachmentView, baseName, pastedName, pathPaste, PUT_MAX, toBase64 } from "../lib/attachments";
import { useDropZone } from "./attachments";

/** Window size changes settle for this long before one `session.resize` goes out. */
const RESIZE_DEBOUNCE_MS = 100;
/** Typed input waiting behind a write in flight; past this the daemon is not keeping up. */
const MAX_PENDING_INPUT = 64 * 1024;
/** Parsed output is acked to the bridge once this much has built up (its window is 256 KiB)… */
const ACK_BATCH = 64 * 1024;
/** …or after this long, whichever comes first. */
const ACK_DELAY_MS = 50;
/** JetBrains Mono, bundled (styles.css @font-face), else the system's monospace. Its
 * ligatures stay off (xterm draws cell by cell; "=>" is two cells). */
export const FONT = '"JetBrains Mono", "SF Mono", Menlo, monospace';
export const FONT_SIZE = 13;
/** The faces the terminal draws with (regular and bold), as `FontFaceSet.load` takes them. */
export const FONT_FACES = [`${FONT_SIZE}px "JetBrains Mono"`, `bold ${FONT_SIZE}px "JetBrains Mono"`];
/** The terminal is graphite in both themes (agents' TUIs assume a dark screen): the
 * Observatório dark surfaces and text (styles.css). */
export const TERMINAL_THEME = { background: "#141519", foreground: "#e7e3dd", cursor: "#e7e3dd", cursorAccent: "#141519", selectionBackground: "#3a3c43" };

/** null when the terminal's font is ready (or the engine cannot say); else a promise of that
 * moment. The first fit (and the attach at its size) waits for it: cells measured in a
 * fallback font's metrics would size the PTY wrong. xterm draws on a canvas, which never
 * asks for a web font, so `fonts.ready` alone could settle before JetBrains Mono was ever
 * fetched: the faces are loaded explicitly. A failed load still settles (the fallback). */
export function fontsPending(doc: Document = document): Promise<void> | null {
  const fonts = (doc as Document & { fonts?: FontFaceSet }).fonts;
  if (!fonts) return null;
  const ready = fonts.status === "loaded" && FONT_FACES.every((f) => fonts.check?.(f) ?? true);
  if (ready) return null;
  const loads = FONT_FACES.map((f) => (typeof fonts.load === "function" ? fonts.load(f) : Promise.resolve([])));
  return Promise.all([...loads, fonts.ready]).then(
    () => undefined,
    () => undefined,
  );
}

/** Re-attach waits after a stream ends (or an attach fails) while the task is live; past
 * the last one the note offers Reconnect. */
export const REATTACH_MS = [500, 1000, 2000, 5000];

/** Where a terminal link may go (see `lib/links`). */
export { openableLink };

/**
 * Terminal output is untrusted. Only the features below are on:
 * - OSC 8 hyperlinks leave through the injected opener (never `window.open`), only to
 *   github.com or a localhost port (see `openableLink`; anything else says to copy it), and
 *   only on a ⌘/Ctrl click so a stray click on spoofed link text does nothing. Hovering one
 *   shows the real target, whatever the link's text claims.
 * - OSC 8 `file://` links (Claude prints them with FORCE_HYPERLINK) never reach the opener:
 *   one on this machine (no host, or localhost) opens in the side pane when it is a file of
 *   the task's worktree (`onFile`); one naming another host says to copy it. Non-http
 *   protocols are let through to `activate` only for this; any other is refused there.
 * - No clipboard addon is loaded, so OSC 52 cannot write the clipboard.
 * - Title changes (OSC 0/2) are not subscribed, so nothing reaches the window title.
 */
function linkHandler(
  openUrl: (url: string) => Promise<void>,
  onHover: (uri: string | null) => void,
  onNote: (note: string) => void,
  onFile: (path: string, line?: number) => void,
): ILinkHandler {
  return {
    allowNonHttpProtocols: true,
    activate(event, uri) {
      if (!(event.metaKey || event.ctrlKey)) return;
      const file = fileUrlTarget(uri);
      if (file === "other-host") return onNote(t("term.fileOther"));
      if (file) return onFile(file.path, file.line);
      let url: URL;
      try {
        url = new URL(uri);
      } catch {
        return;
      }
      if (url.protocol !== "http:" && url.protocol !== "https:") return onNote(t("term.linkBlocked"));
      if (!openableLink(url)) return onNote(t("term.linkBlocked"));
      openUrl(url.href).catch(() => onNote(t("term.linkFailed")));
    },
    hover: (_event, uri) => onHover(uri),
    leave: () => onHover(null),
  };
}

/** xterm's binary strings (mouse reports) carry one byte per char. */
function binaryBytes(s: string): Uint8Array {
  const out = new Uint8Array(s.length);
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i) & 0xff;
  return out;
}

function concat(parts: Uint8Array[]): Uint8Array {
  if (parts.length === 1) return parts[0];
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

const nextFrame = (fn: () => void) => {
  if (typeof requestAnimationFrame === "function") requestAnimationFrame(fn);
  else setTimeout(fn, 16);
};

/** Laid out with a size (not hidden): only then can xterm measure its cells. */
const sized = (el: HTMLElement) => el.isConnected && el.clientWidth > 0 && el.clientHeight > 0;

const selectBoot = (s: State) => s.bootId;
const selectLive = (s: State) => s.connection.state === "live";

/**
 * The task's live PTY over `attach`; keystrokes go back unless the stream is read-only.
 * `active`: its tab is the one shown. It stays mounted and attached while another tab is
 * shown (coming back costs nothing), and first attaches when first shown, at the size it
 * is shown at. The note and the link hint float over the terminal: nothing in it changes
 * its size (each size change is a `session.resize`, and the agent redraws its screen).
 *
 * A stream that ends (or an attach that fails) while the task is live is attached again
 * after `REATTACH_MS`, and at once when the daemon restarts (a new boot) or comes back; the
 * daemon replays the screen on attach, so the terminal is reset first. Past the last wait
 * the note offers Reconnect.
 */
/** Where a terminal's stream comes from: the task's live agent (the default), or one of
 * the person's own shells beside it (`shell.attach`). A shell's stream that ends is not
 * attached again: the shell is gone (`onEnd` says so), and files dropped or pasted into it
 * are not stored as attachments (a text paste is xterm's, as everywhere). */
export type TermSource = { kind: "task" } | { kind: "shell"; shell: string; n: number; onEnd(): void };
const TASK_SOURCE: TermSource = { kind: "task" };

function TerminalView({ t: task, active = true, source = TASK_SOURCE }: { t: TaskView; active?: boolean; source?: TermSource }) {
  const shellId = source.kind === "shell" ? source.shell : null;
  const sourceRef = useRef(source);
  sourceRef.current = source;
  /** A shell runs whether or not the task's agent does. */
  const runs = task.live || shellId !== null;
  const { rpc, openUrl } = useEnv();
  const t = useT();
  // File paths open in the Detail's side pane (none outside a task's Detail).
  const pane = usePane();
  const paneRef = useRef(pane);
  paneRef.current = pane;
  const taskRef = useRef(task);
  taskRef.current = task;
  const bootId = useSelect(selectBoot);
  const connected = useSelect(selectLive);
  const host = useRef<HTMLDivElement>(null);
  const activeRef = useRef(active);
  activeRef.current = active;
  const bootRef = useRef(bootId);
  bootRef.current = bootId;
  const [shown, setShown] = useState(active);
  const [note, setNote] = useState<string | null>(null);
  const [gone, setGone] = useState(false);
  const [hovered, setHovered] = useState<string | null>(null);
  /** Attaches again now (on a reset terminal); `when`: only if not attached, or attached
   * under another boot. Null while no terminal is open. */
  const kick = useRef<((when: "always" | "detached" | "boot") => void) | null>(null);
  /** Files dropped from Finder: stored in the task's attachments, their paths typed in.
   * Null while no terminal takes keys. */
  const dropPaths = useRef<((paths: string[]) => void) | null>(null);
  const wrap = useRef<HTMLDivElement>(null);
  const dropOn = useDropZone(wrap, (paths) => dropPaths.current?.(paths), task.live && shown && shellId === null);

  useEffect(() => {
    if (active) setShown(true);
  }, [active]);

  useEffect(() => {
    setNote(null);
    setGone(false);
    setHovered(null);
    if (!runs || !shown || !host.current) return;
    const el = host.current;
    let closed = false;
    const term = new Terminal({
      convertEol: false,
      cursorBlink: true,
      fontSize: FONT_SIZE,
      fontFamily: FONT,
      theme: TERMINAL_THEME,
      scrollback: 5000,
      // For the Unicode 11 width tables (emoji and CJK take two cells, as agents' TUIs assume).
      allowProposedApi: true,
      linkHandler: linkHandler(
        openUrl,
        (uri) => {
          if (!closed) setHovered(uri);
        },
        (n) => {
          if (!closed) setNote(n);
        },
        (path, line) => openPath(path, line),
      ),
    });
    /** A clicked path: a file of the task's worktree opens in the side pane (the daemon
     * checks it first); anything else is a note, never an error. */
    const openPath = (raw: string, line?: number) => {
      const p = paneRef.current;
      if (closed || !p) return;
      const rel = toWorktreeRel(raw, taskRef.current);
      if (!rel) return setNote(t("file.notInWorktree", { path: raw }));
      rpc<FileView>(M.taskFile, { task: task.id, path: rel, stat: true }).then(
        () => {
          if (!closed) p.openFile(rel, line);
        },
        () => {
          if (!closed) setNote(t("file.notInWorktree", { path: rel }));
        },
      );
    };
    // Bare paths in the output (`src/a.rs:12:3`, `/abs/x.ts#L7`): links on a ⌘/Ctrl click.
    term.registerLinkProvider({
      provideLinks(y, done) {
        if (!paneRef.current) return done(undefined);
        const text = term.buffer.active.getLine(y - 1)?.translateToString(true) ?? "";
        const found = detectPaths(text);
        if (found.length === 0) return done(undefined);
        done(
          found.map((f) => ({
            range: { start: { x: f.start + 1, y }, end: { x: f.end, y } },
            text: f.text,
            decorations: { underline: true, pointerCursor: true },
            activate: (e: MouseEvent) => {
              if (e.metaKey || e.ctrlKey) openPath(f.path, f.line);
            },
            hover: () => {
              if (!closed) setHovered(f.text);
            },
            leave: () => {
              if (!closed) setHovered(null);
            },
          })),
        );
      },
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new Unicode11Addon());
    term.unicode.activeVersion = "11";
    term.open(el);
    // The GPU renderer when the webview gives it a context; else (or while the context is
    // lost) xterm's DOM renderer. A lost context is tried again when the window is shown.
    let glLost = false;
    const loadGl = () => {
      try {
        const gl = new WebglAddon();
        gl.onContextLoss(() => {
          gl.dispose();
          glLost = true;
        });
        term.loadAddon(gl);
        glLost = false;
      } catch {
        // No WebGL2 here: the DOM renderer stays.
      }
    };
    loadGl();
    const onVisible = () => {
      if (!closed && glLost && !document.hidden) loadGl();
    };
    document.addEventListener("visibilitychange", onVisible);
    // The first fit waits for the fonts (see `fontsPending`); the first attach follows it.
    let fontsLoaded = false;
    // The attachment keys go to (null: none now), whether it takes them, and its number:
    // output, ends and replies of an older attach are dropped.
    let id: number | null = null;
    let input = false;
    let gen = 0;
    let attaching = false;
    let attachedBoot: string | null = null;
    let failures = 0;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;

    // Size: the PTY learns the last size once resizing settles, and only when it changed.
    let told = { rows: term.rows, cols: term.cols };
    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    const tellSize = () => {
      resizeTimer = null;
      if (closed || id === null) return;
      if (term.rows === told.rows && term.cols === told.cols) return;
      told = { rows: term.rows, cols: term.cols };
      if (shellId !== null) rpc(M.shellResize, { shell: shellId, rows: told.rows, cols: told.cols }).catch(() => {});
      else rpc(M.sessionResize, { task: task.id, rows: told.rows, cols: told.cols }).catch(() => {});
    };
    term.onResize(() => {
      if (resizeTimer !== null) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(tellSize, RESIZE_DEBOUNCE_MS);
    });
    // At most one fit a frame, none while hidden (it measures 0×0 then); a fit resizes the
    // terminal only when its grid of cells changed.
    let fitQueued = false;
    const ro = new ResizeObserver(() => {
      if (fitQueued) return;
      fitQueued = true;
      nextFrame(() => {
        fitQueued = false;
        if (!closed && fontsLoaded && sized(el)) fit.fit();
      });
    });
    ro.observe(el);

    // Input: one write in flight at a time, so keys arrive in the order typed; whatever is
    // typed meanwhile goes out as one write after it. A failed write drops what queued behind it.
    let pending: Uint8Array[] = [];
    let pendingBytes = 0;
    let inFlight = false;
    const pump = () => {
      if (closed || inFlight || id === null || pending.length === 0) return;
      const chunk = concat(pending);
      pending = [];
      pendingBytes = 0;
      inFlight = true;
      attachWrite(id, chunk).then(
        () => {
          inFlight = false;
          pump();
        },
        (e) => {
          inFlight = false;
          pending = [];
          pendingBytes = 0;
          if (!closed) setNote(e instanceof DaemonError && e.timedOut ? t("term.inputTimedOut") : t("term.inputFailed"));
        },
      );
    };
    const send = (bytes: Uint8Array) => {
      if (closed || !input) return;
      if (pendingBytes + bytes.length > MAX_PENDING_INPUT) {
        setNote(t("term.inputDropped"));
        return;
      }
      pending.push(bytes);
      pendingBytes += bytes.length;
      pump();
    };

    // Attachments (as a terminal app does with a pasted image): a file pasted or dropped is
    // stored in the task's attachments folder, then its stored path is typed in as a paste,
    // so the agent's CLI sees a path it can open (Claude shows an image path as [Image #n]).
    // In order; one that fails is named and the others still go. A plain text paste is xterm's.
    const typePaths = async (uploads: (() => Promise<AttachmentView>)[], names: string[]) => {
      const paths: string[] = [];
      for (let i = 0; i < uploads.length; i++) {
        try {
          paths.push((await uploads[i]()).path);
        } catch (e) {
          if (!closed) setNote(t("att.failed", { name: names[i], error: e instanceof Error ? e.message : String(e) }));
        }
      }
      if (closed || paths.length === 0) return;
      send(new TextEncoder().encode(pathPaste(paths, !!term.modes?.bracketedPasteMode)));
    };
    const onPaste = (e: ClipboardEvent) => {
      const files = Array.from(e.clipboardData?.files ?? []);
      if (files.length === 0 || shellId !== null) return;
      // Ahead of xterm's own handler (capture), which would paste the text, if any.
      e.preventDefault();
      e.stopImmediatePropagation();
      if (!input) return;
      const fit = files.filter((f) => {
        if (f.size <= PUT_MAX) return true;
        setNote(t("att.tooBig", { name: pastedName(f.name, f.type) }));
        return false;
      });
      const names = fit.map((f) => pastedName(f.name, f.type));
      void typePaths(
        fit.map((f, i) => async () => rpc<AttachmentView>(M.attachmentPut, { task: task.id, name: names[i], b64: toBase64(new Uint8Array(await f.arrayBuffer())) })),
        names,
      );
    };
    el.addEventListener("paste", onPaste, true);
    dropPaths.current = (paths) => {
      if (closed || !input || shellId !== null) return;
      void typePaths(
        paths.map((path) => () => rpc<AttachmentView>(M.attachmentImport, { task: task.id, path })),
        paths.map(baseName),
      );
    };

    let listening = false;
    const listen = () => {
      if (listening) return;
      listening = true;
      term.onData((data) => send(new TextEncoder().encode(data)));
      term.onBinary((data) => send(binaryBytes(data)));
    };

    // Output flow control: what xterm has parsed is acked to the bridge, which stops reading
    // with too much unacked (the daemon then skips the backlog and repaints).
    let parsed = 0;
    let ackTimer: ReturnType<typeof setTimeout> | null = null;
    const flushAck = () => {
      if (ackTimer !== null) clearTimeout(ackTimer);
      ackTimer = null;
      if (closed || id === null || parsed === 0) return;
      const n = parsed;
      parsed = 0;
      void attachAck(id, n);
    };
    const drew = (n: number) => {
      parsed += n;
      if (parsed >= ACK_BATCH) flushAck();
      else if (ackTimer === null) ackTimer = setTimeout(flushAck, ACK_DELAY_MS);
    };

    /** The stream is gone (ended, or could not be opened): try again after the next wait,
     * or offer Reconnect once they are used up. */
    const lost = (why: string | null) => {
      if (closed) return;
      // A shell's stream ending is the shell ending: never attached again.
      if (shellId !== null) return sourceRef.current.kind === "shell" ? sourceRef.current.onEnd() : undefined;
      if (failures >= REATTACH_MS.length) {
        setNote(why ?? t("term.lost"));
        setGone(true);
        return;
      }
      setNote(why ?? t("term.reconnecting"));
      retryTimer = setTimeout(() => attach(true), REATTACH_MS[failures++]);
    };

    const attach = (again: boolean) => {
      if (closed) return;
      if (retryTimer !== null) clearTimeout(retryTimer);
      retryTimer = null;
      const g = ++gen;
      if (id !== null) void attachClose(id).catch(() => {});
      id = null;
      input = false;
      attaching = true;
      pending = [];
      pendingBytes = 0;
      parsed = 0;
      if (ackTimer !== null) clearTimeout(ackTimer);
      ackTimer = null;
      // The daemon repaints (replays) the screen on attach: start from a clean one.
      if (again) term.reset();
      told = { rows: term.rows, cols: term.cols };
      const boot = bootRef.current;
      const open = shellId !== null ? (r: number, c: number, d: (b: Uint8Array) => void, e: () => void) => shellAttachOpen(shellId, r, c, d, e) : (r: number, c: number, d: (b: Uint8Array) => void, e: () => void) => attachOpen(task.id, r, c, d, e);
      open(
        told.rows,
        told.cols,
        (bytes) => {
          if (closed || g !== gen) return;
          try {
            term.write(bytes, () => {
              if (!closed && g === gen) drew(bytes.length);
            });
          } catch {
            // Disposed between the check and the write: nothing to show it on.
          }
        },
        () => {
          if (closed || g !== gen) return;
          if (id !== null) void attachClose(id).catch(() => {});
          id = null;
          attaching = false;
          lost(null);
        },
      )
        .then((a) => {
          if (closed || g !== gen) {
            void attachClose(a.id).catch(() => {});
            return;
          }
          id = a.id;
          input = a.input;
          attaching = false;
          attachedBoot = boot;
          failures = 0;
          setGone(false);
          setNote(a.input ? null : t("term.readOnly"));
          flushAck();
          if (activeRef.current) term.focus();
          if (a.input) listen();
          // The size may have changed while the attach was on its way.
          tellSize();
        })
        .catch((e) => {
          if (closed || g !== gen) return;
          attaching = false;
          const why = e instanceof Error ? e.message : String(e);
          // The first attach failing says why; later ones keep trying quietly.
          lost(again ? null : why);
        });
    };
    const start = () => {
      if (closed) return;
      fontsLoaded = true;
      if (sized(el)) fit.fit();
      attach(false);
    };
    const fonts = fontsPending();
    if (fonts) void fonts.then(start);
    else start();
    kick.current = (when) => {
      if (closed || !fontsLoaded || shellId !== null) return;
      if (when === "detached" && (id !== null || attaching || retryTimer !== null)) return;
      // Already attached under this boot (the terminal was opened after it began).
      if (when === "boot" && attachedBoot === bootRef.current) return;
      failures = 0;
      attach(true);
    };
    return () => {
      closed = true;
      kick.current = null;
      dropPaths.current = null;
      el.removeEventListener("paste", onPaste, true);
      if (resizeTimer !== null) clearTimeout(resizeTimer);
      if (ackTimer !== null) clearTimeout(ackTimer);
      if (retryTimer !== null) clearTimeout(retryTimer);
      document.removeEventListener("visibilitychange", onVisible);
      ro.disconnect();
      if (id !== null) void attachClose(id).catch(() => {});
      term.dispose();
    };
    // `t` is the language's lookup: a language switch keeps the terminal.
  }, [rpc, openUrl, task.id, runs, shown, shellId]);

  // A new daemon (its sessions resumed) streams anew: attach at once. Back from a dropped
  // connection: attach if the stream is gone.
  const lastBoot = useRef(bootId);
  useEffect(() => {
    const before = lastBoot.current;
    lastBoot.current = bootId;
    if (before !== null && bootId !== null && before !== bootId) kick.current?.("boot");
  }, [bootId]);
  useEffect(() => {
    if (connected) kick.current?.("detached");
  }, [connected]);

  if (!runs) return <p className="muted">{t("term.noLive")}</p>;
  return (
    <div className={`terminal-wrap${dropOn ? " drop-on" : ""}`} ref={wrap}>
      <div
        className="terminal"
        ref={host}
        role="region"
        aria-label={source.kind === "shell" ? t("shell.region", { n: source.n, branch: task.branch }) : t("term.region", { branch: task.branch })}
      />
      {note && (
        <p className={`small terminal-overlay terminal-note${gone ? " terminal-note-action" : ""}`} role="status">
          {note}
          {gone && (
            <>
              {" "}
              <button className="btn btn-small" onClick={() => kick.current?.("always")}>
                {t("term.reconnect")}
              </button>
            </>
          )}
        </p>
      )}
      {hovered && (
        <p className="small terminal-overlay terminal-link">
          {t("term.linkHint", { url: "\u0000" })
            .split("\u0000")
            .flatMap((part, i) => (i === 0 ? [part] : [<code key={i}>{hovered}</code>, part]))}
        </p>
      )}
    </div>
  );
}

/** Renders again only for what it uses: a task refresh with the same id, liveness and
 * branch leaves the terminal alone. */
export const TerminalTab = memo(
  TerminalView,
  (a, b) =>
    a.t.id === b.t.id &&
    a.t.live === b.t.live &&
    a.t.branch === b.t.branch &&
    (a.active ?? true) === (b.active ?? true) &&
    (a.source?.kind === "shell" ? a.source.shell : null) === (b.source?.kind === "shell" ? b.source.shell : null),
);

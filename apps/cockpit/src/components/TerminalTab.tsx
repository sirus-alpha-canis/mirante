import { FitAddon } from "@xterm/addon-fit";
import { type ILinkHandler, Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";
import { useEnv } from "../env";
import { attachAck, attachClose, attachOpen, attachWrite, DaemonError } from "../lib/daemon";
import { M, type TaskView } from "../lib/protocol";

/** Window size changes settle for this long before one `session.resize` goes out. */
const RESIZE_DEBOUNCE_MS = 100;
/** Typed input waiting behind a write in flight; past this the daemon is not keeping up. */
const MAX_PENDING_INPUT = 64 * 1024;
/** Parsed output is acked to the bridge once this much has built up (its window is 256 KiB)… */
const ACK_BATCH = 64 * 1024;
/** …or after this long, whichever comes first. */
const ACK_DELAY_MS = 50;

/**
 * Terminal output is untrusted. Only the features below are on:
 * - OSC 8 hyperlinks leave through the injected opener (never `window.open`), only http(s),
 *   and only on a ⌘/Ctrl click so a stray click on spoofed link text does nothing. Hovering
 *   one shows the real target, whatever the link's text claims.
 * - No clipboard addon is loaded, so OSC 52 cannot write the clipboard.
 * - Title changes (OSC 0/2) are not subscribed, so nothing reaches the window title.
 */
function linkHandler(openUrl: (url: string) => Promise<void>, onHover: (uri: string | null) => void): ILinkHandler {
  return {
    allowNonHttpProtocols: false,
    activate(event, uri) {
      if (!(event.metaKey || event.ctrlKey)) return;
      let url: URL;
      try {
        url = new URL(uri);
      } catch {
        return;
      }
      if (url.protocol !== "http:" && url.protocol !== "https:") return;
      openUrl(url.href).catch(() => {});
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

/** The task's live PTY over `attach`; keystrokes go back unless the stream is read-only. */
export function TerminalTab({ t }: { t: TaskView }) {
  const { rpc, openUrl } = useEnv();
  const host = useRef<HTMLDivElement>(null);
  const [note, setNote] = useState<string | null>(null);
  const [hovered, setHovered] = useState<string | null>(null);

  useEffect(() => {
    setNote(null);
    setHovered(null);
    if (!t.live || !host.current) return;
    let closed = false;
    const term = new Terminal({
      convertEol: false,
      cursorBlink: true,
      fontSize: 13,
      scrollback: 5000,
      linkHandler: linkHandler(openUrl, (uri) => {
        if (!closed) setHovered(uri);
      }),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host.current);
    fit.fit();
    let id: number | null = null;

    // Size: the PTY learns the last size once resizing settles, and only when it changed.
    let told = { rows: term.rows, cols: term.cols };
    let resizeTimer: ReturnType<typeof setTimeout> | null = null;
    const tellSize = () => {
      resizeTimer = null;
      if (closed || id === null) return;
      if (term.rows === told.rows && term.cols === told.cols) return;
      told = { rows: term.rows, cols: term.cols };
      rpc(M.sessionResize, { task: t.id, rows: told.rows, cols: told.cols }).catch(() => {});
    };
    term.onResize(() => {
      if (resizeTimer !== null) clearTimeout(resizeTimer);
      resizeTimer = setTimeout(tellSize, RESIZE_DEBOUNCE_MS);
    });
    const ro = new ResizeObserver(() => fit.fit());
    ro.observe(host.current);

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
          if (!closed)
            setNote(e instanceof DaemonError && e.timedOut ? "input not delivered (the daemon timed out)" : "input not delivered");
        },
      );
    };
    const send = (bytes: Uint8Array) => {
      if (closed) return;
      if (pendingBytes + bytes.length > MAX_PENDING_INPUT) {
        setNote("input dropped (the daemon is not keeping up)");
        return;
      }
      pending.push(bytes);
      pendingBytes += bytes.length;
      pump();
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

    attachOpen(
      t.id,
      told.rows,
      told.cols,
      (bytes) => {
        if (closed) return;
        try {
          term.write(bytes, () => {
            if (!closed) drew(bytes.length);
          });
        } catch {
          // Disposed between the check and the write: nothing to show it on.
        }
      },
      () => {
        if (!closed) setNote("stream ended");
      },
    )
      .then((a) => {
        if (closed) {
          void attachClose(a.id).catch(() => {});
          return;
        }
        id = a.id;
        flushAck();
        if (!a.input) {
          setNote("read-only stream (this agent takes messages from the composer)");
          return;
        }
        term.onData((data) => send(new TextEncoder().encode(data)));
        term.onBinary((data) => send(binaryBytes(data)));
      })
      .then(() => {
        // The size may have changed while the attach was on its way.
        tellSize();
      })
      .catch((e) => {
        if (!closed) setNote(e instanceof Error ? e.message : String(e));
      });
    return () => {
      closed = true;
      if (resizeTimer !== null) clearTimeout(resizeTimer);
      if (ackTimer !== null) clearTimeout(ackTimer);
      ro.disconnect();
      if (id !== null) void attachClose(id).catch(() => {});
      term.dispose();
    };
  }, [rpc, openUrl, t.id, t.live]);

  if (!t.live) return <p className="muted">No live session. Resume the task to see its terminal.</p>;
  return (
    <div className="terminal-wrap">
      {note && (
        <p className="small muted" role="status">
          {note}
        </p>
      )}
      <div className="terminal" ref={host} role="region" aria-label={`Terminal of ${t.branch}`} />
      {hovered && (
        <p className="small muted terminal-link">
          Link to <code>{hovered}</code> (⌘/Ctrl-click to open)
        </p>
      )}
    </div>
  );
}

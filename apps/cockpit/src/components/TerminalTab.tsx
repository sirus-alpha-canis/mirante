import { FitAddon } from "@xterm/addon-fit";
import { type ILinkHandler, Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";
import { useEnv } from "../env";
import { attachClose, attachOpen, attachWrite, DaemonError } from "../lib/daemon";
import { M, type TaskView } from "../lib/protocol";

/**
 * Terminal output is untrusted. Only the features below are on:
 * - OSC 8 hyperlinks leave through the injected opener (never `window.open`), only http(s),
 *   and only on a ⌘/Ctrl click so a stray click on spoofed link text does nothing.
 * - No clipboard addon is loaded, so OSC 52 cannot write the clipboard.
 * - Title changes (OSC 0/2) are not subscribed, so nothing reaches the window title.
 */
function linkHandler(openUrl: (url: string) => Promise<void>): ILinkHandler {
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
  };
}

/** The task's live PTY over `attach`; keystrokes go back unless the stream is read-only. */
export function TerminalTab({ t }: { t: TaskView }) {
  const { rpc, openUrl } = useEnv();
  const host = useRef<HTMLDivElement>(null);
  const [note, setNote] = useState<string | null>(null);

  useEffect(() => {
    if (!t.live || !host.current) return;
    const term = new Terminal({
      convertEol: false,
      cursorBlink: true,
      fontSize: 13,
      scrollback: 5000,
      linkHandler: linkHandler(openUrl),
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host.current);
    fit.fit();
    let id: number | null = null;
    let closed = false;
    const resize = () => {
      fit.fit();
      if (id !== null) rpc(M.sessionResize, { task: t.id, rows: term.rows, cols: term.cols }).catch(() => {});
    };
    const ro = new ResizeObserver(resize);
    ro.observe(host.current);
    attachOpen(
      t.id,
      term.rows,
      term.cols,
      (bytes) => term.write(bytes),
      () => setNote("stream ended"),
    )
      .then((a) => {
        if (closed) {
          void attachClose(a.id).catch(() => {});
          return;
        }
        id = a.id;
        if (!a.input) setNote("read-only stream (this agent takes messages from the composer)");
        term.onData((data) => {
          if (a.input)
            attachWrite(a.id, data).catch((e) =>
              setNote(e instanceof DaemonError && e.timedOut ? "input not delivered (the daemon timed out)" : "input not delivered"),
            );
        });
      })
      .catch((e) => setNote(e instanceof Error ? e.message : String(e)));
    return () => {
      closed = true;
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
      <div className="terminal" ref={host} aria-label={`Terminal of ${t.branch}`} />
    </div>
  );
}

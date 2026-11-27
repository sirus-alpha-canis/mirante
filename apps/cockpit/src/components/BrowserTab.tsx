// The Browser tab: a dev server on this machine, in an iframe (not a child webview). The
// address is checked by `checkLocalUrl` (http, localhost or 127.0.0.1, an explicit port, no
// userinfo, never the app itself) and the CSP's frame-src allows nothing else, inside the
// frame too. The frame runs the dev server's own origin (scripts, forms, its own storage),
// with no top navigation, popups or modals, no referrer and no feature delegated (`allow=""`).
// Tauri injects nothing into sub-frames and grants no remote origin, so the page gets no IPC.
// Back is the address bar's own history: a cross-origin frame's history is not readable.
import { type FormEvent, useState } from "react";
import { useEnv } from "../env";
import { type Key, useT } from "../lib/i18n";
import { checkLocalUrl, localHost, type LocalUrl } from "../lib/localurl";

const WHY: Record<Exclude<LocalUrl, { ok: true }>["why"], Key> = {
  parse: "browser.bad.parse",
  scheme: "browser.bad.scheme",
  host: "browser.bad.host",
  port: "browser.bad.port",
  userinfo: "browser.bad.userinfo",
  self: "browser.bad.self",
};

export function BrowserTab({
  url,
  onNavigate,
  self = typeof window === "undefined" ? undefined : window.location.origin,
}: {
  /** The tab's address ("" while none). */
  url: string;
  /** A checked address the person went to (the tab keeps it). */
  onNavigate(url: string): void;
  /** The app's own origin, never framed. */
  self?: string;
}) {
  const t = useT();
  const { openUrl } = useEnv();
  const [shown, setShown] = useState(() => {
    const kept = url ? checkLocalUrl(url, self) : null;
    return kept?.ok ? kept.url : "";
  });
  const [text, setText] = useState(shown);
  const [bad, setBad] = useState<Key | null>(null);
  const [past, setPast] = useState<string[]>([]);
  const [load, setLoad] = useState(0);
  const [openFailed, setOpenFailed] = useState(false);

  const go = (to: string, remember: boolean) => {
    if (remember && shown && shown !== to) setPast((p) => [...p, shown].slice(-50));
    setShown(to);
    setText(to);
    setBad(null);
    setLoad((n) => n + 1);
    onNavigate(to);
  };
  const submit = (e: FormEvent) => {
    e.preventDefault();
    const r = checkLocalUrl(text, self);
    if (!r.ok) return setBad(WHY[r.why]);
    go(r.url, true);
  };
  const back = () => {
    const prev = past[past.length - 1];
    if (!prev) return;
    setPast((p) => p.slice(0, -1));
    go(prev, false);
  };
  const open = () => {
    if (!shown) return;
    setOpenFailed(false);
    openUrl(shown).catch(() => setOpenFailed(true));
  };

  return (
    <div className="browser-tab">
      <form className="browser-bar" onSubmit={submit}>
        <button type="button" className="btn btn-small btn-ghost" aria-label={t("browser.back")} title={t("browser.back")} disabled={past.length === 0} onClick={back}>
          ←
        </button>
        <button type="button" className="btn btn-small btn-ghost" aria-label={t("browser.reload")} title={t("browser.reload")} disabled={!shown} onClick={() => setLoad((n) => n + 1)}>
          ↻
        </button>
        <input
          className="dna-input browser-address"
          aria-label={t("browser.address")}
          placeholder="http://localhost:3000"
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          value={text}
          aria-invalid={bad !== null}
          aria-describedby={bad ? "browser-bad" : undefined}
          onChange={(e) => {
            setText(e.target.value);
            setBad(null);
          }}
        />
        <button type="button" className="btn btn-small" disabled={!shown} onClick={open}>
          {t("browser.open")}
        </button>
      </form>
      {bad && (
        <p id="browser-bad" role="alert" className="small browser-bad">
          {t(bad)}
        </p>
      )}
      {openFailed && <p className="small browser-bad">{t("diff.browserFailed")}</p>}
      {shown ? (
        <iframe
          key={`${shown}#${load}`}
          className="browser-frame"
          title={t("browser.frame", { host: localHost(shown) })}
          src={shown}
          sandbox="allow-scripts allow-forms allow-same-origin"
          referrerPolicy="no-referrer"
          allow=""
        />
      ) : (
        <p className="muted small">{t("browser.empty")}</p>
      )}
    </div>
  );
}

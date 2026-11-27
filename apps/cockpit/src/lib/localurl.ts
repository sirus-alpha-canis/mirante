// The Browser tab's address bar: only a dev server on this machine. `http:` only; the host
// is localhost or 127.0.0.1 (the CSP's frame-src; a CSP host source cannot name [::1]); an
// explicit port 1..65535 (URL drops :80, so 80 counts as missing); no user or password;
// never the app's own origin (the cockpit's dev server in a dev build). The URL is parsed
// by `URL` and handed back normalized: what the frame loads is what was checked.

export type LocalUrl = { ok: true; url: string } | { ok: false; why: "parse" | "scheme" | "host" | "port" | "userinfo" | "self" };

export const LOCAL_HOSTS = ["localhost", "127.0.0.1"] as const;
const MAX = 2048;

export function checkLocalUrl(input: string, self?: string): LocalUrl {
  let text = input.trim();
  if (!text || text.length > MAX) return { ok: false, why: "parse" };
  // A port alone, or an address typed without a scheme.
  if (/^\d{1,5}$/.test(text)) text = `http://localhost:${text}`;
  else if (!/^[a-z][a-z0-9+.-]*:\/\//i.test(text) && !/^(about|data|blob|file|javascript):/i.test(text)) text = `http://${text}`;
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return { ok: false, why: "parse" };
  }
  if (url.protocol !== "http:") return { ok: false, why: "scheme" };
  if (url.username || url.password) return { ok: false, why: "userinfo" };
  if (!(LOCAL_HOSTS as readonly string[]).includes(url.hostname)) return { ok: false, why: "host" };
  const port = Number(url.port);
  if (!/^\d{1,5}$/.test(url.port) || port < 1 || port > 65535) return { ok: false, why: "port" };
  if (self && url.origin === self) return { ok: false, why: "self" };
  return { ok: true, url: url.href };
}

/** A checked URL's short name for a tab: `localhost:3000`. */
export function localHost(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

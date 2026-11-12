// Where a link may go when the person clicks it: the opener capability's rule (github.com
// pages, and a local dev server on an explicit port), never with credentials in the URL.
// Shared by the terminal and the Markdown preview; everything else says "copy it".

export function openableLink(url: URL): boolean {
  if (url.username || url.password) return false;
  if (url.protocol === "https:") return url.hostname === "github.com" && url.port === "";
  if (url.protocol === "http:") return (url.hostname === "localhost" || url.hostname === "127.0.0.1") && /^\d{1,5}$/.test(url.port);
  return false;
}

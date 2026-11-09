import { useT } from "../lib/i18n";
import { useUpdate } from "../lib/update";

/** Restarts the daemon (the new version takes over). */
export function RestartButton() {
  const t = useT();
  const u = useUpdate();
  return (
    <button className="btn btn-small" disabled={u.restarting} onClick={u.restart}>
      {t(u.restarting ? "update.restarting" : "update.restart")}
    </button>
  );
}

/** Mirante was updated but the old daemon still runs: says so, non-blocking, with the fix. */
export function UpdateBanner() {
  const t = useT();
  const u = useUpdate();
  if (!u.outdated) return null;
  return (
    <div className="banner banner-update" role="status" aria-label={t("update.aria")}>
      {t(u.autoResume ? "update.banner" : "update.bannerNoResume")} <RestartButton />
      {u.error && <span className="error"> {t("update.failed", { error: u.error })}</span>}
    </div>
  );
}

/** A feature this daemon does not have: `text` (update Mirante), or, when the update is
 * installed and only the daemon is old, the restart that brings it. */
export function Unsupported({ text }: { text: string }) {
  const t = useT();
  const u = useUpdate();
  if (!u.outdated) return <p className="muted">{text}</p>;
  return (
    <p className="muted">
      {t("update.feature")} <RestartButton />
    </p>
  );
}

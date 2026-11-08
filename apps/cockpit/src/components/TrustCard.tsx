import { useRef, useState } from "react";
import { useEnv, useLive } from "../env";
import { DaemonError } from "../lib/daemon";
import { M, RpcCode } from "../lib/protocol";
import { useT } from "../lib/i18n";

/** Claude's workspace-trust dialog: only the user answers it (never auto-accepted). One
 * answer per card: the buttons stay off once it is sent, until the boot event removes it. */
export function TrustCard({ taskId }: { taskId: string }) {
  const { rpc } = useEnv();
  const t = useT();
  const live = useLive();
  const [error, setError] = useState<string | null>(null);
  const [phase, setPhase] = useState<"idle" | "busy" | "done">("idle");
  const sending = useRef(false);
  const answer = async (accept: boolean) => {
    // A ref, not state: two clicks in one frame both see the old state.
    if (sending.current) return;
    sending.current = true;
    setPhase("busy");
    setError(null);
    try {
      await rpc(M.sessionTrust, { task: taskId, accept });
      setPhase("done");
    } catch (e) {
      const conflict = e instanceof DaemonError && e.code === RpcCode.CONFLICT;
      setError(conflict ? t("ui.alreadyAnswered") : e instanceof Error ? e.message : String(e));
      setPhase(conflict ? "done" : "idle");
      sending.current = conflict;
    }
  };
  const off = phase !== "idle" || !live;
  return (
    <div className="perm" role="group" aria-label={t("trust.label")}>
      <div className="perm-what">
        <strong>{t("trust.question")}</strong>
        <span className="muted">{t("trust.declineEnds")}</span>
      </div>
      <div className="perm-actions">
        <button className="btn btn-primary" disabled={off} onClick={() => answer(true)}>
          {t("trust.accept")}
        </button>
        <button className="btn" disabled={off} onClick={() => answer(false)}>
          {t("trust.decline")}
        </button>
      </div>
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

import { useRef, useState } from "react";
import { useEnv } from "../env";
import { DaemonError } from "../lib/daemon";
import { M, RpcCode } from "../lib/protocol";

/** Claude's workspace-trust dialog: only the user answers it (never auto-accepted). One
 * answer per card: the buttons stay off once it is sent, until the boot event removes it. */
export function TrustCard({ taskId }: { taskId: string }) {
  const { rpc } = useEnv();
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
      setError(conflict ? "already answered" : e instanceof Error ? e.message : String(e));
      setPhase(conflict ? "done" : "idle");
      sending.current = conflict;
    }
  };
  const off = phase !== "idle";
  return (
    <div className="perm" role="group" aria-label="Trust this folder">
      <div className="perm-what">
        <strong>Claude asks whether to trust this worktree folder.</strong>
        <span className="muted">Declining ends the session.</span>
      </div>
      <div className="perm-actions">
        <button className="btn btn-primary" disabled={off} onClick={() => answer(true)}>
          Trust folder
        </button>
        <button className="btn" disabled={off} onClick={() => answer(false)}>
          Decline
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

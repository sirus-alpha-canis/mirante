import { useState } from "react";
import { useEnv } from "../env";
import { M } from "../lib/protocol";

/** Claude's workspace-trust dialog: only the user answers it (never auto-accepted). */
export function TrustCard({ taskId }: { taskId: string }) {
  const { rpc } = useEnv();
  const [error, setError] = useState<string | null>(null);
  const answer = (accept: boolean) =>
    rpc(M.sessionTrust, { task: taskId, accept }).catch((e) => setError(e instanceof Error ? e.message : String(e)));
  return (
    <div className="perm" role="group" aria-label="Trust this folder">
      <div className="perm-what">
        <strong>Claude asks whether to trust this worktree folder.</strong>
        <span className="muted">Declining ends the session.</span>
      </div>
      <div className="perm-actions">
        <button className="btn btn-primary" onClick={() => answer(true)}>
          Trust folder
        </button>
        <button className="btn" onClick={() => answer(false)}>
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

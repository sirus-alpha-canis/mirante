import { projectColor } from "../lib/inbox";
import { repoName } from "../store/select";

/** A project's name in its stable color (the same in every list and on every run). */
export function ProjectChip({ path }: { path: string }) {
  return (
    <span className={`proj proj-${projectColor(path)}`} title={path}>
      {repoName(path)}
    </span>
  );
}

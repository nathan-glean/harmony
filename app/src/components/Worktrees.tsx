import type { WorktreeView } from "../types";

const fmt = (secs: number) =>
  new Date(secs * 1000).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

export function Worktrees({
  worktrees,
  isLive,
  onOpen,
  onDelete,
}: {
  worktrees: WorktreeView[];
  isLive: (ticketId: number) => boolean;
  onOpen: (ticketId: number) => void;
  onDelete: (w: WorktreeView) => void;
}) {
  return (
    <div className="sessions">
      <table>
        <thead>
          <tr>
            <th>Ticket</th>
            <th>Repo</th>
            <th>Branch</th>
            <th>Path</th>
            <th>Created</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {worktrees.length === 0 && (
            <tr>
              <td className="empty" colSpan={6}>
                No worktrees yet — they're created when you start a session.
              </td>
            </tr>
          )}
          {worktrees.map((w) => {
            const live = isLive(w.ticket_id);
            return (
              <tr key={w.id} className="srow" onClick={() => onOpen(w.ticket_id)}>
                <td>
                  <span className="card-key">{w.jira_key ?? `local #${w.ticket_id}`}</span>{" "}
                  {w.ticket_title}
                  {w.is_alternate ? <span className="badge alt">alt</span> : null}
                </td>
                <td>{w.repo_name}</td>
                <td className="mono">{w.branch}</td>
                <td className="mono path">{w.path}</td>
                <td>{fmt(w.created_at)}</td>
                <td>
                  <button
                    className="row-del"
                    disabled={live}
                    title={live ? "Stop the session first" : "Delete this worktree"}
                    onClick={(e) => {
                      e.stopPropagation();
                      onDelete(w);
                    }}
                  >
                    ×
                  </button>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

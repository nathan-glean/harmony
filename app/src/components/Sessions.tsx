import { useMemo } from "react";
import type { SessionView } from "../types";

const fmt = (secs: number) =>
  new Date(secs * 1000).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

type Group = {
  worktreeId: number;
  ticketId: number;
  ticketTitle: string;
  jiraKey: string | null;
  branch: string;
  claudeSessionId: string | null;
  lastTool: string | null;
  state: string;
  startedAt: number;
  endedAt: number | null;
  live: boolean;
  runs: number;
};

export function Sessions({
  sessions,
  liveSessionIds,
  onOpen,
  onClearEnded,
  onDeleteGroup,
}: {
  sessions: SessionView[];
  liveSessionIds: Set<number>;
  onOpen: (ticketId: number, live: boolean) => void;
  onClearEnded: () => void;
  onDeleteGroup: (worktreeId: number) => void;
}) {
  // Collapse the many start/resume rows for one worktree into a single conversation row.
  // `sessions` is newest-first (id DESC), so the first row seen per worktree is the most
  // recent run and seeds the "latest" fields.
  const groups = useMemo(() => {
    const map = new Map<number, Group>();
    for (const s of sessions) {
      const live = liveSessionIds.has(s.id);
      let g = map.get(s.worktree_id);
      if (!g) {
        g = {
          worktreeId: s.worktree_id,
          ticketId: s.ticket_id,
          ticketTitle: s.ticket_title,
          jiraKey: s.jira_key,
          branch: s.branch,
          claudeSessionId: s.claude_session_id,
          lastTool: s.last_tool,
          state: s.state,
          startedAt: s.started_at,
          endedAt: live ? null : s.ended_at ?? null,
          live,
          runs: 0,
        };
        map.set(s.worktree_id, g);
      }
      g.runs += 1;
      if (live) {
        g.live = true;
        g.endedAt = null;
      }
      if (g.claudeSessionId == null && s.claude_session_id != null) g.claudeSessionId = s.claude_session_id;
      if (g.lastTool == null && s.last_tool != null) g.lastTool = s.last_tool;
      if (s.started_at < g.startedAt) g.startedAt = s.started_at;
      if (!g.live && s.ended_at != null && (g.endedAt == null || s.ended_at > g.endedAt)) {
        g.endedAt = s.ended_at;
      }
    }
    return [...map.values()];
  }, [sessions, liveSessionIds]);

  const endedCount = sessions.filter((s) => s.ended_at).length;

  return (
    <div className="sessions">
      <div className="sessions-bar">
        <button disabled={endedCount === 0} onClick={onClearEnded}>
          Clear ended ({endedCount})
        </button>
      </div>
      <table>
        <thead>
          <tr>
            <th>Ticket</th>
            <th>State</th>
            <th>Last tool</th>
            <th>Branch</th>
            <th>Started</th>
            <th>Ended</th>
            <th>Claude session</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {groups.length === 0 && (
            <tr>
              <td className="empty" colSpan={8}>
                No sessions yet — start one from a ticket.
              </td>
            </tr>
          )}
          {groups.map((g) => (
            <tr key={g.worktreeId} className="srow" onClick={() => onOpen(g.ticketId, g.live)}>
              <td>
                <span className="card-key">{g.jiraKey ?? `local #${g.ticketId}`}</span>{" "}
                {g.ticketTitle}
                {g.runs > 1 && <span className="runs-badge"> ×{g.runs} runs</span>}
              </td>
              <td>
                <span className={`badge${!g.live && g.state === "error" ? " badge-error" : ""}`}>
                  {g.live ? g.state : g.state === "error" ? "error" : "ended"}
                </span>
              </td>
              <td>{g.lastTool ?? "—"}</td>
              <td className="mono">{g.branch}</td>
              <td>{fmt(g.startedAt)}</td>
              <td>{g.endedAt ? fmt(g.endedAt) : <span className="live">● live</span>}</td>
              <td className="mono">{g.claudeSessionId ? g.claudeSessionId.slice(0, 8) : "—"}</td>
              <td>
                {!g.live && (
                  <button
                    className="row-del"
                    title="Delete this session's runs"
                    onClick={(e) => {
                      e.stopPropagation();
                      onDeleteGroup(g.worktreeId);
                    }}
                  >
                    ×
                  </button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

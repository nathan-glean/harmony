import type { SessionView } from "../types";

const fmt = (secs: number) =>
  new Date(secs * 1000).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

export function Sessions({
  sessions,
  onOpen,
  onClearEnded,
  onDelete,
}: {
  sessions: SessionView[];
  onOpen: (ticketId: number, live: boolean) => void;
  onClearEnded: () => void;
  onDelete: (id: number) => void;
}) {
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
          {sessions.length === 0 && (
            <tr>
              <td className="empty" colSpan={8}>
                No sessions yet — start one from a ticket.
              </td>
            </tr>
          )}
          {sessions.map((s) => (
            <tr key={s.id} className="srow" onClick={() => onOpen(s.ticket_id, !s.ended_at)}>
              <td>
                <span className="card-key">{s.jira_key ?? `local #${s.ticket_id}`}</span>{" "}
                {s.ticket_title}
              </td>
              <td>
                <span className="badge">{s.ended_at ? "ended" : s.state}</span>
              </td>
              <td>{s.last_tool ?? "—"}</td>
              <td className="mono">{s.branch}</td>
              <td>{fmt(s.started_at)}</td>
              <td>{s.ended_at ? fmt(s.ended_at) : <span className="live">● live</span>}</td>
              <td className="mono">
                {s.claude_session_id ? s.claude_session_id.slice(0, 8) : "—"}
              </td>
              <td>
                {s.ended_at && (
                  <button
                    className="row-del"
                    title="Delete this session"
                    onClick={(e) => {
                      e.stopPropagation();
                      onDelete(s.id);
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

import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../api";
import type { OrchestratorEvent, OrchestratorStatus } from "../types";

const fmt = (secs: number) =>
  new Date(secs * 1000).toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });

// Relative "how long ago", coarse — good enough for a "last checked" line.
function ago(secs: number): string {
  const d = Math.max(0, Math.floor(Date.now() / 1000 - secs));
  if (d < 60) return `${d}s ago`;
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  return `${Math.floor(d / 3600)}h ago`;
}

// Icon + label per event kind (matches store::orchestrator_kind).
const KIND: Record<string, { icon: string; label: string }> = {
  dispatch: { icon: "▶", label: "Dispatched" },
  restart: { icon: "↻", label: "Restarted" },
  answer: { icon: "💬", label: "Answered" },
  spec: { icon: "📝", label: "Spec" },
  pr: { icon: "🔀", label: "PR" },
  escalate: { icon: "⚠", label: "Escalated" },
  info: { icon: "•", label: "Info" },
};

/** The Orchestrator tab: live status (on/off, concurrency, last tick, in-flight decision) plus the
 *  persistent decision feed across all tickets. Clicking a row opens that ticket. */
export function Orchestrator({ onOpen }: { onOpen: (ticketId: number) => void }) {
  const [status, setStatus] = useState<OrchestratorStatus | null>(null);
  const [events, setEvents] = useState<OrchestratorEvent[]>([]);

  const refresh = useCallback(async () => {
    try {
      const [s, e] = await Promise.all([
        api.getOrchestratorStatus(),
        api.listOrchestratorEvents(200),
      ]);
      setStatus(s);
      setEvents(e);
    } catch {
      /* transient — next tick retries */
    }
  }, []);

  useEffect(() => {
    refresh();
    const iv = setInterval(refresh, 1500);
    const un = listen("orchestrator-updated", () => refresh());
    return () => {
      clearInterval(iv);
      un.then((u) => u());
    };
  }, [refresh]);

  const deciding = status?.deciding ?? null;

  return (
    <div className="orchestrator">
      <div className="orch-status">
        <span className={"orch-dot " + (status?.enabled ? "on" : "off")} />
        <strong>Orchestrator</strong>
        <span className="muted">
          {status?.enabled ? `on · ${status.max_concurrent} slots` : "off"}
        </span>
        {status?.last_tick_at != null && (
          <span className="muted">· last checked {ago(status.last_tick_at)}</span>
        )}
        <span className="spacer" />
        {deciding ? (
          <span className="orch-deciding">
            <span className="spinner" /> deciding on #{deciding.ticket_id} — {deciding.what}…
          </span>
        ) : (
          <span className="muted">{status?.enabled ? "idle" : "disabled"}</span>
        )}
      </div>

      {!status?.enabled && (
        <p className="empty">
          The orchestrator is off. Enable it in Settings to autonomously dispatch ready tickets,
          answer derivable questions, and unblock the flow — its decisions will appear here.
        </p>
      )}

      {events.length === 0 ? (
        status?.enabled && (
          <p className="empty">No decisions yet — the orchestrator hasn't needed to act.</p>
        )
      ) : (
        <table className="orch-feed">
          <thead>
            <tr>
              <th>When</th>
              <th>Ticket</th>
              <th>Decision</th>
            </tr>
          </thead>
          <tbody>
            {events.map((e) => {
              const k = KIND[e.kind] ?? KIND.info;
              return (
                <tr key={e.id} className="srow" onClick={() => onOpen(e.ticket_id)}>
                  <td className="muted" title={fmt(e.created_at)}>
                    {ago(e.created_at)}
                  </td>
                  <td>
                    <span className="card-key">{e.jira_key ?? `local #${e.ticket_id}`}</span>{" "}
                    {e.ticket_title}
                  </td>
                  <td>
                    <span className={"orch-kind " + e.kind} title={k.label}>
                      {k.icon}
                    </span>{" "}
                    {e.note}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      )}
    </div>
  );
}

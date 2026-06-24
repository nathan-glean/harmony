import { useState } from "react";
import type { Ticket, SessionProgress } from "../types";
import { COLUMNS, COLUMN_LABELS, parseActivity } from "../types";
import { ProgressLine } from "./ProgressLine";

export function Board({
  tickets,
  selectedId,
  progress,
  openingPr,
  onSelect,
  onMove,
}: {
  tickets: Ticket[];
  selectedId: number | null;
  progress: Record<number, SessionProgress>;
  openingPr: Set<number>;
  onSelect: (t: Ticket) => void;
  onMove: (id: number, status: string) => void;
}) {
  const [overCol, setOverCol] = useState<string | null>(null);

  return (
    <div className="board">
      {COLUMNS.map((col) => {
        const items = tickets.filter((t) => t.status === col);
        return (
          <div
            className={"column" + (overCol === col ? " dragover" : "")}
            key={col}
            onDragOver={(e) => {
              e.preventDefault();
              if (overCol !== col) setOverCol(col);
            }}
            onDragLeave={(e) => {
              if (e.currentTarget === e.target) setOverCol(null);
            }}
            onDrop={(e) => {
              e.preventDefault();
              setOverCol(null);
              const id = Number(e.dataTransfer.getData("text/plain"));
              if (id) onMove(id, col);
            }}
          >
            <div className="column-header">
              {COLUMN_LABELS[col]} <span className="count">{items.length}</span>
            </div>
            <div className="column-body">
              {items.map((t) => {
                const act = parseActivity(t.activity);
                // The pill is the single status signal; the hard "no repo" gate keeps its own badge
                // (always derivable, shown before the first activity compute).
                const showPill =
                  act && act.category !== "idle" && act.label !== "Assign a repo";
                return (
                <button
                  key={t.id}
                  className={
                    "card" +
                    (t.id === selectedId ? " selected" : "") +
                    (openingPr.has(t.id) ? " card-busy" : "")
                  }
                  draggable
                  onDragStart={(e) => {
                    e.dataTransfer.setData("text/plain", String(t.id));
                    e.dataTransfer.effectAllowed = "move";
                  }}
                  onClick={() => onSelect(t)}
                >
                  <div className="card-key">
                    {t.jira_key ?? <span className="local">local #{t.id}</span>}
                    {t.repo_id == null ? (
                      <span className="card-norepo" title="Assign a repo before moving this ticket">
                        ⚠ no repo
                      </span>
                    ) : null}
                  </div>
                  <div className="card-title">{t.title}</div>
                  {showPill && (
                    <span
                      className={"card-activity act-" + act!.category}
                      title={act!.detail ?? ""}
                    >
                      {act!.label}
                    </span>
                  )}
                  {progress[t.id] && (
                    <ProgressLine p={progress[t.id]} className="card-progress" />
                  )}
                  {openingPr.has(t.id) && (
                    <div className="card-loading">
                      <span className="spinner" />
                      Opening PR…
                    </div>
                  )}
                </button>
                );
              })}
            </div>
          </div>
        );
      })}
    </div>
  );
}

import { useEffect, useState } from "react";
import { api } from "../api";
import { Skeleton } from "./Skeleton";

function lineClass(l: string): string {
  if (l.startsWith("+++") || l.startsWith("---")) return "d-meta";
  if (l.startsWith("+")) return "d-add";
  if (l.startsWith("-")) return "d-del";
  if (l.startsWith("@@")) return "d-hunk";
  if (l.startsWith("diff ") || l.startsWith("index ")) return "d-meta";
  return "";
}

// gh's check buckets: pass | fail | pending | skipping | cancel
function checkClass(c: any): string {
  const b = String(c.bucket ?? c.state ?? "").toLowerCase();
  if (b.includes("pass") || b.includes("success")) return "ok";
  if (b.includes("fail") || b.includes("error")) return "bad";
  if (b.includes("pend")) return "pending";
  return "";
}

export function DiffPane({ ticketId }: { ticketId: number }) {
  const [diff, setDiff] = useState("");
  const [pr, setPr] = useState<any | null>(null);
  const [checks, setChecks] = useState<any[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setErr(null);
    try {
      const [d, p] = await Promise.all([api.ticketDiff(ticketId), api.ticketPr(ticketId)]);
      setDiff(d);
      setPr(p.pr);
      setChecks(p.checks ?? []);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    load();
  }, [ticketId]);

  return (
    <div className="diffpane">
      <div className="diffpane-head">
        <span>Diff / PR</span>
        <button onClick={load} disabled={loading}>
          {loading ? "Loading…" : "Refresh"}
        </button>
      </div>

      {err && <div className="diff-err">{err}</div>}

      {pr && (
        <div className="pr-meta">
          <a href={pr.url} target="_blank" rel="noreferrer">
            #{pr.number} {pr.title}
          </a>
          <span className="badge">{pr.isDraft ? "draft" : String(pr.state ?? "").toLowerCase()}</span>
        </div>
      )}

      {checks.length > 0 && (
        <ul className="checks">
          {checks.map((c, i) => (
            <li key={i} className={"chk " + checkClass(c)}>
              <span className="chk-dot" />
              {c.link ? (
                <a href={c.link} target="_blank" rel="noreferrer">
                  {c.name}
                </a>
              ) : (
                c.name
              )}
            </li>
          ))}
        </ul>
      )}

      {loading && !diff ? (
        <Skeleton lines={8} />
      ) : (
        <pre className="diff">
          {diff ? (
            diff.split("\n").map((l, i) => (
              <div key={i} className={lineClass(l)}>
                {l || " "}
              </div>
            ))
          ) : (
            <span className="muted">No changes vs base branch.</span>
          )}
        </pre>
      )}
    </div>
  );
}

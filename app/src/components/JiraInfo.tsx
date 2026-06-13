import { useEffect, useState } from "react";
import { api } from "../api";

type Comment = { author: string; created: string; body: string };

const fmtDate = (s: string) => {
  if (!s) return "";
  const d = new Date(s);
  return isNaN(d.getTime())
    ? s
    : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
};

export function JiraInfo({ ticketId }: { ticketId: number }) {
  const [desc, setDesc] = useState("");
  const [comments, setComments] = useState<Comment[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setErr(null);
    try {
      const d = await api.jiraDetail(ticketId);
      setDesc(d.description ?? "");
      setComments(d.comments ?? []);
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
    <div className="jirainfo">
      <div className="jirainfo-head">
        <span>Jira</span>
        <button onClick={load} disabled={loading}>
          {loading ? "Loading…" : "Refresh"}
        </button>
      </div>

      {err && <div className="diff-err">{err}</div>}

      {desc && <div className="jira-desc">{desc}</div>}

      {comments.length > 0 && (
        <div className="jira-comments">
          {comments.map((c, i) => (
            <div className="jira-comment" key={i}>
              <div className="jc-head">
                <strong>{c.author || "—"}</strong> <span className="muted">{fmtDate(c.created)}</span>
              </div>
              <div className="jc-body">{c.body}</div>
            </div>
          ))}
        </div>
      )}

      {!loading && !err && !desc && comments.length === 0 && (
        <div className="muted">No Jira description or comments.</div>
      )}
    </div>
  );
}

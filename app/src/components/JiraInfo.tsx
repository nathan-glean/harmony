import { useEffect, useState } from "react";
import { api } from "../api";
import { Skeleton } from "./Skeleton";

type Comment = { author: string; created: string; body: string };

// Per-session cache of fetched Jira detail, so reopening a ticket is instant (the acli fetch
// takes a second or two). Keyed by ticket id; refreshed silently in the background on reopen.
const cache = new Map<number, { desc: string; comments: Comment[] }>();

const fmtDate = (s: string) => {
  if (!s) return "";
  const d = new Date(s);
  return isNaN(d.getTime())
    ? s
    : d.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
};

export function JiraInfo({ ticketId }: { ticketId: number }) {
  const cached = cache.get(ticketId);
  const [desc, setDesc] = useState(cached?.desc ?? "");
  const [comments, setComments] = useState<Comment[]>(cached?.comments ?? []);
  // Skeleton only when there's nothing cached to show yet.
  const [loading, setLoading] = useState(!cached);
  const [refreshing, setRefreshing] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const load = async (silent: boolean) => {
    if (silent) setRefreshing(true);
    else setLoading(true);
    setErr(null);
    try {
      const d = await api.jiraDetail(ticketId);
      const next = { desc: d.description ?? "", comments: d.comments ?? [] };
      cache.set(ticketId, next);
      setDesc(next.desc);
      setComments(next.comments);
    } catch (e) {
      // A silent background refresh failing shouldn't replace good cached content with an error.
      if (!silent) setErr(String(e));
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  };

  useEffect(() => {
    const c = cache.get(ticketId);
    if (c) {
      // Show cache instantly, revalidate quietly in the background.
      setDesc(c.desc);
      setComments(c.comments);
      setLoading(false);
      load(true);
    } else {
      load(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ticketId]);

  const firstLoad = loading && !desc && comments.length === 0;

  return (
    <div className="jirainfo">
      <div className="jirainfo-head">
        <span>Jira</span>
        <button onClick={() => load(false)} disabled={loading || refreshing}>
          {loading ? "Loading…" : refreshing ? "Refreshing…" : "Refresh"}
        </button>
      </div>

      {err && <div className="diff-err">{err}</div>}

      {firstLoad && <Skeleton lines={5} />}

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

      {!loading && !refreshing && !err && !desc && comments.length === 0 && (
        <div className="muted">No Jira description or comments.</div>
      )}
    </div>
  );
}

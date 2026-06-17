import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  parseDiff,
  Diff,
  Hunk,
  Decoration,
  tokenize,
  getChangeKey,
  computeOldLineNumber,
  computeNewLineNumber,
  findChangeByOldLineNumber,
  findChangeByNewLineNumber,
} from "react-diff-view";
import "react-diff-view/style/index.css";
// Use refractor's empty core and register only the languages we map below — registering a
// language pulls in its transitive grammars (e.g. tsx → jsx + typescript), so the diff still
// highlights correctly without bundling all ~280 Prism languages.
import { refractor } from "refractor/core";
import typescript from "refractor/typescript";
import tsx from "refractor/tsx";
import javascript from "refractor/javascript";
import jsx from "refractor/jsx";
import rust from "refractor/rust";
import python from "refractor/python";
import ruby from "refractor/ruby";
import go from "refractor/go";
import java from "refractor/java";
import kotlin from "refractor/kotlin";
import swift from "refractor/swift";
import c from "refractor/c";
import cpp from "refractor/cpp";
import csharp from "refractor/csharp";
import php from "refractor/php";
import json from "refractor/json";
import css from "refractor/css";
import scss from "refractor/scss";
import less from "refractor/less";
import markup from "refractor/markup";
import markdown from "refractor/markdown";
import yaml from "refractor/yaml";
import toml from "refractor/toml";
import bash from "refractor/bash";
import sql from "refractor/sql";
import { api } from "../api";
import { Skeleton } from "./Skeleton";
import type { DiffComment } from "../types";

for (const lang of [
  typescript, tsx, javascript, jsx, rust, python, ruby, go, java, kotlin, swift,
  c, cpp, csharp, php, json, css, scss, less, markup, markdown, yaml, toml, bash, sql,
]) {
  refractor.register(lang as any);
}
const REGISTERED = new Set(refractor.listLanguages());

// react-diff-view 3.x expects `refractor.highlight` to return an array of hast nodes
// (the refractor v3 shape); refractor v5 returns a hast root, so unwrap `.children`.
const refractorAdapter = {
  highlight: (text: string, language: string) =>
    (refractor.highlight(text, language) as any).children,
  listLanguages: () => refractor.listLanguages(),
};

const LANG_BY_EXT: Record<string, string> = {
  ts: "typescript",
  mts: "typescript",
  cts: "typescript",
  tsx: "tsx",
  js: "javascript",
  mjs: "javascript",
  cjs: "javascript",
  jsx: "jsx",
  rs: "rust",
  py: "python",
  rb: "ruby",
  go: "go",
  java: "java",
  kt: "kotlin",
  swift: "swift",
  c: "c",
  h: "c",
  cpp: "cpp",
  cc: "cpp",
  hpp: "cpp",
  cs: "csharp",
  php: "php",
  json: "json",
  css: "css",
  scss: "scss",
  less: "less",
  html: "markup",
  xml: "markup",
  svg: "markup",
  md: "markdown",
  markdown: "markdown",
  yml: "yaml",
  yaml: "yaml",
  toml: "toml",
  sh: "bash",
  bash: "bash",
  zsh: "bash",
  sql: "sql",
};

// The user-facing path for a file (the new path, except for deletions).
function pathOf(file: any): string {
  return file.type === "delete" ? file.oldPath : file.newPath;
}

function langFor(path: string): string | null {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const lang = LANG_BY_EXT[ext];
  return lang && REGISTERED.has(lang) ? lang : null;
}

// gh's check buckets: pass | fail | pending | skipping | cancel
function checkClass(c: any): string {
  const b = String(c.bucket ?? c.state ?? "").toLowerCase();
  if (b.includes("pass") || b.includes("success")) return "ok";
  if (b.includes("fail") || b.includes("error")) return "bad";
  if (b.includes("pend")) return "pending";
  return "";
}

// One-line summary of CI check states for the collapsed checks header, e.g. "3 passed · 1 failed".
function checksSummary(checks: any[]): string {
  const counts = { ok: 0, bad: 0, pending: 0, other: 0 };
  for (const c of checks) {
    const k = checkClass(c);
    if (k === "ok") counts.ok++;
    else if (k === "bad") counts.bad++;
    else if (k === "pending") counts.pending++;
    else counts.other++;
  }
  const parts: string[] = [];
  if (counts.ok) parts.push(`${counts.ok} passed`);
  if (counts.bad) parts.push(`${counts.bad} failed`);
  if (counts.pending) parts.push(`${counts.pending} pending`);
  if (counts.other) parts.push(`${counts.other} other`);
  return `(${parts.join(" · ") || `${checks.length}`})`;
}

// A pending comment over a (possibly multi-line) range. `anchorKey` is the change key of the
// range's last line — where the composer/comment widget renders, GitHub-style.
type Composing = {
  filePath: string;
  anchorKey: string;
  startLine: number;
  endLine: number;
  side: "old" | "new";
};

// An in-progress drag selection started from a line's "+" button.
type Drag = { filePath: string; side: "old" | "new"; startLine: number; endLine: number };

const lineOnSide = (change: any, side: "old" | "new"): number =>
  side === "old" ? computeOldLineNumber(change) : computeNewLineNumber(change);

// All change keys whose line (on `side`) falls within [lo, hi] — used to highlight the
// selected range via react-diff-view's `selectedChanges`.
function changeKeysInRange(hunks: any[], side: "old" | "new", lo: number, hi: number): string[] {
  const keys: string[] = [];
  for (const h of hunks) {
    for (const c of h.changes) {
      const ln = lineOnSide(c, side);
      if (ln >= lo && ln <= hi) keys.push(getChangeKey(c));
    }
  }
  return keys;
}

export function DiffPane({ ticketId }: { ticketId: number }) {
  const [diff, setDiff] = useState("");
  const [pr, setPr] = useState<any | null>(null);
  const [checks, setChecks] = useState<any[]>([]);
  const [checksOpen, setChecksOpen] = useState(false);
  const [comments, setComments] = useState<DiffComment[]>([]);
  const [loading, setLoading] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [viewType, setViewType] = useState<"unified" | "split">("unified");
  const [composing, setComposing] = useState<Composing | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const [sending, setSending] = useState(false);
  // mirror of `drag` for the document mouseup handler (avoids stale-closure on the listener).
  const dragRef = useRef<Drag | null>(null);
  const setDragState = useCallback((d: Drag | null) => {
    dragRef.current = d;
    setDrag(d);
  }, []);

  const loadComments = useCallback(async () => {
    try {
      setComments(await api.listDiffComments(ticketId));
    } catch {
      /* comments are best-effort; the diff itself still renders */
    }
  }, [ticketId]);

  const load = useCallback(async () => {
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
    loadComments();
  }, [ticketId, loadComments]);

  useEffect(() => {
    setComposing(null);
    load();
  }, [ticketId, load]);

  const files = useMemo(() => {
    try {
      return parseDiff(diff);
    } catch {
      return [];
    }
  }, [diff]);

  // Per-file syntax-highlight tokens (skipped for unknown/unregistered languages).
  const tokensByFile = useMemo(
    () =>
      files.map((file) => {
        const lang = langFor(pathOf(file));
        if (!lang) return undefined;
        try {
          return tokenize(file.hunks, { highlight: true, refractor: refractorAdapter, language: lang } as any);
        } catch {
          return undefined;
        }
      }),
    [files]
  );

  const openCount = comments.filter((c) => c.status === "open").length;

  // Pressing a line's "+" starts a drag selection anchored on that line/side.
  const beginDrag = useCallback(
    (filePath: string, change: any, side: "old" | "new") => {
      let resolvedSide = side;
      let line = lineOnSide(change, side);
      if (line < 0) {
        // empty old-gutter of an inserted line (or vice-versa) — fall back to the other side.
        resolvedSide = side === "old" ? "new" : "old";
        line = lineOnSide(change, resolvedSide);
      }
      if (line < 0) return;
      setComposing(null);
      setDragState({ filePath, side: resolvedSide, startLine: line, endLine: line });
    },
    [setDragState]
  );

  // Dragging over another line in the same file extends the selection along the drag's side.
  const extendDrag = useCallback(
    (filePath: string, change: any) => {
      const d = dragRef.current;
      if (!d || d.filePath !== filePath) return;
      const line = lineOnSide(change, d.side);
      if (line < 0 || line === d.endLine) return;
      setDragState({ ...d, endLine: line });
    },
    [setDragState]
  );

  // Releasing the mouse turns the drag range into an open composer anchored at its last line.
  useEffect(() => {
    const onUp = () => {
      const d = dragRef.current;
      if (!d) return;
      setDragState(null);
      const lo = Math.min(d.startLine, d.endLine);
      const hi = Math.max(d.startLine, d.endLine);
      const file = files.find((f) => pathOf(f) === d.filePath);
      if (!file) return;
      const anchor =
        d.side === "old"
          ? findChangeByOldLineNumber(file.hunks, hi)
          : findChangeByNewLineNumber(file.hunks, hi);
      if (!anchor) return;
      setComposing({ filePath: d.filePath, anchorKey: getChangeKey(anchor), startLine: lo, endLine: hi, side: d.side });
    };
    document.addEventListener("mouseup", onUp);
    return () => document.removeEventListener("mouseup", onUp);
  }, [files, setDragState]);

  const submitComment = useCallback(
    async (filePath: string, startLine: number, endLine: number, side: "old" | "new", body: string) => {
      const text = body.trim();
      if (!text) return;
      await api.addDiffComment(ticketId, filePath, startLine, endLine, side, text);
      setComposing(null);
      loadComments();
    },
    [ticketId, loadComments]
  );

  const deleteComment = useCallback(
    async (id: number) => {
      await api.deleteDiffComment(id);
      loadComments();
    },
    [loadComments]
  );

  const resolveComment = useCallback(
    async (id: number) => {
      await api.resolveDiffComment(id);
      loadComments();
    },
    [loadComments]
  );

  const sendToClaude = useCallback(async () => {
    setSending(true);
    setErr(null);
    try {
      await api.startSession(ticketId, null);
      loadComments(); // open → sent
    } catch (e) {
      setErr(String(e));
    } finally {
      setSending(false);
    }
  }, [ticketId, loadComments]);

  return (
    <div className={"diffpane" + (drag ? " dragging" : "")}>
      <div className="diffpane-head">
        <span>Diff / PR</span>
        <div className="diffpane-actions">
          {openCount > 0 && (
            <button className="send-claude" onClick={sendToClaude} disabled={sending}>
              {sending ? "Sending…" : `Address ${openCount} comment${openCount === 1 ? "" : "s"} in Claude`}
            </button>
          )}
          <div className="view-toggle" role="group" aria-label="Diff layout">
            <button className={viewType === "unified" ? "on" : ""} onClick={() => setViewType("unified")}>
              Unified
            </button>
            <button className={viewType === "split" ? "on" : ""} onClick={() => setViewType("split")}>
              Split
            </button>
          </div>
          <button onClick={load} disabled={loading}>
            {loading ? "Loading…" : "Refresh"}
          </button>
        </div>
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
        <div className="checks-block">
          <button
            className="checks-toggle"
            aria-expanded={checksOpen}
            onClick={() => setChecksOpen((o) => !o)}
          >
            {checksOpen ? "▾" : "▸"} CI checks {checksSummary(checks)}
          </button>
          {checksOpen && (
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
        </div>
      )}

      {loading && !diff ? (
        <Skeleton lines={8} />
      ) : files.length === 0 ? (
        <div className="diff-empty muted">No changes vs base branch.</div>
      ) : (
        <div className="diff-files">
          {files.map((file, i) => (
            <FileDiff
              key={pathOf(file) + "@" + i}
              file={file}
              tokens={tokensByFile[i]}
              viewType={viewType}
              comments={comments}
              composing={composing}
              drag={drag}
              onBeginDrag={beginDrag}
              onExtendDrag={extendDrag}
              onCancelCompose={() => setComposing(null)}
              onSubmit={submitComment}
              onDelete={deleteComment}
              onResolve={resolveComment}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function FileDiff({
  file,
  tokens,
  viewType,
  comments,
  composing,
  drag,
  onBeginDrag,
  onExtendDrag,
  onCancelCompose,
  onSubmit,
  onDelete,
  onResolve,
}: {
  file: any;
  tokens: any;
  viewType: "unified" | "split";
  comments: DiffComment[];
  composing: Composing | null;
  drag: Drag | null;
  onBeginDrag: (filePath: string, change: any, side: "old" | "new") => void;
  onExtendDrag: (filePath: string, change: any) => void;
  onCancelCompose: () => void;
  onSubmit: (filePath: string, startLine: number, endLine: number, side: "old" | "new", body: string) => void;
  onDelete: (id: number) => void;
  onResolve: (id: number) => void;
}) {
  const [collapsed, setCollapsed] = useState(false);
  const filePath = pathOf(file);
  const fileComments = comments.filter((c) => c.file_path === filePath);

  // The active selection range for THIS file (live drag takes precedence over the open composer).
  const activeRange = useMemo(() => {
    if (drag && drag.filePath === filePath) {
      return { side: drag.side, lo: Math.min(drag.startLine, drag.endLine), hi: Math.max(drag.startLine, drag.endLine) };
    }
    if (composing && composing.filePath === filePath) {
      return { side: composing.side, lo: composing.startLine, hi: composing.endLine };
    }
    return null;
  }, [drag, composing, filePath]);

  const selectedChanges = useMemo(
    () => (activeRange ? changeKeysInRange(file.hunks, activeRange.side, activeRange.lo, activeRange.hi) : []),
    [activeRange, file.hunks]
  );

  const adds = file.hunks.reduce(
    (n: number, h: any) => n + h.changes.filter((c: any) => c.type === "insert").length,
    0
  );
  const dels = file.hunks.reduce(
    (n: number, h: any) => n + h.changes.filter((c: any) => c.type === "delete").length,
    0
  );

  // Map comments (and the open composer, if it's on this file) to per-line widgets.
  const widgets = useMemo(() => {
    const byKey: Record<string, DiffComment[]> = {};
    for (const c of fileComments) {
      // Anchor the widget at the range's last line (end_line), GitHub-style. end_line is 0 for
      // comments saved before ranges existed — fall back to the start line.
      const anchorLine = c.end_line || c.line;
      const change =
        c.side === "old"
          ? findChangeByOldLineNumber(file.hunks, anchorLine)
          : findChangeByNewLineNumber(file.hunks, anchorLine);
      if (!change) continue;
      const key = getChangeKey(change);
      (byKey[key] ||= []).push(c);
    }
    const composeKey = composing && composing.filePath === filePath ? composing.anchorKey : null;

    const map: Record<string, React.ReactNode> = {};
    const keys = new Set<string>([...Object.keys(byKey), ...(composeKey ? [composeKey] : [])]);
    for (const key of keys) {
      map[key] = (
        <div className="diff-comments">
          {(byKey[key] ?? []).map((c) => (
            <CommentCard key={c.id} comment={c} onDelete={onDelete} onResolve={onResolve} />
          ))}
          {composeKey === key && composing && (
            <Composer
              rangeLabel={rangeLabel(composing.startLine, composing.endLine)}
              onCancel={onCancelCompose}
              onSubmit={(body) => onSubmit(filePath, composing.startLine, composing.endLine, composing.side, body)}
            />
          )}
        </div>
      );
    }
    return map;
  }, [fileComments, composing, file.hunks, filePath, onCancelCompose, onSubmit, onDelete, onResolve]);

  const renderGutter = ({ change, side, renderDefault }: any) => (
    <>
      {renderDefault()}
      <button
        type="button"
        className="add-comment-btn"
        title="Comment — drag to select multiple lines"
        // mousedown (not click) starts a drag selection; preventDefault stops text-selection.
        onMouseDown={(e) => {
          e.preventDefault();
          e.stopPropagation();
          onBeginDrag(filePath, change, side);
        }}
      >
        +
      </button>
    </>
  );

  // While a drag is active, entering a line extends the selection to it.
  const dragEvents = { onMouseEnter: ({ change }: any) => change && onExtendDrag(filePath, change) };

  return (
    <div className="diff-file">
      <div className="diff-file-head" onClick={() => setCollapsed((v) => !v)}>
        <span className={"caret" + (collapsed ? " collapsed" : "")}>▾</span>
        <span className="diff-file-path">{filePath}</span>
        {file.type !== "delete" && file.oldPath !== file.newPath && file.type === "rename" && (
          <span className="muted diff-file-rename">← {file.oldPath}</span>
        )}
        <span className="diff-file-stat">
          <span className="d-add">+{adds}</span> <span className="d-del">−{dels}</span>
        </span>
      </div>
      {!collapsed &&
        (file.isBinary ? (
          <div className="diff-binary muted">Binary file not shown.</div>
        ) : (
          <Diff
            viewType={viewType}
            diffType={file.type}
            hunks={file.hunks}
            tokens={tokens}
            widgets={widgets}
            selectedChanges={selectedChanges}
            gutterEvents={dragEvents}
            codeEvents={dragEvents}
            renderGutter={renderGutter}
          >
            {(hunks) =>
              hunks.flatMap((hunk: any) => [
                <Decoration key={"deco-" + hunk.content}>
                  <span className="hunk-header">{hunk.content}</span>
                </Decoration>,
                <Hunk key={hunk.content} hunk={hunk} />,
              ])
            }
          </Diff>
        ))}
    </div>
  );
}

// "Line 5" or "Lines 5–9".
function rangeLabel(start: number, end: number): string {
  return end > start ? `Lines ${start}–${end}` : `Line ${start}`;
}

function CommentCard({
  comment,
  onDelete,
  onResolve,
}: {
  comment: DiffComment;
  onDelete: (id: number) => void;
  onResolve: (id: number) => void;
}) {
  const endLine = comment.end_line || comment.line;
  return (
    <div className={"comment-card" + (comment.status === "resolved" ? " resolved" : "")}>
      <div className="comment-head">
        <span className="comment-author">You</span>
        <span className="comment-range muted">{rangeLabel(comment.line, endLine)}</span>
        {comment.status === "sent" && <span className="comment-status sent">sent to Claude</span>}
        {comment.status === "resolved" && <span className="comment-status">resolved</span>}
        <span className="comment-actions">
          {comment.status !== "resolved" && (
            <button onClick={() => onResolve(comment.id)}>Resolve</button>
          )}
          <button onClick={() => onDelete(comment.id)}>Delete</button>
        </span>
      </div>
      <div className="comment-body">{comment.body}</div>
    </div>
  );
}

function Composer({
  onSubmit,
  onCancel,
  rangeLabel,
}: {
  onSubmit: (body: string) => void;
  onCancel: () => void;
  rangeLabel: string;
}) {
  const [body, setBody] = useState("");
  return (
    <div className="comment-composer">
      <div className="comment-composer-range muted">{rangeLabel}</div>
      <textarea
        autoFocus
        value={body}
        placeholder="Leave a comment for Claude to address…"
        onChange={(e) => setBody(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) onSubmit(body);
          if (e.key === "Escape") onCancel();
        }}
      />
      <div className="comment-composer-actions">
        <button className="primary" onClick={() => onSubmit(body)} disabled={!body.trim()}>
          Comment
        </button>
        <button onClick={onCancel}>Cancel</button>
      </div>
    </div>
  );
}

import { useState, useEffect } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "../api";
import type { Repo } from "../types";

const MODES = [
  ["auto", "auto — autonomous (auto-approves most actions)"],
  ["acceptEdits", "acceptEdits — auto-approve edits & safe commands"],
  ["default", "default — ask for permission"],
  ["plan", "plan — propose only, no changes"],
  ["bypassPermissions", "bypassPermissions — no checks (isolated worktree)"],
];

export function Settings({
  repos,
  onAdd,
  onRename,
  onDelete,
}: {
  repos: Repo[];
  onAdd: (name: string, path: string, project: string | null) => void;
  onRename: (id: number, name: string) => void;
  onDelete: (id: number) => void;
}) {
  const [name, setName] = useState("");
  const [path, setPath] = useState("");
  const [project, setProject] = useState("");
  const [editId, setEditId] = useState<number | null>(null);
  const [editVal, setEditVal] = useState("");
  const [mode, setMode] = useState("auto");
  const [autoEndIdle, setAutoEndIdle] = useState(false);
  const [autoReview, setAutoReview] = useState(true);
  const [reviewLoop, setReviewLoop] = useState(false);
  const [autoMerge, setAutoMerge] = useState(false);
  const [orchestrator, setOrchestrator] = useState(false);
  const [maxConcurrent, setMaxConcurrent] = useState(3);

  useEffect(() => {
    api.getPermissionMode().then(setMode).catch(() => {});
    api.getAutoEndIdle().then(setAutoEndIdle).catch(() => {});
    api.getAutoReview().then(setAutoReview).catch(() => {});
    api.getReviewLoop().then(setReviewLoop).catch(() => {});
    api.getAutoMerge().then(setAutoMerge).catch(() => {});
    api.getOrchestrator().then(setOrchestrator).catch(() => {});
    api.getMaxConcurrent().then(setMaxConcurrent).catch(() => {});
  }, []);

  const changeMode = (m: string) => {
    setMode(m);
    api.setPermissionMode(m).catch(() => {});
  };

  const toggleAutoEndIdle = () => {
    const next = !autoEndIdle;
    setAutoEndIdle(next);
    api.setAutoEndIdle(next).catch(() => setAutoEndIdle(!next));
  };

  const toggleAutoReview = () => {
    const next = !autoReview;
    setAutoReview(next);
    api.setAutoReview(next).catch(() => setAutoReview(!next));
  };

  const toggleReviewLoop = () => {
    const next = !reviewLoop;
    setReviewLoop(next);
    api.setReviewLoop(next).catch(() => setReviewLoop(!next));
  };

  const toggleAutoMerge = () => {
    const next = !autoMerge;
    setAutoMerge(next);
    api.setAutoMerge(next).catch(() => setAutoMerge(!next));
  };

  const toggleOrchestrator = () => {
    const next = !orchestrator;
    setOrchestrator(next);
    api.setOrchestrator(next).catch(() => setOrchestrator(!next));
  };

  const changeMaxConcurrent = (n: number) => {
    const v = Math.max(1, Math.floor(n || 1));
    setMaxConcurrent(v);
    api.setMaxConcurrent(v).catch(() => {});
  };

  const commitRename = (r: Repo) => {
    const v = editVal.trim();
    if (v && v !== r.name) onRename(r.id, v);
    setEditId(null);
  };

  const pick = async () => {
    const sel = await open({ directory: true, multiple: false, title: "Choose a git repository" });
    if (typeof sel === "string") {
      setPath(sel);
      if (!name) setName(sel.split("/").filter(Boolean).pop() ?? "");
    }
  };

  const add = () => {
    onAdd(name.trim(), path, project.trim() || null);
    setName("");
    setPath("");
    setProject("");
  };

  return (
    <div className="sessions settings">
      <h3>Claude</h3>
      <div className="settings-add">
        <label className="muted">Permission mode (new sessions)</label>
        <select value={mode} onChange={(e) => changeMode(e.target.value)}>
          {MODES.map(([v, label]) => (
            <option key={v} value={v}>
              {label}
            </option>
          ))}
        </select>
        <label className="muted" title="When Claude stops and is waiting, close its terminal instead of leaving it idle. Resume by moving the card back to In Progress (grills restart fresh).">
          <input type="checkbox" checked={autoEndIdle} onChange={toggleAutoEndIdle} /> End idle sessions
          when Claude is waiting
        </label>
        <label className="muted" title="When a reviewed branch changes (feedback addressed, work resumed, a CI fix landed), automatically re-run /review on the new code. Applies to For Your Review and In PR Review.">
          <input type="checkbox" checked={autoReview} onChange={toggleAutoReview} /> Auto re-review
          when the code changes
        </label>
        <label className="muted" title="Self-correcting review loop: when /review of a 'For Your Review' card finds blocking issues, automatically fix them and re-review until clean (capped, then notifies you). Stays in 'For Your Review' for you to open the PR.">
          <input type="checkbox" checked={reviewLoop} onChange={toggleReviewLoop} /> Auto-fix review
          findings and re-review until clean
        </label>
        <label className="muted" title="When a PR is approved on GitHub and CI is green, automatically merge it and move the card to Done — no manual drag. Merges to your default branch; the agent never self-approves.">
          <input type="checkbox" checked={autoMerge} onChange={toggleAutoMerge} /> Auto-merge PRs once
          approved &amp; green
        </label>
        <label className="muted" title="Orchestrator: autonomously starts ready tickets and restarts crashed sessions (up to the concurrency limit), answers worker questions it can derive from the spec (escalating genuine judgment to you), and auto-advances the loop (opens PRs when a review is clean). It never merges. You still create/spec tickets, offer judgment on escalations, and check outcomes.">
          <input type="checkbox" checked={orchestrator} onChange={toggleOrchestrator} /> Orchestrator —
          run &amp; wrangle sessions autonomously
        </label>
        <label className="muted" title="Maximum worker sessions the orchestrator runs at once.">
          Max concurrent sessions
          <input
            type="number"
            min={1}
            value={maxConcurrent}
            disabled={!orchestrator}
            onChange={(e) => changeMaxConcurrent(Number(e.target.value))}
            style={{ width: 56, marginLeft: 8 }}
          />
        </label>
      </div>

      <h3>Repositories</h3>
      <table>
        <thead>
          <tr>
            <th>Name</th>
            <th>Path</th>
            <th>Default Jira project</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {repos.length === 0 && (
            <tr>
              <td className="empty" colSpan={4}>
                No repos yet — add one below to start working tickets.
              </td>
            </tr>
          )}
          {repos.map((r) => (
            <tr key={r.id}>
              <td>
                {editId === r.id ? (
                  <input
                    autoFocus
                    value={editVal}
                    onChange={(e) => setEditVal(e.target.value)}
                    onBlur={() => commitRename(r)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") commitRename(r);
                      if (e.key === "Escape") setEditId(null);
                    }}
                  />
                ) : (
                  <span
                    className="renamable"
                    title="Click to rename"
                    onClick={() => {
                      setEditId(r.id);
                      setEditVal(r.name);
                    }}
                  >
                    {r.name}
                  </span>
                )}
              </td>
              <td className="mono path">{r.path}</td>
              <td className="mono">{r.default_project_key ?? "—"}</td>
              <td>
                <button className="row-del" title="Remove repo" onClick={() => onDelete(r.id)}>
                  ×
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      <h3>Add a repository</h3>
      <div className="settings-add">
        <input placeholder="name" value={name} onChange={(e) => setName(e.target.value)} />
        <button onClick={pick}>{path || "Choose folder…"}</button>
        <input
          placeholder="default Jira project key (optional, e.g. DNA)"
          value={project}
          onChange={(e) => setProject(e.target.value)}
        />
        <button disabled={!name.trim() || !path} onClick={add}>
          Add repo
        </button>
      </div>
    </div>
  );
}

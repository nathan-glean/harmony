//! Domain types, mapped from SQLite rows via `sqlx::FromRow` (by column name).

use serde::{Deserialize, Serialize};

/// A registered local git repository. `default_project_key` lets `PROJ-*` tickets
/// default to this repo (DESIGN Q8).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Repo {
    pub id: i64,
    pub name: String,
    pub path: String,
    pub default_project_key: Option<String>,
}

/// A harmony ticket: an enriched local entity, optionally linked to a Jira issue
/// via `jira_key` (DESIGN Q5). `spec` is the agent-facing brief (Q10).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Ticket {
    pub id: i64,
    pub jira_key: Option<String>,
    pub source: String, // "jira" | "local"
    pub title: String,
    pub spec: String,
    pub status: String, // see crate::status
    pub repo_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Claude's task list (TodoWrite), as a JSON array of {content, status}.
    pub todos: String,
    /// Pending AskUserQuestion, as a JSON object {session_id, questions:[…]} or "" when
    /// none. Surfaced as an answerable question card; cleared when Claude moves on.
    pub pending_question: String,
    /// 0/1 — whether the one-time initial plan-mode run has happened for this ticket.
    /// Gates plan mode so it runs only once at the very start (not on resume/re-entry).
    pub planned: i64,
    /// 0/1 — a grill/spec session is actively producing this ticket's spec. While 1 the
    /// ticket is a draft (badge + gated from In Progress); cleared when the spec is captured.
    pub drafting: i64,
    /// 0/1 — whether this ticket has been through a grill interview. Gates the auto-grill on
    /// entry to In Progress so a never-grilled (e.g. Jira-synced) ticket gets grilled once.
    pub grilled: i64,
    /// Promoted first-class spec fields (DESIGN Q10). Freeform text alongside the `spec` body;
    /// composed into the opening prompt via `crate::spec::compose_spec`.
    pub acceptance_criteria: String,
    pub relevant_paths: String,
    pub constraints: String,
    /// 0/1 — whether `/review` has run for this ticket at least once (drives the flow `reviewed`
    /// fact / the "must review before PR" gate).
    pub reviewed: i64,
    /// The branch HEAD commit `/review` last ran against; compared to the current HEAD so review
    /// isn't re-run when nothing changed (the flow `review_current` fact). Empty when never reviewed.
    pub reviewed_sha: String,
    /// The prose review `/review` produced last (Claude's final assistant message), captured when
    /// the review session finishes. Latest-only — overwritten on each re-review. Empty when never
    /// reviewed; surfaced in the ticket's Review tab.
    pub review_text: String,
    /// HEAD sha the last CI triage ran against (idempotency fingerprint — the poller skips a
    /// commit it has already triaged). Empty when never triaged.
    pub ci_triaged_sha: String,
    /// Number of automatic CI-fix attempts made for this PR; capped to prevent runaway loops.
    pub ci_fix_attempts: i64,
    /// JSON of the latest `crate::ci::CiTriage` (verdict + reason + failing checks), for the UI.
    pub ci_triage: String,
    /// A spec update Claude proposed while addressing feedback that contradicted the spec
    /// (propose & confirm). Markdown of the full revised spec; empty when none pending. The user
    /// accepts (→ live spec fields) or rejects it in the Spec tab.
    pub proposed_spec: String,
    /// The autonomous review-loop judge's latest verdict over the `/review` output: "pass",
    /// "changes_requested", or "" when not yet judged. Drives the self-correcting review loop.
    pub review_verdict: String,
    /// The judge's must-fix findings, as a JSON array of strings; "" when none. Seeded into the
    /// autonomous review-fix session's prompt.
    pub review_findings: String,
    /// The branch HEAD the review judge last ran against (idempotency fingerprint, like
    /// `reviewed_sha`) — the loop won't re-judge a verdict it already computed for this HEAD.
    pub judged_sha: String,
    /// Number of automatic review-fix attempts made for the current review episode; capped to
    /// prevent a runaway review→fix→re-review loop. Reset when fresh human work lands.
    pub review_fix_attempts: i64,
    /// The derived "what's happening" status (JSON of `crate::activity::Activity`), recomputed by the
    /// backend on every state-machine change and each poll tick. "" until first computed. Rendered as
    /// the per-card activity pill — the UI never derives it itself.
    pub activity: String,
    /// The orchestrator's last action + rationale for this ticket (e.g. "answered Q: chose 'Postgres'",
    /// "escalated: ambiguous scope"). Human-facing audit line; "" when the orchestrator hasn't acted.
    pub orchestrator_note: String,
    /// Idempotency fingerprint for the orchestrator conductor: the "stuck state" it last decided on
    /// (so it doesn't re-answer/re-escalate the same state every tick). "" until first decided.
    pub orchestrator_seen: String,
    /// Number of times the orchestrator has auto-restarted a crashed session for this ticket; capped
    /// to avoid crash loops (reset when work legitimately progresses).
    pub restart_attempts: i64,
    /// The proof-of-work report the proof session produced (markdown: what now works, how to see it,
    /// verbatim verification output), captured from its plan-file write. Latest-only. "" until a
    /// proof run completes. Surfaced in the Proof tab and posted to the PR comment.
    pub proof: String,
    /// The proof session's captured media/evidence artifacts, as a JSON array of
    /// `{kind, path, caption}` (`kind` = image | video | cast | file). Built by scanning the proof
    /// artifact dir when the session finishes. "" when none.
    pub proof_artifacts: String,
    /// The branch HEAD the proof last ran against (idempotency fingerprint, like `reviewed_sha`) — the
    /// proof poller won't regenerate until the branch moves. "" until first produced.
    pub proof_sha: String,
    /// Number of proof-generation attempts for the current HEAD; capped to avoid a runaway loop when
    /// capture keeps failing. Reset when fresh work lands.
    pub proof_attempts: i64,
}

/// An isolated git worktree for a ticket. Per-ticket and reused; `is_alternate`
/// marks an explicit "new alternate attempt" worktree (DESIGN Q9).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Worktree {
    pub id: i64,
    pub ticket_id: i64,
    pub repo_id: i64,
    pub branch: String,
    pub path: String,
    pub is_alternate: i64, // 0/1
    pub created_at: i64,
}

/// A Claude Code session run inside a worktree. `claude_session_id` is learned from
/// the first hook POST (correlated by cwd) and used to `--resume` later.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Session {
    pub id: i64,
    pub ticket_id: i64,
    pub worktree_id: i64,
    pub claude_session_id: Option<String>,
    pub state: String, // working | waiting | done
    pub last_tool: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    /// Launch directory (worktree path for work sessions, repo root for spec sessions).
    pub cwd: Option<String>,
    /// "work" | "spec" | "review" | "proof" | "fix" | "address" — a spec session runs the grill in
    /// plan mode without a worktree; a proof session captures evidence the change works.
    pub kind: String,
}

/// A reviewer comment left for a ticket. Surfaced in the review surfaces and, while
/// `status == "open"`, injected into Claude's next feedback/resume prompt so it can address it.
/// `target` selects the surface: `diff` (anchored to `file_path`/`line`/`side`, GitHub-style
/// inline card), `general` (a free-form note/suggestion), `review` (on Claude's `/review`), or
/// `pr_comment` (on a GitHub PR comment). For non-diff targets `anchor` carries the context (the
/// quoted snippet, or `author: "snippet"` for a PR comment) and the diff fields are empty/zero.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DiffComment {
    pub id: i64,
    pub ticket_id: i64,
    pub file_path: String,
    pub line: i64,     // first line of the commented range (start)
    pub end_line: i64, // last line of the range; == line for a single-line comment
    pub side: String,  // "new" | "old"
    pub body: String,
    pub status: String, // "open" | "sent" | "resolved"
    pub created_at: i64,
    pub target: String, // "general" | "diff" | "review" | "pr_comment"
    pub anchor: String, // context for non-diff targets (quoted snippet / PR author+snippet)
}

/// One orchestrator decision, enriched with its ticket for the Orchestrator tab's decision feed.
/// `kind` is a coarse category (`dispatch|restart|answer|spec|pr|escalate|info`) for icon/colour;
/// `note` is the human-facing rationale (e.g. "answered question — \"Postgres\"").
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct OrchestratorEvent {
    pub id: i64,
    pub ticket_id: i64,
    pub kind: String,
    pub note: String,
    pub created_at: i64,
    pub ticket_title: String,
    pub jira_key: Option<String>,
}

/// A worktree enriched with its ticket + repo info, for the Worktrees view.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct WorktreeView {
    pub id: i64,
    pub ticket_id: i64,
    pub ticket_title: String,
    pub jira_key: Option<String>,
    pub repo_name: String,
    pub repo_path: String,
    pub branch: String,
    pub path: String,
    pub is_alternate: i64,
    pub created_at: i64,
}

/// A session enriched with its ticket + worktree info, for the Sessions view.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct SessionView {
    pub id: i64,
    pub ticket_id: i64,
    pub worktree_id: i64,
    pub ticket_title: String,
    pub jira_key: Option<String>,
    pub branch: String,
    pub state: String,
    pub last_tool: Option<String>,
    pub claude_session_id: Option<String>,
    pub started_at: i64,
    pub ended_at: Option<i64>,
}

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
    /// "work" | "spec" — a spec session runs the grill in plan mode without a worktree.
    pub kind: String,
}

/// A reviewer comment left on a specific diff line, for a ticket. Surfaced in the diff
/// pane (GitHub-style inline cards) and, while `status == "open"`, injected into Claude's
/// next resume prompt so it can address the feedback. `side` is which gutter the comment
/// anchors to ("new" for added/context on the new file, "old" for the original).
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct DiffComment {
    pub id: i64,
    pub ticket_id: i64,
    pub file_path: String,
    pub line: i64,      // first line of the commented range (start)
    pub end_line: i64,  // last line of the range; == line for a single-line comment
    pub side: String,   // "new" | "old"
    pub body: String,
    pub status: String, // "open" | "sent" | "resolved"
    pub created_at: i64,
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

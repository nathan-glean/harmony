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

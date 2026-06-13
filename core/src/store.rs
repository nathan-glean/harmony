//! SQLite persistence (DESIGN: store at `~/.harmony/harmony.db`). Runtime queries
//! (no compile-time `query!` macros), so no DATABASE_URL needed at build time.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;

use crate::models::{Repo, Session, SessionView, Ticket, Worktree, WorktreeView};
use crate::now_unix;

#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (creating the file + parent dirs if missing) and run migrations.
    pub async fn open(db_path: &str) -> Result<Self> {
        if let Some(parent) = Path::new(db_path).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await?;
        let store = Self { pool };
        store.migrate().await?;
        Ok(store)
    }

    async fn migrate(&self) -> Result<()> {
        const DDL: &str = r#"
        CREATE TABLE IF NOT EXISTS repos (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            path TEXT NOT NULL,
            default_project_key TEXT
        );
        CREATE TABLE IF NOT EXISTS tickets (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            jira_key TEXT,
            source TEXT NOT NULL,
            title TEXT NOT NULL,
            spec TEXT NOT NULL DEFAULT '',
            status TEXT NOT NULL DEFAULT 'todo',
            repo_id INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            todos TEXT NOT NULL DEFAULT ''
        );
        UPDATE tickets SET status = 'todo' WHERE status IN ('available', 'ready');
        CREATE TABLE IF NOT EXISTS worktrees (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ticket_id INTEGER NOT NULL,
            repo_id INTEGER NOT NULL,
            branch TEXT NOT NULL,
            path TEXT NOT NULL,
            is_alternate INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sessions (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ticket_id INTEGER NOT NULL,
            worktree_id INTEGER NOT NULL,
            claude_session_id TEXT,
            state TEXT NOT NULL DEFAULT 'working',
            last_tool TEXT,
            started_at INTEGER NOT NULL,
            ended_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#;
        for stmt in DDL.split(';') {
            let s = stmt.trim();
            if !s.is_empty() {
                sqlx::query(s).execute(&self.pool).await?;
            }
        }
        // Add columns introduced after the initial schema (ignore "duplicate column").
        let _ = sqlx::query("ALTER TABLE tickets ADD COLUMN todos TEXT NOT NULL DEFAULT ''")
            .execute(&self.pool)
            .await;
        let _ = sqlx::query("ALTER TABLE sessions ADD COLUMN transcript_path TEXT")
            .execute(&self.pool)
            .await;
        Ok(())
    }

    // ---- repos -----------------------------------------------------------

    pub async fn add_repo(&self, name: &str, path: &str, default_key: Option<&str>) -> Result<i64> {
        let canonical = std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string());
        let r = sqlx::query("INSERT INTO repos (name, path, default_project_key) VALUES (?, ?, ?)")
            .bind(name)
            .bind(&canonical)
            .bind(default_key)
            .execute(&self.pool)
            .await?;
        Ok(r.last_insert_rowid())
    }

    pub async fn list_repos(&self) -> Result<Vec<Repo>> {
        Ok(sqlx::query_as::<_, Repo>(
            "SELECT id, name, path, default_project_key FROM repos ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get_repo(&self, id: i64) -> Result<Option<Repo>> {
        Ok(sqlx::query_as::<_, Repo>(
            "SELECT id, name, path, default_project_key FROM repos WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn get_repo_by_name(&self, name: &str) -> Result<Option<Repo>> {
        Ok(sqlx::query_as::<_, Repo>(
            "SELECT id, name, path, default_project_key FROM repos WHERE name = ?",
        )
        .bind(name)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn rename_repo(&self, id: i64, name: &str) -> Result<()> {
        sqlx::query("UPDATE repos SET name = ? WHERE id = ?")
            .bind(name)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Remove a registered repo. Refuses if it still has worktrees (delete those first);
    /// clears the repo binding on any tickets that referenced it. Files on disk are untouched.
    pub async fn delete_repo(&self, id: i64) -> Result<()> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worktrees WHERE repo_id = ?")
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        if count > 0 {
            return Err(anyhow::anyhow!("repo still has {count} worktree(s) — delete those first"));
        }
        sqlx::query("UPDATE tickets SET repo_id = NULL WHERE repo_id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        sqlx::query("DELETE FROM repos WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn default_repo_for_key(&self, project_key: &str) -> Result<Option<Repo>> {
        Ok(sqlx::query_as::<_, Repo>(
            "SELECT id, name, path, default_project_key FROM repos WHERE default_project_key = ?",
        )
        .bind(project_key)
        .fetch_optional(&self.pool)
        .await?)
    }

    // ---- tickets ---------------------------------------------------------

    pub async fn add_ticket(
        &self,
        jira_key: Option<&str>,
        source: &str,
        title: &str,
        spec: &str,
        repo_id: Option<i64>,
    ) -> Result<i64> {
        let now = now_unix();
        // All new tickets land in Todo (DESIGN Q14); promotion to Ready happens when a
        // spec is saved (see `set_ticket_spec`).
        let status = crate::status::TODO;
        let r = sqlx::query(
            "INSERT INTO tickets (jira_key, source, title, spec, status, repo_id, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(jira_key)
        .bind(source)
        .bind(title)
        .bind(spec)
        .bind(status)
        .bind(repo_id)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(r.last_insert_rowid())
    }

    pub async fn get_ticket(&self, id: i64) -> Result<Option<Ticket>> {
        Ok(sqlx::query_as::<_, Ticket>(
            "SELECT id, jira_key, source, title, spec, status, repo_id, created_at, updated_at, todos
             FROM tickets WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn list_tickets(&self) -> Result<Vec<Ticket>> {
        Ok(sqlx::query_as::<_, Ticket>(
            "SELECT id, jira_key, source, title, spec, status, repo_id, created_at, updated_at, todos
             FROM tickets ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn set_ticket_status(&self, id: i64, status: &str) -> Result<()> {
        sqlx::query("UPDATE tickets SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(now_unix())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_ticket_repo(&self, id: i64, repo_id: i64) -> Result<()> {
        sqlx::query("UPDATE tickets SET repo_id = ?, updated_at = ? WHERE id = ?")
            .bind(repo_id)
            .bind(now_unix())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- worktrees -------------------------------------------------------

    pub async fn add_worktree(
        &self,
        ticket_id: i64,
        repo_id: i64,
        branch: &str,
        path: &str,
        is_alternate: bool,
    ) -> Result<i64> {
        let r = sqlx::query(
            "INSERT INTO worktrees (ticket_id, repo_id, branch, path, is_alternate, created_at)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(ticket_id)
        .bind(repo_id)
        .bind(branch)
        .bind(path)
        .bind(if is_alternate { 1_i64 } else { 0_i64 })
        .bind(now_unix())
        .execute(&self.pool)
        .await?;
        Ok(r.last_insert_rowid())
    }

    pub async fn get_worktree(&self, id: i64) -> Result<Option<Worktree>> {
        Ok(sqlx::query_as::<_, Worktree>(
            "SELECT id, ticket_id, repo_id, branch, path, is_alternate, created_at
             FROM worktrees WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// All worktrees, newest first, joined with ticket + repo info.
    pub async fn list_worktrees(&self) -> Result<Vec<WorktreeView>> {
        Ok(sqlx::query_as::<_, WorktreeView>(
            "SELECT w.id, w.ticket_id, t.title AS ticket_title, t.jira_key,
                    r.name AS repo_name, r.path AS repo_path,
                    w.branch, w.path, w.is_alternate, w.created_at
             FROM worktrees w
             JOIN tickets t ON t.id = w.ticket_id
             JOIN repos r ON r.id = w.repo_id
             ORDER BY w.id DESC",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Delete a worktree row and its sessions (DB only; git worktree removal is the
    /// caller's job via `worktree::remove`).
    pub async fn delete_worktree(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE worktree_id = ?").bind(id).execute(&self.pool).await?;
        sqlx::query("DELETE FROM worktrees WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn worktrees_for_ticket(&self, ticket_id: i64) -> Result<Vec<Worktree>> {
        Ok(sqlx::query_as::<_, Worktree>(
            "SELECT id, ticket_id, repo_id, branch, path, is_alternate, created_at
             FROM worktrees WHERE ticket_id = ?",
        )
        .bind(ticket_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Delete a ticket and its worktree/session rows (DB only; git worktree cleanup is
    /// done by the caller via `worktree::cleanup_for_ticket`). For a Jira-linked ticket
    /// this only removes the local record — the Jira issue is untouched (and a later
    /// `sync` will re-add it).
    pub async fn delete_ticket(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE ticket_id = ?").bind(id).execute(&self.pool).await?;
        sqlx::query("DELETE FROM worktrees WHERE ticket_id = ?").bind(id).execute(&self.pool).await?;
        sqlx::query("DELETE FROM tickets WHERE id = ?").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// The primary (non-alternate) worktree for a ticket, if one exists. Reuse target.
    pub async fn primary_worktree_for_ticket(&self, ticket_id: i64) -> Result<Option<Worktree>> {
        Ok(sqlx::query_as::<_, Worktree>(
            "SELECT id, ticket_id, repo_id, branch, path, is_alternate, created_at
             FROM worktrees WHERE ticket_id = ? AND is_alternate = 0 ORDER BY id LIMIT 1",
        )
        .bind(ticket_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    // ---- sessions --------------------------------------------------------

    pub async fn add_session(&self, ticket_id: i64, worktree_id: i64) -> Result<i64> {
        let r = sqlx::query(
            "INSERT INTO sessions (ticket_id, worktree_id, state, started_at) VALUES (?, ?, 'working', ?)",
        )
        .bind(ticket_id)
        .bind(worktree_id)
        .bind(now_unix())
        .execute(&self.pool)
        .await?;
        Ok(r.last_insert_rowid())
    }

    pub async fn set_session_transcript_path(&self, id: i64, path: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET transcript_path = ? WHERE id = ?")
            .bind(path)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Latest known transcript path for any of a ticket's sessions (resumed sessions
    /// share the same Claude session id → same transcript file).
    pub async fn latest_transcript_path_for_ticket(&self, ticket_id: i64) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT transcript_path FROM sessions
             WHERE ticket_id = ? AND transcript_path IS NOT NULL
             ORDER BY id DESC LIMIT 1",
        )
        .bind(ticket_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Distinct tickets that had an open (not-ended) session — i.e. were live at the last
    /// shutdown. Used to reattach on relaunch (call before `end_all_open_sessions`).
    pub async fn tickets_with_open_session(&self) -> Result<Vec<i64>> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT DISTINCT ticket_id FROM sessions WHERE ended_at IS NULL",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn set_session_claude_id(&self, id: i64, claude_session_id: &str) -> Result<()> {
        sqlx::query("UPDATE sessions SET claude_session_id = ? WHERE id = ?")
            .bind(claude_session_id)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Update session state; keeps the previous `last_tool` when `tool` is None.
    pub async fn set_session_state(&self, id: i64, state: &str, tool: Option<&str>) -> Result<()> {
        sqlx::query("UPDATE sessions SET state = ?, last_tool = COALESCE(?, last_tool) WHERE id = ?")
            .bind(state)
            .bind(tool)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn end_session(&self, id: i64) -> Result<()> {
        sqlx::query("UPDATE sessions SET state = 'done', ended_at = ? WHERE id = ?")
            .bind(now_unix())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Mark every still-open session as ended. Called on app startup — their PTYs died
    /// with the previous process, so they're zombies, not live.
    pub async fn end_all_open_sessions(&self) -> Result<()> {
        sqlx::query("UPDATE sessions SET state = 'done', ended_at = ? WHERE ended_at IS NULL")
            .bind(now_unix())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Delete all ended sessions; returns how many were removed. Live sessions are kept.
    pub async fn delete_ended_sessions(&self) -> Result<u64> {
        let r = sqlx::query("DELETE FROM sessions WHERE ended_at IS NOT NULL")
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// Delete a worktree's ended sessions (used to clear a consolidated group). Live
    /// sessions are kept. Returns how many were removed.
    pub async fn delete_ended_sessions_for_worktree(&self, worktree_id: i64) -> Result<u64> {
        let r = sqlx::query("DELETE FROM sessions WHERE worktree_id = ? AND ended_at IS NOT NULL")
            .bind(worktree_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// Delete a single session, but only if it has ended (won't drop a live one).
    pub async fn delete_session(&self, id: i64) -> Result<()> {
        sqlx::query("DELETE FROM sessions WHERE id = ? AND ended_at IS NOT NULL")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Find the live (not-yet-ended) session whose worktree path equals `path`.
    /// This is how incoming hooks (which carry `cwd`) are correlated to a session.
    pub async fn active_session_by_worktree_path(&self, path: &str) -> Result<Option<Session>> {
        Ok(sqlx::query_as::<_, Session>(
            "SELECT s.id, s.ticket_id, s.worktree_id, s.claude_session_id, s.state, s.last_tool, s.started_at, s.ended_at
             FROM sessions s JOIN worktrees w ON w.id = s.worktree_id
             WHERE w.path = ? AND s.ended_at IS NULL
             ORDER BY s.id DESC LIMIT 1",
        )
        .bind(path)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// All sessions, newest first, joined with ticket + worktree info.
    pub async fn list_sessions(&self) -> Result<Vec<SessionView>> {
        Ok(sqlx::query_as::<_, SessionView>(
            "SELECT s.id, s.ticket_id, w.id AS worktree_id, t.title AS ticket_title, t.jira_key,
                    w.branch, s.state, s.last_tool, s.claude_session_id, s.started_at, s.ended_at
             FROM sessions s
             JOIN tickets t ON t.id = s.ticket_id
             JOIN worktrees w ON w.id = s.worktree_id
             ORDER BY s.id DESC",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Most recent Claude session id for a ticket, for `--resume`.
    pub async fn latest_claude_session_id_for_ticket(&self, ticket_id: i64) -> Result<Option<String>> {
        Ok(sqlx::query_scalar::<_, String>(
            "SELECT claude_session_id FROM sessions
             WHERE ticket_id = ? AND claude_session_id IS NOT NULL
             ORDER BY id DESC LIMIT 1",
        )
        .bind(ticket_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    // ---- Jira sync / spec ------------------------------------------------

    pub async fn get_ticket_by_key(&self, key: &str) -> Result<Option<Ticket>> {
        Ok(sqlx::query_as::<_, Ticket>(
            "SELECT id, jira_key, source, title, spec, status, repo_id, created_at, updated_at, todos
             FROM tickets WHERE jira_key = ?",
        )
        .bind(key)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Insert or update a ticket from a Jira issue. Returns (id, inserted). On update we
    /// refresh the title only — never the harmony status or the locally-authored spec.
    pub async fn upsert_jira_ticket(&self, key: &str, title: &str) -> Result<(i64, bool)> {
        if let Some(t) = self.get_ticket_by_key(key).await? {
            sqlx::query("UPDATE tickets SET title = ?, updated_at = ? WHERE id = ?")
                .bind(title)
                .bind(now_unix())
                .bind(t.id)
                .execute(&self.pool)
                .await?;
            Ok((t.id, false))
        } else {
            let id = self.add_ticket(Some(key), "jira", title, "", None).await?;
            Ok((id, true))
        }
    }

    /// Replace the ticket's Claude task list (JSON array of {content, status}).
    pub async fn set_ticket_todos(&self, id: i64, todos_json: &str) -> Result<()> {
        sqlx::query("UPDATE tickets SET todos = ? WHERE id = ?")
            .bind(todos_json)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Set the agent spec (does not change the column).
    pub async fn set_ticket_spec(&self, id: i64, spec: &str) -> Result<()> {
        sqlx::query("UPDATE tickets SET spec = ?, updated_at = ? WHERE id = ?")
            .bind(spec)
            .bind(now_unix())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ---- settings kv -----------------------------------------------------

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO settings (key, value) VALUES (?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }
}

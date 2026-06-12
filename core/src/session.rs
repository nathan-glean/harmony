//! PTY-based Claude session manager (DESIGN Q4/Q9/Q12).
//!
//! Starts (or resumes) an interactive `claude` process inside a ticket's worktree,
//! after injecting the hook settings. Returns a handle exposing the PTY master so a
//! caller (the CLI today, the Tauri UI later) can bridge/attach a terminal. Session
//! end is detected by the child process exiting (Phase 0: SessionEnd hook unreliable).

use std::sync::Arc;

use anyhow::{anyhow, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::models::Ticket;
use crate::store::Store;
use crate::worktree;

pub struct SessionHandle {
    pub session_id: i64,
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

pub struct SessionManager {
    store: Arc<Store>,
    hook_port: u16,
}

impl SessionManager {
    pub fn new(store: Arc<Store>, hook_port: u16) -> Self {
        Self { store, hook_port }
    }

    /// Create/reuse the ticket's worktree, inject hooks, then spawn (or resume) Claude.
    pub async fn start(&self, ticket_id: i64) -> Result<SessionHandle> {
        let ticket = self
            .store
            .get_ticket(ticket_id)
            .await?
            .ok_or_else(|| anyhow!("no such ticket #{ticket_id}"))?;
        let repo_id = ticket
            .repo_id
            .ok_or_else(|| anyhow!("ticket #{ticket_id} has no repo assigned"))?;
        let repo = self
            .store
            .get_repo(repo_id)
            .await?
            .ok_or_else(|| anyhow!("repo #{repo_id} missing"))?;

        let wt = match self.store.primary_worktree_for_ticket(ticket_id).await? {
            Some(w) => w,
            None => {
                let branch = worktree::branch_name(&ticket);
                let dest = worktree::worktree_path(&repo.name, &branch);
                let base = worktree::default_branch(&repo.path)?;
                worktree::create(&repo.path, &base, &branch, &dest)?;
                let canon = std::fs::canonicalize(&dest)
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|_| dest.to_string_lossy().to_string());
                let id = self
                    .store
                    .add_worktree(ticket_id, repo_id, &branch, &canon, false)
                    .await?;
                self.store
                    .get_worktree(id)
                    .await?
                    .ok_or_else(|| anyhow!("worktree insert failed"))?
            }
        };

        crate::settings::inject_hooks(&wt.path, self.hook_port)?;

        let resume = self
            .store
            .latest_claude_session_id_for_ticket(ticket_id)
            .await?;
        let prompt = render_prompt(&ticket);
        let (master, child) = spawn_claude(&wt.path, &prompt, resume.as_deref())?;

        let session_id = self.store.add_session(ticket_id, wt.id).await?;
        self.store
            .set_ticket_status(ticket_id, crate::status::WORKING)
            .await?;

        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    pub async fn end_session(&self, session_id: i64) -> Result<()> {
        self.store.end_session(session_id).await
    }
}

/// Render the ticket spec into Claude's opening prompt (DESIGN Q10).
fn render_prompt(t: &Ticket) -> String {
    if t.spec.trim().is_empty() {
        format!("Work on this task: {}", t.title)
    } else {
        format!("# {}\n\n{}", t.title, t.spec)
    }
}

fn spawn_claude(
    cwd: &str,
    prompt: &str,
    resume: Option<&str>,
) -> Result<(Box<dyn MasterPty + Send>, Box<dyn Child + Send + Sync>)> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("claude");
    cmd.cwd(cwd);
    cmd.arg("--permission-mode");
    cmd.arg("default"); // supervised; autonomy mode would elevate this
    if let Some(id) = resume {
        cmd.arg("--resume");
        cmd.arg(id);
    }
    cmd.arg(prompt);

    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    Ok((pair.master, child))
}

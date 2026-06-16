//! PTY-based Claude session manager (DESIGN Q4/Q9/Q12).
//!
//! Starts (or resumes) an interactive `claude` process inside a ticket's worktree,
//! after injecting the hook settings. Returns a handle exposing the PTY master so a
//! caller (the CLI today, the Tauri UI later) can bridge/attach a terminal. Session
//! end is detected by the child process exiting (Phase 0: SessionEnd hook unreliable).

use std::sync::Arc;

use anyhow::{anyhow, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde_json::Value;

use crate::models::{Repo, Ticket, Worktree};
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

    /// The ticket's primary worktree, creating it (off the repo's default branch) if absent.
    /// If recording the new worktree in the DB fails, the on-disk worktree is rolled back so we
    /// never leave a half-created worktree (a directory with no row that breaks the next
    /// `git worktree add`).
    async fn ensure_primary_worktree(
        &self,
        ticket: &Ticket,
        repo_id: i64,
        repo: &Repo,
    ) -> Result<Worktree> {
        if let Some(w) = self.store.primary_worktree_for_ticket(ticket.id).await? {
            return Ok(w);
        }
        let branch = worktree::branch_name(ticket);
        let dest = worktree::worktree_path(&repo.name, &branch);
        let base = worktree::default_branch(&repo.path)?;
        worktree::create(&repo.path, &base, &branch, &dest)?;
        let canon = std::fs::canonicalize(&dest)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| dest.to_string_lossy().to_string());
        let recorded = async {
            let id = self
                .store
                .add_worktree(ticket.id, repo_id, &branch, &canon, false)
                .await?;
            self.store
                .get_worktree(id)
                .await?
                .ok_or_else(|| anyhow!("worktree insert failed"))
        }
        .await;
        match recorded {
            Ok(w) => Ok(w),
            Err(e) => {
                let _ = worktree::remove(&repo.path, &dest);
                Err(e)
            }
        }
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

        let wt = self.ensure_primary_worktree(&ticket, repo_id, &repo).await?;

        crate::settings::inject_hooks(&wt.path, self.hook_port)?;

        // The very first work session for a ticket runs in plan mode to build the task list
        // from the spec, and ALWAYS starts fresh — never `--resume`. Resuming here would
        // continue an earlier conversation (in particular the grill/spec interview, which
        // shares this worktree's cwd) instead of implementing the spec. Only after that
        // one-time plan run (persisted via `planned`) do later sessions resume the work
        // conversation to continue where they left off.
        let do_plan = ticket.planned == 0;
        let resume = if do_plan {
            None
        } else {
            self.store.latest_claude_session_id_for_ticket(ticket_id).await?
        };
        // Configured permission mode (DESIGN Q1: autonomy). Defaults to `auto`; the initial
        // plan run forces `plan` regardless.
        let configured_mode = self
            .store
            .get_setting("permission_mode")
            .await?
            .unwrap_or_else(|| "auto".to_string());
        // Fresh start sends the full spec; a resume restores the conversation via
        // `--resume` and only nudges Claude to continue (don't re-paste the spec).
        let (prompt, mode) = if do_plan {
            (render_plan_prompt(&ticket), "plan".to_string())
        } else if resume.is_some() {
            ("Continue where you left off.".to_string(), configured_mode)
        } else {
            (render_prompt(&ticket), configured_mode)
        };
        let (master, child) = spawn_claude(&wt.path, &prompt, resume.as_deref(), &mode)?;
        if do_plan {
            self.store.mark_ticket_planned(ticket_id).await?;
        }

        let session_id = self
            .store
            .add_session(ticket_id, wt.id, &wt.path, "work")
            .await?;
        self.store
            .set_ticket_status(ticket_id, crate::status::WORKING)
            .await?;

        Ok(SessionHandle {
            session_id,
            master,
            child,
        })
    }

    /// Start a worktree-less "spec" session: runs an interactive grill interview in plan mode
    /// to produce the ticket's spec, before any work begins. No worktree/branch is created; the
    /// ticket is flagged `drafting` until the spec is captured (on ExitPlanMode, by the hook
    /// server). Does not move the ticket off Todo. `seed` is optional opening context for the
    /// interview (e.g. a Jira ticket's description), woven into the grill prompt but never
    /// persisted — the captured spec comes from the grill.
    ///
    /// The grill runs in the ticket's **git worktree** (created/reused via
    /// `ensure_primary_worktree`), NOT the repo root. The worktree is a unique per-ticket
    /// directory, so its `cwd` can't be confused with another `claude` session in the same repo,
    /// and it inherits the repo's trust (an empty non-git scratch dir would hit Claude's
    /// interactive trust gate and never start). Plan mode keeps it read-only — it explores the
    /// checkout but makes no commits — and the later work session reuses the same worktree.
    pub async fn start_spec_session(&self, ticket_id: i64, seed: Option<String>) -> Result<SessionHandle> {
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

        let wt = self.ensure_primary_worktree(&ticket, repo_id, &repo).await?;
        crate::settings::inject_hooks(&wt.path, self.hook_port)?;
        // Self-heal: an earlier version injected harmony's hooks into the repo root — remove
        // them so the user's own `claude` sessions in that repo stop reporting to harmony.
        let _ = crate::settings::remove_hooks(&repo.path, self.hook_port);

        let prompt = render_grill_prompt(&ticket, seed.as_deref());
        // Plan mode keeps the grill read-only — safe to run in the ticket's worktree.
        let (master, child) = spawn_claude(&wt.path, &prompt, None, "plan")?;

        // worktree_id = 0: the spec session stays worktree-less in the DB (kept out of the
        // Sessions view); correlation is by cwd, which is now the unique worktree path.
        let session_id = self.store.add_session(ticket_id, 0, &wt.path, "spec").await?;
        self.store.set_ticket_drafting(ticket_id, true).await?;

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

/// Render the ticket spec (body + structured fields) into Claude's opening prompt (DESIGN Q10).
fn render_prompt(t: &Ticket) -> String {
    let composed = crate::spec::compose_spec(t);
    if composed.trim().is_empty() {
        format!("Work on this task: {}", t.title)
    } else {
        format!("# {}\n\n{}", t.title, composed)
    }
}

/// Opening prompt for the one-time initial plan run (phase 2, at first In Progress start).
/// The ticket spec is the agreed plan (produced earlier by the grill, phase 1); this run's
/// job is to decompose it into the concrete, low-level tasks the agent will then execute,
/// recorded via TodoWrite (mirrored onto the ticket). Launched with `--permission-mode plan`.
fn render_plan_prompt(t: &Ticket) -> String {
    format!(
        "{}\n\n---\nThe specification above is the agreed plan for this ticket. You are in \
         plan mode. Break it down into a concrete, ordered list of low-level implementation \
         tasks that you will execute, and record that breakdown with the TodoWrite tool \
         (this saves it to the ticket). Explore the codebase as needed to make the tasks \
         specific. Don't re-litigate the approach, and make no code changes until the plan \
         is approved.",
        render_prompt(t)
    )
}

/// Opening prompt for a spec/grill session (phase 1, at ticket creation). Inlines the
/// `grill-me` interview (the skill isn't installed in target repos) and ends by asking
/// Claude to write the finished spec as its plan and present it via ExitPlanMode — which the
/// hook server captures onto the ticket. Launched with `--permission-mode plan` (read-only).
fn render_grill_prompt(t: &Ticket, seed: Option<&str>) -> String {
    // Opening context = the ticket's existing spec/fields plus any transient seed (e.g. a Jira
    // description), whichever are present.
    let mut idea = crate::spec::compose_spec(t);
    if let Some(s) = seed {
        let s = s.trim();
        if !s.is_empty() {
            if !idea.trim().is_empty() {
                idea.push_str("\n\n");
            }
            idea.push_str(s);
        }
    }
    let seed = if idea.trim().is_empty() {
        format!("We're scoping a new task: {}", t.title)
    } else {
        format!("We're scoping a new task — \"{}\".\n\nInitial idea / context:\n{}", t.title, idea)
    };
    format!(
        "{seed}\n\n\
         Interview me relentlessly about every aspect of this task until we reach a shared \
         understanding. Walk down each branch of the design tree, resolving dependencies \
         between decisions one-by-one. For each question, provide your recommended answer. \
         Ask the questions one at a time. If a question can be answered by exploring the \
         codebase, explore the codebase instead of asking.\n\n\
         When we've reached a shared understanding, write the complete specification for \
         this task as your plan and call ExitPlanMode to present it. Structure the spec as a \
         short body (Goal, Context) followed by these exact markdown sections so it can be \
         parsed into fields: `## Acceptance criteria`, `## Relevant paths`, `## Constraints`. \
         Do not write any code or make changes — this session exists only to produce the spec."
    )
}

/// Render a Claude Code session transcript (JSONL) into a readable plain-text
/// conversation for the "Conversation so far" pane. Best-effort / approximate — the TUI
/// uses the alternate screen, so we can't faithfully rebuild xterm scrollback; this gives
/// the prior conversation instead.
pub fn render_transcript(path: &str) -> Result<String> {
    let content = std::fs::read_to_string(path)?;
    let mut out = String::new();
    for line in content.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let typ = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let msg = v.get("message");
        let role = msg
            .and_then(|m| m.get("role"))
            .and_then(|x| x.as_str())
            .unwrap_or(typ);
        let content_node = msg.and_then(|m| m.get("content")).or_else(|| v.get("content"));
        let text = extract_blocks(content_node);
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        match role {
            "user" => {
                out.push_str("❯ ");
                out.push_str(text);
                out.push_str("\n\n");
            }
            "assistant" => {
                out.push_str(text);
                out.push_str("\n\n");
            }
            _ => {}
        }
    }
    Ok(out.trim_end().to_string())
}

/// A snapshot of in-session progress tailed from the live transcript: the latest assistant
/// text and the most recently invoked tool. Richer than the hook-derived working/waiting flag.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TranscriptProgress {
    /// Latest assistant text block (newlines collapsed, capped), if any.
    pub message: Option<String>,
    /// Name of the most recent `tool_use`, if any.
    pub tool: Option<String>,
}

/// Tail a session's JSONL transcript and extract the latest in-session progress without
/// reading the whole file: we seek to the last `TAIL` bytes, drop the (likely partial) first
/// line, then walk the complete assistant lines tracking the most recent text + tool_use.
pub fn latest_progress(path: &str) -> Option<TranscriptProgress> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL: u64 = 64 * 1024;
    const MAX_MSG: usize = 280;

    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    // The seek may land mid-character; lossily decode and discard a partial leading line.
    let buf = String::from_utf8_lossy(&bytes);
    let body = if start > 0 {
        match buf.find('\n') {
            Some(i) => &buf[i + 1..],
            None => "",
        }
    } else {
        &buf[..]
    };

    let mut progress = TranscriptProgress::default();
    for line in body.lines() {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let msg = v.get("message");
        let role = msg.and_then(|m| m.get("role")).and_then(|x| x.as_str()).unwrap_or("");
        if role != "assistant" {
            continue;
        }
        if let Some(Value::Array(arr)) = msg.and_then(|m| m.get("content")) {
            for b in arr {
                match b.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "text" => {
                        if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                            let t = t.trim();
                            if !t.is_empty() {
                                progress.message = Some(collapse(t, MAX_MSG));
                            }
                        }
                    }
                    "tool_use" => {
                        if let Some(n) = b.get("name").and_then(|x| x.as_str()) {
                            progress.tool = Some(n.to_string());
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if progress.message.is_none() && progress.tool.is_none() {
        None
    } else {
        Some(progress)
    }
}

/// Collapse whitespace runs (incl. newlines) into single spaces and cap the length, so a
/// progress line stays a single tidy line in the UI.
fn collapse(s: &str, max: usize) -> String {
    let one_line: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > max {
        let truncated: String = one_line.chars().take(max).collect();
        format!("{truncated}…")
    } else {
        one_line
    }
}

fn extract_blocks(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(arr)) => {
            let mut s = String::new();
            for b in arr {
                match b.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "text" => {
                        if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                            s.push_str(t);
                            s.push('\n');
                        }
                    }
                    "tool_use" => {
                        let name = b.get("name").and_then(|x| x.as_str()).unwrap_or("tool");
                        s.push_str(&format!("⏺ {name}\n"));
                    }
                    _ => {}
                }
            }
            s
        }
        _ => String::new(),
    }
}

fn spawn_claude(
    cwd: &str,
    prompt: &str,
    resume: Option<&str>,
    permission_mode: &str,
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
    cmd.arg(permission_mode);
    if let Some(id) = resume {
        cmd.arg("--resume");
        cmd.arg(id);
    }
    cmd.arg(prompt);

    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    Ok((pair.master, child))
}

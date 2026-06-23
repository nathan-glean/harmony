//! harmony desktop backend (Tauri 2).
//!
//! Wires `harmony-core` to the React UI: commands for board/ticket/Jira/PR actions, and
//! a PTY↔event bridge for live terminals — the GUI counterpart of the CLI's stdio bridge.
//! Each session's PTY output is streamed to the frontend via `term-output` events;
//! keystrokes come back via the `send_input` command. State is detected out-of-band by
//! the same hook server the core runs (started in `setup`).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

use harmony_core::flow::{self, Action, Column, Ctx, Event};
use harmony_core::models::{DiffComment, Repo, SessionView, Ticket, WorktreeView};
use harmony_core::session::SessionManager;
use harmony_core::store::Store;
use portable_pty::{ChildKiller, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

const HOOK_PORT: u16 = 8787;
/// How often the background poller checks PR-stage tickets' CI.
const CI_POLL_SECS: u64 = 60;
/// Max automatic CI-fix attempts per PR before we stop and leave it for a human (anti-loop).
const MAX_CI_FIX_ATTEMPTS: i64 = 3;

/// Live PTY handles for an active session.
struct SessionIo {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    ticket_id: i64,
}

struct AppState {
    store: Store,
    sessions: Arc<Mutex<HashMap<i64, SessionIo>>>,
    /// Tickets that were live at the last shutdown — drained once by the UI to reattach.
    reattach: Mutex<Vec<i64>>,
    /// Session ids the user deliberately stopped, so their non-zero exit isn't mistaken for a
    /// crash. The wait-task removes the id once it has handled the exit.
    stopping: Arc<Mutex<HashSet<i64>>>,
}

#[derive(Clone, Serialize)]
struct TermOutput {
    session_id: i64,
    data: String,
}

/// Emitted when a session's Claude process exits. `ok` is false for an abnormal exit (crash)
/// that wasn't a user-initiated stop, so the UI can flash a toast and show an error badge.
#[derive(Clone, Serialize)]
struct SessionExit {
    session_id: i64,
    ticket_id: i64,
    ok: bool,
    code: i32,
}

/// Emitted when a background PR-creation finishes. `ok` false means it failed and the ticket was
/// moved back to Human Review; `error` carries the reason for a toast.
#[derive(Clone, Serialize)]
struct PrDone {
    ticket_id: i64,
    ok: bool,
    error: Option<String>,
}

// ---- board / ticket commands --------------------------------------------

#[tauri::command]
async fn list_tickets(state: State<'_, AppState>) -> Result<Vec<Ticket>, String> {
    state.store.list_tickets().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_repos(state: State<'_, AppState>) -> Result<Vec<Repo>, String> {
    state.store.list_repos().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_repo(
    state: State<'_, AppState>,
    name: String,
    path: String,
    project: Option<String>,
) -> Result<i64, String> {
    if !std::path::Path::new(&path).is_dir() {
        return Err(format!("not a directory: {path}"));
    }
    let project = project.filter(|p| !p.trim().is_empty());
    state
        .store
        .add_repo(&name, &path, project.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_permission_mode(state: State<'_, AppState>) -> Result<String, String> {
    Ok(state
        .store
        .get_setting("permission_mode")
        .await
        .map_err(|e| e.to_string())?
        .unwrap_or_else(|| "auto".to_string()))
}

#[tauri::command]
async fn set_permission_mode(state: State<'_, AppState>, mode: String) -> Result<(), String> {
    state.store.set_setting("permission_mode", &mode).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn rename_repo(state: State<'_, AppState>, id: i64, name: String) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("name cannot be empty".into());
    }
    state.store.rename_repo(id, name.trim()).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_repo(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.delete_repo(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_ticket(state: State<'_, AppState>, id: i64) -> Result<Option<Ticket>, String> {
    state.store.get_ticket(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_sessions(state: State<'_, AppState>) -> Result<Vec<SessionView>, String> {
    state.store.list_sessions().await.map_err(|e| e.to_string())
}

/// Tickets that were live at the last shutdown (drained — returns them once).
#[tauri::command]
fn pending_reattach(state: State<'_, AppState>) -> Vec<i64> {
    std::mem::take(&mut *state.reattach.lock().unwrap())
}

/// Prior conversation (rendered from the JSONL transcript) for a ticket's latest session.
#[tauri::command]
async fn session_transcript(state: State<'_, AppState>, ticket_id: i64) -> Result<String, String> {
    match state
        .store
        .latest_transcript_path_for_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
    {
        Some(path) => tokio::task::spawn_blocking(move || harmony_core::session::render_transcript(&path))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string()),
        None => Ok(String::new()),
    }
}

/// (ticket_id, session_id) for sessions actually live in THIS process — the source of
/// truth for "live" (survives webview reloads; excludes zombies from a prior process).
#[tauri::command]
fn live_sessions(state: State<'_, AppState>) -> Vec<(i64, i64)> {
    let map = state.sessions.lock().unwrap();
    map.iter().map(|(sid, io)| (io.ticket_id, *sid)).collect()
}

#[derive(Clone, Serialize)]
struct SessionProgress {
    ticket_id: i64,
    session_id: i64,
    message: Option<String>,
    tool: Option<String>,
}

/// Live "latest progress" for every session live in THIS process, tailed from each one's
/// JSONL transcript (last assistant message + current tool). Richer than the hook-derived
/// working/waiting flag; polled alongside the board to drive the card/detail progress line.
#[tauri::command]
async fn live_progress(state: State<'_, AppState>) -> Result<Vec<SessionProgress>, String> {
    // Snapshot the live pairs and release the lock before any awaits.
    let live: Vec<(i64, i64)> = {
        let map = state.sessions.lock().unwrap();
        map.iter().map(|(sid, io)| (io.ticket_id, *sid)).collect()
    };
    let mut out = Vec::new();
    for (ticket_id, session_id) in live {
        let path = state
            .store
            .latest_transcript_path_for_ticket(ticket_id)
            .await
            .ok()
            .flatten();
        let Some(path) = path else { continue };
        let prog = tokio::task::spawn_blocking(move || harmony_core::session::latest_progress(&path))
            .await
            .ok()
            .flatten();
        if let Some(p) = prog {
            out.push(SessionProgress {
                ticket_id,
                session_id,
                message: p.message,
                tool: p.tool,
            });
        }
    }
    Ok(out)
}

#[tauri::command]
async fn clear_ended_sessions(state: State<'_, AppState>) -> Result<u64, String> {
    state.store.delete_ended_sessions().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_worktrees(state: State<'_, AppState>) -> Result<Vec<WorktreeView>, String> {
    state.store.list_worktrees().await.map_err(|e| e.to_string())
}

/// Total uncommitted changes across all of a ticket's worktrees (so the UI can warn before a
/// destructive move-to-Done / delete). 0 means clean.
async fn ticket_uncommitted(state: &AppState, ticket_id: i64) -> usize {
    let worktrees = state.store.worktrees_for_ticket(ticket_id).await.unwrap_or_default();
    worktrees
        .iter()
        .map(|wt| harmony_core::worktree::uncommitted_count(&wt.path).unwrap_or(0))
        .sum()
}

/// Whether a single worktree has uncommitted changes — the UI calls this before deleting it so
/// it can confirm before discarding work.
#[tauri::command]
async fn worktree_dirty(state: State<'_, AppState>, id: i64) -> Result<bool, String> {
    let wt = state
        .store
        .get_worktree(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such worktree")?;
    Ok(harmony_core::worktree::is_dirty(&wt.path).unwrap_or(false))
}

/// Remove ALL of a ticket's worktrees from disk + DB (keeps the ticket). Used when a
/// ticket is moved to Done. The pushed branch/PR is unaffected. Unless `force`, refuses if any
/// worktree has uncommitted changes (returns a `DIRTY:` error the UI confirms before retrying).
#[tauri::command]
async fn cleanup_ticket_worktrees(
    state: State<'_, AppState>,
    ticket_id: i64,
    force: bool,
) -> Result<(), String> {
    cleanup_worktrees(&state, ticket_id, force).await
}

/// Remove a ticket's worktrees from disk + DB (the executor's `DeleteWorktree` and the
/// `cleanup_ticket_worktrees` command both call this). Refuses on dirty unless `force`.
async fn cleanup_worktrees(state: &AppState, ticket_id: i64, force: bool) -> Result<(), String> {
    if !force {
        let n = ticket_uncommitted(state, ticket_id).await;
        if n > 0 {
            return Err(format!("DIRTY: {n} uncommitted change(s) would be discarded"));
        }
    }
    harmony_core::worktree::cleanup_for_ticket(&state.store, ticket_id).await;
    let worktrees = state
        .store
        .worktrees_for_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?;
    for wt in worktrees {
        state.store.delete_worktree(wt.id).await.map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Remove a worktree from disk then delete its DB row + sessions. Unless `force`, refuses a
/// worktree with uncommitted changes (returns a `DIRTY:` error the UI confirms before retrying)
/// so we never silently `--force`-discard the user's work.
#[tauri::command]
async fn delete_worktree(state: State<'_, AppState>, id: i64, force: bool) -> Result<(), String> {
    if let Ok(Some(wt)) = state.store.get_worktree(id).await {
        if !force {
            let n = harmony_core::worktree::uncommitted_count(&wt.path).unwrap_or(0);
            if n > 0 {
                return Err(format!("DIRTY: {n} uncommitted change(s) would be discarded"));
            }
        }
        if let Ok(Some(repo)) = state.store.get_repo(wt.repo_id).await {
            let _ = harmony_core::worktree::remove(&repo.path, std::path::Path::new(&wt.path));
        }
    }
    state.store.delete_worktree(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_session(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.delete_session(id).await.map_err(|e| e.to_string())
}

/// Delete the ended sessions for a worktree (clears a consolidated group).
#[tauri::command]
async fn delete_worktree_sessions(state: State<'_, AppState>, worktree_id: i64) -> Result<u64, String> {
    state
        .store
        .delete_ended_sessions_for_worktree(worktree_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_local_ticket(
    state: State<'_, AppState>,
    title: String,
    spec: String,
    repo: Option<String>,
) -> Result<i64, String> {
    let repo_id = match repo {
        Some(n) => Some(
            state
                .store
                .get_repo_by_name(&n)
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no repo named {n}"))?
                .id,
        ),
        None => None,
    };
    state
        .store
        .add_ticket(None, "local", &title, &spec, repo_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_spec(state: State<'_, AppState>, id: i64, spec: String) -> Result<(), String> {
    state.store.set_ticket_spec(id, &spec).await.map_err(|e| e.to_string())
}

/// Save the spec body and all three first-class fields together (the detail editor's Save).
#[tauri::command]
async fn set_spec_fields(
    state: State<'_, AppState>,
    id: i64,
    spec: String,
    acceptance_criteria: String,
    relevant_paths: String,
    constraints: String,
) -> Result<(), String> {
    state
        .store
        .set_ticket_spec_fields(id, &spec, &acceptance_criteria, &relevant_paths, &constraints)
        .await
        .map_err(|e| e.to_string())
}

/// Move a ticket to a column (drag-and-drop / manual override).
#[tauri::command]
async fn set_ticket_status(state: State<'_, AppState>, id: i64, status: String) -> Result<(), String> {
    if !harmony_core::status::is_valid(&status) {
        return Err(format!("invalid status: {status}"));
    }
    state.store.set_ticket_status(id, &status).await.map_err(|e| e.to_string())
}

/// Reflect a board-column move onto the linked Jira issue (best-effort; only if a matching
/// status exists in the issue's workflow). No-op for non-Jira tickets and the
/// "For Your Review" (waiting) state, which has no Jira equivalent.
#[tauri::command]
async fn jira_apply_column(state: State<'_, AppState>, ticket_id: i64, status: String) -> Result<(), String> {
    let ticket = match state.store.get_ticket(ticket_id).await.map_err(|e| e.to_string())? {
        Some(t) => t,
        None => return Ok(()),
    };
    let key = match ticket.jira_key {
        Some(k) => k,
        None => return Ok(()),
    };
    let candidates: &[&str] = match status.as_str() {
        "todo" => &["To Do", "Backlog", "Open"],
        "working" => &["In Progress"],
        "in_review" => &["In Review", "Code Review", "Review"],
        "done" => &["Done", "Closed", "Resolved"],
        _ => return Ok(()),
    };
    let _ = harmony_core::jira::transition_to_any(&key, candidates).await;
    Ok(())
}

/// Delete a ticket: remove its git worktrees then its DB records. Unless `force`, refuses if
/// any worktree has uncommitted changes (returns a `DIRTY:` error the UI confirms first).
#[tauri::command]
async fn delete_ticket(state: State<'_, AppState>, ticket_id: i64, force: bool) -> Result<(), String> {
    if !force {
        let n = ticket_uncommitted(&state, ticket_id).await;
        if n > 0 {
            return Err(format!("DIRTY: {n} uncommitted change(s) would be discarded"));
        }
    }
    harmony_core::worktree::cleanup_for_ticket(&state.store, ticket_id).await;
    state.store.delete_ticket(ticket_id).await.map_err(|e| e.to_string())
}

// ---- jira ----------------------------------------------------------------

#[derive(Clone, Serialize)]
struct JiraEnv {
    acli_installed: bool,
    site: Option<String>,
}

/// Whether the Atlassian CLI is installed, and the connected site (if logged in).
#[tauri::command]
async fn jira_env() -> JiraEnv {
    let acli_installed = harmony_core::jira::cli_installed();
    let site = if acli_installed {
        harmony_core::jira::connected_site().await
    } else {
        None
    };
    JiraEnv { acli_installed, site }
}

/// Install acli via Homebrew (best-effort). Returns the installed version.
#[tauri::command]
async fn install_acli() -> Result<String, String> {
    tokio::task::spawn_blocking(harmony_core::jira::install_via_brew)
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn jira_logout() -> Result<(), String> {
    harmony_core::jira::logout().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn jira_sync(state: State<'_, AppState>) -> Result<usize, String> {
    let issues = harmony_core::jira::search_assigned().await.map_err(|e| e.to_string())?;
    for issue in &issues {
        state
            .store
            .upsert_jira_ticket(&issue.key, &issue.summary)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(issues.len())
}

#[derive(Serialize)]
struct JiraDetail {
    description: String,
    comments: Vec<harmony_core::jira::JiraComment>,
}

/// Open the ticket's linked Jira issue in the browser.
#[tauri::command]
async fn open_in_jira(state: State<'_, AppState>, ticket_id: i64) -> Result<(), String> {
    let ticket = state
        .store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    let key = ticket.jira_key.ok_or("ticket is not linked to Jira")?;
    harmony_core::jira::open_in_browser(&key).await.map_err(|e| e.to_string())
}

/// The linked Jira issue's description + comments (read-only) for the detail panel.
#[tauri::command]
async fn jira_detail(state: State<'_, AppState>, ticket_id: i64) -> Result<JiraDetail, String> {
    let ticket = state
        .store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    let key = ticket.jira_key.ok_or("ticket is not linked to Jira")?;
    let issue = harmony_core::jira::get_issue(&key).await.map_err(|e| e.to_string())?;
    let comments = harmony_core::jira::comments(&key).await.unwrap_or_default();
    Ok(JiraDetail { description: issue.description, comments })
}

// ---- pull request --------------------------------------------------------

#[tauri::command]
async fn open_pr(state: State<'_, AppState>, ticket_id: i64) -> Result<String, String> {
    let url = open_pr_for(&state.store, ticket_id).await?;
    state
        .store
        .set_ticket_status(ticket_id, harmony_core::status::IN_REVIEW)
        .await
        .map_err(|e| e.to_string())?;
    Ok(url)
}

/// Push the branch and open the draft PR (generated body, Jira writeback). Does NOT change the
/// ticket column — the caller (the `open_pr` command, or the flow executor) owns the status.
/// Takes `&Store` (not `&AppState`) so the executor can run it in a spawned background task.
async fn open_pr_for(store: &Store, ticket_id: i64) -> Result<String, String> {
    let ticket = store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    let wt = store
        .primary_worktree_for_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no worktree — start a session first")?;
    let repo = store
        .get_repo(wt.repo_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("repo missing")?;
    let composed = harmony_core::spec::compose_spec(&ticket);
    let fallback = if composed.trim().is_empty() {
        format!("Ticket: {}", ticket.title)
    } else {
        composed
    };
    // Jira issue link woven into the generated PR body (browse URL when the site is known).
    let ticket_ref: Option<String> = match ticket.jira_key.as_deref() {
        Some(key) => Some(match harmony_core::jira::connected_site().await {
            Some(site) => format!("https://{site}/browse/{key}"),
            None => key.to_string(),
        }),
        None => None,
    };
    let (title, path, branch, repo_path) =
        (ticket.title.clone(), wt.path.clone(), wt.branch.clone(), repo.path.clone());
    let commit_msg = commit_message(&ticket);

    let url = tokio::task::spawn_blocking(move || -> Result<String, String> {
        // Commit any leftover uncommitted work (e.g. edits made directly during human review) so
        // the branch isn't empty, then refuse clearly if there's still nothing to PR.
        harmony_core::github::commit_all(&path, &commit_msg).map_err(|e| e.to_string())?;
        let base = harmony_core::worktree::default_branch(&repo_path).unwrap_or_else(|_| "main".into());
        if harmony_core::github::commits_ahead(&path, &base) == 0 {
            return Err("no committed changes to open a PR for".into());
        }
        // Generated diff summary (conforms to the repo's PR template if present), else the spec.
        let body =
            harmony_core::github::generated_pr_body(&path, &repo_path, ticket_ref.as_deref(), &fallback);
        harmony_core::github::push_branch(&path, &branch).map_err(|e| e.to_string())?;
        harmony_core::github::create_draft_pr(&path, &title, &body, &branch).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    if let Some(key) = ticket.jira_key.as_deref() {
        let _ = harmony_core::jira::transition(key, "In Review").await;
        let _ = harmony_core::jira::add_comment(key, &format!("PR opened by harmony: {url}")).await;
    }
    Ok(url)
}

// ---- CI monitoring / auto-fix --------------------------------------------

/// True if a live session is attached for this ticket (don't triage / spawn fixes mid-session).
fn has_live_session(state: &AppState, ticket_id: i64) -> bool {
    state.sessions.lock().unwrap().values().any(|io| io.ticket_id == ticket_id)
}

/// Auto-fix kill-switch (default on — the user opted into auto-fix). `"off"` → suggest-only.
async fn ci_autofix_enabled(state: &AppState) -> bool {
    state
        .store
        .get_setting("ci_autofix")
        .await
        .ok()
        .flatten()
        .map(|v| v != "off")
        .unwrap_or(true)
}

/// One poll pass: triage CI for every PR-stage ticket (skipping those with a live session).
async fn poll_ci_once(app: &AppHandle, state: &AppState) {
    let tickets = match state.store.list_tickets().await {
        Ok(t) => t,
        Err(_) => return,
    };
    for ticket in tickets {
        if ticket.status != harmony_core::status::IN_REVIEW || has_live_session(state, ticket.id) {
            continue;
        }
        let _ = triage_and_maybe_fix(app, state, ticket.id, false).await;
    }
}

/// Triage a ticket's PR CI and, when actionable (or `manual`), spawn an autonomous fix session.
/// Persists the triage on the ticket for the UI. `manual` bypasses the idempotency fingerprint,
/// the kill-switch, and the attempt cap (an explicit user request).
async fn triage_and_maybe_fix(
    app: &AppHandle,
    state: &AppState,
    ticket_id: i64,
    manual: bool,
) -> Result<String, String> {
    let ticket = state
        .store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    let wt = state
        .store
        .primary_worktree_for_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no worktree")?;
    let repo = state.store.get_repo(wt.repo_id).await.map_err(|e| e.to_string())?.ok_or("repo missing")?;
    let (path, repo_path) = (wt.path.clone(), repo.path.clone());

    // Cheap pre-check (no LLM): current HEAD + whether any checks are failing.
    let pre = {
        let path = path.clone();
        tokio::task::spawn_blocking(move || {
            let head = harmony_core::github::head_sha(&path).unwrap_or_default();
            let failing = harmony_core::github::pr_checks_json(&path)
                .ok()
                .map(|j| harmony_core::ci::parse_failing_checks(&j))
                .unwrap_or_default();
            (head, failing)
        })
        .await
        .map_err(|e| e.to_string())?
    };
    let (head, failing) = pre;

    // No failing checks → green; reset CI state so a later failure triages afresh.
    if failing.is_empty() {
        if !ticket.ci_triage.is_empty() || ticket.ci_fix_attempts > 0 {
            let _ = state.store.reset_ci(ticket_id).await;
            let _ = app.emit("ticket-updated", ticket_id);
        }
        return Ok("no failing checks".into());
    }
    // Already triaged this exact commit (and not a manual request) → nothing new to do (avoids
    // re-running the LLM / re-spawning a fix for the same HEAD).
    if !manual && !head.is_empty() && head == ticket.ci_triaged_sha {
        return Ok("already triaged this commit".into());
    }

    // Full triage (gh + LLM attribution) off-thread.
    let triage = {
        let (path, repo_path) = (path.clone(), repo_path.clone());
        tokio::task::spawn_blocking(move || {
            let base = harmony_core::worktree::default_branch(&repo_path).unwrap_or_else(|_| "main".into());
            let diff = harmony_core::github::diff(&path, &base).unwrap_or_default();
            harmony_core::ci::triage(&path, &base, &diff)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?
    };

    if let Ok(json) = serde_json::to_string(&triage) {
        let _ = state.store.set_ticket_ci_triage(ticket_id, &head, &json).await;
    }
    let _ = app.emit("ticket-updated", ticket_id);

    let should_fix = if manual {
        true
    } else {
        triage.actionable && ci_autofix_enabled(state).await && ticket.ci_fix_attempts < MAX_CI_FIX_ATTEMPTS
    };
    if !should_fix {
        return Ok(triage.reason.clone());
    }

    let context = ci_fix_context(&triage);
    let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
    let handle = mgr.start_ci_fix(ticket_id, &context).await.map_err(|e| e.to_string())?;
    wire_session(app, state, handle, ticket_id)?;
    let _ = state.store.bump_ci_fix_attempts(ticket_id).await;
    Ok(format!("fixing: {}", triage.reason))
}

/// Build the opening-prompt context for a fix session from a triage result.
fn ci_fix_context(triage: &harmony_core::ci::CiTriage) -> String {
    let mut s = format!("Failing checks: {}\n", triage.failing_checks.join(", "));
    if let Some(v) = &triage.verdict {
        s.push_str(&format!("\nWhy it's attributed to this PR: {}\n", v.rationale));
        if !v.proposed_fix.trim().is_empty() {
            s.push_str(&format!("\nSuggested fix: {}\n", v.proposed_fix));
        }
    }
    s
}

/// `FixFinished`: commit + push the fix session's changes (re-triggers CI), keeping the ticket in
/// the PR column. The next poll tick re-triages the new HEAD (and stops at the attempt cap).
async fn on_ci_fix_finished(app: &AppHandle, state: &AppState, ticket_id: i64) {
    let mut pushed = false;
    if let Ok(Some(ticket)) = state.store.get_ticket(ticket_id).await {
        if let Ok(Some(wt)) = state.store.primary_worktree_for_ticket(ticket_id).await {
            let (path, branch, msg) = (wt.path.clone(), wt.branch.clone(), commit_message(&ticket));
            pushed = tokio::task::spawn_blocking(move || {
                if harmony_core::github::commit_all(&path, &msg).unwrap_or(false) {
                    let _ = harmony_core::github::push_branch(&path, &branch);
                    return harmony_core::github::pr_status(&path).exists;
                }
                false
            })
            .await
            .unwrap_or(false);
        }
    }
    let _ = state.store.set_ticket_status(ticket_id, harmony_core::status::IN_REVIEW).await;
    let _ = app.emit("ticket-updated", ticket_id);
    if pushed {
        maybe_update_pr_desc(app, state, ticket_id).await;
    }
}

/// Whether automatic PR-description updates are enabled (kill-switch; default on).
async fn pr_desc_autoupdate_enabled(state: &AppState) -> bool {
    state
        .store
        .get_setting("pr_desc_autoupdate")
        .await
        .ok()
        .flatten()
        .map(|v| v != "off")
        .unwrap_or(true)
}

/// Ask Claude whether review changes made the PR description stale and, if so, update it on GitHub.
/// Gated by the `pr_desc_autoupdate` kill-switch unless `force` (a manual request). No-op when
/// there's no PR. One extra `claude -p` per call — only invoked after a pushed change.
async fn update_pr_description(app: &AppHandle, state: &AppState, ticket_id: i64, force: bool) {
    if !force && !pr_desc_autoupdate_enabled(state).await {
        return;
    }
    let ticket = match state.store.get_ticket(ticket_id).await.ok().flatten() {
        Some(t) => t,
        None => return,
    };
    let wt = match state.store.primary_worktree_for_ticket(ticket_id).await.ok().flatten() {
        Some(w) => w,
        None => return,
    };
    let repo = match state.store.get_repo(wt.repo_id).await.ok().flatten() {
        Some(r) => r,
        None => return,
    };
    // Jira browse URL woven into the body (kept across updates), as in `open_pr_for`.
    let ticket_ref: Option<String> = match ticket.jira_key.as_deref() {
        Some(key) => Some(match harmony_core::jira::connected_site().await {
            Some(site) => format!("https://{site}/browse/{key}"),
            None => key.to_string(),
        }),
        None => None,
    };
    let (path, repo_path) = (wt.path.clone(), repo.path.clone());
    let updated = tokio::task::spawn_blocking(move || {
        let body = harmony_core::github::pr_body(&path)?;
        let base = harmony_core::worktree::default_branch(&repo_path).unwrap_or_else(|_| "main".into());
        let diff = harmony_core::github::diff(&path, &base).unwrap_or_default();
        match harmony_core::draft::maybe_update_pr_description(&path, &body, &diff, ticket_ref.as_deref()) {
            Ok(Some(new_body)) => {
                let _ = harmony_core::github::update_pr_body(&path, &new_body);
                Some(true)
            }
            _ => Some(false),
        }
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
    if updated {
        let _ = app.emit("ticket-updated", ticket_id);
    }
}

/// Convenience wrapper used by the session-finished handlers (auto, respects the kill-switch).
async fn maybe_update_pr_desc(app: &AppHandle, state: &AppState, ticket_id: i64) {
    update_pr_description(app, state, ticket_id, false).await;
}

/// `AddressFinished`: commit the feedback-addressing session's changes; push only when a PR
/// already exists (so we don't create a remote branch pre-PR). Restore the ticket's review column
/// (PR column if a PR exists, else Human Review).
async fn on_address_finished(app: &AppHandle, state: &AppState, ticket_id: i64) {
    let mut pr_exists = false;
    let mut pushed = false;
    if let Ok(Some(ticket)) = state.store.get_ticket(ticket_id).await {
        if let Ok(Some(wt)) = state.store.primary_worktree_for_ticket(ticket_id).await {
            let (path, branch, msg) = (wt.path.clone(), wt.branch.clone(), commit_message(&ticket));
            let res = tokio::task::spawn_blocking(move || {
                let committed = harmony_core::github::commit_all(&path, &msg).unwrap_or(false);
                let exists = harmony_core::github::pr_status(&path).exists;
                if committed && exists {
                    let _ = harmony_core::github::push_branch(&path, &branch);
                }
                (exists, committed && exists)
            })
            .await
            .unwrap_or((false, false));
            pr_exists = res.0;
            pushed = res.1;
        }
    }
    let status = if pr_exists { harmony_core::status::IN_REVIEW } else { harmony_core::status::WAITING };
    let _ = state.store.set_ticket_status(ticket_id, status).await;
    let _ = app.emit("ticket-updated", ticket_id);
    if pushed {
        maybe_update_pr_desc(app, state, ticket_id).await;
    }
}

/// Manual "Fix CI" button: triage now and fix regardless of the auto gates.
#[tauri::command]
async fn request_ci_fix(app: AppHandle, state: State<'_, AppState>, ticket_id: i64) -> Result<String, String> {
    triage_and_maybe_fix(&app, &state, ticket_id, true).await
}

#[tauri::command]
async fn get_ci_autofix(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(ci_autofix_enabled(&state).await)
}

#[tauri::command]
async fn get_pr_desc_autoupdate(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(pr_desc_autoupdate_enabled(&state).await)
}

#[tauri::command]
async fn set_pr_desc_autoupdate(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state
        .store
        .set_setting("pr_desc_autoupdate", if enabled { "on" } else { "off" })
        .await
        .map_err(|e| e.to_string())
}

/// Manual "Regenerate PR description" — runs the staleness check + update now, ignoring the toggle.
#[tauri::command]
async fn update_pr_description_now(app: AppHandle, state: State<'_, AppState>, ticket_id: i64) -> Result<(), String> {
    update_pr_description(&app, &state, ticket_id, true).await;
    Ok(())
}

#[tauri::command]
async fn set_ci_autofix(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state
        .store
        .set_setting("ci_autofix", if enabled { "on" } else { "off" })
        .await
        .map_err(|e| e.to_string())
}

// ---- diff / PR pane ------------------------------------------------------

#[derive(Serialize)]
struct PrStatus {
    pr: Option<serde_json::Value>,
    checks: Vec<serde_json::Value>,
}

/// Worktree diff against its base branch (committed + uncommitted).
#[tauri::command]
async fn ticket_diff(state: State<'_, AppState>, ticket_id: i64) -> Result<String, String> {
    let wt = state
        .store
        .primary_worktree_for_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no worktree — start a session first")?;
    let repo = state
        .store
        .get_repo(wt.repo_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("repo missing")?;
    let (path, repo_path) = (wt.path, repo.path);
    tokio::task::spawn_blocking(move || {
        let base = harmony_core::worktree::default_branch(&repo_path).unwrap_or_else(|_| "main".into());
        harmony_core::github::diff(&path, &base)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// PR metadata + check status for the ticket's branch (empty if no PR / gh not set up).
#[tauri::command]
async fn ticket_pr(state: State<'_, AppState>, ticket_id: i64) -> Result<PrStatus, String> {
    let wt = state
        .store
        .primary_worktree_for_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no worktree — start a session first")?;
    let path = wt.path;
    let status = tokio::task::spawn_blocking(move || {
        let pr = harmony_core::github::pr_view_json(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
        let checks = harmony_core::github::pr_checks_json(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
            .unwrap_or_default();
        PrStatus { pr, checks }
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(status)
}

/// GitHub PR comments (conversation thread, review summaries, inline diff comments) for the
/// ticket's branch PR. Empty when there's no PR / gh isn't set up.
#[tauri::command]
async fn ticket_pr_comments(
    state: State<'_, AppState>,
    ticket_id: i64,
) -> Result<Vec<harmony_core::github::PrComment>, String> {
    let wt = state
        .store
        .primary_worktree_for_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no worktree — start a session first")?;
    let path = wt.path;
    tokio::task::spawn_blocking(move || harmony_core::github::pr_comments(&path))
        .await
        .map_err(|e| e.to_string())
}

// ---- diff comments -------------------------------------------------------

/// All review comments left on a ticket's diff (open + sent + resolved).
#[tauri::command]
async fn list_diff_comments(
    state: State<'_, AppState>,
    ticket_id: i64,
) -> Result<Vec<DiffComment>, String> {
    state
        .store
        .list_diff_comments(ticket_id)
        .await
        .map_err(|e| e.to_string())
}

/// Leave a new comment on a diff line; returns its id. `side` is "new" or "old".
#[tauri::command]
async fn add_diff_comment(
    state: State<'_, AppState>,
    ticket_id: i64,
    target: String,
    anchor: String,
    file_path: String,
    line: i64,
    end_line: i64,
    side: String,
    body: String,
) -> Result<i64, String> {
    state
        .store
        .add_diff_comment(ticket_id, &target, &anchor, &file_path, line, end_line, &side, &body)
        .await
        .map_err(|e| e.to_string())
}

/// Send all open review comments (any surface) to Claude: spawn an autonomous "address" session
/// that folds them into its prompt, addresses them, and (on finish) commits + pushes.
#[tauri::command]
async fn address_feedback(app: AppHandle, state: State<'_, AppState>, ticket_id: i64) -> Result<i64, String> {
    let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
    let handle = mgr.start_address(ticket_id).await.map_err(|e| e.to_string())?;
    wire_session(&app, &state, handle, ticket_id)
}

/// Accept Claude's proposed spec update: parse it into the first-class fields, write it as the
/// live spec, and clear the proposal.
#[tauri::command]
async fn accept_proposed_spec(state: State<'_, AppState>, ticket_id: i64) -> Result<(), String> {
    let ticket = state
        .store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    if ticket.proposed_spec.trim().is_empty() {
        return Err("no proposed spec to accept".into());
    }
    let f = harmony_core::spec::parse_spec(&ticket.proposed_spec);
    state
        .store
        .set_ticket_spec_fields(ticket_id, &f.spec, &f.acceptance_criteria, &f.relevant_paths, &f.constraints)
        .await
        .map_err(|e| e.to_string())?;
    state.store.set_ticket_proposed_spec(ticket_id, "").await.map_err(|e| e.to_string())
}

/// Reject Claude's proposed spec update (discard it; the live spec is unchanged).
#[tauri::command]
async fn reject_proposed_spec(state: State<'_, AppState>, ticket_id: i64) -> Result<(), String> {
    state.store.set_ticket_proposed_spec(ticket_id, "").await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_diff_comment(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state
        .store
        .delete_diff_comment(id)
        .await
        .map_err(|e| e.to_string())
}

/// Mark a comment resolved (kept for history, no longer injected into Claude's prompt).
#[tauri::command]
async fn resolve_diff_comment(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state
        .store
        .set_diff_comment_status(id, "resolved")
        .await
        .map_err(|e| e.to_string())
}

// ---- sessions (PTY ↔ events) ---------------------------------------------

/// Ensure a ticket has a repo assigned before a session starts: use the explicit `repo`
/// name, else the default repo for the Jira project key, else error asking the user to pick.
async fn ensure_ticket_repo(
    state: &AppState,
    ticket_id: i64,
    repo: Option<String>,
) -> Result<(), String> {
    let ticket = state
        .store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    if ticket.repo_id.is_some() {
        return Ok(());
    }
    let repo_id = if let Some(n) = repo {
        state
            .store
            .get_repo_by_name(&n)
            .await
            .map_err(|e| e.to_string())?
            .ok_or(format!("no repo named {n}"))?
            .id
    } else if let Some(k) = ticket.jira_key.as_deref().and_then(|k| k.split('-').next()) {
        state
            .store
            .default_repo_for_key(k)
            .await
            .map_err(|e| e.to_string())?
            .ok_or("ticket has no repo; choose one")?
            .id
    } else {
        return Err("ticket has no repo; choose one".into());
    };
    state.store.set_ticket_repo(ticket_id, repo_id).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn start_session(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
    repo: Option<String>,
) -> Result<i64, String> {
    ensure_ticket_repo(&state, ticket_id, repo).await?;
    let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
    let handle = mgr.start(ticket_id).await.map_err(|e| e.to_string())?;
    wire_session(&app, &state, handle, ticket_id)
}

/// Start a worktree-less grill/spec session that interviews the user (plan mode) to produce
/// the ticket's spec. Resolves the repo first (new-ticket flow already assigned one; a Jira
/// ticket grilled on entry to In Progress resolves via the default-repo mapping).
#[tauri::command]
async fn start_spec_session(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
    repo: Option<String>,
) -> Result<i64, String> {
    ensure_ticket_repo(&state, ticket_id, repo).await?;
    // Seed the grill with the Jira description for Jira-linked tickets (best-effort: a fetch
    // failure just starts the interview unseeded, never aborts it).
    let seed = match state.store.get_ticket(ticket_id).await.ok().flatten() {
        Some(t) => match t.jira_key.as_deref() {
            Some(key) => harmony_core::jira::get_issue(key)
                .await
                .ok()
                .map(|i| i.description)
                .filter(|d| !d.trim().is_empty()),
            None => None,
        },
        None => None,
    };
    let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
    let handle = mgr.start_spec_session(ticket_id, seed).await.map_err(|e| e.to_string())?;
    wire_session(&app, &state, handle, ticket_id)
}

/// Shared post-spawn wiring for both `start_session` and `start_spec_session`: bridge PTY
/// output to `term-output` events, register the session in the live map, and on child exit
/// end the session row, clear any draft flag, and notify the UI.
fn wire_session(
    app: &AppHandle,
    state: &AppState,
    handle: harmony_core::session::SessionHandle,
    ticket_id: i64,
) -> Result<i64, String> {
    let session_id = handle.session_id;
    let master = handle.master;
    let child = handle.child;
    let killer = child.clone_killer();

    let mut reader = master.try_clone_reader().map_err(|e| e.to_string())?;
    let writer = master.take_writer().map_err(|e| e.to_string())?;

    // PTY output -> term-output events
    {
        let app = app.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let data = String::from_utf8_lossy(&buf[..n]).to_string();
                        let _ = app.emit("term-output", TermOutput { session_id, data });
                    }
                }
            }
        });
    }

    state
        .sessions
        .lock()
        .unwrap()
        .insert(session_id, SessionIo { master, writer, killer, ticket_id });

    // Wait for exit -> mark ended (or failed), clear draft flag, drop handles, notify UI
    {
        let app = app.clone();
        let store = state.store.clone();
        let sessions = state.sessions.clone();
        let stopping = state.stopping.clone();
        tauri::async_runtime::spawn(async move {
            let wait = tokio::task::spawn_blocking(move || {
                let mut child = child;
                child.wait()
            })
            .await;
            // exit_code() is 0 on a clean exit; treat a join/wait error as abnormal (code -1).
            let (ok, code) = match wait {
                Ok(Ok(status)) => (status.success(), status.exit_code() as i32),
                _ => (false, -1),
            };
            // A user-initiated stop (kill) exits non-zero but isn't a crash.
            let user_stopped = stopping.lock().unwrap().remove(&session_id);
            if ok || user_stopped {
                let _ = store.end_session(session_id).await;
            } else {
                // Crash: end the session but mark it errored so the UI can surface it. Either
                // way the session is ended — never left orphaned as "working".
                let _ = store.fail_session(session_id).await;
            }
            // A grill stopped before producing a spec must not leave the ticket "Drafting".
            let _ = store.set_ticket_drafting(ticket_id, false).await;
            sessions.lock().unwrap().remove(&session_id);
            let _ = app.emit(
                "session-exit",
                SessionExit { session_id, ticket_id, ok: ok || user_stopped, code },
            );
        });
    }

    Ok(session_id)
}

#[tauri::command]
fn send_input(state: State<'_, AppState>, session_id: i64, data: String) -> Result<(), String> {
    let mut map = state.sessions.lock().unwrap();
    if let Some(io) = map.get_mut(&session_id) {
        io.writer.write_all(data.as_bytes()).map_err(|e| e.to_string())?;
        io.writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Relay an answer to a Claude `AskUserQuestion` prompt by driving the live TUI over the
/// PTY — so the user never has to type in the terminal. The recipe lives here in one place
/// so it can be tuned against the real TUI (see plan Step 0): the option list starts with
/// item 0 highlighted; Down arrow moves the cursor, Space toggles a multi-select item,
/// Enter confirms. A custom answer selects the auto-appended "Other" item (index ==
/// `option_count`) then types the text.
#[tauri::command]
fn answer_question(
    state: State<'_, AppState>,
    session_id: i64,
    option_count: usize,
    selected: Vec<usize>,
    custom_text: Option<String>,
    multi_select: bool,
) -> Result<(), String> {
    const DOWN: &str = "\x1b[B";
    const ENTER: &str = "\r";
    let mut keys = String::new();
    match custom_text {
        Some(text) if !text.is_empty() => {
            for _ in 0..option_count {
                keys.push_str(DOWN);
            }
            keys.push_str(ENTER);
            keys.push_str(&text);
            keys.push_str(ENTER);
        }
        _ if multi_select => {
            let mut sorted = selected;
            sorted.sort_unstable();
            sorted.dedup();
            let mut cur = 0usize;
            for idx in sorted {
                while cur < idx {
                    keys.push_str(DOWN);
                    cur += 1;
                }
                keys.push(' '); // toggle this item
            }
            keys.push_str(ENTER);
        }
        _ => {
            let idx = selected.first().copied().unwrap_or(0);
            for _ in 0..idx {
                keys.push_str(DOWN);
            }
            keys.push_str(ENTER);
        }
    }
    let mut map = state.sessions.lock().unwrap();
    if let Some(io) = map.get_mut(&session_id) {
        io.writer.write_all(keys.as_bytes()).map_err(|e| e.to_string())?;
        io.writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Kill a running session's Claude process. Its wait-task then fires `session-exit`,
/// which ends the DB row and removes it from the live map.
#[tauri::command]
fn stop_session(state: State<'_, AppState>, session_id: i64) -> Result<(), String> {
    let mut map = state.sessions.lock().unwrap();
    if let Some(io) = map.get_mut(&session_id) {
        // Record the intentional stop first so the wait-task ends it as 'done', not 'error'.
        state.stopping.lock().unwrap().insert(session_id);
        io.killer.kill().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn resize(state: State<'_, AppState>, session_id: i64, cols: u16, rows: u16) -> Result<(), String> {
    let map = state.sessions.lock().unwrap();
    if let Some(io) = map.get(&session_id) {
        io.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

// ---- flow executor -------------------------------------------------------
//
// The single runtime path that turns `flow::decide` decisions into effects. Kept inline in
// lib.rs (rather than a separate module) because it leans on this file's private session
// plumbing — `AppState`, `wire_session`, the `sessions`/`stopping` maps, `HOOK_PORT`. User
// drags call `transition_ticket`; system events (grill/work/review done) will call `apply_event`
// from the hook→app channel in Phase 2.

/// Gather the `flow::Ctx` facts for a ticket from the store + live sessions + git/gh.
async fn build_ctx(state: &AppState, ticket: &Ticket) -> Ctx {
    let session_live = {
        let map = state.sessions.lock().unwrap();
        map.values().any(|io| io.ticket_id == ticket.id)
    };
    let wt = state.store.primary_worktree_for_ticket(ticket.id).await.ok().flatten();
    let has_worktree = wt.is_some();

    // git/gh facts (blocking + network) — only meaningful once a worktree exists.
    let (has_changes, review_current, pr) = if let Some(wt) = wt.as_ref() {
        let path = wt.path.clone();
        let repo_path = state.store.get_repo(wt.repo_id).await.ok().flatten().map(|r| r.path);
        let reviewed = ticket.reviewed == 1;
        let reviewed_sha = ticket.reviewed_sha.clone();
        tokio::task::spawn_blocking(move || {
            let base = repo_path
                .as_deref()
                .and_then(|rp| harmony_core::worktree::default_branch(rp).ok())
                .unwrap_or_else(|| "main".into());
            let has_changes = harmony_core::github::diff(&path, &base)
                .map(|d| !d.trim().is_empty())
                .unwrap_or(false);
            let head = harmony_core::github::head_sha(&path).unwrap_or_default();
            let review_current = reviewed && !head.is_empty() && head == reviewed_sha;
            let pr = harmony_core::github::pr_status(&path);
            (has_changes, review_current, pr)
        })
        .await
        .unwrap_or((false, false, harmony_core::github::PrStatus::default()))
    } else {
        (false, false, harmony_core::github::PrStatus::default())
    };

    Ctx {
        has_repo: ticket.repo_id.is_some(),
        has_spec: ticket.grilled == 1,
        drafting: ticket.drafting == 1,
        planned: ticket.planned == 1,
        session_live,
        from: Column::from_status(&ticket.status).unwrap_or(Column::Todo),
        has_worktree,
        has_changes,
        review_current,
        reviewed: ticket.reviewed == 1,
        pr_exists: pr.exists,
        pr_approved: pr.approved,
        is_jira: ticket.jira_key.is_some(),
    }
}

/// Run one lifecycle `event` through `flow::decide` and execute the resulting actions, then
/// persist the target column. A blocked decision returns its reason as `Err` (no state change).
async fn apply_event(
    app: &AppHandle,
    state: &AppState,
    ticket_id: i64,
    event: Event,
    force: bool,
) -> Result<(), String> {
    let ticket = state
        .store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    let ctx = build_ctx(state, &ticket).await;
    let decision = flow::decide(event, &ctx);
    if let Some(reason) = decision.blocked {
        return Err(reason.to_string());
    }
    // Work just finished → commit the agent's changes (harmony owns version control). Doing it
    // here, before the move to Human Review / `/review`, means the review and the reviewed-SHA
    // fingerprint see committed state and the branch is PR-ready.
    if event == Event::WorkFinished {
        if let Ok(Some(wt)) = state.store.primary_worktree_for_ticket(ticket_id).await {
            let (path, msg) = (wt.path.clone(), commit_message(&ticket));
            let _ = tokio::task::spawn_blocking(move || harmony_core::github::commit_all(&path, &msg)).await;
        }
    }
    // `OpenPr` is slow (Claude-generated body + push + gh create) — run it in the background so
    // the card moves to the PR column immediately with a loading indicator. Every other action
    // runs synchronously here.
    let opening_pr = decision.actions.contains(&Action::OpenPr);
    for action in &decision.actions {
        if *action == Action::OpenPr {
            continue;
        }
        run_action(app, state, &ticket, *action, force).await?;
    }
    // After a review completes, fingerprint the reviewed HEAD so `/review` isn't re-run until
    // the branch changes again (`flow::Ctx.review_current`).
    if event == Event::ReviewFinished {
        if let Ok(Some(wt)) = state.store.primary_worktree_for_ticket(ticket_id).await {
            let path = wt.path.clone();
            if let Ok(Ok(sha)) =
                tokio::task::spawn_blocking(move || harmony_core::github::head_sha(&path)).await
            {
                let _ = state.store.mark_reviewed(ticket_id, &sha).await;
            }
        }
        // Capture the review prose (Claude's final assistant message) from the session
        // transcript onto the ticket so it shows in the Review tab. Latest-only — overwrites.
        if let Ok(Some(tp)) = state.store.latest_transcript_path_for_ticket(ticket_id).await {
            if let Ok(Some(text)) =
                tokio::task::spawn_blocking(move || harmony_core::session::last_assistant_message(&tp)).await
            {
                let _ = state.store.set_ticket_review_text(ticket_id, &text).await;
            }
        }
    }
    let target = decision.target.as_status();
    state.store.set_ticket_status(ticket_id, target).await.map_err(|e| e.to_string())?;
    apply_jira_column(state, ticket_id, target).await;
    let _ = app.emit("ticket-updated", ticket_id);

    // Background PR creation: the card is already in the PR column; signal a loading indicator,
    // create the PR off-thread, and revert to Human Review if it fails.
    if opening_pr {
        let _ = app.emit("pr-opening", ticket_id);
        let store = state.store.clone();
        let app = app.clone();
        tauri::async_runtime::spawn(async move {
            match open_pr_for(&store, ticket_id).await {
                Ok(_url) => {
                    let _ = app.emit("pr-done", PrDone { ticket_id, ok: true, error: None });
                }
                Err(e) => {
                    // Only revert if the user hasn't since moved the ticket elsewhere.
                    if let Ok(Some(t)) = store.get_ticket(ticket_id).await {
                        if t.status == harmony_core::status::IN_REVIEW {
                            let _ = store
                                .set_ticket_status(ticket_id, harmony_core::status::WAITING)
                                .await;
                        }
                    }
                    let _ = app.emit("pr-done", PrDone { ticket_id, ok: false, error: Some(e) });
                    let _ = app.emit("ticket-updated", ticket_id);
                }
            }
        });
    }
    Ok(())
}

/// Perform a single `flow::Action`'s side effect.
async fn run_action(
    app: &AppHandle,
    state: &AppState,
    ticket: &Ticket,
    action: Action,
    force: bool,
) -> Result<(), String> {
    let id = ticket.id;
    let mgr = || SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
    match action {
        Action::StartGrill => {
            let seed = jira_seed(ticket).await;
            let handle = mgr().start_spec_session(id, seed).await.map_err(|e| e.to_string())?;
            wire_session(app, state, handle, id)?;
        }
        // The paired `StartImplement` (via `SessionManager::start`) ensures the worktree; nothing
        // extra to do here — making this idempotent and explicit in the action list.
        Action::EnsureWorktree => {}
        Action::StartImplement | Action::ResumeWork => {
            // `start` itself chooses fresh plan-from-spec vs `--resume` from the `planned` flag.
            let handle = mgr().start(id).await.map_err(|e| e.to_string())?;
            wire_session(app, state, handle, id)?;
        }
        Action::StopSession => stop_ticket_sessions(state, id),
        Action::RunReview => {
            let handle = mgr().start_review(id).await.map_err(|e| e.to_string())?;
            wire_session(app, state, handle, id)?;
        }
        // `OpenPr` is handled asynchronously by `apply_event` (so the card moves immediately with
        // a loading indicator); it's intentionally a no-op here in the synchronous action loop.
        Action::OpenPr => {}
        Action::MergePr => {
            let wt = state
                .store
                .primary_worktree_for_ticket(id)
                .await
                .map_err(|e| e.to_string())?
                .ok_or("no worktree to merge")?;
            let path = wt.path.clone();
            tokio::task::spawn_blocking(move || harmony_core::github::merge_pr(&path))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        }
        Action::DeleteWorktree => {
            cleanup_worktrees(state, id, force).await?;
        }
    }
    Ok(())
}

/// Kill every live session for a ticket (records the intentional stop so the wait-task doesn't
/// flag it as a crash).
fn stop_ticket_sessions(state: &AppState, ticket_id: i64) {
    let mut map = state.sessions.lock().unwrap();
    let ids: Vec<i64> = map
        .iter()
        .filter(|(_, io)| io.ticket_id == ticket_id)
        .map(|(sid, _)| *sid)
        .collect();
    for sid in ids {
        state.stopping.lock().unwrap().insert(sid);
        if let Some(io) = map.get_mut(&sid) {
            let _ = io.killer.kill();
        }
    }
}

/// Commit message for harmony's auto-commit of a ticket's work: `"<KEY>: <title>"` (or just the
/// title for local tickets).
fn commit_message(ticket: &Ticket) -> String {
    match ticket.jira_key.as_deref() {
        Some(key) => format!("{key}: {}", ticket.title),
        None => ticket.title.clone(),
    }
}

/// Best-effort Jira description seed for a grill (same as `start_spec_session`).
async fn jira_seed(ticket: &Ticket) -> Option<String> {
    let key = ticket.jira_key.as_deref()?;
    harmony_core::jira::get_issue(key)
        .await
        .ok()
        .map(|i| i.description)
        .filter(|d| !d.trim().is_empty())
}

/// Mirror a column onto the linked Jira issue (best-effort), reused by `apply_event` and the
/// `jira_apply_column` command.
async fn apply_jira_column(state: &AppState, ticket_id: i64, status: &str) {
    let key = match state.store.get_ticket(ticket_id).await.ok().flatten().and_then(|t| t.jira_key) {
        Some(k) => k,
        None => return,
    };
    let candidates: &[&str] = match status {
        "todo" => &["To Do", "Backlog", "Open"],
        "working" => &["In Progress"],
        "in_review" => &["In Review", "Code Review", "Review"],
        "done" => &["Done", "Closed", "Resolved"],
        _ => return,
    };
    let _ = harmony_core::jira::transition_to_any(&key, candidates).await;
}

/// User dragged a ticket to `status` — run the `Move` event through the flow executor. On a
/// `DIRTY:` error the UI confirms then retries with `force=true`; a blocked move returns its reason.
#[tauri::command]
async fn transition_ticket(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
    status: String,
    force: bool,
) -> Result<(), String> {
    let to = Column::from_status(&status).ok_or_else(|| format!("invalid status: {status}"))?;
    // Best-effort: assign the default repo for a Jira ticket so the move isn't blocked when a
    // mapping exists. If none, the flow's repo gate returns a clear "assign a repo first".
    let _ = ensure_ticket_repo(&state, ticket_id, None).await;
    apply_event(&app, &state, ticket_id, Event::Move(to), force).await
}

/// The Todo "build spec / grill me" button.
#[tauri::command]
async fn grill_ticket(app: AppHandle, state: State<'_, AppState>, ticket_id: i64) -> Result<(), String> {
    let _ = ensure_ticket_repo(&state, ticket_id, None).await;
    apply_event(&app, &state, ticket_id, Event::GrillRequested, false).await
}

/// The Review tab's "Request review" button: re-run `/review` on demand, even if HEAD hasn't
/// changed since the last review.
#[tauri::command]
async fn request_review(app: AppHandle, state: State<'_, AppState>, ticket_id: i64) -> Result<(), String> {
    apply_event(&app, &state, ticket_id, Event::ReviewRequested, false).await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let handle = app.handle().clone();
            tauri::async_runtime::block_on(async move {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                let db = format!("{home}/.harmony/harmony.db");
                let store = Store::open(&db).await.expect("open store");
                // Capture which tickets were live before the (dead) sessions are reconciled
                // — the UI reattaches these on launch.
                let reattach = store.tickets_with_open_session().await.unwrap_or_default();
                // Sessions don't survive a process restart (PTYs are our children), so
                // any still-open session in the DB is a zombie — mark it ended.
                let _ = store.end_all_open_sessions().await;
                // Hook server → executor channel: the hook raises domain events (grill/work/
                // review done); the consumer task below runs them through the flow executor.
                let (tx, mut rx) =
                    tokio::sync::mpsc::unbounded_channel::<harmony_core::hooks::SystemEvent>();
                let _ =
                    harmony_core::hooks::spawn_server(Arc::new(store.clone()), HOOK_PORT, Some(tx)).await;
                handle.manage(AppState {
                    store,
                    sessions: Arc::new(Mutex::new(HashMap::new())),
                    reattach: Mutex::new(reattach),
                    stopping: Arc::new(Mutex::new(HashSet::new())),
                });
                // Consume system events and drive the flow executor (auto-advance).
                let ev_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    use harmony_core::hooks::SystemEvent;
                    while let Some(ev) = rx.recv().await {
                        let event = match ev {
                            SystemEvent::GrillFinished { ticket_id } => Some((ticket_id, Event::GrillFinished)),
                            SystemEvent::WorkFinished { ticket_id } => Some((ticket_id, Event::WorkFinished)),
                            SystemEvent::ReviewFinished { ticket_id } => Some((ticket_id, Event::ReviewFinished)),
                            // A CI-fix session finished: commit + push its changes (re-triggers CI),
                            // outside the flow state machine — the card stays in the PR column.
                            SystemEvent::FixFinished { ticket_id } => {
                                let state = ev_handle.state::<AppState>();
                                on_ci_fix_finished(&ev_handle, &state, ticket_id).await;
                                None
                            }
                            // A feedback-addressing session finished: commit (+ push if a PR
                            // exists) so the change is reflected; keep the ticket in its column.
                            SystemEvent::AddressFinished { ticket_id } => {
                                let state = ev_handle.state::<AppState>();
                                on_address_finished(&ev_handle, &state, ticket_id).await;
                                None
                            }
                        };
                        if let Some((ticket_id, event)) = event {
                            let state = ev_handle.state::<AppState>();
                            if let Err(e) = apply_event(&ev_handle, &state, ticket_id, event, false).await {
                                eprintln!("[flow] {event:?} for #{ticket_id} failed: {e}");
                            }
                        }
                    }
                });

                // Background poller: watch PR-stage tickets' CI and auto-fix PR-caused failures.
                let poll_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let mut tick = tokio::time::interval(std::time::Duration::from_secs(CI_POLL_SECS));
                    loop {
                        tick.tick().await;
                        let state = poll_handle.state::<AppState>();
                        poll_ci_once(&poll_handle, &state).await;
                    }
                });
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_tickets,
            list_repos,
            add_repo,
            rename_repo,
            delete_repo,
            get_permission_mode,
            set_permission_mode,
            get_ticket,
            list_sessions,
            live_sessions,
            live_progress,
            pending_reattach,
            session_transcript,
            clear_ended_sessions,
            delete_session,
            delete_worktree_sessions,
            list_worktrees,
            delete_worktree,
            worktree_dirty,
            cleanup_ticket_worktrees,
            ticket_diff,
            ticket_pr,
            ticket_pr_comments,
            list_diff_comments,
            add_diff_comment,
            delete_diff_comment,
            resolve_diff_comment,
            add_local_ticket,
            set_spec,
            set_spec_fields,
            set_ticket_status,
            jira_apply_column,
            transition_ticket,
            grill_ticket,
            request_review,
            request_ci_fix,
            get_ci_autofix,
            set_ci_autofix,
            get_pr_desc_autoupdate,
            set_pr_desc_autoupdate,
            update_pr_description_now,
            address_feedback,
            accept_proposed_spec,
            reject_proposed_spec,
            delete_ticket,
            jira_env,
            install_acli,
            jira_logout,
            jira_sync,
            jira_detail,
            open_in_jira,
            open_pr,
            start_session,
            start_spec_session,
            send_input,
            answer_question,
            stop_session,
            resize
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

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
/// Max automatic review-fix attempts (review→fix→re-review cycles) before we stop and escalate.
const MAX_REVIEW_FIX_ATTEMPTS: i64 = 3;
/// Max proof-generation attempts per reviewed HEAD before we stop retrying (anti-loop when capture
/// keeps failing). The change still advances — proof is best-effort.
const MAX_PROOF_ATTEMPTS: i64 = 2;
/// Max times the orchestrator auto-restarts a crashed session for a ticket before escalating.
const MAX_RESTART_ATTEMPTS: i64 = 2;
/// Stuck-session watchdog: a live session whose transcript has been idle at least this long AND is at
/// a finished turn is treated as "the completion hook was missed" → the watchdog fires the recovery
/// event. Must exceed normal between-record gaps; the mid-tool guard covers long-running commands.
const STUCK_IDLE_SECS: u64 = 45;
/// A live session idle at least this long but NOT at a cleanly-finished turn (e.g. a possibly-hung
/// tool) is ambiguous → escalate to the orchestrator's LLM judge (only when Orchestrator is on).
const ESCALATE_IDLE_SECS: u64 = 240;

/// Best-effort desktop notification — the escalation channel for when the autonomous loop needs a
/// human (e.g. the review loop is exhausted) or finishes an outward-facing step (auto-merge).
fn notify(app: &AppHandle, title: &str, body: &str) {
    use tauri_plugin_notification::NotificationExt;
    let _ = app.notification().builder().title(title).body(body).show();
}

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
    /// Session ids the stuck-session watchdog has already fired a recovery event for, so it doesn't
    /// re-fire each tick (for events that don't tear the session down). Cleared on session exit.
    watchdog_fired: Arc<Mutex<HashSet<i64>>>,
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
    state
        .store
        .set_setting("permission_mode", &mode)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn rename_repo(state: State<'_, AppState>, id: i64, name: String) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("name cannot be empty".into());
    }
    state
        .store
        .rename_repo(id, name.trim())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_repo(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state.store.delete_repo(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_ticket(state: State<'_, AppState>, id: i64) -> Result<Option<Ticket>, String> {
    state.store.get_ticket(id).await.map_err(|e| e.to_string())
}

/// Manually assign a ticket to a repo (for tickets that didn't auto-pick one — e.g. a Jira project
/// with no default repo, or a local ticket). Refreshes the derived activity pill so the "Assign a
/// repo" state clears immediately.
#[tauri::command]
async fn assign_ticket_repo(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
    repo_id: i64,
) -> Result<(), String> {
    // Validate the repo exists (guards against a stale UI id).
    state
        .store
        .get_repo(repo_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such repo")?;
    state
        .store
        .set_ticket_repo(ticket_id, repo_id)
        .await
        .map_err(|e| e.to_string())?;
    store_activity(&app, &state, ticket_id).await;
    Ok(())
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
        Some(path) => {
            tokio::task::spawn_blocking(move || harmony_core::session::render_transcript(&path))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
        }
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
        let prog =
            tokio::task::spawn_blocking(move || harmony_core::session::latest_progress(&path))
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
    state
        .store
        .delete_ended_sessions()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn list_worktrees(state: State<'_, AppState>) -> Result<Vec<WorktreeView>, String> {
    state
        .store
        .list_worktrees()
        .await
        .map_err(|e| e.to_string())
}

/// Total uncommitted changes across all of a ticket's worktrees (so the UI can warn before a
/// destructive move-to-Done / delete). 0 means clean.
async fn ticket_uncommitted(state: &AppState, ticket_id: i64) -> usize {
    let worktrees = state
        .store
        .worktrees_for_ticket(ticket_id)
        .await
        .unwrap_or_default();
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
            return Err(format!(
                "DIRTY: {n} uncommitted change(s) would be discarded"
            ));
        }
    }
    harmony_core::worktree::cleanup_for_ticket(&state.store, ticket_id).await;
    let worktrees = state
        .store
        .worktrees_for_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?;
    for wt in worktrees {
        state
            .store
            .delete_worktree(wt.id)
            .await
            .map_err(|e| e.to_string())?;
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
                return Err(format!(
                    "DIRTY: {n} uncommitted change(s) would be discarded"
                ));
            }
        }
        if let Ok(Some(repo)) = state.store.get_repo(wt.repo_id).await {
            let _ = harmony_core::worktree::remove(&repo.path, std::path::Path::new(&wt.path));
        }
    }
    state
        .store
        .delete_worktree(id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn delete_session(state: State<'_, AppState>, id: i64) -> Result<(), String> {
    state
        .store
        .delete_session(id)
        .await
        .map_err(|e| e.to_string())
}

/// Delete the ended sessions for a worktree (clears a consolidated group).
#[tauri::command]
async fn delete_worktree_sessions(
    state: State<'_, AppState>,
    worktree_id: i64,
) -> Result<u64, String> {
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
    state
        .store
        .set_ticket_spec(id, &spec)
        .await
        .map_err(|e| e.to_string())
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
        .set_ticket_spec_fields(
            id,
            &spec,
            &acceptance_criteria,
            &relevant_paths,
            &constraints,
        )
        .await
        .map_err(|e| e.to_string())
}

/// Move a ticket to a column (drag-and-drop / manual override).
#[tauri::command]
async fn set_ticket_status(
    state: State<'_, AppState>,
    id: i64,
    status: String,
) -> Result<(), String> {
    if !harmony_core::status::is_valid(&status) {
        return Err(format!("invalid status: {status}"));
    }
    state
        .store
        .set_ticket_status(id, &status)
        .await
        .map_err(|e| e.to_string())
}

/// Reflect a board-column move onto the linked Jira issue (best-effort; only if a matching
/// status exists in the issue's workflow). No-op for non-Jira tickets and the
/// "For Your Review" (waiting) state, which has no Jira equivalent.
#[tauri::command]
async fn jira_apply_column(
    state: State<'_, AppState>,
    ticket_id: i64,
    status: String,
) -> Result<(), String> {
    let ticket = match state
        .store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
    {
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
async fn delete_ticket(
    state: State<'_, AppState>,
    ticket_id: i64,
    force: bool,
) -> Result<(), String> {
    if !force {
        let n = ticket_uncommitted(&state, ticket_id).await;
        if n > 0 {
            return Err(format!(
                "DIRTY: {n} uncommitted change(s) would be discarded"
            ));
        }
    }
    harmony_core::worktree::cleanup_for_ticket(&state.store, ticket_id).await;
    state
        .store
        .delete_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())
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
    JiraEnv {
        acli_installed,
        site,
    }
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
    harmony_core::jira::logout()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn jira_sync(state: State<'_, AppState>) -> Result<usize, String> {
    let issues = harmony_core::jira::search_assigned()
        .await
        .map_err(|e| e.to_string())?;
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
    harmony_core::jira::open_in_browser(&key)
        .await
        .map_err(|e| e.to_string())
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
    let issue = harmony_core::jira::get_issue(&key)
        .await
        .map_err(|e| e.to_string())?;
    let comments = harmony_core::jira::comments(&key).await.unwrap_or_default();
    Ok(JiraDetail {
        description: issue.description,
        comments,
    })
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
    let (title, path, branch, repo_path) = (
        ticket.title.clone(),
        wt.path.clone(),
        wt.branch.clone(),
        repo.path.clone(),
    );
    let commit_msg = commit_message(&ticket);
    // Proof of work (evidence the change functions) → posted as a PR comment so teammates review the
    // output, not the diff. Empty when no proof was produced (disabled, or nothing to evidence).
    let (proof_report, proof_artifacts_json, proof_tid) = (
        ticket.proof.clone(),
        ticket.proof_artifacts.clone(),
        ticket_id,
    );

    let url = tokio::task::spawn_blocking(move || -> Result<String, String> {
        // Commit any leftover uncommitted work (e.g. edits made directly during human review) so
        // the branch isn't empty, then refuse clearly if there's still nothing to PR.
        harmony_core::github::commit_all(&path, &commit_msg).map_err(|e| e.to_string())?;
        let base =
            harmony_core::worktree::default_branch(&repo_path).unwrap_or_else(|_| "main".into());
        if harmony_core::github::commits_ahead(&path, &base) == 0 {
            return Err("no committed changes to open a PR for".into());
        }
        // Generated diff summary (conforms to the repo's PR template if present), else the spec.
        let body = harmony_core::github::generated_pr_body(
            &path,
            &repo_path,
            ticket_ref.as_deref(),
            &fallback,
        );
        harmony_core::github::push_branch(&path, &branch).map_err(|e| e.to_string())?;
        let url = harmony_core::github::create_draft_pr(&path, &title, &body, &branch)
            .map_err(|e| e.to_string())?;

        // Best-effort proof comment: host any media (fills in URLs), render, and post. Never fails
        // the PR — proof is supplementary.
        let mut artifacts: Vec<harmony_core::proof::ProofArtifact> =
            serde_json::from_str(&proof_artifacts_json).unwrap_or_default();
        if !proof_report.trim().is_empty() || !artifacts.is_empty() {
            harmony_core::github::host_proof_artifacts(&path, proof_tid, &mut artifacts);
            let comment = harmony_core::proof::render_pr_comment(&proof_report, &artifacts);
            let _ = harmony_core::github::post_pr_comment(&path, &comment);
        }
        Ok(url)
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
    state
        .sessions
        .lock()
        .unwrap()
        .values()
        .any(|io| io.ticket_id == ticket_id)
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

/// Auto re-review kill-switch (default on — the user opted into auto re-review). `"off"` → never
/// auto-redoes a review (the manual "Request review" button still works).
async fn auto_review_enabled(state: &AppState) -> bool {
    state
        .store
        .get_setting("auto_review")
        .await
        .ok()
        .flatten()
        .map(|v| v != "off")
        .unwrap_or(true)
}

/// One poll pass: auto-redo `/review` for any review-stage ticket whose reviewed change-set has
/// moved on since the last review. Mirrors `poll_ci_once`: cheap HEAD-SHA fingerprint for
/// idempotency, skips live sessions (which also debounces mid-edit churn). `/review` runs in plan
/// mode (no commits) so it can't move HEAD and re-trigger itself; `mark_reviewed` then re-points
/// `reviewed_sha` at the reviewed HEAD, closing the gate until the branch changes again.
async fn poll_reviews_once(app: &AppHandle, state: &AppState) {
    if !auto_review_enabled(state).await {
        return;
    }
    let tickets = match state.store.list_tickets().await {
        Ok(t) => t,
        Err(_) => return,
    };
    for ticket in tickets {
        // Only the two review stages, only previously-reviewed tickets (the first review stays
        // owned by the column-entry flow), and never mid-session.
        let in_review_stage = matches!(
            ticket.status.as_str(),
            harmony_core::status::WAITING | harmony_core::status::IN_REVIEW
        );
        if !in_review_stage || ticket.reviewed != 1 || has_live_session(state, ticket.id) {
            continue;
        }
        let wt = match state.store.primary_worktree_for_ticket(ticket.id).await {
            Ok(Some(wt)) => wt,
            _ => continue,
        };
        // Cheap pre-check (no LLM): has HEAD moved since the review's fingerprint?
        let path = wt.path.clone();
        let head = tokio::task::spawn_blocking(move || {
            harmony_core::github::head_sha(&path).unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        if head.is_empty() || head == ticket.reviewed_sha {
            continue; // review is current
        }
        // Stale → re-run `/review` in place (stays in the current column).
        let _ = apply_event(app, state, ticket.id, Event::ReviewRequested, false).await;
    }
}

/// One poll pass for the self-correcting review loop (the review-stage sibling of `poll_ci_once`).
/// For each pre-PR `For Your Review` ticket with a *current* `/review`, judge it once per reviewed
/// HEAD; on a `changes_requested` verdict, auto-spawn a fix session seeded with the findings (the
/// fix commits → HEAD moves → `poll_reviews_once` re-reviews → judged again → loop). Capped by
/// `MAX_REVIEW_FIX_ATTEMPTS`, then escalates. Gated by `review_loop`; skips live sessions.
async fn poll_review_loop_once(app: &AppHandle, state: &AppState) {
    if !review_loop_enabled(state).await {
        return;
    }
    let tickets = match state.store.list_tickets().await {
        Ok(t) => t,
        Err(_) => return,
    };
    for ticket in tickets {
        // Pre-PR review stage only; post-PR iteration is covered by CI-fix + auto re-review.
        if ticket.status != harmony_core::status::WAITING
            || ticket.reviewed != 1
            || has_live_session(state, ticket.id)
        {
            continue;
        }
        let wt = match state.store.primary_worktree_for_ticket(ticket.id).await {
            Ok(Some(wt)) => wt,
            _ => continue,
        };
        let repo_path = match state.store.get_repo(wt.repo_id).await {
            Ok(Some(r)) => r.path,
            _ => continue,
        };
        // Only act on a CURRENT review (a `/review` exists for this HEAD). If HEAD moved past the
        // last review, let `poll_reviews_once` re-review first. And judge each reviewed HEAD once
        // (the `judged_sha` fingerprint) — that also bounds the loop: a fix that changes nothing
        // doesn't move HEAD, so it isn't re-judged.
        let path = wt.path.clone();
        let head = tokio::task::spawn_blocking(move || {
            harmony_core::github::head_sha(&path).unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        if head.is_empty() || head != ticket.reviewed_sha || head == ticket.judged_sha {
            continue;
        }

        // Judge the review (off-thread `claude -p`), persist the verdict + findings.
        let judgement = {
            let (path, review_text, repo_path) = (
                wt.path.clone(),
                ticket.review_text.clone(),
                repo_path.clone(),
            );
            tokio::task::spawn_blocking(move || {
                let base = harmony_core::worktree::default_branch(&repo_path)
                    .unwrap_or_else(|_| "main".into());
                let diff = harmony_core::github::diff(&path, &base).unwrap_or_default();
                harmony_core::review::judge(&path, &review_text, &diff)
            })
            .await
        };
        let judgement = match judgement {
            Ok(Ok(j)) => j,
            _ => continue, // judge failed → leave for the next tick / a human
        };
        let findings_json =
            serde_json::to_string(&judgement.findings).unwrap_or_else(|_| "[]".into());
        let _ = state
            .store
            .set_ticket_review_verdict(ticket.id, &head, judgement.verdict.as_str(), &findings_json)
            .await;
        let _ = app.emit("ticket-updated", ticket.id);

        // Clean (or nothing actionable) → rest in `For Your Review` for the human to open the PR.
        if judgement.verdict != harmony_core::review::Verdict::ChangesRequested
            || judgement.findings.is_empty()
        {
            continue;
        }

        // Blocking findings: auto-fix while under the cap, else stop (the activity pill flips to
        // "Review loop needs you" and `store_activity` fires the escalation notification).
        if ticket.review_fix_attempts >= MAX_REVIEW_FIX_ATTEMPTS {
            continue;
        }
        let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
        match mgr.start_review_fix(ticket.id).await {
            Ok(handle) => {
                let _ = wire_session(app, state, handle, ticket.id);
                let _ = state.store.bump_review_fix_attempts(ticket.id).await;
            }
            Err(e) => eprintln!(
                "[review-loop] start_review_fix for #{} failed: {e}",
                ticket.id
            ),
        }
    }
}

/// One poll pass for proof-of-work generation (the review-stage sibling of `poll_review_loop_once`).
/// For each pre-PR `For Your Review` ticket whose change has passed review, capture proof once per
/// reviewed HEAD (the `proof_sha` fingerprint): spawn a proof session that records evidence the
/// change works. Gated by `proof`; capped by `MAX_PROOF_ATTEMPTS`; skips live sessions. When the
/// review loop is on, waits for its judge to PASS this HEAD before evidencing (so proof reflects the
/// final, accepted state); when it's off, evidences any current review.
async fn poll_proof_loop_once(app: &AppHandle, state: &AppState) {
    if !proof_enabled(state).await {
        return;
    }
    let review_loop = review_loop_enabled(state).await;
    let tickets = match state.store.list_tickets().await {
        Ok(t) => t,
        Err(_) => return,
    };
    for ticket in tickets {
        // Pre-PR review stage only, reviewed at least once, never mid-session, under the cap.
        if ticket.status != harmony_core::status::WAITING
            || ticket.reviewed != 1
            || ticket.proof_attempts >= MAX_PROOF_ATTEMPTS
            || has_live_session(state, ticket.id)
        {
            continue;
        }
        let wt = match state.store.primary_worktree_for_ticket(ticket.id).await {
            Ok(Some(wt)) => wt,
            _ => continue,
        };
        let path = wt.path.clone();
        let head = tokio::task::spawn_blocking(move || {
            harmony_core::github::head_sha(&path).unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        // Only evidence a CURRENT review (proof reflects the reviewed HEAD), and only once per HEAD.
        if head.is_empty() || head != ticket.reviewed_sha || head == ticket.proof_sha {
            continue;
        }
        // With the review loop on, don't evidence until its judge has PASSED this exact HEAD.
        if review_loop && (ticket.judged_sha != head || ticket.review_verdict != "pass") {
            continue;
        }

        let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
        match mgr.start_proof(ticket.id).await {
            Ok(handle) => {
                let _ = wire_session(app, state, handle, ticket.id);
                let _ = state.store.bump_proof_attempts(ticket.id).await;
                store_activity(app, state, ticket.id).await;
            }
            Err(e) => eprintln!("[proof] start_proof for #{} failed: {e}", ticket.id),
        }
    }
}

/// One poll pass for gated auto-merge. For each `In PR Review` ticket whose PR is approved on GitHub
/// (`reviewDecision == APPROVED`) AND has no failing checks, inject `Move(Done)` — `flow::decide`
/// then merges (`MergePr`) and cleans up. The continuous approval poll is the missing link that lets
/// a human approving on GitHub advance the ticket with no drag. Gated by `auto_merge` (default off).
async fn poll_auto_merge_once(app: &AppHandle, state: &AppState) {
    if !auto_merge_enabled(state).await {
        return;
    }
    let tickets = match state.store.list_tickets().await {
        Ok(t) => t,
        Err(_) => return,
    };
    for ticket in tickets {
        if ticket.status != harmony_core::status::IN_REVIEW || has_live_session(state, ticket.id) {
            continue;
        }
        let wt = match state.store.primary_worktree_for_ticket(ticket.id).await {
            Ok(Some(wt)) => wt,
            _ => continue,
        };
        let path = wt.path.clone();
        let (mergeable, green) = tokio::task::spawn_blocking(move || {
            let status = harmony_core::github::pr_status(&path);
            let failing = harmony_core::github::pr_checks_json(&path)
                .ok()
                .map(|j| harmony_core::ci::parse_failing_checks(&j))
                .unwrap_or_default();
            // Only an OPEN, approved PR is mergeable — a MERGED/CLOSED state means we're done (and
            // guards against re-attempting a merge if a post-merge cleanup step had failed).
            (
                status.exists && status.approved && status.state == "OPEN",
                failing.is_empty(),
            )
        })
        .await
        .unwrap_or((false, false));
        if !(mergeable && green) {
            continue;
        }
        match apply_event(app, state, ticket.id, Event::Move(Column::Done), false).await {
            Ok(()) => notify(
                app,
                "Auto-merged",
                &format!(
                    "#{} ({}) was approved + green — merged to Done.",
                    ticket.id, ticket.title
                ),
            ),
            Err(e) => eprintln!("[auto-merge] #{} failed: {e}", ticket.id),
        }
    }
}

// ---- orchestrator (autonomous coordinator) -------------------------------

/// Distinct tickets that currently have a live session (concurrency accounting).
fn live_ticket_count(state: &AppState) -> usize {
    let map = state.sessions.lock().unwrap();
    let mut ids: Vec<i64> = map.values().map(|io| io.ticket_id).collect();
    ids.sort_unstable();
    ids.dedup();
    ids.len()
}

/// The live session id for a ticket, if any.
fn live_session_id_for_ticket(state: &AppState, ticket_id: i64) -> Option<i64> {
    let map = state.sessions.lock().unwrap();
    map.iter()
        .find(|(_, io)| io.ticket_id == ticket_id)
        .map(|(sid, _)| *sid)
}

/// A stable fingerprint of a "stuck state" (so the conductor decides it once, not every tick).
fn state_fingerprint(kind: &str, content: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut h);
    format!("{kind}:{:x}", h.finish())
}

/// The `label` of a ticket's persisted `activity` JSON (used to route auto-advance).
fn activity_label(json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("label")
                .and_then(|l| l.as_str())
                .map(|s| s.to_string())
        })
}

/// Truncate for a human-facing note.
fn short(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Parse a ticket's `pending_question` JSON into (first question text, option labels, multiSelect).
fn parse_pending_question(json: &str) -> Option<(String, Vec<String>, bool)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let q = v.get("questions")?.as_array()?.first()?;
    let question = q.get("question")?.as_str()?.to_string();
    let multi = q
        .get("multiSelect")
        .and_then(|x| x.as_bool())
        .unwrap_or(false);
    let options = q
        .get("options")
        .and_then(|o| o.as_array())
        .map(|arr| {
            arr.iter()
                .map(|o| {
                    let label = o.get("label").and_then(|x| x.as_str()).unwrap_or("");
                    let desc = o.get("description").and_then(|x| x.as_str()).unwrap_or("");
                    if desc.is_empty() {
                        label.to_string()
                    } else {
                        format!("{label} — {desc}")
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Some((question, options, multi))
}

/// The domain event a finished session of `kind` should fire — the same mapping the `Stop` hook uses
/// (`core/src/hooks.rs`). Returned so the watchdog can re-fire a missed completion.
fn finish_event_for_kind(kind: &str) -> Event {
    match kind {
        "work" => Event::WorkFinished,
        "review" => Event::ReviewFinished,
        "fix" => Event::FixFinished,
        "address" => Event::AddressFinished,
        "proof" => Event::ProofFinished,
        _ => Event::SessionIdle,
    }
}

/// Seconds since a file was last modified, or `None` if it can't be read.
fn file_idle_secs(path: &str) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    modified.elapsed().ok().map(|d| d.as_secs())
}

/// Stuck-session watchdog (always-on recovery). Harmony advances the flow off Claude Code hooks
/// (`Stop` for work, plan-file write for review), but those are sometimes missed — plan-mode sessions
/// don't reliably fire `Stop`, and a cwd/plan-path mismatch drops the event silently — so a finished
/// session sits live forever, the column never moves, and the Review tab stays empty.
///
/// This pass detects that from the transcript (written by Claude directly, so it advances even when
/// hooks don't): for each session live in-process, if its transcript has been idle a while AND the
/// last record is a finished turn, it re-fires the same completion event the hook would have (after
/// clearing a stale question and, for a review, backfilling the review text). Long-idle-but-ambiguous
/// sessions (a possibly-hung tool) escalate to the orchestrator's LLM judge, but only when the
/// Orchestrator setting is on. Never disturbs a session that's still writing, mid-tool, or waiting on
/// the user.
async fn poll_stuck_sessions_once(app: &AppHandle, state: &AppState) {
    use harmony_core::session::TurnState;

    // Snapshot the live (session_id, ticket_id) pairs, skipping ones already being stopped or
    // already recovered, so we don't hold the lock across awaits.
    let live: Vec<(i64, i64)> = {
        let map = state.sessions.lock().unwrap();
        let stopping = state.stopping.lock().unwrap();
        let fired = state.watchdog_fired.lock().unwrap();
        map.iter()
            .filter(|(sid, _)| !stopping.contains(sid) && !fired.contains(sid))
            .map(|(sid, io)| (*sid, io.ticket_id))
            .collect()
    };
    if live.is_empty() {
        return;
    }
    let escalate_enabled = orchestrator_enabled(state).await;

    for (session_id, ticket_id) in live {
        let path = match state
            .store
            .latest_transcript_path_for_ticket(ticket_id)
            .await
        {
            Ok(Some(p)) => p,
            _ => continue, // no transcript yet — nothing to judge
        };
        let idle = match file_idle_secs(&path) {
            Some(s) => s,
            None => continue,
        };
        if idle < STUCK_IDLE_SECS {
            continue; // still active (an actively-steered session keeps the transcript fresh)
        }

        let turn = tokio::task::spawn_blocking({
            let path = path.clone();
            move || harmony_core::session::transcript_turn_state(&path)
        })
        .await
        .unwrap_or(TurnState::Working);

        match turn {
            // Genuinely waiting on the user, or still mid-turn → leave it alone.
            TurnState::WaitingOnQuestion => continue,
            TurnState::Working => {
                // Ambiguous: idle a long time but not a clean finish (e.g. a hung tool). Only the
                // opt-in LLM judge decides here; otherwise leave it for the human.
                if !escalate_enabled || idle < ESCALATE_IDLE_SECS {
                    continue;
                }
                let wt = match state.store.primary_worktree_for_ticket(ticket_id).await {
                    Ok(Some(wt)) => wt,
                    _ => continue,
                };
                let tail = tokio::task::spawn_blocking({
                    let path = path.clone();
                    move || harmony_core::session::render_transcript(&path).unwrap_or_default()
                })
                .await
                .unwrap_or_default();
                let verdict = tokio::task::spawn_blocking({
                    let wt = wt.path.clone();
                    move || harmony_core::orchestrator::judge_stuck(&wt, &tail)
                })
                .await;
                match verdict {
                    Ok(Ok(harmony_core::orchestrator::StuckVerdict::Done)) => {
                        recover_finished_session(app, state, ticket_id, session_id, &path).await;
                    }
                    Ok(Ok(harmony_core::orchestrator::StuckVerdict::Escalate { reason })) => {
                        eprintln!("[watchdog] #{ticket_id} escalated: {reason}");
                        let _ = state
                            .store
                            .set_orchestrator_note(
                                ticket_id,
                                &format!("stuck session — needs you: {reason}"),
                                &state_fingerprint("stuck", &reason),
                            )
                            .await;
                        // Mark handled so we don't re-judge every tick; store_activity fires the
                        // "needs you" notification via the activity pill.
                        state.watchdog_fired.lock().unwrap().insert(session_id);
                        store_activity(app, state, ticket_id).await;
                    }
                    _ => {} // Working / judge failed → leave for the next tick or the human
                }
            }
            // Clear-cut: the turn finished but the completion event never arrived — re-fire it.
            TurnState::Finished => {
                recover_finished_session(app, state, ticket_id, session_id, &path).await;
            }
        }
    }
}

/// Re-fire the completion event a finished-but-stuck session missed: clear a stale pending question,
/// backfill a review's text from the transcript if the plan-file capture was missed, then inject the
/// kind's finish event through the normal executor. Marks the session recovered (de-dup).
async fn recover_finished_session(
    app: &AppHandle,
    state: &AppState,
    ticket_id: i64,
    session_id: i64,
    transcript_path: &str,
) {
    let kind = state
        .store
        .active_session_kind_for_ticket(ticket_id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "work".to_string());

    // A finished turn is not blocked on a question — clear any stale one so WorkFinished isn't a
    // no-op (the `user_question_pending` gate in flow::decide).
    let _ = state.store.clear_ticket_question(ticket_id).await;

    // Review completion is normally captured from the plan-file write; if that hook was missed the
    // Review tab is empty, so backfill from the transcript's final assistant message.
    if kind == "review" {
        if let Ok(Some(t)) = state.store.get_ticket(ticket_id).await {
            if t.review_text.trim().is_empty() {
                let path = transcript_path.to_string();
                if let Ok(Some(msg)) = tokio::task::spawn_blocking(move || {
                    harmony_core::session::final_assistant_message(&path)
                })
                .await
                {
                    let _ = state.store.set_ticket_review_text(ticket_id, &msg).await;
                }
            }
        }
    }

    state.watchdog_fired.lock().unwrap().insert(session_id);
    let event = finish_event_for_kind(&kind);
    eprintln!("[watchdog] #{ticket_id} finished-but-stuck ({kind}) → {event:?}");
    if let Err(e) = apply_event(app, state, ticket_id, event, false).await {
        eprintln!("[watchdog] {event:?} for #{ticket_id} failed: {e}");
    }
}

/// The autonomous coordinator pass, gated by `orchestrator` (runs last in the tick, on fresh
/// activity). (A) reconcile crashed sessions + dispatch ready work under the concurrency cap; (B)
/// try to unstick each ticket the human would otherwise handle — answer derivable worker questions,
/// accept low-risk spec proposals, open PRs on a clean review — escalating genuine judgment (left in
/// place; the WaitingOnYou desktop notification already fired). The state machine stays
/// authoritative: every action goes through the existing commands.
async fn poll_orchestrator_once(app: &AppHandle, state: &AppState) {
    if !orchestrator_enabled(state).await {
        return;
    }
    let tickets = match state.store.list_tickets().await {
        Ok(t) => t,
        Err(_) => return,
    };
    let cap = max_concurrent(state).await;
    let mut slots = harmony_core::orchestrator::dispatch_slots(cap, live_ticket_count(state));

    // (A1) Reconcile: restart sessions that crashed (latest session ended `error`) for a ticket
    // still in the working column, capped to avoid crash loops (escalate past the cap).
    for t in &tickets {
        if slots == 0 {
            break;
        }
        if t.status != harmony_core::status::WORKING || has_live_session(state, t.id) {
            continue;
        }
        let crashed = matches!(
            state
                .store
                .latest_session_state_for_ticket(t.id)
                .await
                .ok()
                .flatten()
                .as_deref(),
            Some("error")
        );
        if !crashed {
            continue;
        }
        if t.restart_attempts >= MAX_RESTART_ATTEMPTS {
            let seen = state_fingerprint("crash", &t.status);
            if t.orchestrator_seen != seen {
                let _ = state
                    .store
                    .set_orchestrator_note(t.id, "escalated: session crashed repeatedly", &seen)
                    .await;
                notify(
                    app,
                    &format!("{} — needs you", t.title),
                    "A session crashed repeatedly.",
                );
            }
            continue;
        }
        let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
        match mgr.start(t.id).await {
            Ok(handle) => {
                if wire_session(app, state, handle, t.id).is_ok() {
                    let _ = state.store.bump_restart_attempts(t.id).await;
                    let _ = state
                        .store
                        .set_orchestrator_note(t.id, "restarted crashed session", "")
                        .await;
                    slots -= 1;
                }
            }
            Err(e) => eprintln!("[orchestrator] restart #{} failed: {e}", t.id),
        }
    }

    // (A2) Dispatch: start eligible ready Todos (has repo + grilled), oldest first, up to slots.
    let mut ready: Vec<&Ticket> = tickets
        .iter()
        .filter(|t| {
            t.status == harmony_core::status::TODO
                && harmony_core::orchestrator::todo_dispatch_eligible(
                    t.repo_id.is_some(),
                    t.grilled == 1,
                    t.drafting == 1,
                    has_live_session(state, t.id),
                )
        })
        .collect();
    ready.sort_by_key(|t| t.created_at);
    for t in ready {
        if slots == 0 {
            break;
        }
        match apply_event(app, state, t.id, Event::Move(Column::InProgress), false).await {
            Ok(()) => {
                let _ = state
                    .store
                    .set_orchestrator_note(t.id, "dispatched: started work", "")
                    .await;
                slots -= 1;
            }
            Err(e) => eprintln!("[orchestrator] dispatch #{} failed: {e}", t.id),
        }
    }

    // (B) Unstick tickets the human would otherwise handle.
    for t in &tickets {
        // 1) Answer a live worker's outstanding question (conductor; escalate genuine judgment).
        if !t.pending_question.trim().is_empty() {
            if let Some(session_id) = live_session_id_for_ticket(state, t.id) {
                let seen = state_fingerprint("q", &t.pending_question);
                if t.orchestrator_seen != seen {
                    orchestrator_answer_question(app, state, t, session_id, &seen).await;
                }
                continue;
            }
        }
        // 2) Accept a low-risk proposed spec (conductor; escalate genuine judgment).
        if !t.proposed_spec.trim().is_empty() {
            let seen = state_fingerprint("spec", &t.proposed_spec);
            if t.orchestrator_seen != seen {
                orchestrator_judge_spec(app, state, t, &seen).await;
            }
            continue;
        }
        // 3) Auto-advance: a clean, reviewed change waiting for a PR → open it (deterministic).
        if activity_label(&t.activity).as_deref() == Some("Ready to open PR")
            && !has_live_session(state, t.id)
        {
            match apply_event(app, state, t.id, Event::Move(Column::Pr), false).await {
                Ok(()) => {
                    let _ = state
                        .store
                        .set_orchestrator_note(t.id, "opened PR (review clean)", "")
                        .await;
                }
                Err(e) => eprintln!("[orchestrator] open-PR #{} failed: {e}", t.id),
            }
        }
    }
}

/// Conductor: decide how to answer a worker's question (or escalate), then deliver it via the PTY.
async fn orchestrator_answer_question(
    app: &AppHandle,
    state: &AppState,
    ticket: &Ticket,
    session_id: i64,
    seen: &str,
) {
    let Some((question, options, multi)) = parse_pending_question(&ticket.pending_question) else {
        return;
    };
    let wt = match state.store.primary_worktree_for_ticket(ticket.id).await {
        Ok(Some(w)) => w,
        _ => return,
    };
    let spec = harmony_core::spec::compose_spec(ticket);
    let (path, q, opts) = (wt.path.clone(), question.clone(), options.clone());
    let decision = tokio::task::spawn_blocking(move || {
        harmony_core::orchestrator::answer_question(&path, &q, &opts, multi, &spec)
    })
    .await;
    let decision = match decision {
        Ok(Ok(d)) => d,
        _ => return, // conductor failed → leave for the human next tick
    };
    use harmony_core::orchestrator::QDecision;
    match decision {
        QDecision::Answer { selected, custom } => {
            let note = match &custom {
                Some(txt) => format!("answered question — \"{}\"", short(txt, 60)),
                None => format!(
                    "answered question — option {}",
                    selected
                        .iter()
                        .map(|i| i.to_string())
                        .collect::<Vec<_>>()
                        .join(",")
                ),
            };
            let _ = deliver_answer(state, session_id, options.len(), selected, custom, multi);
            let _ = state
                .store
                .set_orchestrator_note(ticket.id, &note, seen)
                .await;
            let _ = app.emit("ticket-updated", ticket.id);
        }
        QDecision::Escalate { reason } => {
            let _ = state
                .store
                .set_orchestrator_note(
                    ticket.id,
                    &format!("escalated question: {}", short(&reason, 80)),
                    seen,
                )
                .await;
        }
    }
}

/// Conductor: decide whether to accept a proposed spec revision (or escalate).
async fn orchestrator_judge_spec(app: &AppHandle, state: &AppState, ticket: &Ticket, seen: &str) {
    let wt = match state.store.primary_worktree_for_ticket(ticket.id).await {
        Ok(Some(w)) => w,
        _ => return,
    };
    let current = harmony_core::spec::compose_spec(ticket);
    let (path, proposed) = (wt.path.clone(), ticket.proposed_spec.clone());
    let decision = tokio::task::spawn_blocking(move || {
        harmony_core::orchestrator::judge_spec(&path, &current, &proposed)
    })
    .await;
    let decision = match decision {
        Ok(Ok(d)) => d,
        _ => return,
    };
    use harmony_core::orchestrator::SpecDecision;
    match decision {
        SpecDecision::Accept => {
            if apply_proposed_spec(&state.store, ticket.id).await.is_ok() {
                let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
                if let Ok(handle) = mgr.start_implement_spec(ticket.id).await {
                    let _ = wire_session(app, state, handle, ticket.id);
                }
                let _ = state
                    .store
                    .set_orchestrator_note(ticket.id, "accepted proposed spec + resumed", seen)
                    .await;
                let _ = app.emit("ticket-updated", ticket.id);
            }
        }
        SpecDecision::Escalate { reason } => {
            let _ = state
                .store
                .set_orchestrator_note(
                    ticket.id,
                    &format!("escalated spec change: {}", short(&reason, 80)),
                    seen,
                )
                .await;
        }
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
    let repo = state
        .store
        .get_repo(wt.repo_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("repo missing")?;
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
            let base = harmony_core::worktree::default_branch(&repo_path)
                .unwrap_or_else(|_| "main".into());
            let diff = harmony_core::github::diff(&path, &base).unwrap_or_default();
            harmony_core::ci::triage(&path, &base, &diff)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?
    };

    if let Ok(json) = serde_json::to_string(&triage) {
        let _ = state
            .store
            .set_ticket_ci_triage(ticket_id, &head, &json)
            .await;
    }
    let _ = app.emit("ticket-updated", ticket_id);

    let should_fix = if manual {
        true
    } else {
        triage.actionable
            && ci_autofix_enabled(state).await
            && ticket.ci_fix_attempts < MAX_CI_FIX_ATTEMPTS
    };
    if !should_fix {
        return Ok(triage.reason.clone());
    }

    let context = ci_fix_context(&triage);
    let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
    let handle = mgr
        .start_ci_fix(ticket_id, &context)
        .await
        .map_err(|e| e.to_string())?;
    wire_session(app, state, handle, ticket_id)?;
    let _ = state.store.bump_ci_fix_attempts(ticket_id).await;
    Ok(format!("fixing: {}", triage.reason))
}

/// Build the opening-prompt context for a fix session from a triage result.
fn ci_fix_context(triage: &harmony_core::ci::CiTriage) -> String {
    let mut s = format!("Failing checks: {}\n", triage.failing_checks.join(", "));
    if let Some(v) = &triage.verdict {
        s.push_str(&format!(
            "\nWhy it's attributed to this PR: {}\n",
            v.rationale
        ));
        if !v.proposed_fix.trim().is_empty() {
            s.push_str(&format!("\nSuggested fix: {}\n", v.proposed_fix));
        }
    }
    s
}

/// `FixFinished`: commit + push the fix session's changes (re-triggers CI), keeping the ticket in
/// the PR column. The next poll tick re-triages the new HEAD (and stops at the attempt cap).
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
    let wt = match state
        .store
        .primary_worktree_for_ticket(ticket_id)
        .await
        .ok()
        .flatten()
    {
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
        let base =
            harmony_core::worktree::default_branch(&repo_path).unwrap_or_else(|_| "main".into());
        let diff = harmony_core::github::diff(&path, &base).unwrap_or_default();
        match harmony_core::draft::maybe_update_pr_description(
            &path,
            &body,
            &diff,
            ticket_ref.as_deref(),
        ) {
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

/// Manual "Fix CI" button: triage now and fix regardless of the auto gates.
#[tauri::command]
async fn request_ci_fix(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
) -> Result<String, String> {
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
async fn update_pr_description_now(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
) -> Result<(), String> {
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

#[tauri::command]
async fn get_auto_review(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(auto_review_enabled(&state).await)
}

#[tauri::command]
async fn set_auto_review(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state
        .store
        .set_setting("auto_review", if enabled { "on" } else { "off" })
        .await
        .map_err(|e| e.to_string())
}

/// Auto-end-idle toggle (default off). When on, a session that comes to rest in `waiting` after a
/// Stop with no pending question has its PTY freed instead of left hanging (read in `hooks.rs`).
#[tauri::command]
async fn get_auto_end_idle(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state
        .store
        .get_setting("auto_end_idle")
        .await
        .ok()
        .flatten()
        .map(|v| v == "on")
        .unwrap_or(false))
}

#[tauri::command]
async fn set_auto_end_idle(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state
        .store
        .set_setting("auto_end_idle", if enabled { "on" } else { "off" })
        .await
        .map_err(|e| e.to_string())
}

/// Self-correcting review loop toggle (default OFF — opt-in autonomy). When on, a `/review` whose
/// judge verdict is `changes_requested` auto-spawns a fix session and re-reviews until clean or the
/// attempt cap (then escalates). Read by `poll_review_loop_once`.
async fn review_loop_enabled(state: &AppState) -> bool {
    state
        .store
        .get_setting("review_loop")
        .await
        .ok()
        .flatten()
        .map(|v| v == "on")
        .unwrap_or(false)
}

#[tauri::command]
async fn get_review_loop(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(review_loop_enabled(&state).await)
}

#[tauri::command]
async fn set_review_loop(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state
        .store
        .set_setting("review_loop", if enabled { "on" } else { "off" })
        .await
        .map_err(|e| e.to_string())
}

/// Proof-of-work generation toggle (default ON). When on, once a change passes review harmony runs a
/// proof session that captures evidence it works (video/screenshots/report), shown in the Proof tab
/// and posted to the PR. Read by `poll_proof_loop_once`.
async fn proof_enabled(state: &AppState) -> bool {
    state
        .store
        .get_setting("proof")
        .await
        .ok()
        .flatten()
        .map(|v| v != "off")
        .unwrap_or(true)
}

#[tauri::command]
async fn get_proof_enabled(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(proof_enabled(&state).await)
}

#[tauri::command]
async fn set_proof_enabled(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state
        .store
        .set_setting("proof", if enabled { "on" } else { "off" })
        .await
        .map_err(|e| e.to_string())
}

/// Auto-merge toggle (default OFF — outward-facing and irreversible). When on, an approved + CI-green
/// PR is merged and the ticket advances to Done with no human drag. Read by `poll_auto_merge_once`.
async fn auto_merge_enabled(state: &AppState) -> bool {
    state
        .store
        .get_setting("auto_merge")
        .await
        .ok()
        .flatten()
        .map(|v| v == "on")
        .unwrap_or(false)
}

#[tauri::command]
async fn get_auto_merge(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(auto_merge_enabled(&state).await)
}

#[tauri::command]
async fn set_auto_merge(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state
        .store
        .set_setting("auto_merge", if enabled { "on" } else { "off" })
        .await
        .map_err(|e| e.to_string())
}

/// Orchestrator toggle (default OFF). When on, the coordinator autonomously dispatches/reconciles
/// sessions, answers derivable worker questions (escalating judgment), and auto-advances the loop.
async fn orchestrator_enabled(state: &AppState) -> bool {
    state
        .store
        .get_setting("orchestrator")
        .await
        .ok()
        .flatten()
        .map(|v| v == "on")
        .unwrap_or(false)
}

/// Max concurrent live worker sessions the orchestrator will run at once (default 3).
async fn max_concurrent(state: &AppState) -> usize {
    state
        .store
        .get_setting("max_concurrent")
        .await
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(3)
}

#[tauri::command]
async fn get_orchestrator(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(orchestrator_enabled(&state).await)
}

#[tauri::command]
async fn set_orchestrator(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    state
        .store
        .set_setting("orchestrator", if enabled { "on" } else { "off" })
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_max_concurrent(state: State<'_, AppState>) -> Result<u32, String> {
    Ok(max_concurrent(&state).await as u32)
}

#[tauri::command]
async fn set_max_concurrent(state: State<'_, AppState>, n: u32) -> Result<(), String> {
    let n = n.max(1);
    state
        .store
        .set_setting("max_concurrent", &n.to_string())
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
        let base =
            harmony_core::worktree::default_branch(&repo_path).unwrap_or_else(|_| "main".into());
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
// A Tauri command whose args mirror the diff-comment record fields — they don't usefully group.
#[allow(clippy::too_many_arguments)]
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
        .add_diff_comment(
            ticket_id, &target, &anchor, &file_path, line, end_line, &side, &body,
        )
        .await
        .map_err(|e| e.to_string())
}

/// Send all open review comments (any surface) to Claude: spawn an autonomous "address" session
/// that folds them into its prompt, addresses them, and (on finish) commits + pushes.
#[tauri::command]
async fn address_feedback(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
) -> Result<i64, String> {
    let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
    let handle = mgr
        .start_address(ticket_id)
        .await
        .map_err(|e| e.to_string())?;
    wire_session(&app, &state, handle, ticket_id)
}

/// Apply Claude's proposed spec to the ticket's live fields: parse the proposal into the first-class
/// fields, write them, and clear the proposal. Shared by plain "Accept" and "Accept & implement".
async fn apply_proposed_spec(store: &Store, ticket_id: i64) -> Result<(), String> {
    let ticket = store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    if ticket.proposed_spec.trim().is_empty() {
        return Err("no proposed spec to accept".into());
    }
    let f = harmony_core::spec::parse_spec(&ticket.proposed_spec);
    store
        .set_ticket_spec_fields(
            ticket_id,
            &f.spec,
            &f.acceptance_criteria,
            &f.relevant_paths,
            &f.constraints,
        )
        .await
        .map_err(|e| e.to_string())?;
    store
        .set_ticket_proposed_spec(ticket_id, "")
        .await
        .map_err(|e| e.to_string())
}

/// Accept Claude's proposed spec update: parse it into the first-class fields, write it as the
/// live spec, and clear the proposal.
#[tauri::command]
async fn accept_proposed_spec(state: State<'_, AppState>, ticket_id: i64) -> Result<(), String> {
    apply_proposed_spec(&state.store, ticket_id).await
}

/// Accept Claude's proposed spec update (as `accept_proposed_spec`) and immediately resume Claude to
/// implement the now-agreed change. Returns the new session id.
#[tauri::command]
async fn accept_proposed_spec_and_implement(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
) -> Result<i64, String> {
    apply_proposed_spec(&state.store, ticket_id).await?;
    let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
    let handle = mgr
        .start_implement_spec(ticket_id)
        .await
        .map_err(|e| e.to_string())?;
    wire_session(&app, &state, handle, ticket_id)
}

/// A unified diff (live spec → pending proposal) for the Spec tab's diff view; `""` when no proposal.
#[tauri::command]
async fn proposed_spec_diff(state: State<'_, AppState>, ticket_id: i64) -> Result<String, String> {
    let ticket = state
        .store
        .get_ticket(ticket_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    Ok(harmony_core::spec::proposed_spec_diff(&ticket))
}

/// Reject Claude's proposed spec update (discard it; the live spec is unchanged).
#[tauri::command]
async fn reject_proposed_spec(state: State<'_, AppState>, ticket_id: i64) -> Result<(), String> {
    state
        .store
        .set_ticket_proposed_spec(ticket_id, "")
        .await
        .map_err(|e| e.to_string())
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
    state
        .store
        .set_ticket_repo(ticket_id, repo_id)
        .await
        .map_err(|e| e.to_string())?;
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
    let handle = mgr
        .start_spec_session(ticket_id, seed)
        .await
        .map_err(|e| e.to_string())?;
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

    state.sessions.lock().unwrap().insert(
        session_id,
        SessionIo {
            master,
            writer,
            killer,
            ticket_id,
        },
    );

    // Wait for exit -> mark ended (or failed), clear draft flag, drop handles, notify UI
    {
        let app = app.clone();
        let store = state.store.clone();
        let sessions = state.sessions.clone();
        let stopping = state.stopping.clone();
        let watchdog_fired = state.watchdog_fired.clone();
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
            // A question the session was asking can no longer be answered (no live PTY to receive
            // the reply) — clear it so a dead session's prompt doesn't linger in the UI.
            let _ = store.clear_ticket_question(ticket_id).await;
            sessions.lock().unwrap().remove(&session_id);
            watchdog_fired.lock().unwrap().remove(&session_id);
            let _ = app.emit(
                "session-exit",
                SessionExit {
                    session_id,
                    ticket_id,
                    ok: ok || user_stopped,
                    code,
                },
            );
        });
    }

    Ok(session_id)
}

#[tauri::command]
fn send_input(state: State<'_, AppState>, session_id: i64, data: String) -> Result<(), String> {
    let mut map = state.sessions.lock().unwrap();
    if let Some(io) = map.get_mut(&session_id) {
        io.writer
            .write_all(data.as_bytes())
            .map_err(|e| e.to_string())?;
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
    deliver_answer(
        &state,
        session_id,
        option_count,
        selected,
        custom_text,
        multi_select,
    )
}

/// Translate an AskUserQuestion answer into TUI keystrokes and write them to the session's PTY.
/// Shared by the `answer_question` command (the human via `QuestionCard`) and the orchestrator
/// conductor (answering derivable questions autonomously).
fn deliver_answer(
    state: &AppState,
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
        io.writer
            .write_all(keys.as_bytes())
            .map_err(|e| e.to_string())?;
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
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
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
    let wt = state
        .store
        .primary_worktree_for_ticket(ticket.id)
        .await
        .ok()
        .flatten();
    let has_worktree = wt.is_some();

    // git/gh facts (blocking + network) — only meaningful once a worktree exists.
    let (has_changes, review_current, pr) = if let Some(wt) = wt.as_ref() {
        let path = wt.path.clone();
        let repo_path = state
            .store
            .get_repo(wt.repo_id)
            .await
            .ok()
            .flatten()
            .map(|r| r.path);
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

    let auto_end_idle = state
        .store
        .get_setting("auto_end_idle")
        .await
        .ok()
        .flatten()
        .map(|v| v == "on")
        .unwrap_or(false);

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
        // Claude is mid-question → a Stop isn't "done"; keep the session live (see `flow::decide`).
        user_question_pending: !ticket.pending_question.trim().is_empty(),
        auto_end_idle,
    }
}

/// Whether the persisted CI triage indicates failing checks.
fn ci_triage_failing(json: &str) -> bool {
    if json.trim().is_empty() {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("failing_checks")
                .and_then(|f| f.as_array())
                .map(|a| !a.is_empty())
        })
        .unwrap_or(false)
}

/// The `category` string of a previously-persisted `Activity` JSON (for transition detection).
fn prev_activity_category(json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| {
            v.get("category")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
        })
}

/// Recompute the ticket's derived activity status, persist it, emit `ticket-updated`, and — when the
/// ticket *newly* enters a "waiting on you" state — fire a desktop notification. This is the single
/// owner of "needs you" notifications. Builds fresh facts (`build_ctx`) so it reflects state after an
/// event's actions ran; called from `apply_event` and the activity poll pass.
async fn store_activity(app: &AppHandle, state: &AppState, ticket_id: i64) {
    let ticket = match state.store.get_ticket(ticket_id).await {
        Ok(Some(t)) => t,
        _ => return,
    };
    let ctx = build_ctx(state, &ticket).await;
    let session_kind = state
        .store
        .active_session_kind_for_ticket(ticket_id)
        .await
        .ok()
        .flatten();
    let input = harmony_core::activity::ActivityInput {
        from: ctx.from,
        has_repo: ctx.has_repo,
        session_live: ctx.session_live,
        session_kind,
        user_question_pending: ctx.user_question_pending,
        has_changes: ctx.has_changes,
        review_current: ctx.review_current,
        reviewed: ctx.reviewed,
        review_changes_requested: ticket.review_verdict == "changes_requested",
        review_fix_attempts: ticket.review_fix_attempts,
        review_fix_max: MAX_REVIEW_FIX_ATTEMPTS,
        ci_failing: ci_triage_failing(&ticket.ci_triage),
        ci_fix_attempts: ticket.ci_fix_attempts,
        ci_fix_max: MAX_CI_FIX_ATTEMPTS,
        pr_exists: ctx.pr_exists,
        pr_approved: ctx.pr_approved,
        auto_review: auto_review_enabled(state).await,
        review_loop: review_loop_enabled(state).await,
        ci_autofix: ci_autofix_enabled(state).await,
        auto_merge: auto_merge_enabled(state).await,
    };
    let activity = harmony_core::activity::classify(&input);
    let prev = prev_activity_category(&ticket.activity);
    let json = serde_json::to_string(&activity).unwrap_or_default();
    let _ = state.store.set_ticket_activity(ticket_id, &json).await;
    let _ = app.emit("ticket-updated", ticket_id);

    // Notify only on a real transition INTO "waiting on you" (skip the initial seed: prev == None).
    if activity.category == harmony_core::activity::Category::WaitingOnYou
        && prev.is_some()
        && prev.as_deref() != Some("waiting_on_you")
    {
        let body = activity
            .detail
            .clone()
            .unwrap_or_else(|| "Needs your attention.".to_string());
        notify(
            app,
            &format!("{} — {}", ticket.title, activity.label),
            &body,
        );
    }
}

/// One poll pass: refresh the activity pill for non-terminal tickets, picking up external changes
/// (PR approval, CI) that don't arrive as flow events. Done tickets are terminal (set on the move).
async fn poll_activity_once(app: &AppHandle, state: &AppState) {
    let tickets = match state.store.list_tickets().await {
        Ok(t) => t,
        Err(_) => return,
    };
    for ticket in tickets {
        if ticket.status == harmony_core::status::DONE {
            continue;
        }
        store_activity(app, state, ticket.id).await;
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
    // A fresh autonomous work cycle just finished → reset the review loop's bookkeeping so the
    // upcoming review episode (and its attempt cap) starts clean, and clear the orchestrator's
    // crash-restart counter (work progressed legitimately).
    if event == Event::WorkFinished {
        let _ = state.store.reset_review_loop(ticket_id).await;
        let _ = state.store.reset_restart_attempts(ticket_id).await;
        // Fresh work cycle → the prior proof no longer describes the change; regenerate it after the
        // upcoming review settles.
        let _ = state.store.reset_proof(ticket_id).await;
    }
    let ctx = build_ctx(state, &ticket).await;
    let decision = flow::decide(event, &ctx);
    if let Some(reason) = decision.blocked {
        return Err(reason.to_string());
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
    let target = decision.target.as_status();
    state
        .store
        .set_ticket_status(ticket_id, target)
        .await
        .map_err(|e| e.to_string())?;
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
                    let _ = app.emit(
                        "pr-done",
                        PrDone {
                            ticket_id,
                            ok: true,
                            error: None,
                        },
                    );
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
                    let _ = app.emit(
                        "pr-done",
                        PrDone {
                            ticket_id,
                            ok: false,
                            error: Some(e),
                        },
                    );
                    let _ = app.emit("ticket-updated", ticket_id);
                }
            }
        });
    }
    // Refresh the derived activity pill now that the state machine has acted (and notify if this
    // change means the ticket now needs the user).
    store_activity(app, state, ticket_id).await;
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
            let handle = mgr()
                .start_spec_session(id, seed)
                .await
                .map_err(|e| e.to_string())?;
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
        // Commit the agent's working changes (harmony owns version control). A no-op when clean.
        Action::CommitChanges => {
            if let Some(wt) = state
                .store
                .primary_worktree_for_ticket(id)
                .await
                .map_err(|e| e.to_string())?
            {
                let (path, msg) = (wt.path.clone(), commit_message(ticket));
                let _ = tokio::task::spawn_blocking(move || {
                    harmony_core::github::commit_all(&path, &msg)
                })
                .await;
            }
        }
        // Push the branch (re-triggers CI / updates the PR). When a PR exists, refresh its
        // description if the change made it stale (respects the `pr_desc_autoupdate` kill-switch).
        Action::PushBranch => {
            if let Some(wt) = state
                .store
                .primary_worktree_for_ticket(id)
                .await
                .map_err(|e| e.to_string())?
            {
                let (path, branch) = (wt.path.clone(), wt.branch.clone());
                let pushed = tokio::task::spawn_blocking(move || {
                    harmony_core::github::push_branch(&path, &branch).is_ok()
                        && harmony_core::github::pr_status(&path).exists
                })
                .await
                .unwrap_or(false);
                if pushed {
                    maybe_update_pr_desc(app, state, id).await;
                }
            }
        }
        // Fingerprint the reviewed HEAD so the column-entry `/review` isn't re-run until the branch
        // moves again (`flow::Ctx.review_current`). The review prose itself is captured live by the
        // hook server (the `/review` skill's plan-file write — see `core/src/hooks.rs`).
        Action::MarkReviewed => {
            if let Some(wt) = state
                .store
                .primary_worktree_for_ticket(id)
                .await
                .map_err(|e| e.to_string())?
            {
                let path = wt.path.clone();
                if let Ok(Ok(sha)) =
                    tokio::task::spawn_blocking(move || harmony_core::github::head_sha(&path)).await
                {
                    let _ = state.store.mark_reviewed(id, &sha).await;
                }
            }
        }
        // A proof session finished: fingerprint the evidenced HEAD (so the proof poller won't
        // regenerate until the branch moves) and collect the media it wrote to the artifact dir. The
        // prose report itself is captured live by the hook server (the proof session's plan-file
        // write). Proof is best-effort — a missing/empty artifact dir just yields no artifacts.
        Action::MarkProofDone => {
            if let Some(wt) = state
                .store
                .primary_worktree_for_ticket(id)
                .await
                .map_err(|e| e.to_string())?
            {
                let path = wt.path.clone();
                let sha =
                    tokio::task::spawn_blocking(move || harmony_core::github::head_sha(&path))
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .unwrap_or_default();
                let dir = harmony_core::settings::proof_artifact_dir(id)
                    .to_string_lossy()
                    .to_string();
                let artifacts = harmony_core::proof::scan_artifacts(&dir);
                let json = serde_json::to_string(&artifacts).unwrap_or_else(|_| "[]".into());
                let _ = state.store.mark_proof_done(id, &sha, &json).await;
            }
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
    let key = match state
        .store
        .get_ticket(ticket_id)
        .await
        .ok()
        .flatten()
        .and_then(|t| t.jira_key)
    {
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
async fn grill_ticket(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
) -> Result<(), String> {
    let _ = ensure_ticket_repo(&state, ticket_id, None).await;
    apply_event(&app, &state, ticket_id, Event::GrillRequested, false).await
}

/// The Review tab's "Request review" button: re-run `/review` on demand, even if HEAD hasn't
/// changed since the last review.
#[tauri::command]
async fn request_review(
    app: AppHandle,
    state: State<'_, AppState>,
    ticket_id: i64,
) -> Result<(), String> {
    apply_event(&app, &state, ticket_id, Event::ReviewRequested, false).await
}

/// Restore the user's login-shell `PATH`. A macOS `.app` launched from Finder/Dock inherits a bare
/// PATH (`/usr/bin:/bin:/usr/sbin:/sbin`), so tools installed under `~/.local/bin`,
/// `/opt/homebrew/bin`, npm/volta/bun, etc. — including `claude`, `gh`, `git`, `node` — aren't on it
/// and every spawn fails with "not found in PATH". Query the login shell's PATH once and merge it
/// (plus common install dirs) into this process's env; every spawned child then inherits it. In
/// `tauri dev` the terminal PATH is already correct, so this is a harmless no-op. Best-effort.
fn ensure_user_path() {
    let mut dirs: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let push = |d: String, dirs: &mut Vec<String>| {
        if !d.is_empty() && !dirs.iter().any(|e| e == &d) {
            dirs.push(d);
        }
    };

    // 1) The user's real PATH from a login+interactive shell (sources .zprofile/.zshrc/.bash_profile).
    //    Bracket it with a marker so shell-startup noise (MOTD, etc.) is easy to strip.
    #[cfg(unix)]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        const MARK: &str = "__HARMONY_PATH__";
        let script = format!(r#"printf %s {MARK}; printf %s "$PATH"; printf %s {MARK}"#);
        if let Ok(out) = std::process::Command::new(&shell)
            .args(["-ilc", &script])
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout);
            if let (Some(a), Some(b)) = (s.find(MARK), s.rfind(MARK)) {
                let start = a + MARK.len();
                if b > start {
                    for d in s[start..b].split(':') {
                        push(d.to_string(), &mut dirs);
                    }
                }
            }
        }
    }

    // 2) Belt-and-suspenders: common install locations that exist on disk.
    if let Ok(home) = std::env::var("HOME") {
        for rel in [
            ".local/bin",
            ".claude/local",
            ".npm-global/bin",
            ".bun/bin",
            ".cargo/bin",
        ] {
            let p = format!("{home}/{rel}");
            if std::path::Path::new(&p).is_dir() {
                push(p, &mut dirs);
            }
        }
    }
    for p in ["/opt/homebrew/bin", "/usr/local/bin"] {
        if std::path::Path::new(p).is_dir() {
            push(p.to_string(), &mut dirs);
        }
    }

    std::env::set_var("PATH", dirs.join(":"));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Fix PATH before anything spawns a child process (the store/hooks don't, but sessions do).
    ensure_user_path();
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
                // No sessions are live yet, so any stored AskUserQuestion is stale (its session
                // died before the answer's PostToolUse cleared it) — drop them all.
                let _ = store.clear_all_questions().await;
                // Likewise clear a stale `drafting` flag left by a grill that a crash/force-quit
                // interrupted — otherwise the ticket is stuck ("finish the interview first").
                let _ = store.clear_all_drafting().await;
                // Hook server → executor channel: the hook raises domain events (grill/work/
                // review done); the consumer task below runs them through the flow executor.
                let (tx, mut rx) =
                    tokio::sync::mpsc::unbounded_channel::<harmony_core::hooks::SystemEvent>();
                let _ =
                    harmony_core::hooks::spawn_server(Arc::new(store.clone()), HOOK_PORT, Some(tx))
                        .await;
                handle.manage(AppState {
                    store,
                    sessions: Arc::new(Mutex::new(HashMap::new())),
                    reattach: Mutex::new(reattach),
                    stopping: Arc::new(Mutex::new(HashSet::new())),
                    watchdog_fired: Arc::new(Mutex::new(HashSet::new())),
                });
                // Consume system events and drive the flow executor (auto-advance).
                let ev_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    use harmony_core::hooks::SystemEvent;
                    // Every system event maps 1:1 to a `flow::Event` and runs through the single
                    // executor path — `decide` owns the column, actions (commit/push/stop/...), and
                    // all gating (pending question, `auto_end_idle`). No event-specific side paths.
                    while let Some(ev) = rx.recv().await {
                        let (ticket_id, event) = match ev {
                            SystemEvent::GrillFinished { ticket_id } => {
                                (ticket_id, Event::GrillFinished)
                            }
                            SystemEvent::WorkFinished { ticket_id } => {
                                (ticket_id, Event::WorkFinished)
                            }
                            SystemEvent::ReviewFinished { ticket_id } => {
                                (ticket_id, Event::ReviewFinished)
                            }
                            SystemEvent::ProofFinished { ticket_id } => {
                                (ticket_id, Event::ProofFinished)
                            }
                            SystemEvent::FixFinished { ticket_id } => {
                                (ticket_id, Event::FixFinished)
                            }
                            SystemEvent::AddressFinished { ticket_id } => {
                                (ticket_id, Event::AddressFinished)
                            }
                            SystemEvent::SessionIdle { ticket_id } => {
                                (ticket_id, Event::SessionIdle)
                            }
                        };
                        let state = ev_handle.state::<AppState>();
                        if let Err(e) =
                            apply_event(&ev_handle, &state, ticket_id, event, false).await
                        {
                            eprintln!("[flow] {event:?} for #{ticket_id} failed: {e}");
                        }
                    }
                });

                // Background poller: watch PR-stage tickets' CI and auto-fix PR-caused failures.
                let poll_handle = handle.clone();
                tauri::async_runtime::spawn(async move {
                    let mut tick =
                        tokio::time::interval(std::time::Duration::from_secs(CI_POLL_SECS));
                    loop {
                        tick.tick().await;
                        let state = poll_handle.state::<AppState>();
                        // CI first so a pending fix claims the live-session slot before the
                        // re-review pass considers the same ticket. Then re-review stale changes,
                        // then judge+auto-fix current reviews, then auto-merge approved PRs.
                        poll_ci_once(&poll_handle, &state).await;
                        poll_reviews_once(&poll_handle, &state).await;
                        poll_review_loop_once(&poll_handle, &state).await;
                        // After the review loop settles, evidence passed changes (proof of work).
                        poll_proof_loop_once(&poll_handle, &state).await;
                        poll_auto_merge_once(&poll_handle, &state).await;
                        // Recover finished-but-stuck sessions (missed Stop / plan-file hook) so the
                        // flow advances even when a completion event was dropped.
                        poll_stuck_sessions_once(&poll_handle, &state).await;
                        // Last: refresh each ticket's activity pill from the (now-current) facts.
                        poll_activity_once(&poll_handle, &state).await;
                        // Last: the autonomous coordinator acts on the now-fresh board state.
                        poll_orchestrator_once(&poll_handle, &state).await;
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
            assign_ticket_repo,
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
            get_auto_review,
            set_auto_review,
            get_auto_end_idle,
            set_auto_end_idle,
            get_review_loop,
            set_review_loop,
            get_proof_enabled,
            set_proof_enabled,
            get_auto_merge,
            set_auto_merge,
            get_orchestrator,
            set_orchestrator,
            get_max_concurrent,
            set_max_concurrent,
            get_pr_desc_autoupdate,
            set_pr_desc_autoupdate,
            update_pr_description_now,
            address_feedback,
            accept_proposed_spec,
            accept_proposed_spec_and_implement,
            proposed_spec_diff,
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

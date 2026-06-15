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

use harmony_core::models::{Repo, SessionView, Ticket, WorktreeView};
use harmony_core::session::SessionManager;
use harmony_core::store::Store;
use portable_pty::{ChildKiller, MasterPty, PtySize};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

const HOOK_PORT: u16 = 8787;

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
    if !force {
        let n = ticket_uncommitted(&state, ticket_id).await;
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

#[tauri::command]
async fn draft_ticket(
    state: State<'_, AppState>,
    id: i64,
) -> Result<harmony_core::spec::SpecFields, String> {
    let ticket = state
        .store
        .get_ticket(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no such ticket")?;
    let key = ticket.jira_key.ok_or("ticket is not linked to Jira")?;
    let issue = harmony_core::jira::get_issue(&key).await.map_err(|e| e.to_string())?;
    let spec = tokio::task::spawn_blocking(move || {
        harmony_core::draft::draft_spec(&issue.summary, &issue.description)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    // Split the drafted markdown into the first-class fields and persist them independently.
    let f = harmony_core::spec::parse_spec(&spec);
    state
        .store
        .set_ticket_spec_fields(id, &f.spec, &f.acceptance_criteria, &f.relevant_paths, &f.constraints)
        .await
        .map_err(|e| e.to_string())?;
    Ok(f)
}

// ---- pull request --------------------------------------------------------

#[tauri::command]
async fn open_pr(state: State<'_, AppState>, ticket_id: i64) -> Result<String, String> {
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
        .ok_or("no worktree — start a session first")?;

    let composed = harmony_core::spec::compose_spec(&ticket);
    let body = if composed.trim().is_empty() {
        format!("Ticket: {}", ticket.title)
    } else {
        composed
    };
    let (title, path, branch) = (ticket.title.clone(), wt.path.clone(), wt.branch.clone());

    let url = tokio::task::spawn_blocking(move || -> Result<String, String> {
        harmony_core::github::push_branch(&path, &branch).map_err(|e| e.to_string())?;
        harmony_core::github::create_draft_pr(&path, &title, &body, &branch).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    state
        .store
        .set_ticket_status(ticket_id, harmony_core::status::IN_REVIEW)
        .await
        .map_err(|e| e.to_string())?;

    if let Some(key) = ticket.jira_key.as_deref() {
        let _ = harmony_core::jira::transition(key, "In Review").await;
        let _ = harmony_core::jira::add_comment(key, &format!("PR opened by harmony: {url}")).await;
    }
    Ok(url)
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
    let mgr = SessionManager::new(Arc::new(state.store.clone()), HOOK_PORT);
    let handle = mgr.start_spec_session(ticket_id).await.map_err(|e| e.to_string())?;
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
                // Run the hook server in-process (same one the CLI uses).
                let _ = harmony_core::hooks::spawn_server(Arc::new(store.clone()), HOOK_PORT).await;
                handle.manage(AppState {
                    store,
                    sessions: Arc::new(Mutex::new(HashMap::new())),
                    reattach: Mutex::new(reattach),
                    stopping: Arc::new(Mutex::new(HashSet::new())),
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
            add_local_ticket,
            set_spec,
            set_spec_fields,
            set_ticket_status,
            jira_apply_column,
            delete_ticket,
            jira_env,
            install_acli,
            jira_logout,
            jira_sync,
            jira_detail,
            open_in_jira,
            draft_ticket,
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

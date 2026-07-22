//! Local hook server. Receives Claude Code HTTP hook POSTs, correlates each to a
//! live session by `cwd` (= worktree path), and updates session + ticket state.
//!
//! Phase 0 findings applied: the tool name field is `tool_name` (not `tool`), and we
//! key the (future) permission path off `PreToolUse`. For now harmony is supervised:
//! it returns no decision, so Claude shows its normal prompt. Autonomy mode would
//! return `{"permissionDecision":"allow"}` here.

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::store::Store;

/// Domain events the hook server raises for the app's flow executor to act on (auto-advance).
/// Absent in headless/CLI mode (no executor) — see `HookCtx::events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemEvent {
    /// A spec/grill session captured its spec.
    GrillFinished { ticket_id: i64 },
    /// A work session finished its turn with no pending question (Claude is "done").
    WorkFinished { ticket_id: i64 },
    /// A `/review` session finished.
    ReviewFinished { ticket_id: i64 },
    /// A proof-of-work session finished capturing evidence (fingerprint HEAD + collect artifacts).
    ProofFinished { ticket_id: i64 },
    /// An autonomous CI-fix session finished (commit + push its changes to re-trigger CI).
    FixFinished { ticket_id: i64 },
    /// An autonomous conflict-resolve session finished (commit the merge + push to update the PR).
    ConflictFinished { ticket_id: i64 },
    /// A feedback-addressing session finished (commit + push so the PR reflects the changes).
    AddressFinished { ticket_id: i64 },
    /// A session came to rest in `waiting` after a `Stop` with no pending question, and the
    /// `auto_end_idle` setting is on → free its PTY instead of leaving an idle terminal hanging.
    /// The executor stops the session; the ticket stays in its column (resume by moving it back).
    SessionIdle { ticket_id: i64 },
}

/// Shared state for the hook router: the store, plus an optional channel to the app executor.
/// When `events` is `None` (CLI), the hook keeps its legacy direct ticket-status writes.
#[derive(Clone)]
struct HookCtx {
    store: Arc<Store>,
    events: Option<UnboundedSender<SystemEvent>>,
}

/// Append a hook event to `~/.harmony/harmony.log`. We must NOT print hook events to
/// stdout while a session's terminal is bridged there — it corrupts the Claude TUI.
fn log_event(line: &str) {
    use std::io::Write;
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let path = std::path::Path::new(&home)
        .join(".harmony")
        .join("harmony.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Extract a plan/spec document from a `PreToolUse` payload: an `ExitPlanMode` call's `plan`
/// field (older Claude Code), or the content of a `Write` to a `~/.claude/plans/*.md` file
/// (current Claude Code). `None` for any other tool / non-plan write.
fn extract_plan(v: &Value, tool: Option<&str>) -> Option<String> {
    match tool {
        Some("ExitPlanMode") => v
            .get("tool_input")
            .and_then(|ti| ti.get("plan"))
            .and_then(|p| p.as_str())
            .map(|s| s.to_string())
            // Fallback if the field name differs: serialise the whole input.
            .or_else(|| v.get("tool_input").map(|ti| ti.to_string())),
        Some("Write") => {
            let ti = v.get("tool_input");
            let path = ti
                .and_then(|t| t.get("file_path"))
                .and_then(|x| x.as_str())
                .unwrap_or("");
            if path.contains("/.claude/plans/") {
                ti.and_then(|t| t.get("content"))
                    .and_then(|x| x.as_str())
                    .map(|s| s.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

fn router(ctx: HookCtx) -> Router {
    Router::new()
        .route("/hook/:event", post(handle))
        .with_state(ctx)
}

/// Bind + spawn the server as a background task (used when also running a session). `events` is
/// the app executor's channel (`None` for the headless CLI).
pub async fn spawn_server(
    store: Arc<Store>,
    port: u16,
    events: Option<UnboundedSender<SystemEvent>>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let app = router(HookCtx { store, events });
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(())
}

/// Bind + serve forever (the `harmony serve` debug command).
pub async fn serve_forever(
    store: Arc<Store>,
    port: u16,
    events: Option<UnboundedSender<SystemEvent>>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("[harmony] hook server on http://127.0.0.1:{port}");
    axum::serve(listener, router(HookCtx { store, events })).await?;
    Ok(())
}

async fn handle(Path(event): Path<String>, State(ctx): State<HookCtx>, body: Bytes) -> Json<Value> {
    let store = &ctx.store;
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let cwd = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
    let claude_id = v.get("session_id").and_then(|x| x.as_str());
    let tool = v.get("tool_name").and_then(|x| x.as_str());

    let cwd_canon = std::fs::canonicalize(cwd)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| cwd.to_string());

    // The hook response. Default: no programmatic decision (supervised). The review arm below may
    // set it to an auto-approve so a plan-mode review's bash never stalls on an approval prompt.
    let mut response = json!({});

    match store.active_session_by_cwd(&cwd_canon).await {
        Ok(Some(sess)) => {
            if sess.claude_session_id.is_none() {
                if let Some(cid) = claude_id {
                    let _ = store.set_session_claude_id(sess.id, cid).await;
                }
            }
            if let Some(tp) = v.get("transcript_path").and_then(|x| x.as_str()) {
                let _ = store.set_session_transcript_path(sess.id, tp).await;
            }
            let (session_state, ticket_status) = match event.as_str() {
                "Stop" | "Notification" => ("waiting", crate::status::WAITING),
                _ => ("working", crate::status::WORKING),
            };
            // Always update the session's own state (drives live-progress in the UI).
            let _ = store.set_session_state(sess.id, session_state, tool).await;

            match &ctx.events {
                // App mode: the flow executor owns ticket status + advancement. This handler is a
                // thin adapter — on a `Stop`, map the session kind to its domain event and emit it.
                // All gating (a pending question means work isn't done; `auto_end_idle` decides
                // whether an idle session is torn down) lives in `flow::decide` via `flow::Ctx`, so
                // we emit unconditionally here and never write ticket status.
                Some(tx) => {
                    if event == "Stop" {
                        let id = sess.ticket_id;
                        let ev = match sess.kind.as_str() {
                            "work" => SystemEvent::WorkFinished { ticket_id: id },
                            "proof" => SystemEvent::ProofFinished { ticket_id: id },
                            "fix" => SystemEvent::FixFinished { ticket_id: id },
                            "conflict" => SystemEvent::ConflictFinished { ticket_id: id },
                            "address" => SystemEvent::AddressFinished { ticket_id: id },
                            // A `review` (plan mode) completes when it presents its plan via
                            // ExitPlanMode (the definitive capture below), NOT on a `Stop` — an
                            // interactive turn can yield mid-review, and firing ReviewFinished then
                            // would capture a partial review. So a review Stop is just an idle event.
                            // Likewise `spec`/grill has no domain event to tear it down. `decide`
                            // frees an idle PTY when `auto_end_idle` is on and no question is pending.
                            _ => SystemEvent::SessionIdle { ticket_id: id },
                        };
                        let _ = tx.send(ev);
                    }
                }
                // Headless/CLI mode (no executor): keep the legacy direct ticket-status write so
                // the CLI's board state still advances on Stop/working.
                None => {
                    if sess.kind == "work" {
                        let _ = store.set_ticket_status(sess.ticket_id, ticket_status).await;
                    }
                }
            }

            // Claude's task list (TodoWrite) → mirror onto the ticket as a checklist.
            if tool == Some("TodoWrite") {
                if let Some(arr) = v
                    .get("tool_input")
                    .and_then(|ti| ti.get("todos"))
                    .and_then(|t| t.as_array())
                {
                    let compact: Vec<Value> = arr
                        .iter()
                        .map(|t| {
                            json!({
                                "content": t.get("content").and_then(|x| x.as_str())
                                    .or_else(|| t.get("activeForm").and_then(|x| x.as_str()))
                                    .unwrap_or(""),
                                "status": t.get("status").and_then(|x| x.as_str()).unwrap_or("pending"),
                            })
                        })
                        .collect();
                    if let Ok(s) = serde_json::to_string(&compact) {
                        let _ = store.set_ticket_todos(sess.ticket_id, &s).await;
                    }
                }
            }

            // AskUserQuestion → surface as an answerable question card in the UI.
            // PreToolUse carries the questions+options; PostToolUse means it's answered.
            if tool == Some("AskUserQuestion") {
                if event == "PostToolUse" {
                    let _ = store.clear_ticket_question(sess.ticket_id).await;
                } else if let Some(questions) = v
                    .get("tool_input")
                    .and_then(|ti| ti.get("questions"))
                    .and_then(|q| q.as_array())
                {
                    let compact: Vec<Value> = questions
                        .iter()
                        .map(|q| {
                            let options: Vec<Value> = q
                                .get("options")
                                .and_then(|o| o.as_array())
                                .map(|arr| {
                                    arr.iter()
                                        .map(|o| {
                                            json!({
                                                "label": o.get("label").and_then(|x| x.as_str()).unwrap_or(""),
                                                "description": o.get("description").and_then(|x| x.as_str()).unwrap_or(""),
                                            })
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            json!({
                                "question": q.get("question").and_then(|x| x.as_str()).unwrap_or(""),
                                "header": q.get("header").and_then(|x| x.as_str()).unwrap_or(""),
                                "multiSelect": q.get("multiSelect").and_then(|x| x.as_bool()).unwrap_or(false),
                                "options": options,
                            })
                        })
                        .collect();
                    let payload = json!({ "session_id": sess.id, "questions": compact });
                    if let Ok(s) = serde_json::to_string(&payload) {
                        let _ = store.set_ticket_question(sess.ticket_id, &s).await;
                    }
                }
            }

            // Grill/spec session: the finished spec arrives one of two ways depending on the
            // Claude Code version's plan mode —
            //   * older: an `ExitPlanMode` call carrying the plan in `tool_input.plan`;
            //   * current: the plan is written to a plan file (`~/.claude/plans/*.md`) via the
            //     `Write` tool, so `ExitPlanMode` never fires with the text.
            // Capture the plan from whichever fired, split it into the first-class fields, and
            // clear `drafting` (the app then auto-stops the grill).
            if sess.kind == "spec" && event == "PreToolUse" {
                if let Some(plan) = extract_plan(&v, tool) {
                    let f = crate::spec::parse_spec(&plan);
                    let _ = store
                        .set_ticket_spec_fields(
                            sess.ticket_id,
                            &f.spec,
                            &f.acceptance_criteria,
                            &f.relevant_paths,
                            &f.constraints,
                        )
                        .await;
                    let _ = store.set_ticket_drafting(sess.ticket_id, false).await;
                    let _ = store.mark_ticket_grilled(sess.ticket_id).await;
                    // Tell the executor the spec is ready (it stops the grill and, if the ticket
                    // is In Progress, starts the implement session).
                    if let Some(tx) = &ctx.events {
                        let _ = tx.send(SystemEvent::GrillFinished {
                            ticket_id: sess.ticket_id,
                        });
                    }
                }
            }

            // Address session: a plan-file write / ExitPlanMode means feedback contradicted the
            // spec, so Claude proposed a revised spec. Store it as a *proposed* spec for the user
            // to accept/reject in the Spec tab — never overwrite the live spec here (propose &
            // confirm). The session keeps running to implement the non-contradicting feedback.
            if sess.kind == "address" && event == "PreToolUse" {
                if let Some(plan) = extract_plan(&v, tool) {
                    let _ = store.set_ticket_proposed_spec(sess.ticket_id, &plan).await;
                }
            }

            // Review session (plan mode): completion is the moment it presents its verdict via
            // `ExitPlanMode` (or, older Claude, a `/.claude/plans/` write) — captured by
            // `extract_plan`. That plan IS the full review, so capturing it = completion: store it and
            // emit `ReviewFinished` to tear the session down + fingerprint the reviewed HEAD. This is
            // the single definitive signal (the review `Stop` is deliberately NOT mapped to
            // ReviewFinished — an interactive turn can yield mid-review). Idempotent: a repeat lands
            // on an already-ended session and is ignored.
            if sess.kind == "review" && event == "PreToolUse" {
                if let Some(plan) = extract_plan(&v, tool) {
                    let _ = store.set_ticket_review_text(sess.ticket_id, &plan).await;
                    if let Some(tx) = &ctx.events {
                        let _ = tx.send(SystemEvent::ReviewFinished {
                            ticket_id: sess.ticket_id,
                        });
                    }
                }
                // Auto-approve the review's tool calls so plan mode doesn't stall on a bash approval
                // prompt (it runs the test suite). Plan mode still blocks edits, so read-only holds.
                // Exclude ExitPlanMode/AskUserQuestion — those are captured/surfaced above, not
                // silently allowed (allowing ExitPlanMode would exit plan mode into edit mode).
                if !matches!(tool, Some("ExitPlanMode") | Some("AskUserQuestion")) {
                    response = json!({
                        "hookSpecificOutput": {
                            "hookEventName": "PreToolUse",
                            "permissionDecision": "allow",
                            "permissionDecisionReason": "harmony: auto-approved read-only review tool call"
                        }
                    });
                }
            }

            // Proof session: the grounded report is written to a plan file — capture it as the
            // ticket's `proof` text. Unlike review, the proof session runs with execute perms (it
            // records the app / runs tests), so it fires a real `Stop`; completion + artifact
            // collection happens there (`ProofFinished`), not here — the session keeps running after
            // the report write to finish any capture.
            if sess.kind == "proof" && event == "PreToolUse" {
                if let Some(plan) = extract_plan(&v, tool) {
                    let _ = store.set_ticket_proof(sess.ticket_id, &plan).await;
                    // Durably record proof completion AT CAPTURE, stamped with the worktree HEAD — not
                    // at the session's `Stop` (which a restart/missed hook can lose, causing proof to
                    // be redone). Scan the artifact dir (already populated before the report write) so
                    // the PR comment has the media regardless of how the session ends. The idempotency
                    // gate reads this log row, so an unchanged HEAD won't re-run proof after a restart.
                    let ticket_id = sess.ticket_id;
                    let wt = cwd_canon.clone();
                    let (head, artifacts_json) = tokio::task::spawn_blocking(move || {
                        let head = crate::github::head_sha(&wt).unwrap_or_default();
                        let dir = crate::settings::proof_artifact_dir(ticket_id)
                            .to_string_lossy()
                            .to_string();
                        let arts = crate::proof::scan_artifacts(&dir);
                        let json = serde_json::to_string(&arts).unwrap_or_else(|_| "[]".into());
                        (head, json)
                    })
                    .await
                    .unwrap_or_default();
                    if !head.is_empty() {
                        let _ = store
                            .mark_proof_done(sess.ticket_id, &head, &artifacts_json)
                            .await;
                    }
                }
            }

            log_event(&format!(
                "[hook] {event} ticket=#{} session=#{} tool={}",
                sess.ticket_id,
                sess.id,
                tool.unwrap_or("-")
            ));
        }
        Ok(None) => log_event(&format!(
            "[hook] {event} (no live session for cwd={cwd_canon})"
        )),
        Err(e) => log_event(&format!("[hook] {event} store error: {e}")),
    }

    Json(response) // supervised by default; auto-approve for review tool calls (see above)
}

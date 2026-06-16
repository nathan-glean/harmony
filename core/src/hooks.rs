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

use crate::store::Store;

/// Append a hook event to `~/.harmony/harmony.log`. We must NOT print hook events to
/// stdout while a session's terminal is bridged there — it corrupts the Claude TUI.
fn log_event(line: &str) {
    use std::io::Write;
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let path = std::path::Path::new(&home).join(".harmony").join("harmony.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{line}");
    }
}

fn router(store: Arc<Store>) -> Router {
    Router::new()
        .route("/hook/:event", post(handle))
        .with_state(store)
}

/// Bind + spawn the server as a background task (used when also running a session).
pub async fn spawn_server(store: Arc<Store>, port: u16) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let app = router(store);
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok(())
}

/// Bind + serve forever (the `harmony serve` debug command).
pub async fn serve_forever(store: Arc<Store>, port: u16) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("[harmony] hook server on http://127.0.0.1:{port}");
    axum::serve(listener, router(store)).await?;
    Ok(())
}

async fn handle(
    Path(event): Path<String>,
    State(store): State<Arc<Store>>,
    body: Bytes,
) -> Json<Value> {
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let cwd = v.get("cwd").and_then(|x| x.as_str()).unwrap_or("");
    let claude_id = v.get("session_id").and_then(|x| x.as_str());
    let tool = v.get("tool_name").and_then(|x| x.as_str());

    let cwd_canon = std::fs::canonicalize(cwd)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| cwd.to_string());

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
            let _ = store.set_session_state(sess.id, session_state, tool).await;
            // A spec/grill session must not drag its draft ticket onto the board
            // (it stays a Todo draft); only work sessions drive ticket status.
            if sess.kind == "work" {
                let _ = store.set_ticket_status(sess.ticket_id, ticket_status).await;
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
                let plan = match tool {
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
                        // Only the plan file is the spec — not some other file the grill writes.
                        if path.contains("/.claude/plans/") {
                            ti.and_then(|t| t.get("content"))
                                .and_then(|x| x.as_str())
                                .map(|s| s.to_string())
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                if let Some(plan) = plan {
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
                }
            }

            log_event(&format!(
                "[hook] {event} ticket=#{} session=#{} tool={}",
                sess.ticket_id,
                sess.id,
                tool.unwrap_or("-")
            ));
        }
        Ok(None) => log_event(&format!("[hook] {event} (no live session for cwd={cwd_canon})")),
        Err(e) => log_event(&format!("[hook] {event} store error: {e}")),
    }

    Json(json!({})) // supervised: no programmatic decision
}

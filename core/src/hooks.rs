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

    match store.active_session_by_worktree_path(&cwd_canon).await {
        Ok(Some(sess)) => {
            if sess.claude_session_id.is_none() {
                if let Some(cid) = claude_id {
                    let _ = store.set_session_claude_id(sess.id, cid).await;
                }
            }
            let (session_state, ticket_status) = match event.as_str() {
                "Stop" | "Notification" => ("waiting", crate::status::WAITING),
                _ => ("working", crate::status::WORKING),
            };
            let _ = store.set_session_state(sess.id, session_state, tool).await;
            let _ = store.set_ticket_status(sess.ticket_id, ticket_status).await;
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

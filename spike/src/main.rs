//! harmony — Phase 0 de-risking spike (Task 0.1).
//!
//! Proves the single assumption the whole design leans on:
//!   1. Claude Code's HTTP hooks fire for an *interactive* (PTY-hosted) session.
//!   2. A hook can RETURN a permission decision that actually controls the session
//!      (so harmony can build a programmatic approve/deny path, not just scrape stdout).
//!
//! What it does:
//!   - starts a localhost HTTP server that logs every hook POST and answers
//!     PreToolUse/PermissionRequest with a configurable permissionDecision;
//!   - writes a per-project `.claude/settings.json` wiring all hooks to that server;
//!   - spawns `claude` inside a real PTY (cwd = scratch repo) with an initial prompt
//!     that forces a tool use needing permission;
//!   - bridges your terminal <-> the PTY so you can watch/answer;
//!   - prints a PASS/MISS summary of which hook events were received.
//!
//! Run:  cargo run            (uses ./spike-scratch, DECISION=allow, port 8787)
//! Env:  HARMONY_SPIKE_PORT, HARMONY_SPIKE_DECISION=allow|deny|ask, HARMONY_SPIKE_PROMPT
//! Arg:  optional scratch dir as argv[1]

use std::io::{Read, Write as _};
use std::sync::{Arc, Mutex};

use axum::{
    body::Bytes,
    extract::{Path, State},
    routing::post,
    Json, Router,
};
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::{json, Value};

#[derive(Default)]
struct SpikeState {
    events: Vec<String>,
    session_id: Option<String>,
}

type Shared = (Arc<Mutex<SpikeState>>, String); // (state, decision policy)

/// Append a line to `spike.log` (a clean channel that survives the Claude TUI
/// wiping the alternate screen on exit). Best-effort.
fn log_line(line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("spike.log")
    {
        let _ = writeln!(f, "{line}");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port: u16 = std::env::var("HARMONY_SPIKE_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8787);
    let decision = std::env::var("HARMONY_SPIKE_DECISION").unwrap_or_else(|_| "allow".to_string());
    let scratch = std::env::args().nth(1).unwrap_or_else(|| "spike-scratch".to_string());
    let prompt = std::env::var("HARMONY_SPIKE_PROMPT").unwrap_or_else(|_| {
        "Use the Write tool to create a file named spike-proof.txt containing 'harmony hook spike OK', \
         then use the Read tool to read it back."
            .to_string()
    });

    let state = Arc::new(Mutex::new(SpikeState::default()));

    // --- hook server -------------------------------------------------------
    let app = Router::new()
        .route("/hook/:event", post(hook_handler))
        .with_state((state.clone(), decision.clone()));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    println!("[spike] hook server listening on http://127.0.0.1:{port}");
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("[spike] server error: {e}");
        }
    });

    // --- scratch repo + settings.json -------------------------------------
    prepare_scratch(&scratch, port)?;
    // Reset the proof file so the Write always exercises the allow/deny path cleanly.
    let _ = std::fs::remove_file(std::path::Path::new(&scratch).join("spike-proof.txt"));
    log_line(&format!("===== spike run: decision={decision} port={port} ====="));
    println!("[spike] scratch repo:     {scratch}");
    println!("[spike] clean log:        spike/spike.log  (read this if the TUI garbles stdout)");
    println!("[spike] decision policy:  HARMONY_SPIKE_DECISION={decision}");
    println!("[spike] launching `claude` in a PTY...");
    println!("[spike] (if Claude asks to trust this folder / its hooks, answer yes)\n");

    // --- run claude in a PTY (blocking) -----------------------------------
    let scratch_run = scratch.clone();
    let exit = tokio::task::spawn_blocking(move || run_claude(&scratch_run, &prompt)).await??;

    print_summary(&state, &scratch, exit);
    Ok(())
}

/// Receives every Claude Code hook POST. Logs it, records the event, and for
/// PreToolUse / PermissionRequest returns the configured permission decision.
async fn hook_handler(
    Path(event): Path<String>,
    State((state, decision)): State<Shared>,
    body: Bytes,
) -> Json<Value> {
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let session_id = parsed
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    // NB: the hook payload names this `tool_name` (not `tool`) on Claude Code v2.1.173.
    let tool = parsed
        .get("tool_name")
        .or_else(|| parsed.get("tool"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let tool_input = parsed.get("tool_input").cloned().unwrap_or(Value::Null);

    println!("\n========== HOOK: {event} ==========");
    if let Some(sid) = &session_id {
        println!("  session_id : {sid}");
    }
    if !tool.is_empty() {
        println!("  tool       : {tool}");
    }
    if !tool_input.is_null() {
        println!(
            "  tool_input : {}",
            serde_json::to_string(&tool_input).unwrap_or_default()
        );
    }
    println!(
        "  raw        : {}",
        serde_json::to_string(&parsed).unwrap_or_default()
    );

    log_line(&format!(
        "HOOK {event} session={} tool={}",
        session_id.as_deref().unwrap_or(""),
        tool
    ));

    {
        let mut s = state.lock().unwrap();
        s.events.push(event.clone());
        if s.session_id.is_none() {
            s.session_id = session_id;
        }
    }

    // For the two permission-gating events, try to control the session.
    // We return BOTH the modern `hookSpecificOutput` shape and a top-level
    // `permissionDecision` so the spike works across Claude Code versions;
    // unknown fields are ignored. Watch stdout to see which one takes effect.
    match event.as_str() {
        "PreToolUse" | "PermissionRequest" => {
            println!("  --> responding permissionDecision = {decision}");
            Json(json!({
                "hookSpecificOutput": {
                    "hookEventName": event,
                    "permissionDecision": decision,
                    "permissionDecisionReason": "harmony spike"
                },
                "permissionDecision": decision
            }))
        }
        _ => Json(json!({})),
    }
}

/// Create the scratch repo and write `.claude/settings.json` wiring hooks to us.
fn prepare_scratch(dir: &str, port: u16) -> anyhow::Result<()> {
    use std::fs;
    let p = std::path::Path::new(dir);
    fs::create_dir_all(p.join(".claude"))?;
    if !p.join(".git").exists() {
        // Realistic context; ignore failure (hooks don't strictly need git).
        let _ = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(p)
            .status();
    }

    let base = format!("http://127.0.0.1:{port}/hook");
    let http = |event: &str, timeout: u32| {
        json!({ "type": "http", "url": format!("{base}/{event}"), "timeout": timeout })
    };
    let settings = json!({
        "hooks": {
            "SessionStart":      [{ "hooks": [http("SessionStart", 10)] }],
            "PreToolUse":        [{ "matcher": "*", "hooks": [http("PreToolUse", 30)] }],
            "PermissionRequest": [{ "matcher": "*", "hooks": [http("PermissionRequest", 30)] }],
            "PostToolUse":       [{ "matcher": "*", "hooks": [http("PostToolUse", 10)] }],
            "Notification":      [{ "hooks": [http("Notification", 10)] }],
            "Stop":              [{ "hooks": [http("Stop", 10)] }],
            "SessionEnd":        [{ "hooks": [http("SessionEnd", 10)] }]
        }
    });
    fs::write(
        p.join(".claude/settings.json"),
        serde_json::to_string_pretty(&settings)?,
    )?;
    Ok(())
}

/// Spawn `claude` interactively in a PTY and bridge it to this terminal.
fn run_claude(dir: &str, prompt: &str) -> anyhow::Result<portable_pty::ExitStatus> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows: 40,
        cols: 120,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let mut cmd = CommandBuilder::new("claude");
    cmd.cwd(dir);
    cmd.arg("--permission-mode");
    cmd.arg("default"); // default => Write/Bash will need permission => exercises the hooks
    cmd.arg(prompt);

    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    // PTY -> our stdout (detached; dies with the process — we must NOT join it,
    // or shutdown hangs because the reader can block instead of seeing EOF).
    let mut reader = pair.master.try_clone_reader()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut stdout = std::io::stdout();
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = stdout.write_all(&buf[..n]);
                    let _ = stdout.flush();
                }
            }
        }
    });

    // our stdin -> PTY (so you can answer prompts when DECISION=ask)
    let mut writer = pair.master.take_writer()?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut stdin = std::io::stdin();
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = writer.write_all(&buf[..n]);
                    let _ = writer.flush();
                }
            }
        }
    });

    let status = child.wait()?;
    drop(pair.master);
    Ok(status)
}

fn print_summary(state: &Arc<Mutex<SpikeState>>, scratch: &str, exit: portable_pty::ExitStatus) {
    use std::fmt::Write as _;
    let s = state.lock().unwrap();
    let seen = |name: &str| s.events.iter().any(|e| e == name);
    let mark = |b: bool| if b { "PASS" } else { "MISS" };

    let mut o = String::new();
    let _ = writeln!(o, "\n\n================ SPIKE SUMMARY ================");
    let _ = writeln!(o, "claude exit         : {exit:?}");
    let _ = writeln!(
        o,
        "session_id observed : {}",
        s.session_id.clone().unwrap_or_else(|| "<none>".into())
    );
    let _ = writeln!(o, "events received     : {:?}", s.events);
    let _ = writeln!(o, "  [{}] SessionStart", mark(seen("SessionStart")));
    let _ = writeln!(o, "  [{}] PreToolUse        (critical: gates tool use)", mark(seen("PreToolUse")));
    let _ = writeln!(o, "  [{}] PermissionRequest (may not fire if PreToolUse already decided)", mark(seen("PermissionRequest")));
    let _ = writeln!(o, "  [{}] PostToolUse", mark(seen("PostToolUse")));
    let _ = writeln!(o, "  [{}] Stop", mark(seen("Stop")));
    let _ = writeln!(o, "  [{}] SessionEnd", mark(seen("SessionEnd")));
    let _ = writeln!(o, "\nManual checks (the real point of the spike):");
    let _ = writeln!(o, "  1. With DECISION=allow, did the Write run WITHOUT a TUI prompt?  -> programmatic control works");
    let _ = writeln!(o, "  2. cat {scratch}/spike-proof.txt   (file should exist if allow worked)");
    let _ = writeln!(o, "  3. With DECISION=deny, was the Write blocked?                   -> deny path works");
    let _ = writeln!(o, "  4. Find the transcript: ls ~/.claude/projects/*/<session_id>.jsonl");
    let _ = writeln!(o, "===============================================");

    print!("{o}");
    let _ = std::io::stdout().flush();
    log_line(&o); // also persisted to spike.log, which the TUI can't wipe
}

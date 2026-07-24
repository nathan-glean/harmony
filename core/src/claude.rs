//! Shared launcher for one-shot headless `claude -p` calls (judges, triage, PR-description drafting).
//!
//! Every non-interactive Claude call in harmony is a read-only plan-mode classifier that emits a
//! short structured verdict or a diff summary. They share the same launch shape, so it lives here
//! once: build `claude -p <prompt> --permission-mode plan [--model <model>]`, pipe `stdin_body`, and
//! return stdout (or a `cmd_err`-classified error).
//!
//! Pinning these calls to a cheap `model` (see [`DEFAULT_TRIAGE_MODEL`]) is the single biggest token
//! lever — the interactive coding sessions in `session.rs` are deliberately left on the user's
//! default model. An empty `model` means "use the account default" (no `--model` flag).
//!
//! Note: `claude -p` counts against separate Agent-SDK usage credits (Phase 0 finding).

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;

/// Default model for headless triage/judgment calls — cheap and fast, ample for one-line verdicts
/// and diff summaries. The Tauri/CLI layer reads the user's configured `triage_model` and passes it
/// down; this is the fallback when the setting is unset.
pub const DEFAULT_TRIAGE_MODEL: &str = "claude-haiku-4-5";

/// The `claude` CLI args for a headless call. Pure, for testability. `--model` is added only when
/// `model` is non-empty (empty ⇒ the account default model).
fn build_args(prompt: &str, model: &str) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--permission-mode".to_string(),
        "plan".to_string(),
    ];
    if !model.trim().is_empty() {
        args.push("--model".to_string());
        args.push(model.trim().to_string());
    }
    args
}

/// Run a one-shot read-only `claude -p` in `worktree` with `stdin_body` piped in; return stdout.
/// When `model` is non-empty it is passed as `--model`, otherwise the account default is used.
pub fn run_headless(worktree: &str, prompt: &str, stdin_body: &str, model: &str) -> Result<String> {
    let mut child = Command::new("claude")
        .args(build_args(prompt, model))
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| crate::cmd_err::spawn_error("claude", &e))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_body.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| crate::cmd_err::spawn_error("claude", &e))?;
    if !out.status.success() {
        return Err(crate::cmd_err::classify(
            "claude",
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_include_model_when_set() {
        let args = build_args("hello", "claude-haiku-4-5");
        assert_eq!(
            args,
            vec![
                "-p",
                "hello",
                "--permission-mode",
                "plan",
                "--model",
                "claude-haiku-4-5"
            ]
        );
    }

    #[test]
    fn args_omit_model_when_empty() {
        // Empty (or whitespace) model ⇒ no --model flag ⇒ the account default is used.
        for m in ["", "   "] {
            let args = build_args("hello", m);
            assert_eq!(args, vec!["-p", "hello", "--permission-mode", "plan"]);
            assert!(!args.iter().any(|a| a == "--model"));
        }
    }

    #[test]
    fn args_trim_model() {
        let args = build_args("p", "  claude-haiku-4-5  ");
        assert_eq!(args.last().unwrap(), "claude-haiku-4-5");
    }
}

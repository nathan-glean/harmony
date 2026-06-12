//! "Draft from Jira" (DESIGN Q10): a one-shot `claude -p` that expands a terse Jira
//! issue into an editable first-pass agent spec. Run in a temp dir so it's a pure text
//! transform (no repo scan in Phase 2; the optional repo-aware draft is a follow-up).
//!
//! Note: `claude -p` counts against separate Agent-SDK usage credits (Phase 0 finding).

use anyhow::{anyhow, Result};
use std::process::Command;

pub fn draft_spec(summary: &str, description: &str) -> Result<String> {
    let desc = if description.trim().is_empty() {
        "(no description provided)"
    } else {
        description
    };
    let prompt = format!(
        "You are drafting an implementation spec for a coding agent from a Jira ticket.\n\n\
         Jira summary: {summary}\n\nJira description:\n{desc}\n\n\
         Write a concise, actionable spec in markdown with these sections: Goal, Context, \
         Relevant files (best guess), Acceptance criteria, Out of scope. Output ONLY the \
         spec markdown, no preamble."
    );

    let out = Command::new("claude")
        .args(["-p", &prompt])
        .current_dir(std::env::temp_dir())
        .output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "claude -p failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

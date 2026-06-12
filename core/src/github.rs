//! GitHub PR creation via `gh` (DESIGN Q11): push the worktree's branch and open a
//! draft PR. harmony never merges — hand off to normal review.

use anyhow::{anyhow, Result};
use std::process::Command;

fn run(cmd: &str, args: &[&str], cwd: &str) -> Result<String> {
    let out = Command::new(cmd).args(args).current_dir(cwd).output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "{cmd} {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn push_branch(worktree: &str, branch: &str) -> Result<()> {
    run("git", &["push", "-u", "origin", branch], worktree)?;
    Ok(())
}

/// Open a draft PR; returns the PR URL (`gh` prints it to stdout).
pub fn create_draft_pr(worktree: &str, title: &str, body: &str, branch: &str) -> Result<String> {
    let out = run(
        "gh",
        &[
            "pr", "create", "--draft", "--title", title, "--body", body, "--head", branch,
        ],
        worktree,
    )?;
    let url = out
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with("http"))
        .map(|l| l.trim().to_string())
        .unwrap_or_else(|| out.trim().to_string());
    Ok(url)
}

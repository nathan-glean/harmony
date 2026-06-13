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

/// Diff of the worktree (committed + uncommitted) against the merge-base with `base` —
/// i.e. everything this branch changed. Empty string if no changes.
pub fn diff(worktree: &str, base: &str) -> Result<String> {
    run("git", &["diff", "--merge-base", base], worktree)
}

/// `gh pr view --json …` for the branch's PR (Err if there's no PR).
pub fn pr_view_json(worktree: &str) -> Result<String> {
    run("gh", &["pr", "view", "--json", "number,title,url,state,isDraft"], worktree)
}

/// `gh pr checks --json …`. NOTE: `gh pr checks` exits non-zero when checks are failing or
/// pending, but still prints the JSON — so we read stdout regardless of exit code.
pub fn pr_checks_json(worktree: &str) -> Result<String> {
    let out = Command::new("gh")
        .args(["pr", "checks", "--json", "name,state,link,bucket"])
        .current_dir(worktree)
        .output()?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if stdout.trim().is_empty() {
        return Err(anyhow!("{}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(stdout)
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

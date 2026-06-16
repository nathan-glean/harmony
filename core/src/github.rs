//! GitHub PR operations via `gh` (DESIGN Q11): push the worktree's branch, open a draft PR,
//! read PR status/approval, and squash-merge once approved (on the move to Done).

use anyhow::{anyhow, Result};
use serde::Serialize;
use std::process::Command;

fn run(cmd: &str, args: &[&str], cwd: &str) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| crate::cmd_err::spawn_error(cmd, &e))?;
    if !out.status.success() {
        return Err(crate::cmd_err::classify(cmd, &String::from_utf8_lossy(&out.stderr)));
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

/// Build a PR body: a Claude-generated summary of the branch diff (conforming to the repo's PR
/// template when one exists, and referencing `ticket_ref`), falling back to `fallback` (the
/// composed spec) whenever a summary can't be produced — no diff, or `claude`/`git` unavailable.
/// The single place the "generated summary when available, else spec" rule lives.
pub fn generated_pr_body(
    worktree: &str,
    repo_path: &str,
    ticket_ref: Option<&str>,
    fallback: &str,
) -> String {
    let base = crate::worktree::default_branch(repo_path).unwrap_or_else(|_| "main".into());
    let diff = match diff(worktree, &base) {
        Ok(d) if !d.trim().is_empty() => d,
        _ => return fallback.to_string(),
    };
    match crate::draft::pr_summary(worktree, &diff, ticket_ref) {
        Ok(s) if !s.trim().is_empty() => s,
        _ => fallback.to_string(),
    }
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
        .output()
        .map_err(|e| crate::cmd_err::spawn_error("gh", &e))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if stdout.trim().is_empty() {
        return Err(crate::cmd_err::classify("gh", &String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout)
}

/// The branch's HEAD commit SHA — used to fingerprint "the current change-set" so `/review`
/// isn't re-run when nothing has changed since the last review.
pub fn head_sha(worktree: &str) -> Result<String> {
    Ok(run("git", &["rev-parse", "HEAD"], worktree)?.trim().to_string())
}

/// PR status for the worktree's branch. `exists` is false when there is no PR (or `gh` can't
/// answer) — that's a normal state, not an error, so this never returns `Err`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PrStatus {
    pub exists: bool,
    /// GitHub `reviewDecision == "APPROVED"`.
    pub approved: bool,
    /// `OPEN` | `MERGED` | `CLOSED` (empty when no PR).
    pub state: String,
    pub is_draft: bool,
    pub url: String,
}

/// Read the branch's PR status + approval via `gh pr view`. No PR / `gh` unavailable → a
/// default (`exists: false`) status.
pub fn pr_status(worktree: &str) -> PrStatus {
    match run(
        "gh",
        &["pr", "view", "--json", "number,state,isDraft,url,reviewDecision"],
        worktree,
    ) {
        Ok(json) => parse_pr_status(&json),
        Err(_) => PrStatus::default(),
    }
}

/// Parse `gh pr view --json …` output into a `PrStatus` (split out for testing).
fn parse_pr_status(json: &str) -> PrStatus {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let exists = v.get("number").map(|n| !n.is_null()).unwrap_or(false);
    PrStatus {
        exists,
        approved: v.get("reviewDecision").and_then(|x| x.as_str()) == Some("APPROVED"),
        state: v.get("state").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        is_draft: v.get("isDraft").and_then(|x| x.as_bool()).unwrap_or(false),
        url: v.get("url").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    }
}

/// Squash-merge the branch's PR and delete the remote branch (on the move to Done, once the PR
/// is approved on GitHub). harmony only merges here — never mid-flow.
pub fn merge_pr(worktree: &str) -> Result<()> {
    run("gh", &["pr", "merge", "--squash", "--delete-branch"], worktree)?;
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
    // `gh pr create` succeeded; the PR URL is the last http line it printed. If we can't find
    // one, don't hand the raw output back as a "URL" — surface it as an error instead.
    out.lines()
        .rev()
        .find(|l| l.trim_start().starts_with("http"))
        .map(|l| l.trim().to_string())
        .ok_or_else(|| anyhow!("gh pr create did not return a PR URL: {}", out.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_status_approved() {
        let s = parse_pr_status(
            r#"{"number":12,"state":"OPEN","isDraft":false,"url":"https://x/pr/12","reviewDecision":"APPROVED"}"#,
        );
        assert!(s.exists && s.approved && !s.is_draft);
        assert_eq!(s.state, "OPEN");
        assert_eq!(s.url, "https://x/pr/12");
    }

    #[test]
    fn parse_pr_status_unreviewed_draft() {
        let s = parse_pr_status(
            r#"{"number":3,"state":"OPEN","isDraft":true,"url":"u","reviewDecision":"REVIEW_REQUIRED"}"#,
        );
        assert!(s.exists && !s.approved && s.is_draft);
    }

    #[test]
    fn parse_pr_status_no_pr() {
        // `gh` prints an empty object / null fields when there's no PR.
        assert_eq!(parse_pr_status("{}"), PrStatus::default());
        assert_eq!(parse_pr_status(""), PrStatus::default());
    }
}

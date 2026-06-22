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

/// SHA a ref resolves to. Tries the remote-tracking `origin/<ref>` first (so a bare base-branch
/// name resolves to the commit CI actually ran against), then the bare ref.
pub fn rev_parse(worktree: &str, refname: &str) -> Result<String> {
    if let Ok(s) = run("git", &["rev-parse", &format!("origin/{refname}")], worktree) {
        return Ok(s.trim().to_string());
    }
    Ok(run("git", &["rev-parse", refname], worktree)?.trim().to_string())
}

/// Workflow runs for `branch` (`gh run list --json …`) — used to map a failing PR check to the
/// run id whose logs we need, filtered by HEAD sha downstream. Raw JSON array string.
pub fn run_list_json(worktree: &str, branch: &str) -> Result<String> {
    run(
        "gh",
        &[
            "run", "list", "--branch", branch, "--limit", "40",
            "--json", "databaseId,headSha,conclusion,status,name,workflowName",
        ],
        worktree,
    )
}

/// The failed-step logs for a workflow run (`gh run view <id> --log-failed`). Can be large — the
/// caller truncates before feeding it to the model. `gh` exits non-zero on some run states but
/// still prints logs, so read stdout regardless of exit code (like `pr_checks_json`).
pub fn failed_logs(worktree: &str, run_id: i64) -> Result<String> {
    let id = run_id.to_string();
    let out = Command::new("gh")
        .args(["run", "view", &id, "--log-failed"])
        .current_dir(worktree)
        .output()
        .map_err(|e| crate::cmd_err::spawn_error("gh", &e))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if stdout.trim().is_empty() && !out.status.success() {
        return Err(crate::cmd_err::classify("gh", &String::from_utf8_lossy(&out.stderr)));
    }
    Ok(stdout)
}

/// Check-run results for a commit (`gh api repos/{owner}/{repo}/commits/<sha>/check-runs`). Used to
/// test whether a check is *already red on the base branch* (i.e. not caused by this PR). `gh`
/// substitutes `{owner}/{repo}` from the repo in `worktree`. Raw JSON object string.
pub fn check_runs_json(worktree: &str, sha: &str) -> Result<String> {
    run(
        "gh",
        &["api", &format!("repos/{{owner}}/{{repo}}/commits/{sha}/check-runs"), "--paginate"],
        worktree,
    )
}

/// Branch-protection required status checks (`gh api …/branches/<base>/protection/required_status_checks`).
/// 404s (no protection) / permission errors surface as `Err`; the caller treats that as "unknown".
pub fn required_checks_json(worktree: &str, base: &str) -> Result<String> {
    run(
        "gh",
        &["api", &format!("repos/{{owner}}/{{repo}}/branches/{base}/protection/required_status_checks")],
        worktree,
    )
}

/// Stage everything and commit it (`git add -A` + `git commit`), if there's anything to commit.
/// Returns whether a commit was actually made (false when the worktree is clean). harmony owns
/// committing the agent's work — deterministic, and the squash-merge makes granularity moot.
pub fn commit_all(worktree: &str, message: &str) -> Result<bool> {
    run("git", &["add", "-A"], worktree)?;
    // `diff --cached --quiet` exits 1 when there ARE staged changes.
    let staged = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(worktree)
        .status()
        .map(|s| !s.success())
        .unwrap_or(false);
    if !staged {
        return Ok(false);
    }
    run("git", &["commit", "-q", "-m", message], worktree)?;
    Ok(true)
}

/// Number of commits the branch is ahead of `base` (`git rev-list --count <base>..HEAD`).
/// Used to refuse opening an empty PR with a clear message instead of a cryptic `gh` failure.
pub fn commits_ahead(worktree: &str, base: &str) -> usize {
    run("git", &["rev-list", "--count", &format!("{base}..HEAD")], worktree)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
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

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(Command::new("git").arg("-C").arg(dir).args(args).status().unwrap().success());
    }

    #[test]
    fn commit_all_commits_when_dirty_and_noops_when_clean() {
        let dir = std::env::temp_dir().join(format!("harmony-gh-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.to_string_lossy().to_string();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.email", "t@h.local"]);
        git(&dir, &["config", "user.name", "T"]);
        std::fs::write(dir.join("README.md"), "hi").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);

        assert_eq!(commits_ahead(&p, "main"), 0);
        // Clean tree → no-op.
        assert_eq!(commit_all(&p, "noop").unwrap(), false);

        // Dirty → commits, HEAD advances, but still 0 ahead of main (we committed onto main here).
        std::fs::write(dir.join("new.txt"), "x").unwrap();
        assert_eq!(commit_all(&p, "feat: add").unwrap(), true);
        assert_eq!(commit_all(&p, "again").unwrap(), false); // now clean

        let _ = std::fs::remove_dir_all(&dir);
    }
}

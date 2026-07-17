//! Git worktree management (DESIGN Q8/Q9): one reused worktree per ticket, created
//! off the repo's fresh default branch, under `~/.harmony/worktrees/<repo>/<branch>`,
//! on branch `harmony/<KEY|local-id>-<slug>`.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::models::Ticket;

fn git(repo: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Best-effort default branch: prefer `origin/HEAD`, else current `HEAD`.
pub fn default_branch(repo: &str) -> Result<String> {
    if let Ok(s) = git(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        if let Some(b) = s.trim().strip_prefix("origin/") {
            if !b.is_empty() {
                return Ok(b.to_string());
            }
        }
    }
    Ok(git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string())
}

pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(40).collect()
}

pub fn branch_name(t: &Ticket) -> String {
    let id = t
        .jira_key
        .clone()
        .unwrap_or_else(|| format!("local-{}", t.id));
    format!("harmony/{}-{}", id, slugify(&t.title))
}

pub fn worktree_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".harmony").join("worktrees")
}

pub fn worktree_path(repo_name: &str, branch: &str) -> PathBuf {
    // branch contains '/', which we flatten for the directory name
    worktree_root()
        .join(repo_name)
        .join(branch.replace('/', "__"))
}

/// Create a worktree for `branch`, fetching first (best-effort). Creates the branch off
/// `base` if it doesn't exist yet, or reuses it if it does (e.g. the worktree was deleted
/// on Done but the branch — possibly with an open PR — was kept).
pub fn create(repo: &str, base: &str, branch: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = git(repo, &["fetch", "--quiet"]);
    // Clean up any stale worktree registration pointing at a since-removed dir.
    let _ = git(repo, &["worktree", "prune"]);

    let branch_exists = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    let dest = dest.to_string_lossy();
    if branch_exists {
        git(repo, &["worktree", "add", &dest, branch])?;
    } else {
        git(repo, &["worktree", "add", "-b", branch, &dest, base])?;
    }
    Ok(())
}

pub fn remove(repo: &str, dest: &Path) -> Result<()> {
    git(
        repo,
        &["worktree", "remove", &dest.to_string_lossy(), "--force"],
    )?;
    Ok(())
}

/// Number of uncommitted changes (staged, unstaged, or untracked) in a worktree, via
/// `git status --porcelain`. Used to gate destructive removal so we never silently
/// `--force` away the user's work.
pub fn uncommitted_count(worktree: &str) -> Result<usize> {
    let out = git(worktree, &["status", "--porcelain"])?;
    Ok(out.lines().filter(|l| !l.trim().is_empty()).count())
}

/// Whether a worktree has uncommitted changes.
pub fn is_dirty(worktree: &str) -> Result<bool> {
    Ok(uncommitted_count(worktree)? > 0)
}

/// Best-effort removal of all git worktrees for a ticket (used before deleting it).
/// Errors (e.g. a dirty or in-use worktree) are ignored.
pub async fn cleanup_for_ticket(store: &crate::store::Store, ticket_id: i64) {
    if let Ok(worktrees) = store.worktrees_for_ticket(ticket_id).await {
        for wt in worktrees {
            if let Ok(Some(repo)) = store.get_repo(wt.repo_id).await {
                let _ = remove(&repo.path, Path::new(&wt.path));
            }
        }
    }
}

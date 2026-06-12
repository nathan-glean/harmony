//! Git worktree management (DESIGN Q8/Q9): one reused worktree per ticket, created
//! off the repo's fresh default branch, under `~/.harmony/worktrees/<repo>/<branch>`,
//! on branch `harmony/<KEY|local-id>-<slug>`.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::models::Ticket;

fn git(repo: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("git").arg("-C").arg(repo).args(args).output()?;
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
    if let Ok(s) = git(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        if let Some(b) = s.trim().strip_prefix("origin/") {
            if !b.is_empty() {
                return Ok(b.to_string());
            }
        }
    }
    Ok(git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string())
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
    worktree_root().join(repo_name).join(branch.replace('/', "__"))
}

/// Create a fresh worktree + branch off `base`, fetching first (best-effort).
pub fn create(repo: &str, base: &str, branch: &str, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let _ = git(repo, &["fetch", "--quiet"]);
    git(
        repo,
        &["worktree", "add", "-b", branch, &dest.to_string_lossy(), base],
    )?;
    Ok(())
}

pub fn remove(repo: &str, dest: &Path) -> Result<()> {
    git(repo, &["worktree", "remove", &dest.to_string_lossy(), "--force"])?;
    Ok(())
}

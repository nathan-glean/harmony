//! `claude -p` PR helper: `pr_summary` summarizes a branch diff into a PR description,
//! conforming to the repo's pull-request template when one exists.
//!
//! Note: `claude -p` counts against separate Agent-SDK usage credits (Phase 0 finding).

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;

/// Max diff bytes piped to `claude` for a PR summary — keep the call fast and within context.
const MAX_DIFF_BYTES: usize = 60 * 1024;

/// Summarize a branch diff into a PR description. Runs in the worktree under read-only plan mode
/// so Claude can discover the repo's PR template(s) and conform to the most relevant one; the
/// diff is piped on stdin. `ticket_ref` (e.g. a Jira issue URL) is woven into the body when given.
pub fn pr_summary(worktree: &str, diff: &str, ticket_ref: Option<&str>) -> Result<String> {
    let reference = match ticket_ref {
        Some(r) if !r.trim().is_empty() => {
            format!(" Include this reference to the originating ticket in the body: {r}.")
        }
        _ => String::new(),
    };
    let prompt = format!(
        "The full diff of a git branch (vs its base) is provided on stdin. Write the pull-request \
         description in markdown for these changes.\n\n\
         First check whether this repository defines a pull-request template (e.g. \
         `.github/PULL_REQUEST_TEMPLATE.md`, files under `.github/PULL_REQUEST_TEMPLATE/`, or a \
         root/`docs/` `pull_request_template.md`). If one exists, fill it out faithfully from the \
         diff; if several exist, choose the single most relevant template and fill that one. If \
         there is no template, write a concise description: one short paragraph of what changed \
         and why, then a bulleted list of the notable changes.{reference}\n\n\
         Base the description on what the diff actually changes. Output ONLY the final PR \
         description markdown, with no preamble."
    );

    let mut child = Command::new("claude")
        .arg("-p")
        .arg(&prompt)
        .arg("--permission-mode")
        .arg("plan")
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| crate::cmd_err::spawn_error("claude", &e))?;
    // Pipe the (truncated) diff to stdin, then close it so claude can finish.
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(truncate_diff(diff, MAX_DIFF_BYTES).as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| crate::cmd_err::spawn_error("claude", &e))?;
    if !out.status.success() {
        return Err(crate::cmd_err::classify("claude", &String::from_utf8_lossy(&out.stderr)));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Cap a diff to `max` bytes on a line boundary, appending a truncation marker when cut. Keeps
/// the `claude -p` call fast and inside the context window for very large branches.
pub fn truncate_diff(diff: &str, max: usize) -> String {
    if diff.len() <= max {
        return diff.to_string();
    }
    let cut = diff[..max].rfind('\n').map(|i| i + 1).unwrap_or(max);
    format!("{}\n…(diff truncated)…\n", &diff[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_diff_leaves_short_input() {
        let d = "line one\nline two\n";
        assert_eq!(truncate_diff(d, 1024), d);
    }

    #[test]
    fn truncate_diff_caps_on_line_boundary() {
        let diff = "aaaa\nbbbb\ncccc\ndddd\n"; // 5 bytes/line, 20 total
        let out = truncate_diff(diff, 12); // 12 lands inside "cccc"
        // Kept whole lines only (line boundary), dropped the partial "cccc" and beyond.
        assert!(out.starts_with("aaaa\nbbbb\n"));
        assert!(!out.contains("cccc"));
        assert!(out.contains("(diff truncated)"));
    }
}

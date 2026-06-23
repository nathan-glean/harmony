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
         description markdown itself — no preamble, no explanation, no sign-off. Do NOT wrap the \
         description in a code block or fenced block (no ``` fences around the whole output). \
         Begin your response directly with the first line of the description (the template's \
         first line, or the first heading)."
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
    Ok(sanitize_pr_description(&String::from_utf8_lossy(&out.stdout)))
}

/// Sentinel the model returns when the PR description is still accurate (no update needed).
const NO_UPDATE: &str = "NO_UPDATE";

/// Decide whether a branch's PR description has gone stale (e.g. after a large change during
/// review) and, if so, produce a revised one. Runs `claude -p` in read-only plan mode with the
/// current description + the branch diff on stdin. Returns `Ok(None)` when the description is still
/// accurate, `Ok(Some(body))` with the revised description otherwise.
pub fn maybe_update_pr_description(
    worktree: &str,
    current_body: &str,
    diff: &str,
    ticket_ref: Option<&str>,
) -> Result<Option<String>> {
    let reference = match ticket_ref {
        Some(r) if !r.trim().is_empty() => {
            format!(" Keep the ticket reference in the body: {r}.")
        }
        _ => String::new(),
    };
    let prompt = format!(
        "A pull request's CURRENT description and the branch's full diff (vs base) are provided on \
         stdin, separated by a line `=== DIFF ===`. The code may have changed materially since the \
         description was written (e.g. a large change during review).\n\n\
         If the current description still accurately reflects what the branch does, reply with \
         EXACTLY `{NO_UPDATE}` and nothing else. Otherwise, output the REVISED description: edit the \
         current one in place — preserve its template structure, headings, badges and checkboxes, \
         and change only what is now inaccurate.{reference}\n\n\
         Output ONLY `{NO_UPDATE}` or the final description markdown — no preamble, no explanation, \
         and do NOT wrap the description in a code block or fenced block."
    );
    let stdin_body = format!(
        "{}\n=== DIFF ===\n{}",
        current_body.trim(),
        truncate_diff(diff, MAX_DIFF_BYTES)
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
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_body.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| crate::cmd_err::spawn_error("claude", &e))?;
    if !out.status.success() {
        return Err(crate::cmd_err::classify("claude", &String::from_utf8_lossy(&out.stderr)));
    }
    Ok(interpret_pr_update(&String::from_utf8_lossy(&out.stdout)))
}

/// Interpret the model's response to the description-staleness check: `None` when it declined to
/// update (first non-empty line is the `NO_UPDATE` sentinel), else the sanitized revised body.
fn interpret_pr_update(raw: &str) -> Option<String> {
    let first = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if first.eq_ignore_ascii_case(NO_UPDATE) {
        return None;
    }
    let body = sanitize_pr_description(raw);
    if body.is_empty() {
        None
    } else {
        Some(body)
    }
}

/// Clean up `claude -p`'s output into the bare PR description: trim, and — when the model ignores
/// the prompt and wraps the whole description in a fenced block (often preceded by a reasoning
/// sentence) — drop the preamble and unwrap the fence. A description that merely *contains* code
/// blocks is left untouched (see `unwrap_wrapping_fence`).
fn sanitize_pr_description(raw: &str) -> String {
    let text = raw.trim();
    unwrap_wrapping_fence(text).unwrap_or_else(|| text.to_string()).trim().to_string()
}

/// If `text` is a single fenced block wrapping the entire description (optionally preceded by a
/// preamble), return its inner content; otherwise `None`. Conservative on purpose: only unwraps
/// a clear whole-document wrapper, never a description that legitimately contains code blocks.
fn unwrap_wrapping_fence(text: &str) -> Option<String> {
    let is_fence = |l: &str| {
        let t = l.trim_start();
        t.starts_with("```") || t.starts_with("~~~")
    };
    let lines: Vec<&str> = text.lines().collect();
    let fence_idxs: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| is_fence(l)).map(|(i, _)| i).collect();
    if fence_idxs.len() < 2 {
        return None;
    }
    let open = *fence_idxs.first().unwrap();
    let close = *fence_idxs.last().unwrap();
    if open >= close {
        return None;
    }
    // The wrapper must close at the end of the output (only blank lines may follow).
    if lines[close + 1..].iter().any(|l| !l.trim().is_empty()) {
        return None;
    }
    // Opener language label, e.g. "```markdown" → "markdown".
    let lang = lines[open].trim_start().trim_start_matches(['`', '~']).trim().to_lowercase();
    let first_nonblank = lines.iter().position(|l| !l.trim().is_empty());

    // (a) explicit whole-doc wrapper (```markdown / ```md), preamble allowed; or
    // (b) a bare ``` block wrapping everything, no inner fences and no preamble.
    let is_wrapper = matches!(lang.as_str(), "markdown" | "md")
        || (lang.is_empty() && fence_idxs.len() == 2 && first_nonblank == Some(open));
    if !is_wrapper {
        return None;
    }
    Some(lines[open + 1..close].join("\n"))
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

    #[test]
    fn sanitize_strips_preamble_and_markdown_fence() {
        let raw = "I'll fill out the Analytics template, which fits these changes.\n\n\
                   ```markdown\n## Description\n\nModels the quiz events.\n\n- item one\n```\n";
        let out = sanitize_pr_description(raw);
        assert_eq!(out, "## Description\n\nModels the quiz events.\n\n- item one");
        assert!(!out.contains("I'll fill out"));
        assert!(!out.contains("```"));
    }

    #[test]
    fn sanitize_unwraps_bare_fence_wrapping_whole_output() {
        let raw = "```\n## Title\n\nbody text\n```";
        assert_eq!(sanitize_pr_description(raw), "## Title\n\nbody text");
    }

    #[test]
    fn sanitize_keeps_description_with_inner_code_block() {
        // A legitimate description that merely contains a code block must be left intact.
        let raw = "## Description\n\nRuns this query:\n\n```sql\nSELECT 1;\n```\n\nDone.";
        assert_eq!(sanitize_pr_description(raw), raw);
    }

    #[test]
    fn sanitize_leaves_clean_description_unchanged() {
        let raw = "## Description\n\nA plain description with no fences.";
        assert_eq!(sanitize_pr_description(&format!("  {raw}  \n")), raw);
    }

    // ---- should unwrap (whole-document wrappers) ----

    #[test]
    fn sanitize_unwraps_md_language_label() {
        let raw = "```md\n## Title\n\nbody\n```";
        assert_eq!(sanitize_pr_description(raw), "## Title\n\nbody");
    }

    #[test]
    fn sanitize_unwraps_case_insensitive_language() {
        for label in ["Markdown", "MARKDOWN", "Md"] {
            let raw = format!("```{label}\n## Title\n\nbody\n```");
            assert_eq!(sanitize_pr_description(&raw), "## Title\n\nbody", "label={label}");
        }
    }

    #[test]
    fn sanitize_unwraps_markdown_fence_without_preamble() {
        let raw = "```markdown\n## Title\n\nbody text\n```";
        assert_eq!(sanitize_pr_description(raw), "## Title\n\nbody text");
    }

    #[test]
    fn sanitize_unwraps_tilde_fence_wrapper() {
        let raw = "~~~markdown\n## Title\n\nbody\n~~~";
        assert_eq!(sanitize_pr_description(raw), "## Title\n\nbody");
    }

    #[test]
    fn sanitize_allows_trailing_blank_lines_after_close() {
        let raw = "```markdown\n## Title\n\nbody\n```\n\n   \n\n";
        assert_eq!(sanitize_pr_description(raw), "## Title\n\nbody");
    }

    #[test]
    fn sanitize_preserves_inner_code_block_inside_markdown_wrapper() {
        // The outer ```markdown wrapper is removed, but an inner ```sql block stays intact.
        let raw = "I'll write the description.\n\n\
                   ```markdown\n## Description\n\nRuns:\n\n```sql\nSELECT 1;\n```\n\nDone.\n```";
        let out = sanitize_pr_description(raw);
        assert_eq!(out, "## Description\n\nRuns:\n\n```sql\nSELECT 1;\n```\n\nDone.");
        assert!(out.contains("```sql"));
        assert!(!out.contains("I'll write"));
    }

    // ---- should NOT unwrap (conservative guards) ----

    #[test]
    fn sanitize_does_not_unwrap_bare_fence_with_preamble() {
        // A bare ``` fence with leading prose is ambiguous (could be legit content) — left as-is.
        let raw = "Here is the migration:\n\n```\nSELECT 1;\n```";
        assert_eq!(sanitize_pr_description(raw), raw);
    }

    #[test]
    fn sanitize_keeps_content_when_text_follows_closing_fence() {
        // Non-blank content after the closing fence means it isn't a whole-document wrapper.
        let raw = "```markdown\n## Title\n\nbody\n```\n\nTrailing note outside the fence.";
        assert_eq!(sanitize_pr_description(raw), raw);
    }

    #[test]
    fn sanitize_keeps_unterminated_single_fence() {
        let raw = "```markdown\n## Title\n\nbody with no closing fence";
        assert_eq!(sanitize_pr_description(raw), raw);
    }

    #[test]
    fn sanitize_keeps_two_separate_code_blocks() {
        let raw = "## Steps\n\nFirst:\n\n```sql\nSELECT 1;\n```\n\nThen:\n\n```sql\nSELECT 2;\n```\n\nEnd.";
        assert_eq!(sanitize_pr_description(raw), raw);
    }

    #[test]
    fn sanitize_keeps_inline_code_not_a_fence() {
        // Lines starting with a single/double backtick are inline code, not a ``` fence.
        let raw = "`foo` is the new flag.\n\n``bar`` too.";
        assert_eq!(sanitize_pr_description(raw), raw);
    }

    // ---- misc / robustness ----

    #[test]
    fn sanitize_handles_empty_and_whitespace_only() {
        assert_eq!(sanitize_pr_description(""), "");
        assert_eq!(sanitize_pr_description("   \n\n  \t\n"), "");
    }

    #[test]
    fn interpret_pr_update_no_update_sentinel() {
        assert_eq!(interpret_pr_update("NO_UPDATE"), None);
        assert_eq!(interpret_pr_update("  no_update  \n"), None);
        // Sentinel on the first line wins even if the model rambles after it.
        assert_eq!(interpret_pr_update("NO_UPDATE\nthe description is still fine"), None);
        assert_eq!(interpret_pr_update("   \n\nNO_UPDATE"), None);
    }

    #[test]
    fn interpret_pr_update_returns_revised_body() {
        let out = interpret_pr_update("## Description\n\nNow also adds CSV export.");
        assert_eq!(out.as_deref(), Some("## Description\n\nNow also adds CSV export."));
    }

    #[test]
    fn interpret_pr_update_unwraps_fenced_body() {
        let raw = "```markdown\n## Description\n\nRevised.\n```";
        assert_eq!(interpret_pr_update(raw).as_deref(), Some("## Description\n\nRevised."));
    }

    #[test]
    fn interpret_pr_update_empty_is_none() {
        assert_eq!(interpret_pr_update("   \n  "), None);
    }

    #[test]
    fn sanitize_strips_realistic_full_description() {
        let raw = "I'll fill out the Analytics template, which fits these dbt modelling changes.\n\n\
                   ```markdown\n\
                   <!-- template:analytics -->\n\
                   [![Jira: DNA-1801](https://example/badge)](https://example/browse/DNA-1801)\n\n\
                   ## Description & motivation\n\n\
                   Models the Written Answer Quiz elastic events.\n\n\
                   ## Type of change\n\n\
                   - New model\n\n\
                   ## To-do before merge\n\n\
                   - [ ] Run new models locally\n\
                   ```";
        let out = sanitize_pr_description(raw);
        assert!(out.starts_with("<!-- template:analytics -->"), "got: {out:?}");
        assert!(out.contains("## Description & motivation"));
        assert!(out.contains("- [ ] Run new models locally"));
        assert!(!out.contains("I'll fill out"));
        assert!(!out.contains("```"));
    }
}

//! Autonomous review-loop judge + fix prompt.
//!
//! `judge` is the gate for the self-correcting review loop: it reads a `/review` session's findings
//! plus the branch diff and decides whether the change is ready (`PASS`) or has blocking issues
//! (`CHANGES_REQUESTED` + a must-fix list), via a read-only `claude -p` call (the same pattern as
//! `crate::draft`). `render_review_fix_prompt` turns those findings into the prompt for an
//! autonomous fix session (the review-loop sibling of `render_feedback_prompt`).
//!
//! Note: `claude -p` counts against separate Agent-SDK usage credits (Phase 0 finding); the driver
//! only judges a fresh review (fingerprinted by HEAD), not on every poll.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::draft::truncate_diff;
use crate::models::Ticket;

/// Max diff bytes piped to the judge — keep the call fast and within context.
const MAX_DIFF_BYTES: usize = 60 * 1024;

/// Sentinels the judge emits on its first line.
const PASS: &str = "PASS";
const CHANGES_REQUESTED: &str = "CHANGES_REQUESTED";

/// The judge's decision on a reviewed change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Ready to advance — no blocking issues (rest in review for the human to open the PR).
    Pass,
    /// Blocking issues remain; `findings` lists the must-fix items to auto-address.
    ChangesRequested,
}

/// The parsed result of a judge run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgement {
    pub verdict: Verdict,
    /// Must-fix items (empty for `Pass`).
    pub findings: Vec<String>,
}

impl Verdict {
    /// The persisted string form (stored on the ticket as `review_verdict`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Pass => "pass",
            Verdict::ChangesRequested => "changes_requested",
        }
    }
}

/// Judge a reviewed change: classify the `/review` output + diff into a [`Verdict`] (+ findings).
/// Runs `claude -p` in read-only plan mode in the worktree, with the review prose and the branch
/// diff piped on stdin.
pub fn judge(worktree: &str, review_text: &str, diff: &str) -> Result<Judgement> {
    let prompt = format!(
        "You are the merge gate for an autonomous coding loop. A code reviewer's findings and the \
         branch's full diff (vs base) are provided on stdin, separated by a line `=== DIFF ===`.\n\n\
         Decide whether the change is ready to advance, or whether there are BLOCKING issues that \
         must be fixed first — correctness bugs, broken or missing tests, security holes, or unmet \
         acceptance criteria. Ignore nitpicks, style preferences, and optional polish; a change \
         with only non-blocking suggestions PASSES.\n\n\
         Respond in EXACTLY this format and nothing else:\n\
         - First line: `{PASS}` or `{CHANGES_REQUESTED}`.\n\
         - If `{CHANGES_REQUESTED}`, then one `- ` bullet per must-fix item: imperative and \
         specific, naming the file/line where possible.\n\
         No preamble, no explanation, no code fences."
    );
    let stdin_body = format!(
        "{}\n=== DIFF ===\n{}",
        review_text.trim(),
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
    Ok(parse_judgement(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse the judge's response. Fail-safe: only an explicit `CHANGES_REQUESTED` first line triggers
/// the auto-fix loop — anything else (incl. `PASS`, an empty or unparseable reply) is treated as
/// `Pass` so the loop never spins on garbage; the change just waits for the human.
fn parse_judgement(raw: &str) -> Judgement {
    let first = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if !first.eq_ignore_ascii_case(CHANGES_REQUESTED) {
        return Judgement { verdict: Verdict::Pass, findings: vec![] };
    }
    let findings: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    Judgement { verdict: Verdict::ChangesRequested, findings }
}

/// Render the autonomous review-fix session's prompt from the judge's must-fix findings. The
/// review-loop sibling of `session::render_feedback_prompt`: same shape, but the items come from
/// the judge instead of human comments, and it carries the same "don't silently diverge from the
/// spec" guard.
pub fn render_review_fix_prompt(findings: &[String], t: &Ticket) -> String {
    let mut out = String::from(
        "An automated code review found blocking issues in your change. Fix each item below — make \
         the necessary code edits — then briefly summarize what you changed per item.\n\n\
         Must-fix findings:\n",
    );
    for f in findings {
        out.push_str(&format!("- {f}\n"));
    }
    out.push('\n');

    let spec = crate::spec::compose_spec(t);
    if !spec.trim().is_empty() {
        out.push_str(
            "---\nThe agreed spec is below. If any finding contradicts it, do NOT silently \
             diverge — it may mean our agreed direction has changed. For any such item, write the \
             full revised spec (with the exact sections `## Acceptance criteria`, \
             `## Relevant paths`, `## Constraints`) to a file under `.claude/plans/`, and clearly \
             note which finding contradicted which part of the spec and why. Do not implement a \
             spec-contradicting change until the spec update is accepted; implement all \
             non-contradicting findings now.\n\n# Spec\n",
        );
        out.push_str(&spec);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pass() {
        let j = parse_judgement("PASS");
        assert_eq!(j.verdict, Verdict::Pass);
        assert!(j.findings.is_empty());
    }

    #[test]
    fn parses_pass_case_insensitive_with_trailing_noise() {
        let j = parse_judgement("  pass  \nlooks good, only nits\n- optional: rename x");
        assert_eq!(j.verdict, Verdict::Pass);
        assert!(j.findings.is_empty(), "PASS must not collect findings");
    }

    #[test]
    fn parses_changes_requested_with_findings() {
        let raw = "CHANGES_REQUESTED\n- Fix the off-by-one in src/a.rs:10\n- Add a test for the empty case\n";
        let j = parse_judgement(raw);
        assert_eq!(j.verdict, Verdict::ChangesRequested);
        assert_eq!(
            j.findings,
            vec![
                "Fix the off-by-one in src/a.rs:10".to_string(),
                "Add a test for the empty case".to_string(),
            ]
        );
    }

    #[test]
    fn collects_asterisk_bullets_too() {
        let j = parse_judgement("CHANGES_REQUESTED\n* one\n* two");
        assert_eq!(j.findings, vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn unparseable_is_failsafe_pass() {
        // Garbage / empty must not trigger the fix loop.
        for raw in ["", "   \n\n", "I think this is mostly fine?", "VERDICT: maybe"] {
            assert_eq!(parse_judgement(raw).verdict, Verdict::Pass, "raw={raw:?}");
        }
    }

    #[test]
    fn changes_requested_without_bullets_has_no_findings() {
        // The driver treats changes_requested + empty findings as a no-op (nothing to fix on).
        let j = parse_judgement("CHANGES_REQUESTED");
        assert_eq!(j.verdict, Verdict::ChangesRequested);
        assert!(j.findings.is_empty());
    }
}

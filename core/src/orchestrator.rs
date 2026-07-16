//! Orchestrator "conductor" — the surgical LLM judgment calls for the autonomous coordinator.
//!
//! The orchestrator's *mechanical* work (dispatch, reconcile, concurrency, auto-advance) is
//! deterministic and lives in the app's `poll_orchestrator_once`. This module holds the few
//! decisions that need understanding, made via read-only `claude -p` calls (the same pattern as
//! [`crate::review::judge`]): answering a worker's question, and judging a proposed spec revision.
//!
//! Guiding rule: **escalate on any doubt.** A wrong autonomous action is worse than asking the
//! human, so every parser fails safe to `Escalate`, and the prompts make escalation the default for
//! genuine judgment calls (ambiguous direction, product/UX/risk/scope trade-offs).

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;

/// The conductor's decision on a worker's outstanding `AskUserQuestion`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QDecision {
    /// Answer confidently: 0-based option indices to select and/or a custom free-text answer.
    Answer { selected: Vec<usize>, custom: Option<String> },
    /// Leave it for the human — a genuine judgment call.
    Escalate { reason: String },
}

/// The conductor's decision on a pending proposed spec revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecDecision {
    Accept,
    Escalate { reason: String },
}

/// Run a one-shot read-only `claude -p` in the worktree with `stdin_body` piped in; return stdout.
fn run_claude_p(worktree: &str, prompt: &str, stdin_body: &str) -> Result<String> {
    let mut child = Command::new("claude")
        .arg("-p")
        .arg(prompt)
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
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Decide how to answer a worker's `AskUserQuestion`, or escalate to the human. `options` are the
/// choice labels (0-based); `spec` is the ticket's agreed spec. Runs read-only in the worktree so
/// the conductor can consult the repo.
pub fn answer_question(
    worktree: &str,
    question: &str,
    options: &[String],
    multi_select: bool,
    spec: &str,
) -> Result<QDecision> {
    let opts = options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{i}. {o}"))
        .collect::<Vec<_>>()
        .join("\n");
    let cardinality = if multi_select { "one or more" } else { "exactly one" };
    let prompt = format!(
        "You are the orchestrator of an autonomous coding loop. A worker agent paused to ask the \
         question on stdin (with the ticket spec and the numbered options). Answer it ONLY if the \
         answer is confidently derivable from the spec and the repository (you may read files). If \
         it is a genuine judgment call — ambiguous product/UX direction, a risk or scope trade-off, \
         or anything a human should decide — ESCALATE. When unsure, ESCALATE: a wrong autonomous \
         answer is worse than asking the human.\n\n\
         Respond in EXACTLY one of these forms and nothing else (no preamble, no code fences):\n\
         - `ANSWER <indices>` — comma-separated 0-based option numbers ({cardinality}).\n\
         - `CUSTOM <text>` — a free-text answer, only if no option fits.\n\
         - `ESCALATE <reason>` — leave it for the human."
    );
    let stdin_body =
        format!("# Spec\n{}\n\n# Question\n{}\n\n# Options\n{}", spec.trim(), question.trim(), opts);
    let raw = run_claude_p(worktree, &prompt, &stdin_body)?;
    Ok(parse_q(&raw))
}

/// Parse the conductor's answer. Fail-safe: anything unrecognised → `Escalate`.
fn parse_q(raw: &str) -> QDecision {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let (tag, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    match tag.to_ascii_uppercase().as_str() {
        "ANSWER" => {
            let selected: Vec<usize> =
                rest.split(',').filter_map(|s| s.trim().parse().ok()).collect();
            if selected.is_empty() {
                QDecision::Escalate { reason: "unparseable answer".into() }
            } else {
                QDecision::Answer { selected, custom: None }
            }
        }
        "CUSTOM" if !rest.trim().is_empty() => {
            QDecision::Answer { selected: vec![], custom: Some(rest.trim().to_string()) }
        }
        "ESCALATE" => QDecision::Escalate { reason: rest.trim().to_string() },
        _ => QDecision::Escalate { reason: "unparseable conductor output".into() },
    }
}

/// Decide whether to accept a proposed spec revision, or escalate. Read-only in the worktree.
pub fn judge_spec(worktree: &str, current_spec: &str, proposed_spec: &str) -> Result<SpecDecision> {
    let prompt =
        "You are the orchestrator of an autonomous coding loop. A worker proposed a revision to the \
         ticket's agreed spec (CURRENT and PROPOSED on stdin, separated by `=== PROPOSED ===`). \
         Accept it ONLY if it is a low-risk clarification that stays true to the original intent. If \
         it changes scope, direction, or acceptance criteria in a way a human should approve, \
         ESCALATE. When unsure, ESCALATE.\n\n\
         Respond with EXACTLY `ACCEPT` or `ESCALATE <reason>` — nothing else."
            .to_string();
    let stdin_body = format!("{}\n=== PROPOSED ===\n{}", current_spec.trim(), proposed_spec.trim());
    let raw = run_claude_p(worktree, &prompt, &stdin_body)?;
    Ok(parse_spec(&raw))
}

/// Parse the spec verdict. Fail-safe: anything other than `ACCEPT` → `Escalate`.
fn parse_spec(raw: &str) -> SpecDecision {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let (tag, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
    if tag.eq_ignore_ascii_case("ACCEPT") {
        SpecDecision::Accept
    } else if tag.eq_ignore_ascii_case("ESCALATE") {
        SpecDecision::Escalate { reason: rest.trim().to_string() }
    } else {
        SpecDecision::Escalate { reason: "unparseable conductor output".into() }
    }
}

// ---- deterministic dispatch helpers (pure) -------------------------------

/// Free dispatch slots given the concurrency cap and current live-session count.
pub fn dispatch_slots(max_concurrent: usize, live: usize) -> usize {
    max_concurrent.saturating_sub(live)
}

/// A `Todo` ticket is dispatch-eligible when it has a repo and a captured spec (grilled), isn't
/// mid-grill, and has no live session. Spec-less Todos wait for the human to grill (the intake
/// boundary — the orchestrator executes ready work, it doesn't invent specs).
pub fn todo_dispatch_eligible(has_repo: bool, grilled: bool, drafting: bool, live: bool) -> bool {
    has_repo && grilled && !drafting && !live
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_q_answer_single() {
        assert_eq!(parse_q("ANSWER 2"), QDecision::Answer { selected: vec![2], custom: None });
    }

    #[test]
    fn parse_q_answer_multi() {
        assert_eq!(
            parse_q("ANSWER 0, 2 ,3"),
            QDecision::Answer { selected: vec![0, 2, 3], custom: None }
        );
    }

    #[test]
    fn parse_q_custom() {
        assert_eq!(
            parse_q("CUSTOM use Postgres with a JSONB column"),
            QDecision::Answer { selected: vec![], custom: Some("use Postgres with a JSONB column".into()) }
        );
    }

    #[test]
    fn parse_q_escalate() {
        match parse_q("ESCALATE ambiguous product direction") {
            QDecision::Escalate { reason } => assert!(reason.contains("ambiguous")),
            other => panic!("expected escalate, got {other:?}"),
        }
    }

    #[test]
    fn parse_q_failsafe_to_escalate() {
        // Garbage, empty, or an ANSWER with no valid indices must never become an autonomous answer.
        for raw in ["", "  \n ", "I think option two is best", "ANSWER", "ANSWER foo"] {
            assert!(
                matches!(parse_q(raw), QDecision::Escalate { .. }),
                "raw={raw:?} should escalate"
            );
        }
    }

    #[test]
    fn parse_spec_accept_and_escalate() {
        assert_eq!(parse_spec("ACCEPT"), SpecDecision::Accept);
        assert_eq!(parse_spec("  accept  "), SpecDecision::Accept);
        assert!(matches!(parse_spec("ESCALATE changes scope"), SpecDecision::Escalate { .. }));
        // Fail-safe.
        assert!(matches!(parse_spec("maybe?"), SpecDecision::Escalate { .. }));
        assert!(matches!(parse_spec(""), SpecDecision::Escalate { .. }));
    }

    #[test]
    fn dispatch_slots_saturates() {
        assert_eq!(dispatch_slots(3, 0), 3);
        assert_eq!(dispatch_slots(3, 2), 1);
        assert_eq!(dispatch_slots(3, 3), 0);
        assert_eq!(dispatch_slots(3, 5), 0);
    }

    #[test]
    fn todo_eligibility() {
        assert!(todo_dispatch_eligible(true, true, false, false));
        assert!(!todo_dispatch_eligible(false, true, false, false)); // no repo
        assert!(!todo_dispatch_eligible(true, false, false, false)); // not grilled (no spec)
        assert!(!todo_dispatch_eligible(true, true, true, false)); // mid-grill
        assert!(!todo_dispatch_eligible(true, true, false, true)); // already live
    }
}

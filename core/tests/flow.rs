//! Executable spec for the ticket-lifecycle state machine (`harmony_core::flow`).
//!
//! These tests ARE the contract: each encodes one rule of the golden path or an edge case the
//! app must obey. `flow::decide`/`flow::warnings` are currently `todo!()`, so this suite is RED
//! by design — a later task implements the state machine until every case here is green. The
//! planner is pure, so there's no DB/Claude/git here: just `(event, ctx) -> Decision`.

use harmony_core::flow::{decide, warnings, Action, Column, Ctx, Event, Warning};
use Action::*;
use Column::*;

/// Sensible default context: a repo IS assigned (mandatory to leave Todo), nothing else set,
/// currently in Todo. Override fields per case with `Ctx { .., ..base() }`.
fn base() -> Ctx {
    Ctx { has_repo: true, ..Default::default() }
}

fn has(actions: &[Action], a: Action) -> bool {
    actions.contains(&a)
}

// ===================================================================
// Golden path
// ===================================================================

#[test]
fn grill_requested_on_todo_starts_grill() {
    let d = decide(Event::GrillRequested, &Ctx { from: Todo, ..base() });
    assert_eq!(d.target, Todo);
    assert_eq!(d.actions, vec![StartGrill]);
    assert!(d.blocked.is_none());
}

#[test]
fn grill_finished_on_todo_saves_and_stays() {
    let d = decide(Event::GrillFinished, &Ctx { from: Todo, has_spec: true, ..base() });
    assert_eq!(d.target, Todo);
    assert_eq!(d.actions, vec![StopSession]); // grill session ends; spec is saved, ticket stays
}

#[test]
fn move_in_progress_with_spec_implements_autonomously() {
    // First time into In Progress: create the worktree, then start the fresh implement session.
    let d = decide(Event::Move(InProgress), &Ctx { from: Todo, has_spec: true, ..base() });
    assert_eq!(d.target, InProgress);
    assert_eq!(d.actions, vec![EnsureWorktree, StartImplement]);
}

#[test]
fn work_finished_moves_to_human_review_and_runs_review() {
    let d = decide(
        Event::WorkFinished,
        &Ctx { from: InProgress, has_spec: true, planned: true, session_live: true, has_changes: true, ..base() },
    );
    assert_eq!(d.target, HumanReview);
    assert_eq!(d.actions, vec![StopSession, RunReview]);
}

#[test]
fn review_finished_stops_and_stays_in_human_review() {
    let d = decide(
        Event::ReviewFinished,
        &Ctx { from: HumanReview, session_live: true, has_changes: true, ..base() },
    );
    assert_eq!(d.target, HumanReview);
    assert_eq!(d.actions, vec![StopSession]);
}

#[test]
fn move_pr_after_review_opens_pr() {
    let d = decide(
        Event::Move(Pr),
        &Ctx { from: HumanReview, reviewed: true, has_changes: true, ..base() },
    );
    assert_eq!(d.target, Pr);
    assert_eq!(d.actions, vec![OpenPr]);
}

#[test]
fn move_done_with_approved_pr_merges_and_cleans_up() {
    let d = decide(
        Event::Move(Done),
        &Ctx { from: Pr, reviewed: true, has_changes: true, pr_exists: true, pr_approved: true, ..base() },
    );
    assert_eq!(d.target, Done);
    assert_eq!(d.actions, vec![MergePr, DeleteWorktree]);
}

// ===================================================================
// Repo mandatory
// ===================================================================

#[test]
fn moving_past_todo_without_repo_is_blocked() {
    for to in [InProgress, HumanReview, Pr] {
        let d = decide(Event::Move(to), &Ctx { from: Todo, has_repo: false, has_spec: true, ..base() });
        assert!(d.blocked.is_some(), "move to {to:?} without a repo must be blocked");
        assert_eq!(d.target, Todo, "blocked move must leave the ticket put");
        assert!(d.actions.is_empty());
    }
}

#[test]
fn repo_less_ticket_warns() {
    assert_eq!(
        warnings(&Ctx { from: Todo, has_repo: false, is_jira: true, ..Default::default() }),
        vec![Warning::NoRepo]
    );
    assert!(warnings(&Ctx { from: Todo, ..base() }).is_empty());
}

// ===================================================================
// Grill / spec gating
// ===================================================================

#[test]
fn move_in_progress_without_spec_grills_first() {
    let d = decide(Event::Move(InProgress), &Ctx { from: Todo, has_spec: false, drafting: false, ..base() });
    assert_eq!(d.target, InProgress);
    assert_eq!(d.actions, vec![StartGrill]);
}

#[test]
fn grill_finished_in_progress_implements_immediately() {
    let d = decide(Event::GrillFinished, &Ctx { from: InProgress, has_spec: true, ..base() });
    assert_eq!(d.target, InProgress);
    // Grill ends, then implement; EnsureWorktree reuses the worktree the grill already made.
    assert_eq!(d.actions, vec![StopSession, EnsureWorktree, StartImplement]);
}

#[test]
fn move_in_progress_while_drafting_is_blocked() {
    let d = decide(Event::Move(InProgress), &Ctx { from: Todo, drafting: true, ..base() });
    assert!(d.blocked.is_some());
    assert_eq!(d.target, Todo);
}

#[test]
fn move_in_progress_after_plan_run_resumes() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx { from: Todo, has_spec: true, planned: true, ..base() },
    );
    assert_eq!(d.target, InProgress);
    assert_eq!(d.actions, vec![ResumeWork]);
    assert!(!has(&d.actions, EnsureWorktree), "resume must not create a new worktree");
}

#[test]
fn todo_in_progress_todo_in_progress_cycle() {
    // 1) Todo -> In Progress (fresh): new worktree + implement.
    let start = decide(Event::Move(InProgress), &Ctx { from: Todo, has_spec: true, ..base() });
    assert_eq!(start.actions, vec![EnsureWorktree, StartImplement]);

    // 2) In Progress -> Todo mid-implement: stop the session, keep the worktree.
    let pause = decide(
        Event::Move(Todo),
        &Ctx { from: InProgress, has_spec: true, planned: true, session_live: true, ..base() },
    );
    assert_eq!(pause.actions, vec![StopSession]);
    assert!(!has(&pause.actions, DeleteWorktree));

    // 3) Todo -> In Progress again: worktree already exists, just resume (no new worktree).
    let resume = decide(
        Event::Move(InProgress),
        &Ctx { from: Todo, has_spec: true, planned: true, ..base() },
    );
    assert_eq!(resume.actions, vec![ResumeWork]);
    assert!(!has(&resume.actions, EnsureWorktree));
}

// ===================================================================
// Human review / /review gating
// ===================================================================

#[test]
fn entering_human_review_with_new_changes_runs_review() {
    // via WorkFinished
    let d = decide(
        Event::WorkFinished,
        &Ctx { from: InProgress, session_live: true, has_changes: true, review_current: false, ..base() },
    );
    assert!(has(&d.actions, RunReview));
    // via manual Move
    let d = decide(
        Event::Move(HumanReview),
        &Ctx { from: InProgress, session_live: true, has_changes: true, review_current: false, ..base() },
    );
    assert_eq!(d.target, HumanReview);
    assert_eq!(d.actions, vec![StopSession, RunReview]);
}

#[test]
fn review_not_rerun_when_nothing_changed() {
    let d = decide(
        Event::Move(HumanReview),
        &Ctx { from: InProgress, session_live: true, has_changes: true, review_current: true, ..base() },
    );
    assert!(!has(&d.actions, RunReview), "must not re-run /review when nothing changed");
    assert!(has(&d.actions, StopSession));
}

#[test]
fn no_review_when_no_changes() {
    let d = decide(
        Event::Move(HumanReview),
        &Ctx { from: InProgress, session_live: true, has_changes: false, ..base() },
    );
    assert!(!has(&d.actions, RunReview), "no changes => nothing to review");
}

// ===================================================================
// PR stage
// ===================================================================

#[test]
fn move_pr_without_human_review_redirects_back_to_review() {
    let d = decide(
        Event::Move(Pr),
        &Ctx { from: InProgress, reviewed: false, has_changes: true, session_live: true, review_current: false, ..base() },
    );
    assert_eq!(d.target, HumanReview, "PR before human review must redirect to human review");
    assert!(!has(&d.actions, OpenPr));
    assert!(has(&d.actions, RunReview));
}

#[test]
fn move_pr_with_no_changes_is_blocked() {
    let d = decide(
        Event::Move(Pr),
        &Ctx { from: HumanReview, reviewed: true, has_changes: false, ..base() },
    );
    assert!(d.blocked.is_some(), "nothing to PR when there are no changes");
    assert!(!has(&d.actions, OpenPr));
}

#[test]
fn move_pr_when_pr_already_exists_is_idempotent() {
    let d = decide(
        Event::Move(Pr),
        &Ctx { from: HumanReview, reviewed: true, has_changes: true, pr_exists: true, ..base() },
    );
    assert_eq!(d.target, Pr);
    assert!(!has(&d.actions, OpenPr), "must not open a second PR");
}

// ===================================================================
// Done / cleanup
// ===================================================================

#[test]
fn move_done_without_pr_just_deletes_worktree() {
    let d = decide(Event::Move(Done), &Ctx { from: HumanReview, reviewed: true, has_changes: true, ..base() });
    assert_eq!(d.target, Done);
    assert!(has(&d.actions, DeleteWorktree));
    assert!(!has(&d.actions, MergePr));
}

#[test]
fn move_done_with_unapproved_pr_does_not_merge() {
    let d = decide(
        Event::Move(Done),
        &Ctx { from: Pr, reviewed: true, has_changes: true, pr_exists: true, pr_approved: false, ..base() },
    );
    assert_eq!(d.target, Done);
    assert!(has(&d.actions, DeleteWorktree));
    assert!(!has(&d.actions, MergePr), "must not merge an unapproved PR");
}

#[test]
fn move_done_from_in_progress_stops_session_and_cleans_up() {
    let d = decide(
        Event::Move(Done),
        &Ctx { from: InProgress, session_live: true, has_spec: true, planned: true, ..base() },
    );
    assert_eq!(d.target, Done);
    assert!(has(&d.actions, StopSession));
    assert!(has(&d.actions, DeleteWorktree));
}

// ===================================================================
// Session-stop cross-cutting + resume
// ===================================================================

#[test]
fn leaving_in_progress_always_stops_the_session() {
    for to in [HumanReview, Todo, Done, Pr] {
        let d = decide(
            Event::Move(to),
            &Ctx { from: InProgress, session_live: true, has_spec: true, planned: true, reviewed: true, has_changes: true, ..base() },
        );
        assert!(has(&d.actions, StopSession), "moving InProgress -> {to:?} must stop the session");
    }
}

#[test]
fn move_in_progress_to_todo_stops_but_keeps_worktree() {
    let d = decide(
        Event::Move(Todo),
        &Ctx { from: InProgress, session_live: true, has_spec: true, planned: true, ..base() },
    );
    assert_eq!(d.target, Todo);
    assert_eq!(d.actions, vec![StopSession]); // no DeleteWorktree — kept for resume
    assert!(!has(&d.actions, DeleteWorktree));
}

#[test]
fn back_to_in_progress_from_todo_resumes() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx { from: Todo, has_spec: true, planned: true, ..base() },
    );
    assert_eq!(d.actions, vec![ResumeWork]);
}

#[test]
fn back_to_in_progress_from_human_review_resumes_to_address_comments() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx { from: HumanReview, has_spec: true, planned: true, reviewed: true, ..base() },
    );
    assert_eq!(d.target, InProgress);
    assert_eq!(d.actions, vec![ResumeWork]);
}

#[test]
fn entering_human_review_without_live_session_does_not_stop() {
    let d = decide(
        Event::Move(HumanReview),
        &Ctx { from: InProgress, session_live: false, has_changes: true, review_current: false, ..base() },
    );
    assert!(!has(&d.actions, StopSession), "nothing to stop when no session is live");
    assert!(has(&d.actions, RunReview));
}

#[test]
fn moving_to_the_same_column_is_a_noop() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx { from: InProgress, has_spec: true, planned: true, session_live: true, ..base() },
    );
    assert!(d.actions.is_empty(), "re-moving to the current column must not start a duplicate session");
    assert!(d.blocked.is_none());
}

//! Executable spec for the ticket-lifecycle state machine (`harmony_core::flow`).
//!
//! These tests ARE the contract: each encodes one rule of the golden path or an edge case the
//! app must obey. The planner is pure, so there's no DB/Claude/git here: just
//! `(event, ctx) -> Decision`.

use harmony_core::flow::{decide, warnings, Action, Column, Ctx, Event, Warning};
use Action::*;
use Column::*;

/// Sensible default context: a repo IS assigned (mandatory to leave Todo), nothing else set,
/// currently in Todo. Override fields per case with `Ctx { .., ..base() }`.
fn base() -> Ctx {
    Ctx {
        has_repo: true,
        ..Default::default()
    }
}

fn has(actions: &[Action], a: Action) -> bool {
    actions.contains(&a)
}

// ===================================================================
// Golden path
// ===================================================================

#[test]
fn grill_requested_on_todo_starts_grill() {
    let d = decide(
        Event::GrillRequested,
        &Ctx {
            from: Todo,
            ..base()
        },
    );
    assert_eq!(d.target, Todo);
    assert_eq!(d.actions, vec![StartGrill]);
    assert!(d.blocked.is_none());
}

#[test]
fn grill_finished_on_todo_saves_and_stays() {
    let d = decide(
        Event::GrillFinished,
        &Ctx {
            from: Todo,
            has_spec: true,
            ..base()
        },
    );
    assert_eq!(d.target, Todo);
    assert_eq!(d.actions, vec![StopSession]); // grill session ends; spec is saved, ticket stays
}

#[test]
fn move_in_progress_with_spec_implements_autonomously() {
    // First time into In Progress: create the worktree, then start the fresh implement session.
    let d = decide(
        Event::Move(InProgress),
        &Ctx {
            from: Todo,
            has_spec: true,
            ..base()
        },
    );
    assert_eq!(d.target, InProgress);
    assert_eq!(d.actions, vec![EnsureWorktree, StartImplement]);
}

#[test]
fn work_finished_moves_to_human_review_and_runs_review() {
    let d = decide(
        Event::WorkFinished,
        &Ctx {
            from: InProgress,
            has_spec: true,
            planned: true,
            session_live: true,
            has_changes: true,
            ..base()
        },
    );
    assert_eq!(d.target, HumanReview);
    // Commit the agent's work first (so review + the reviewed-SHA see committed state), stop, review.
    assert_eq!(d.actions, vec![CommitChanges, StopSession, RunReview]);
}

#[test]
fn review_finished_stops_and_stays_in_human_review() {
    let d = decide(
        Event::ReviewFinished,
        &Ctx {
            from: HumanReview,
            session_live: true,
            has_changes: true,
            ..base()
        },
    );
    assert_eq!(d.target, HumanReview);
    assert_eq!(d.actions, vec![StopSession, MarkReviewed]);
}

#[test]
fn move_pr_after_review_opens_pr() {
    let d = decide(
        Event::Move(Pr),
        &Ctx {
            from: HumanReview,
            reviewed: true,
            has_changes: true,
            ..base()
        },
    );
    assert_eq!(d.target, Pr);
    assert_eq!(d.actions, vec![OpenPr]);
}

#[test]
fn move_done_with_approved_pr_merges_and_cleans_up() {
    let d = decide(
        Event::Move(Done),
        &Ctx {
            from: Pr,
            reviewed: true,
            has_changes: true,
            has_worktree: true,
            pr_exists: true,
            pr_approved: true,
            ..base()
        },
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
        let d = decide(
            Event::Move(to),
            &Ctx {
                from: Todo,
                has_repo: false,
                has_spec: true,
                ..base()
            },
        );
        assert!(
            d.blocked.is_some(),
            "move to {to:?} without a repo must be blocked"
        );
        assert_eq!(d.target, Todo, "blocked move must leave the ticket put");
        assert!(d.actions.is_empty());
    }
}

#[test]
fn repo_less_ticket_warns() {
    assert_eq!(
        warnings(&Ctx {
            from: Todo,
            has_repo: false,
            is_jira: true,
            ..Default::default()
        }),
        vec![Warning::NoRepo]
    );
    assert!(warnings(&Ctx {
        from: Todo,
        ..base()
    })
    .is_empty());
}

// ===================================================================
// Grill / spec gating
// ===================================================================

#[test]
fn move_in_progress_without_spec_grills_first() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx {
            from: Todo,
            has_spec: false,
            drafting: false,
            ..base()
        },
    );
    assert_eq!(d.target, InProgress);
    assert_eq!(d.actions, vec![StartGrill]);
}

#[test]
fn grill_finished_in_progress_implements_immediately() {
    let d = decide(
        Event::GrillFinished,
        &Ctx {
            from: InProgress,
            has_spec: true,
            ..base()
        },
    );
    assert_eq!(d.target, InProgress);
    // Grill ends, then implement; EnsureWorktree reuses the worktree the grill already made.
    assert_eq!(d.actions, vec![StopSession, EnsureWorktree, StartImplement]);
}

#[test]
fn move_in_progress_while_drafting_is_blocked() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx {
            from: Todo,
            drafting: true,
            ..base()
        },
    );
    assert!(d.blocked.is_some());
    assert_eq!(d.target, Todo);
}

#[test]
fn move_in_progress_after_plan_run_resumes() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx {
            from: Todo,
            has_spec: true,
            planned: true,
            ..base()
        },
    );
    assert_eq!(d.target, InProgress);
    assert_eq!(d.actions, vec![ResumeWork]);
    assert!(
        !has(&d.actions, EnsureWorktree),
        "resume must not create a new worktree"
    );
}

#[test]
fn todo_in_progress_todo_in_progress_cycle() {
    // 1) Todo -> In Progress (fresh): new worktree + implement.
    let start = decide(
        Event::Move(InProgress),
        &Ctx {
            from: Todo,
            has_spec: true,
            ..base()
        },
    );
    assert_eq!(start.actions, vec![EnsureWorktree, StartImplement]);

    // 2) In Progress -> Todo mid-implement: stop the session, keep the worktree.
    let pause = decide(
        Event::Move(Todo),
        &Ctx {
            from: InProgress,
            has_spec: true,
            planned: true,
            session_live: true,
            ..base()
        },
    );
    assert_eq!(pause.actions, vec![StopSession]);
    assert!(!has(&pause.actions, DeleteWorktree));

    // 3) Todo -> In Progress again: worktree already exists, just resume (no new worktree).
    let resume = decide(
        Event::Move(InProgress),
        &Ctx {
            from: Todo,
            has_spec: true,
            planned: true,
            ..base()
        },
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
        &Ctx {
            from: InProgress,
            session_live: true,
            has_changes: true,
            review_current: false,
            ..base()
        },
    );
    assert!(has(&d.actions, RunReview));
    // via manual Move
    let d = decide(
        Event::Move(HumanReview),
        &Ctx {
            from: InProgress,
            session_live: true,
            has_changes: true,
            review_current: false,
            ..base()
        },
    );
    assert_eq!(d.target, HumanReview);
    assert_eq!(d.actions, vec![StopSession, RunReview]);
}

#[test]
fn review_not_rerun_when_nothing_changed() {
    let d = decide(
        Event::Move(HumanReview),
        &Ctx {
            from: InProgress,
            session_live: true,
            has_changes: true,
            review_current: true,
            ..base()
        },
    );
    assert!(
        !has(&d.actions, RunReview),
        "must not re-run /review when nothing changed"
    );
    assert!(has(&d.actions, StopSession));
}

#[test]
fn no_review_when_no_changes() {
    let d = decide(
        Event::Move(HumanReview),
        &Ctx {
            from: InProgress,
            session_live: true,
            has_changes: false,
            ..base()
        },
    );
    assert!(
        !has(&d.actions, RunReview),
        "no changes => nothing to review"
    );
}

// ===================================================================
// PR stage
// ===================================================================

#[test]
fn move_pr_without_human_review_redirects_back_to_review() {
    let d = decide(
        Event::Move(Pr),
        &Ctx {
            from: InProgress,
            reviewed: false,
            has_changes: true,
            session_live: true,
            review_current: false,
            ..base()
        },
    );
    assert_eq!(
        d.target, HumanReview,
        "PR before human review must redirect to human review"
    );
    assert!(!has(&d.actions, OpenPr));
    assert!(has(&d.actions, RunReview));
}

#[test]
fn move_pr_with_no_changes_is_blocked() {
    let d = decide(
        Event::Move(Pr),
        &Ctx {
            from: HumanReview,
            reviewed: true,
            has_changes: false,
            ..base()
        },
    );
    assert!(
        d.blocked.is_some(),
        "nothing to PR when there are no changes"
    );
    assert!(!has(&d.actions, OpenPr));
}

#[test]
fn move_pr_when_pr_already_exists_is_idempotent() {
    let d = decide(
        Event::Move(Pr),
        &Ctx {
            from: HumanReview,
            reviewed: true,
            has_changes: true,
            pr_exists: true,
            ..base()
        },
    );
    assert_eq!(d.target, Pr);
    assert!(!has(&d.actions, OpenPr), "must not open a second PR");
}

// ===================================================================
// Done / cleanup
// ===================================================================

#[test]
fn move_done_without_pr_just_deletes_worktree() {
    let d = decide(
        Event::Move(Done),
        &Ctx {
            from: HumanReview,
            reviewed: true,
            has_changes: true,
            has_worktree: true,
            ..base()
        },
    );
    assert_eq!(d.target, Done);
    assert!(has(&d.actions, DeleteWorktree));
    assert!(!has(&d.actions, MergePr));
}

#[test]
fn move_done_with_unapproved_pr_does_not_merge() {
    let d = decide(
        Event::Move(Done),
        &Ctx {
            from: Pr,
            reviewed: true,
            has_changes: true,
            has_worktree: true,
            pr_exists: true,
            pr_approved: false,
            ..base()
        },
    );
    assert_eq!(d.target, Done);
    assert!(has(&d.actions, DeleteWorktree));
    assert!(!has(&d.actions, MergePr), "must not merge an unapproved PR");
}

#[test]
fn move_done_from_in_progress_stops_session_and_cleans_up() {
    let d = decide(
        Event::Move(Done),
        &Ctx {
            from: InProgress,
            session_live: true,
            has_spec: true,
            planned: true,
            has_worktree: true,
            ..base()
        },
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
            &Ctx {
                from: InProgress,
                session_live: true,
                has_spec: true,
                planned: true,
                reviewed: true,
                has_changes: true,
                ..base()
            },
        );
        assert!(
            has(&d.actions, StopSession),
            "moving InProgress -> {to:?} must stop the session"
        );
    }
}

#[test]
fn move_in_progress_to_todo_stops_but_keeps_worktree() {
    let d = decide(
        Event::Move(Todo),
        &Ctx {
            from: InProgress,
            session_live: true,
            has_spec: true,
            planned: true,
            ..base()
        },
    );
    assert_eq!(d.target, Todo);
    assert_eq!(d.actions, vec![StopSession]); // no DeleteWorktree — kept for resume
    assert!(!has(&d.actions, DeleteWorktree));
}

#[test]
fn back_to_in_progress_from_todo_resumes() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx {
            from: Todo,
            has_spec: true,
            planned: true,
            ..base()
        },
    );
    assert_eq!(d.actions, vec![ResumeWork]);
}

#[test]
fn back_to_in_progress_from_human_review_resumes_to_address_comments() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx {
            from: HumanReview,
            has_spec: true,
            planned: true,
            reviewed: true,
            ..base()
        },
    );
    assert_eq!(d.target, InProgress);
    assert_eq!(d.actions, vec![ResumeWork]);
}

#[test]
fn entering_human_review_without_live_session_does_not_stop() {
    let d = decide(
        Event::Move(HumanReview),
        &Ctx {
            from: InProgress,
            session_live: false,
            has_changes: true,
            review_current: false,
            ..base()
        },
    );
    assert!(
        !has(&d.actions, StopSession),
        "nothing to stop when no session is live"
    );
    assert!(has(&d.actions, RunReview));
}

#[test]
fn moving_to_the_same_column_is_a_noop() {
    let d = decide(
        Event::Move(InProgress),
        &Ctx {
            from: InProgress,
            has_spec: true,
            planned: true,
            session_live: true,
            ..base()
        },
    );
    assert!(
        d.actions.is_empty(),
        "re-moving to the current column must not start a duplicate session"
    );
    assert!(d.blocked.is_none());
}

// ===================================================================
// Round 2: stale/out-of-order system events (race hardening)
// ===================================================================

#[test]
fn work_finished_off_in_progress_is_ignored() {
    // A stale WorkFinished arriving after the user already moved the ticket must not yank it.
    for from in [Todo, HumanReview, Pr, Done] {
        let d = decide(
            Event::WorkFinished,
            &Ctx {
                from,
                has_spec: true,
                planned: true,
                ..base()
            },
        );
        assert_eq!(
            d.target, from,
            "stale WorkFinished from {from:?} must not move the ticket"
        );
        assert!(
            d.actions.is_empty(),
            "stale WorkFinished from {from:?} must do nothing"
        );
    }
}

#[test]
fn grill_finished_after_ticket_moved_on_only_stops() {
    for from in [HumanReview, Pr, Done] {
        let d = decide(
            Event::GrillFinished,
            &Ctx {
                from,
                has_spec: true,
                ..base()
            },
        );
        assert_eq!(d.target, from);
        assert_eq!(
            d.actions,
            vec![StopSession],
            "from {from:?}: just stop, don't implement/move"
        );
    }
}

#[test]
fn review_finished_off_human_review_only_stops() {
    for from in [Todo, InProgress, Pr, Done] {
        let d = decide(
            Event::ReviewFinished,
            &Ctx {
                from,
                session_live: true,
                ..base()
            },
        );
        assert_eq!(
            d.target, from,
            "from {from:?}: a stale ReviewFinished must not move the ticket"
        );
        assert!(has(&d.actions, StopSession));
        assert!(
            !has(&d.actions, RunReview),
            "from {from:?}: never re-run review on a finish event"
        );
    }
}

#[test]
fn work_finished_with_no_changes_moves_but_skips_review() {
    let d = decide(
        Event::WorkFinished,
        &Ctx {
            from: InProgress,
            session_live: true,
            has_spec: true,
            planned: true,
            has_changes: false,
            ..base()
        },
    );
    assert_eq!(d.target, HumanReview);
    assert!(has(&d.actions, StopSession));
    assert!(
        !has(&d.actions, RunReview),
        "no changes => nothing to review"
    );
}

// ===================================================================
// Round 2: Done is terminal
// ===================================================================

#[test]
fn done_is_terminal() {
    for to in [Todo, InProgress, HumanReview, Pr] {
        let d = decide(
            Event::Move(to),
            &Ctx {
                from: Done,
                has_spec: true,
                planned: true,
                ..base()
            },
        );
        assert!(
            d.blocked.is_some(),
            "moving Done -> {to:?} must be blocked (Done is terminal)"
        );
        assert_eq!(d.target, Done);
        assert!(d.actions.is_empty());
    }
}

// ===================================================================
// Round 2: worktree-existence gating
// ===================================================================

#[test]
fn move_done_with_worktree_deletes_it() {
    let d = decide(
        Event::Move(Done),
        &Ctx {
            from: HumanReview,
            reviewed: true,
            has_changes: true,
            has_worktree: true,
            ..base()
        },
    );
    assert!(has(&d.actions, DeleteWorktree));
}

#[test]
fn move_done_from_todo_with_no_worktree_deletes_nothing() {
    let d = decide(
        Event::Move(Done),
        &Ctx {
            from: Todo,
            has_worktree: false,
            ..base()
        },
    );
    assert_eq!(d.target, Done);
    assert!(
        !has(&d.actions, DeleteWorktree),
        "nothing built => nothing to delete"
    );
    assert!(!has(&d.actions, MergePr));
}

#[test]
fn move_done_without_repo_is_allowed() {
    // Repo-less ticket can still be abandoned to Done (no worktree, just a status change).
    let d = decide(
        Event::Move(Done),
        &Ctx {
            from: Todo,
            has_repo: false,
            has_worktree: false,
            ..base()
        },
    );
    assert_eq!(d.target, Done);
    assert!(d.blocked.is_none(), "Done is allowed without a repo");
    assert!(d.actions.is_empty());
}

// ===================================================================
// Round 2: grill guards (Todo-only)
// ===================================================================

#[test]
fn grill_requested_without_repo_is_blocked() {
    let d = decide(
        Event::GrillRequested,
        &Ctx {
            from: Todo,
            has_repo: false,
            ..base()
        },
    );
    assert!(
        d.blocked.is_some(),
        "grill needs a worktree, which needs a repo"
    );
}

#[test]
fn grill_requested_while_drafting_is_blocked() {
    let d = decide(
        Event::GrillRequested,
        &Ctx {
            from: Todo,
            drafting: true,
            ..base()
        },
    );
    assert!(d.blocked.is_some(), "no second grill while one is running");
}

#[test]
fn grill_requested_off_todo_is_blocked() {
    for from in [InProgress, HumanReview, Pr, Done] {
        let d = decide(
            Event::GrillRequested,
            &Ctx {
                from,
                has_spec: true,
                ..base()
            },
        );
        assert!(
            d.blocked.is_some(),
            "grill is Todo-only; from {from:?} must block"
        );
    }
}

// ===================================================================
// Round 2: PR-stage edges
// ===================================================================

#[test]
fn move_pr_with_no_changes_always_blocked() {
    // Whether already reviewed or coming straight from Todo, an empty diff can't open a PR.
    for from in [Todo, HumanReview] {
        let reviewed = from == HumanReview;
        let d = decide(
            Event::Move(Pr),
            &Ctx {
                from,
                reviewed,
                has_changes: false,
                ..base()
            },
        );
        assert!(
            d.blocked.is_some(),
            "from {from:?}: no changes => nothing to PR"
        );
        assert!(!has(&d.actions, OpenPr));
    }
}

#[test]
fn move_pr_while_review_session_live_stops_then_opens() {
    let d = decide(
        Event::Move(Pr),
        &Ctx {
            from: HumanReview,
            reviewed: true,
            has_changes: true,
            session_live: true,
            ..base()
        },
    );
    assert_eq!(d.target, Pr);
    assert_eq!(d.actions, vec![StopSession, OpenPr]);
}

#[test]
fn move_pr_does_not_force_a_rereview() {
    let d = decide(
        Event::Move(Pr),
        &Ctx {
            from: HumanReview,
            reviewed: true,
            has_changes: true,
            review_current: false,
            ..base()
        },
    );
    assert_eq!(d.target, Pr);
    assert_eq!(
        d.actions,
        vec![OpenPr],
        "moving to PR opens it; it does not re-run /review"
    );
}

// ===================================================================
// Round 2: backward / demote moves
// ===================================================================

#[test]
fn move_human_review_to_todo_stops_keeps_worktree() {
    let d = decide(
        Event::Move(Todo),
        &Ctx {
            from: HumanReview,
            session_live: true,
            has_worktree: true,
            has_spec: true,
            planned: true,
            reviewed: true,
            ..base()
        },
    );
    assert_eq!(d.target, Todo);
    assert!(has(&d.actions, StopSession));
    assert!(!has(&d.actions, DeleteWorktree), "worktree kept for later");
}

#[test]
fn move_pr_to_human_review_demotes_without_touching_pr() {
    let d = decide(
        Event::Move(HumanReview),
        &Ctx {
            from: Pr,
            reviewed: true,
            has_changes: true,
            review_current: false,
            pr_exists: true,
            ..base()
        },
    );
    assert_eq!(d.target, HumanReview);
    assert!(
        !has(&d.actions, OpenPr) && !has(&d.actions, MergePr),
        "PR left intact on demote"
    );
    assert!(
        has(&d.actions, RunReview),
        "new changes since review => re-review"
    );
}

// ===================================================================
// Round 2: generalized same-column no-op
// ===================================================================

#[test]
fn same_column_move_is_noop_for_all_columns() {
    for col in [Todo, InProgress, HumanReview, Pr, Done] {
        let d = decide(
            Event::Move(col),
            &Ctx {
                from: col,
                has_spec: true,
                planned: true,
                reviewed: true,
                has_changes: true,
                has_worktree: true,
                ..base()
            },
        );
        assert!(
            d.actions.is_empty(),
            "{col:?} -> {col:?} must be a no-op (esp. no re-review)"
        );
        assert!(d.blocked.is_none());
    }
}

// ===================================================================
// Round 2: comment-addressing loop (end-to-end)
// ===================================================================

#[test]
fn comment_addressing_loop() {
    // 1) In human review, user moves back to In Progress to address comments → resume.
    let resume = decide(
        Event::Move(InProgress),
        &Ctx {
            from: HumanReview,
            has_spec: true,
            planned: true,
            reviewed: true,
            has_worktree: true,
            has_changes: true,
            ..base()
        },
    );
    assert_eq!(resume.target, InProgress);
    assert_eq!(resume.actions, vec![ResumeWork]);

    // 2) Claude finishes the fixes (new changes since the last review) → back to review, re-run /review.
    let back = decide(
        Event::WorkFinished,
        &Ctx {
            from: InProgress,
            session_live: true,
            has_spec: true,
            planned: true,
            reviewed: true,
            has_worktree: true,
            has_changes: true,
            review_current: false,
            ..base()
        },
    );
    assert_eq!(back.target, HumanReview);
    assert_eq!(back.actions, vec![CommitChanges, StopSession, RunReview]);
}

// ===================================================================
// Auto re-review (the background poller's contract)
// ===================================================================
// The poller in lib.rs fires `Event::ReviewRequested` for a review-stage ticket whose reviewed
// HEAD has moved. These pin the decision that path relies on: re-run `/review` in place,
// regardless of `review_current`, from both review columns.

#[test]
fn review_requested_reruns_in_place_from_human_review() {
    for from in [HumanReview, Pr] {
        let d = decide(
            Event::ReviewRequested,
            &Ctx {
                from,
                reviewed: true,
                has_changes: true,
                review_current: true,
                ..base()
            },
        );
        assert_eq!(d.target, from, "re-review must stay in the current column");
        assert!(
            has(&d.actions, RunReview),
            "{from:?}: ReviewRequested must run /review even when review_current"
        );
        assert!(d.blocked.is_none());
    }
}

#[test]
fn review_requested_without_changes_is_blocked() {
    let d = decide(
        Event::ReviewRequested,
        &Ctx {
            from: HumanReview,
            reviewed: true,
            has_changes: false,
            ..base()
        },
    );
    assert!(
        d.blocked.is_some(),
        "nothing to review when there are no changes"
    );
    assert!(!has(&d.actions, RunReview));
}

// ===================================================================
// Work finished: commit + question-pending gate
// ===================================================================

#[test]
fn work_finished_commits_before_review() {
    let d = decide(
        Event::WorkFinished,
        &Ctx {
            from: InProgress,
            session_live: true,
            has_spec: true,
            planned: true,
            has_changes: true,
            ..base()
        },
    );
    // Commit is first so the /review and the reviewed-SHA fingerprint see committed state.
    assert_eq!(d.actions.first(), Some(&CommitChanges));
    assert!(has(&d.actions, RunReview));
}

#[test]
fn work_finished_with_question_pending_stays_in_progress() {
    // A Stop while an AskUserQuestion is outstanding isn't "done" — Claude is waiting on the user.
    let d = decide(
        Event::WorkFinished,
        &Ctx {
            from: InProgress,
            session_live: true,
            has_spec: true,
            planned: true,
            has_changes: true,
            user_question_pending: true,
            ..base()
        },
    );
    assert_eq!(
        d.target, InProgress,
        "a pending question means work isn't finished"
    );
    assert!(
        d.actions.is_empty(),
        "leave the session live to receive the answer"
    );
}

// ===================================================================
// Review finished: reviewed-SHA fingerprint
// ===================================================================

#[test]
fn review_finished_marks_reviewed() {
    let d = decide(
        Event::ReviewFinished,
        &Ctx {
            from: HumanReview,
            session_live: true,
            has_changes: true,
            ..base()
        },
    );
    assert_eq!(d.actions, vec![StopSession, MarkReviewed]);
}

// ===================================================================
// CI-fix / feedback-addressing session finished
// ===================================================================

#[test]
fn fix_finished_commits_pushes_and_stays_in_pr() {
    let d = decide(
        Event::FixFinished,
        &Ctx {
            from: Pr,
            reviewed: true,
            has_changes: true,
            has_worktree: true,
            pr_exists: true,
            ..base()
        },
    );
    assert_eq!(d.target, Pr);
    assert_eq!(d.actions, vec![CommitChanges, PushBranch]);
}

#[test]
fn address_finished_with_pr_commits_pushes_and_returns_to_pr() {
    let d = decide(
        Event::AddressFinished,
        &Ctx {
            from: InProgress,
            reviewed: true,
            has_changes: true,
            has_worktree: true,
            pr_exists: true,
            ..base()
        },
    );
    assert_eq!(d.target, Pr, "a PR exists => land back in the PR column");
    assert_eq!(d.actions, vec![CommitChanges, PushBranch]);
}

#[test]
fn address_finished_without_pr_commits_only_and_returns_to_human_review() {
    let d = decide(
        Event::AddressFinished,
        &Ctx {
            from: InProgress,
            reviewed: true,
            has_changes: true,
            has_worktree: true,
            pr_exists: false,
            ..base()
        },
    );
    assert_eq!(
        d.target, HumanReview,
        "no PR => don't create a remote branch; stay pre-PR"
    );
    assert_eq!(d.actions, vec![CommitChanges], "no push without a PR");
}

// ===================================================================
// Idle session teardown (auto_end_idle)
// ===================================================================

#[test]
fn session_idle_stops_when_auto_end_idle_on() {
    let d = decide(
        Event::SessionIdle,
        &Ctx {
            from: HumanReview,
            session_live: true,
            auto_end_idle: true,
            ..base()
        },
    );
    assert_eq!(
        d.target, HumanReview,
        "idle teardown never moves the ticket"
    );
    assert_eq!(d.actions, vec![StopSession]);
}

#[test]
fn session_idle_noop_when_auto_end_idle_off() {
    let d = decide(
        Event::SessionIdle,
        &Ctx {
            from: HumanReview,
            session_live: true,
            auto_end_idle: false,
            ..base()
        },
    );
    assert!(
        d.actions.is_empty(),
        "leave the idle session alone when the setting is off"
    );
}

#[test]
fn session_idle_with_question_pending_keeps_session_alive() {
    // An AskUserQuestion keeps the session alive so its answer card can reach a live PTY.
    let d = decide(
        Event::SessionIdle,
        &Ctx {
            from: HumanReview,
            session_live: true,
            auto_end_idle: true,
            user_question_pending: true,
            ..base()
        },
    );
    assert!(
        d.actions.is_empty(),
        "don't free a session that's waiting on the user"
    );
}

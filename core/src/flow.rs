//! Ticket-lifecycle state machine — the single source of truth for "what should happen".
//!
//! This is a PURE decision function: given an [`Event`] (a user column-move or a system event
//! like a finished session) and the ticket's [`Ctx`] (its current state + repo/PR/changes
//! facts), it returns a [`Decision`] — the column the ticket should end in and the ordered list
//! of [`Action`]s to execute. No I/O, no async: trivially testable. The Tauri app's executor
//! (`apply_event` in `app/src-tauri/src/lib.rs`) is the single runtime path that turns these
//! [`Action`]s into real effects, so every lifecycle decision flows through `decide`.
//!
//! The behaviour is pinned by the suite in `core/tests/flow.rs`, and a human-readable diagram +
//! transition table is generated from `decide` itself by [`crate::flow_doc`] (see `docs/flow.md`),
//! so the documentation can never silently drift from the code.

/// A board column. Maps 1:1 to the `crate::status` strings. The user-facing names differ:
/// `HumanReview` is the "For Your Review" (pre-PR sanity check) column, and `Pr` is "In PR
/// Review" (awaiting external GitHub approval).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Column {
    #[default]
    Todo,
    InProgress,
    HumanReview,
    Pr,
    Done,
}

impl Column {
    /// The persisted `crate::status` string for this column.
    pub fn as_status(self) -> &'static str {
        match self {
            Column::Todo => crate::status::TODO,
            Column::InProgress => crate::status::WORKING,
            Column::HumanReview => crate::status::WAITING,
            Column::Pr => crate::status::IN_REVIEW,
            Column::Done => crate::status::DONE,
        }
    }

    /// Parse a persisted `crate::status` string into a `Column`.
    pub fn from_status(s: &str) -> Option<Column> {
        match s {
            crate::status::TODO => Some(Column::Todo),
            crate::status::WORKING => Some(Column::InProgress),
            crate::status::WAITING => Some(Column::HumanReview),
            crate::status::IN_REVIEW => Some(Column::Pr),
            crate::status::DONE => Some(Column::Done),
            _ => None,
        }
    }
}

/// What triggered a decision: a user-initiated column move, or a system event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// User dragged the ticket to `Column`.
    Move(Column),
    /// User asked to build/refine the spec on a Todo ticket (the "grill me" button).
    GrillRequested,
    /// The grill/spec interview finished and captured a spec (drafting 1 → 0).
    GrillFinished,
    /// The autonomous work session reported it has finished implementing.
    WorkFinished,
    /// User asked to (re-)run `/review` on demand (the Review tab's "Request review" button).
    /// Unlike the column-entry review, this ignores `review_current` so it always re-reviews.
    ReviewRequested,
    /// The `/review` run completed.
    ReviewFinished,
    /// A proof-of-work session finished capturing evidence — stop it and fingerprint the evidenced
    /// HEAD (so the proof poller doesn't regenerate until the branch moves).
    ProofFinished,
    /// An autonomous CI-fix session finished — commit + push its changes (re-triggers CI).
    FixFinished,
    /// An autonomous conflict-resolve session finished — commit (completing the merge) + push, which
    /// updates the PR and clears the conflict.
    ConflictFinished,
    /// A feedback-addressing session finished — commit (and push when a PR exists) so the change
    /// is reflected on the branch/PR.
    AddressFinished,
    /// A session came to rest in `waiting` after a `Stop` with no pending question (an idle PTY
    /// with no domain event to tear it down — e.g. a finished grill). Frees the PTY when the
    /// `auto_end_idle` setting is on; otherwise a no-op.
    SessionIdle,
}

/// A side effect to carry out as a result of a decision. Pure markers — the executor (a later
/// task) maps these onto sessions / worktrees / git.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Start a grill/spec interview session.
    StartGrill,
    /// Ensure the ticket's worktree exists — create it if absent, reuse it if already present
    /// (e.g. the grill already created one). Emitted on the fresh-implement path only; resuming
    /// never re-creates a worktree.
    EnsureWorktree,
    /// Start a fresh autonomous work session: the one-time plan-from-spec run, then implement.
    /// Paired with `EnsureWorktree`.
    StartImplement,
    /// Resume the existing work session and continue where it left off (worktree already exists).
    ResumeWork,
    /// Stop the live Claude session.
    StopSession,
    /// Run the `/review` skill (a read-only session that emits suggestions, then stops).
    RunReview,
    /// Commit the worktree's working changes (harmony owns version control). A no-op when the
    /// tree is clean. Emitted before review / push so they see committed state.
    CommitChanges,
    /// Push the ticket's branch to its remote (re-triggers CI / updates the PR).
    PushBranch,
    /// Fingerprint the current HEAD as the reviewed SHA so `/review` isn't re-run until the branch
    /// moves again (drives [`Ctx::review_current`]).
    MarkReviewed,
    /// Fingerprint the current HEAD as the proof SHA and capture the proof session's media artifacts
    /// (scan the artifact dir), so the proof poller doesn't regenerate until the branch moves.
    MarkProofDone,
    /// Push the branch and open a draft PR.
    OpenPr,
    /// Merge the approved PR.
    MergePr,
    /// Remove the ticket's worktree from disk + DB.
    DeleteWorktree,
}

/// A non-blocking warning to surface in the UI (e.g. on the ticket card).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Warning {
    /// The ticket has no repo assigned — it can't progress past Todo until one is.
    NoRepo,
}

/// Everything `decide` needs to know about a ticket's current state and the world.
#[derive(Debug, Clone, Default)]
pub struct Ctx {
    /// A repo/project is assigned (mandatory before leaving Todo).
    pub has_repo: bool,
    /// A spec has been produced (grilled and captured).
    pub has_spec: bool,
    /// A grill/spec interview is currently running.
    pub drafting: bool,
    /// The one-time plan-from-spec run has happened (=> resume, not a fresh implement).
    pub planned: bool,
    /// A Claude session is currently live for this ticket.
    pub session_live: bool,
    /// The ticket's column before this event.
    pub from: Column,
    /// A worktree currently exists on disk for this ticket (gates `DeleteWorktree`; a resume
    /// implies this is true).
    pub has_worktree: bool,
    /// The worktree has code changes vs the base branch (something to review / PR).
    pub has_changes: bool,
    /// `/review` has already run against the current change-set (no new changes since).
    pub review_current: bool,
    /// The ticket has been through the human-review column at least once.
    pub reviewed: bool,
    /// A PR exists for the branch.
    pub pr_exists: bool,
    /// The PR has been approved externally on GitHub.
    pub pr_approved: bool,
    /// The PR has already been merged on GitHub (state == MERGED) — so a move to Done should just
    /// clean up, never try to merge again.
    pub pr_merged: bool,
    /// The ticket is linked to Jira (used for the repo-less warning surface).
    pub is_jira: bool,
    /// A live `AskUserQuestion` is outstanding for this ticket — Claude is waiting on the user, so
    /// a `WorkFinished`/`SessionIdle` `Stop` is NOT a real "done" and must keep the session alive.
    pub user_question_pending: bool,
    /// The `auto_end_idle` setting is on: free an idle session's PTY instead of leaving it hanging.
    pub auto_end_idle: bool,
}

/// The outcome of a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// The column the ticket should end in (may differ from a requested move — an auto-redirect,
    /// e.g. Pr → HumanReview when it hasn't been reviewed).
    pub target: Column,
    /// Ordered side effects to execute.
    pub actions: Vec<Action>,
    /// `Some(reason)` when the transition is refused: the ticket stays in `Ctx::from` and no
    /// actions run.
    pub blocked: Option<&'static str>,
}

impl Decision {
    /// A refusal that leaves the ticket where it was.
    pub fn blocked(from: Column, reason: &'static str) -> Decision {
        Decision {
            target: from,
            actions: Vec::new(),
            blocked: Some(reason),
        }
    }
}

/// Decide what should happen for `event` given the ticket's `ctx`. Pure — the contract is
/// pinned by `core/tests/flow.rs`.
pub fn decide(event: Event, ctx: &Ctx) -> Decision {
    use Action::*;
    use Column::*;

    // `[StopSession]` when a session is live, else nothing.
    let stop_if_live = || -> Vec<Action> {
        if ctx.session_live {
            vec![StopSession]
        } else {
            vec![]
        }
    };
    // Entering Human review: stop the work session (if any), then run `/review` only when there
    // are changes we haven't already reviewed.
    let enter_human_review = || -> Decision {
        let mut actions = stop_if_live();
        if ctx.has_changes && !ctx.review_current {
            actions.push(RunReview);
        }
        Decision {
            target: HumanReview,
            actions,
            blocked: None,
        }
    };

    match event {
        Event::Move(to) => {
            // Re-moving to the current column is a no-op (incl. Done → Done — handled before the
            // terminal-Done guard so it isn't reported as blocked).
            if to == ctx.from {
                return Decision {
                    target: ctx.from,
                    actions: vec![],
                    blocked: None,
                };
            }
            // Done is terminal.
            if ctx.from == Done {
                return Decision::blocked(ctx.from, "Done is terminal");
            }
            // A repo is mandatory before any working stage (Done may abandon without one).
            if !ctx.has_repo && matches!(to, InProgress | HumanReview | Pr) {
                return Decision::blocked(ctx.from, "assign a repo first");
            }
            match to {
                Todo => Decision {
                    target: Todo,
                    actions: stop_if_live(),
                    blocked: None,
                },
                InProgress => {
                    if ctx.drafting {
                        Decision::blocked(ctx.from, "finish the interview first")
                    } else if !ctx.has_spec {
                        Decision {
                            target: InProgress,
                            actions: vec![StartGrill],
                            blocked: None,
                        }
                    } else if ctx.planned {
                        Decision {
                            target: InProgress,
                            actions: vec![ResumeWork],
                            blocked: None,
                        }
                    } else {
                        Decision {
                            target: InProgress,
                            actions: vec![EnsureWorktree, StartImplement],
                            blocked: None,
                        }
                    }
                }
                HumanReview => enter_human_review(),
                Pr => {
                    if !ctx.has_changes {
                        Decision::blocked(ctx.from, "no changes to open a PR for")
                    } else if !ctx.reviewed {
                        // Must go through human review before a PR — redirect there.
                        enter_human_review()
                    } else {
                        let mut actions = stop_if_live();
                        if !ctx.pr_exists {
                            actions.push(OpenPr);
                        }
                        Decision {
                            target: Pr,
                            actions,
                            blocked: None,
                        }
                    }
                }
                Done => {
                    let mut actions = stop_if_live();
                    // Merge only an open, approved PR ourselves. If it's already merged on GitHub
                    // (a human merged it), skip MergePr — moving to Done is just cleanup.
                    if ctx.pr_exists && ctx.pr_approved && !ctx.pr_merged {
                        actions.push(MergePr);
                    }
                    if ctx.has_worktree {
                        actions.push(DeleteWorktree);
                    }
                    Decision {
                        target: Done,
                        actions,
                        blocked: None,
                    }
                }
            }
        }

        // Grill is Todo-only, needs a repo, and won't start a second concurrent grill.
        Event::GrillRequested => {
            if ctx.from != Todo {
                Decision::blocked(ctx.from, "grill is only available in Todo")
            } else if !ctx.has_repo {
                Decision::blocked(ctx.from, "assign a repo first")
            } else if ctx.drafting {
                Decision::blocked(ctx.from, "already drafting a spec")
            } else {
                Decision {
                    target: Todo,
                    actions: vec![StartGrill],
                    blocked: None,
                }
            }
        }

        // Spec captured. From In Progress (grill kicked off en route to work) → implement now;
        // from Todo → just save and stay; arriving off either is a stale event → only stop.
        Event::GrillFinished => {
            let mut actions = vec![StopSession];
            if ctx.from == InProgress {
                actions.push(EnsureWorktree);
                actions.push(StartImplement);
            }
            Decision {
                target: ctx.from,
                actions,
                blocked: None,
            }
        }

        // Autonomous work done → commit the changes, move to Human review and review them. A
        // WorkFinished arriving when the ticket is no longer In Progress is stale and ignored; a
        // WorkFinished while a question is outstanding isn't really "done" — Claude is waiting on
        // the user, so leave the session live in In Progress.
        Event::WorkFinished => {
            if ctx.from != InProgress {
                return Decision {
                    target: ctx.from,
                    actions: vec![],
                    blocked: None,
                };
            }
            if ctx.user_question_pending {
                return Decision {
                    target: InProgress,
                    actions: vec![],
                    blocked: None,
                };
            }
            // Commit first so the review and the reviewed-SHA fingerprint see committed state.
            let mut actions = vec![CommitChanges, StopSession];
            if ctx.has_changes && !ctx.review_current {
                actions.push(RunReview);
            }
            Decision {
                target: HumanReview,
                actions,
                blocked: None,
            }
        }

        // User pressed "Request review": (re-)run `/review` in place, ignoring `review_current`
        // so it re-reviews even when HEAD hasn't moved. Needs a repo and actual changes; stops a
        // live session first. The card stays in its current column.
        Event::ReviewRequested => {
            if !ctx.has_repo {
                Decision::blocked(ctx.from, "assign a repo first")
            } else if !ctx.has_changes {
                Decision::blocked(ctx.from, "no changes to review")
            } else {
                let mut actions = stop_if_live();
                actions.push(RunReview);
                Decision {
                    target: ctx.from,
                    actions,
                    blocked: None,
                }
            }
        }

        // The /review run finished: stop its session and fingerprint the reviewed HEAD (so the
        // column-entry review isn't re-run until the branch moves). The ticket stays where it is.
        Event::ReviewFinished => Decision {
            target: ctx.from,
            actions: vec![StopSession, MarkReviewed],
            blocked: None,
        },

        // A proof session finished: stop it and fingerprint the evidenced HEAD (+ capture artifacts).
        // The ticket stays where it is — proof is produced in place in the review column. Mirrors
        // `ReviewFinished`. The proof session is spawned by the poller (`poll_proof_loop_once`), not
        // by `decide`, exactly as the review-loop judge spawns its fix sessions.
        Event::ProofFinished => Decision {
            target: ctx.from,
            actions: vec![StopSession, MarkProofDone],
            blocked: None,
        },

        // An autonomous CI-fix session finished: commit + push its changes (re-triggers CI). The
        // card belongs in the PR column (that's the only stage CI-fixes run in).
        Event::FixFinished => Decision {
            target: Pr,
            actions: vec![CommitChanges, PushBranch],
            blocked: None,
        },

        // An autonomous conflict-resolve session finished: commit (completing the base-merge) + push,
        // updating the PR. Same shape as FixFinished; the card stays in the PR column.
        Event::ConflictFinished => Decision {
            target: Pr,
            actions: vec![CommitChanges, PushBranch],
            blocked: None,
        },

        // A feedback-addressing session finished: commit, and push only when a PR already exists
        // (so we don't create a remote branch pre-PR). Land back in the review column — PR if a PR
        // exists, otherwise Human review.
        Event::AddressFinished => {
            let mut actions = vec![CommitChanges];
            let target = if ctx.pr_exists {
                actions.push(PushBranch);
                Pr
            } else {
                HumanReview
            };
            Decision {
                target,
                actions,
                blocked: None,
            }
        }

        // An idle session came to rest with no domain event to tear it down. Free its PTY when
        // `auto_end_idle` is on and Claude isn't waiting on a question; otherwise leave it be. The
        // ticket stays in its column (resume by moving it back).
        Event::SessionIdle => {
            let actions = if ctx.auto_end_idle && !ctx.user_question_pending {
                vec![StopSession]
            } else {
                vec![]
            };
            Decision {
                target: ctx.from,
                actions,
                blocked: None,
            }
        }
    }
}

/// Non-blocking warnings to surface for a ticket in its current state. Pure.
pub fn warnings(ctx: &Ctx) -> Vec<Warning> {
    if ctx.has_repo {
        vec![]
    } else {
        vec![Warning::NoRepo]
    }
}

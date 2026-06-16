//! Ticket-lifecycle state machine (the single source of truth for "what should happen").
//!
//! This is a PURE decision function: given an [`Event`] (a user column-move or a system event
//! like a finished session) and the ticket's [`Ctx`] (its current state + repo/PR/changes
//! facts), it returns a [`Decision`] — the column the ticket should end in and the ordered list
//! of [`Action`]s to execute. No I/O, no async: trivially testable, and meant to replace the
//! orchestration logic currently scattered across the React frontend and the Tauri commands.
//!
//! Status: the behaviour is specified by the suite in `core/tests/flow.rs`; `decide`/`warnings`
//! are intentionally `todo!()` until that spec is implemented.

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
    /// The `/review` run completed.
    ReviewFinished,
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
    /// The ticket is linked to Jira (used for the repo-less warning surface).
    pub is_jira: bool,
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
        Decision { target: from, actions: Vec::new(), blocked: Some(reason) }
    }
}

/// Decide what should happen for `event` given the ticket's `ctx`. Pure.
///
/// Unimplemented: the contract is pinned by `core/tests/flow.rs`.
pub fn decide(_event: Event, _ctx: &Ctx) -> Decision {
    todo!("implement the lifecycle state machine specified by core/tests/flow.rs")
}

/// Non-blocking warnings to surface for a ticket in its current state. Pure.
///
/// Unimplemented: the contract is pinned by `core/tests/flow.rs`.
pub fn warnings(_ctx: &Ctx) -> Vec<Warning> {
    todo!("implement the warning rules specified by core/tests/flow.rs")
}

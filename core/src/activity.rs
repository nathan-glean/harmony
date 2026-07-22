//! State-machine-derived "what's happening" status for a ticket.
//!
//! A pure classifier (sibling of [`crate::flow::decide`]/[`crate::flow::warnings`]): given the
//! ticket's current facts + the autonomy settings, it returns a single [`Activity`] the UI renders
//! as a per-card pill. The guiding rule is: **if the system will act on this state automatically
//! (given the settings + attempt caps), it's [`Category::Working`] (auto); once it has done all it
//! can and now needs a person or an external party, it's [`Category::WaitingOnYou`] /
//! [`Category::WaitingExternal`].** No I/O — the app gathers the [`ActivityInput`] and persists the
//! result (`tickets.activity`).

use serde::Serialize;

use crate::flow::Column;

/// The coarse bucket that drives the pill's colour. Serialized snake_case for the frontend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// The system is doing something autonomously (or will, imminently) — no user action needed.
    Working,
    /// Blocked on the user: an answer, a decision, or a manual step.
    WaitingOnYou,
    /// Blocked on something outside harmony — a GitHub PR approval, external CI.
    WaitingExternal,
    /// Nothing is happening and nothing is owed (Todo at rest, Done, paused).
    Idle,
}

/// A ticket's current activity, persisted as JSON on `tickets.activity` and rendered as a pill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Activity {
    pub category: Category,
    /// Short label for the pill (e.g. "Implementing…", "Awaiting PR approval").
    pub label: String,
    /// Optional longer explanation for the ticket modal (e.g. the attempt count, the next step).
    pub detail: Option<String>,
}

/// Everything [`classify`] needs. The app builds this from `flow::Ctx`, the ticket's persisted
/// fields, the live session's `kind`, and the autonomy settings.
#[derive(Debug, Clone, Default)]
pub struct ActivityInput {
    /// The ticket's current column.
    pub from: Column,
    pub has_repo: bool,
    /// A Claude session is live for this ticket.
    pub session_live: bool,
    /// The live session's kind ("work" | "spec" | "review" | "proof" | "fix" | "address"), if any.
    pub session_kind: Option<String>,
    /// A live `AskUserQuestion` is outstanding (Claude is waiting on the user).
    pub user_question_pending: bool,
    /// The worktree has changes vs base (something to review / PR).
    pub has_changes: bool,
    /// `/review` has run against the current HEAD (no new changes since the last review).
    pub review_current: bool,
    /// The ticket has been through review at least once.
    pub reviewed: bool,
    /// The review judge's latest verdict is `changes_requested`.
    pub review_changes_requested: bool,
    pub review_fix_attempts: i64,
    pub review_fix_max: i64,
    /// CI has failing checks (per the latest triage).
    pub ci_failing: bool,
    pub ci_fix_attempts: i64,
    pub ci_fix_max: i64,
    pub pr_exists: bool,
    pub pr_approved: bool,
    // ---- autonomy settings (decide whether "the system will handle it") ----
    pub auto_review: bool,
    pub review_loop: bool,
    pub ci_autofix: bool,
    pub auto_merge: bool,
}

fn working(label: &str) -> Activity {
    Activity {
        category: Category::Working,
        label: label.into(),
        detail: None,
    }
}
fn waiting_you(label: &str) -> Activity {
    Activity {
        category: Category::WaitingOnYou,
        label: label.into(),
        detail: None,
    }
}
fn waiting_external(label: &str) -> Activity {
    Activity {
        category: Category::WaitingExternal,
        label: label.into(),
        detail: None,
    }
}
fn idle(label: &str) -> Activity {
    Activity {
        category: Category::Idle,
        label: label.into(),
        detail: None,
    }
}

/// Derive the ticket's [`Activity`] from its facts. Pure; pinned by the tests below.
pub fn classify(i: &ActivityInput) -> Activity {
    use Column::*;

    // A live session: the agent is actively working — unless it has stopped to ask the user.
    if i.session_live {
        if i.user_question_pending {
            return waiting_you("Waiting for your answer");
        }
        return match i.session_kind.as_deref() {
            Some("spec") => working("Drafting spec…"),
            Some("review") => working("Reviewing…"),
            Some("proof") => working("Capturing proof…"),
            Some("fix") => working("Fixing CI…"),
            Some("conflict") => working("Resolving conflicts…"),
            Some("address") => working("Addressing feedback…"),
            // "work" or anything else.
            _ => working("Implementing…"),
        };
    }

    match i.from {
        Todo => {
            if i.has_repo {
                idle("Todo")
            } else {
                waiting_you("Assign a repo")
            }
        }
        // In Progress with no live session = work was paused/interrupted (resume by moving it).
        InProgress => idle("Paused"),
        HumanReview => {
            // A reviewed branch whose HEAD has moved: auto re-review will pick it up.
            if i.reviewed && !i.review_current && i.auto_review {
                return working("Re-reviewing soon…");
            }
            if i.review_changes_requested {
                let escalated = i.review_fix_attempts >= i.review_fix_max;
                return if escalated {
                    Activity {
                        detail: Some(format!(
                            "Auto-fix gave up after {} attempts — review the findings.",
                            i.review_fix_max
                        )),
                        ..waiting_you("Review loop needs you")
                    }
                } else if i.review_loop {
                    Activity {
                        detail: Some(format!(
                            "Auto-fixing review findings (attempt {}/{}).",
                            i.review_fix_attempts + 1,
                            i.review_fix_max
                        )),
                        ..working("Fixing review findings…")
                    }
                } else {
                    Activity {
                        detail: Some("Turn on the review loop, or address the findings.".into()),
                        ..waiting_you("Review found issues")
                    }
                };
            }
            if i.has_changes {
                Activity {
                    detail: Some("Drag to In PR Review to open the PR.".into()),
                    ..waiting_you("Ready to open PR")
                }
            } else {
                idle("For your review")
            }
        }
        Pr => {
            if i.ci_failing {
                let exhausted = i.ci_fix_attempts >= i.ci_fix_max;
                return if i.ci_autofix && !exhausted {
                    working("Fixing CI…")
                } else {
                    waiting_you("CI failing")
                };
            }
            if i.pr_exists && i.pr_approved {
                return if i.auto_merge {
                    working("Merging…")
                } else {
                    Activity {
                        detail: Some("Drag to Done to merge.".into()),
                        ..waiting_you("Approved — ready to merge")
                    }
                };
            }
            if i.pr_exists {
                waiting_external("Awaiting PR approval")
            } else {
                idle("In PR review")
            }
        }
        Done => idle("Done"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(i: &ActivityInput) -> Category {
        classify(i).category
    }

    // Sensible defaults: a repo is assigned, caps at 3.
    fn base() -> ActivityInput {
        ActivityInput {
            has_repo: true,
            review_fix_max: 3,
            ci_fix_max: 3,
            ..Default::default()
        }
    }

    #[test]
    fn live_session_is_working_by_kind() {
        for (kind, label) in [
            ("spec", "Drafting spec…"),
            ("review", "Reviewing…"),
            ("fix", "Fixing CI…"),
            ("address", "Addressing feedback…"),
            ("work", "Implementing…"),
        ] {
            let a = classify(&ActivityInput {
                session_live: true,
                session_kind: Some(kind.into()),
                ..base()
            });
            assert_eq!(a.category, Category::Working, "{kind}");
            assert_eq!(a.label, label, "{kind}");
        }
    }

    #[test]
    fn pending_question_beats_live_work() {
        let a = classify(&ActivityInput {
            session_live: true,
            session_kind: Some("work".into()),
            user_question_pending: true,
            ..base()
        });
        assert_eq!(a.category, Category::WaitingOnYou);
        assert_eq!(a.label, "Waiting for your answer");
    }

    #[test]
    fn todo_needs_repo() {
        assert_eq!(
            cat(&ActivityInput {
                from: Column::Todo,
                has_repo: false,
                ..base()
            }),
            Category::WaitingOnYou
        );
        assert_eq!(
            cat(&ActivityInput {
                from: Column::Todo,
                ..base()
            }),
            Category::Idle
        );
    }

    #[test]
    fn review_loop_on_under_cap_is_working() {
        let a = classify(&ActivityInput {
            from: Column::HumanReview,
            reviewed: true,
            review_current: true,
            review_changes_requested: true,
            review_loop: true,
            review_fix_attempts: 1,
            ..base()
        });
        assert_eq!(a.category, Category::Working);
        assert!(a.detail.unwrap().contains("attempt 2/3"));
    }

    #[test]
    fn review_loop_at_cap_escalates_to_user() {
        let a = classify(&ActivityInput {
            from: Column::HumanReview,
            reviewed: true,
            review_current: true,
            review_changes_requested: true,
            review_loop: true,
            review_fix_attempts: 3,
            ..base()
        });
        assert_eq!(a.category, Category::WaitingOnYou);
        assert_eq!(a.label, "Review loop needs you");
    }

    #[test]
    fn review_changes_with_loop_off_waits_on_user() {
        let a = classify(&ActivityInput {
            from: Column::HumanReview,
            reviewed: true,
            review_current: true,
            review_changes_requested: true,
            review_loop: false,
            ..base()
        });
        assert_eq!(a.category, Category::WaitingOnYou);
        assert_eq!(a.label, "Review found issues");
    }

    #[test]
    fn clean_review_is_ready_to_open_pr() {
        let a = classify(&ActivityInput {
            from: Column::HumanReview,
            reviewed: true,
            review_current: true,
            has_changes: true,
            ..base()
        });
        assert_eq!(a.category, Category::WaitingOnYou);
        assert_eq!(a.label, "Ready to open PR");
    }

    #[test]
    fn stale_review_with_auto_review_is_working() {
        let a = classify(&ActivityInput {
            from: Column::HumanReview,
            reviewed: true,
            review_current: false,
            auto_review: true,
            has_changes: true,
            ..base()
        });
        assert_eq!(a.category, Category::Working);
    }

    #[test]
    fn pr_awaiting_approval_is_external() {
        let a = classify(&ActivityInput {
            from: Column::Pr,
            pr_exists: true,
            pr_approved: false,
            ..base()
        });
        assert_eq!(a.category, Category::WaitingExternal);
        assert_eq!(a.label, "Awaiting PR approval");
    }

    #[test]
    fn approved_pr_auto_merge_off_vs_on() {
        let off = classify(&ActivityInput {
            from: Column::Pr,
            pr_exists: true,
            pr_approved: true,
            auto_merge: false,
            ..base()
        });
        assert_eq!(off.category, Category::WaitingOnYou);
        let on = classify(&ActivityInput {
            from: Column::Pr,
            pr_exists: true,
            pr_approved: true,
            auto_merge: true,
            ..base()
        });
        assert_eq!(on.category, Category::Working);
        assert_eq!(on.label, "Merging…");
    }

    #[test]
    fn ci_failing_autofix_on_vs_off() {
        let on = classify(&ActivityInput {
            from: Column::Pr,
            pr_exists: true,
            ci_failing: true,
            ci_autofix: true,
            ci_fix_attempts: 1,
            ..base()
        });
        assert_eq!(on.category, Category::Working);
        let exhausted = classify(&ActivityInput {
            from: Column::Pr,
            pr_exists: true,
            ci_failing: true,
            ci_autofix: true,
            ci_fix_attempts: 3,
            ..base()
        });
        assert_eq!(exhausted.category, Category::WaitingOnYou);
        let off = classify(&ActivityInput {
            from: Column::Pr,
            pr_exists: true,
            ci_failing: true,
            ci_autofix: false,
            ..base()
        });
        assert_eq!(off.category, Category::WaitingOnYou);
    }

    #[test]
    fn done_is_idle() {
        assert_eq!(
            cat(&ActivityInput {
                from: Column::Done,
                ..base()
            }),
            Category::Idle
        );
    }

    #[test]
    fn same_state_flips_with_autonomy_setting() {
        // The whole point: identical facts read as auto vs needs-you depending on the setting.
        let facts = |auto_merge| ActivityInput {
            from: Column::Pr,
            pr_exists: true,
            pr_approved: true,
            auto_merge,
            ..base()
        };
        assert_eq!(cat(&facts(true)), Category::Working);
        assert_eq!(cat(&facts(false)), Category::WaitingOnYou);
    }
}

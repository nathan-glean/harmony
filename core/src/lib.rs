//! harmony-core (Phase 1, headless).
//!
//! The engine behind the future Tauri app: a SQLite store, a repo registry, a git
//! worktree manager, the local hook server (lifted + corrected from the Phase 0 spike),
//! a per-worktree settings injector, and a PTY-based Claude session manager.
//!
//! No UI here — exercised via the `harmony` CLI (see `main.rs`).

pub mod cmd_err;
pub mod draft;
pub mod github;
pub mod hooks;
pub mod jira;
pub mod models;
pub mod session;
pub mod settings;
pub mod spec;
pub mod store;
pub mod worktree;

/// Unix epoch seconds, for created/updated/started/ended timestamps.
pub fn now_unix() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Ticket lifecycle states — the harmony-native board columns (DESIGN Q14).
pub mod status {
    /// Default landing column for new tickets (displayed as "Todo").
    pub const TODO: &str = "todo";
    pub const WORKING: &str = "working"; // "In Progress"
    pub const WAITING: &str = "waiting"; // "For Your Review"
    pub const IN_REVIEW: &str = "in_review"; // "In PR Review"
    pub const DONE: &str = "done";

    /// The board columns, in order, with display labels.
    pub const COLUMNS: [(&str, &str); 5] = [
        (TODO, "Todo"),
        (WORKING, "In Progress"),
        (WAITING, "For Your Review"),
        (IN_REVIEW, "In PR Review"),
        (DONE, "Done"),
    ];

    pub fn is_valid(s: &str) -> bool {
        COLUMNS.iter().any(|(k, _)| *k == s)
    }
}

//! harmony-core (Phase 1, headless).
//!
//! The engine behind the future Tauri app: a SQLite store, a repo registry, a git
//! worktree manager, the local hook server (lifted + corrected from the Phase 0 spike),
//! a per-worktree settings injector, and a PTY-based Claude session manager.
//!
//! No UI here — exercised via the `harmony` CLI (see `main.rs`).

pub mod draft;
pub mod github;
pub mod hooks;
pub mod jira;
pub mod models;
pub mod session;
pub mod settings;
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
    pub const AVAILABLE: &str = "available";
    pub const READY: &str = "ready";
    pub const WORKING: &str = "working";
    pub const WAITING: &str = "waiting";
    pub const IN_REVIEW: &str = "in_review";
    pub const DONE: &str = "done";
}

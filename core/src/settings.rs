//! Settings injector: adds harmony's HTTP hooks to a worktree session.
//!
//! IMPORTANT (Phase 1 finding): the repo's `.claude/settings.json` is typically a
//! *tracked, committed* file holding the team's own (safety) hooks. We must never
//! write it — doing so clobbers those hooks and creates a spurious diff the agent sees.
//!
//! Instead we merge into `.claude/settings.local.json`, which is gitignored by Claude
//! Code convention and whose hooks MERGE additively with settings.json — so the repo's
//! hooks stay active and ours fire too. We merge (never overwrite) and replace only our
//! own previously-injected entries, so re-runs are idempotent and we don't disturb the
//! repo's files or Claude's own locally-saved choices (e.g. MCP/trust).
//!
//! Only the hooks that actually fire for an externally-launched interactive session are
//! wired (Phase 0 finding): PreToolUse / PostToolUse / Stop / Notification.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::Path;

pub fn inject_hooks(worktree_path: &str, port: u16) -> Result<()> {
    let dir = Path::new(worktree_path).join(".claude");
    std::fs::create_dir_all(&dir)?;
    let file = dir.join("settings.local.json");

    let mut root: Value = match std::fs::read(&file) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({})),
        Err(_) => json!({}),
    };
    if !root.is_object() {
        root = json!({});
    }

    let base = format!("http://127.0.0.1:{port}/hook");
    // (event, needs a tool matcher)
    let events: [(&str, bool); 4] = [
        ("PreToolUse", true),
        ("PostToolUse", true),
        ("Stop", false),
        ("Notification", false),
    ];

    let obj = root.as_object_mut().unwrap();
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("`hooks` in settings.local.json is not an object"))?;

    for (event, with_matcher) in events {
        let arr = hooks
            .entry(event.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| anyhow!("`hooks.{event}` is not an array"))?;

        // Idempotent: drop our previously-injected entries, keep everyone else's.
        arr.retain(|e| {
            let s = serde_json::to_string(e).unwrap_or_default();
            !(s.contains("127.0.0.1") && s.contains("/hook/"))
        });

        let http = json!({ "type": "http", "url": format!("{base}/{event}"), "timeout": 30 });
        let entry = if with_matcher {
            json!({ "matcher": "*", "hooks": [http] })
        } else {
            json!({ "hooks": [http] })
        };
        arr.push(entry);
    }

    std::fs::write(&file, serde_json::to_string_pretty(&root)?)?;
    Ok(())
}

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
use std::path::{Path, PathBuf};

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

/// Remove harmony's previously-injected hooks from `<dir>/.claude/settings.local.json`,
/// preserving every other setting. Used to clean a repo root that an earlier version polluted
/// (the grill now runs in an isolated scratch dir, never the repo). No-op when the file or our
/// hooks are absent.
pub fn remove_hooks(dir: &str, port: u16) -> Result<()> {
    let file = Path::new(dir).join(".claude").join("settings.local.json");
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };
    let mut root: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    let needle = format!("127.0.0.1:{port}/hook");
    let mut changed = false;

    if let Some(obj) = root.as_object_mut() {
        let mut hooks_now_empty = false;
        if let Some(hooks) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) {
            let events: Vec<String> = hooks.keys().cloned().collect();
            for event in events {
                if let Some(arr) = hooks.get_mut(&event).and_then(|a| a.as_array_mut()) {
                    let before = arr.len();
                    arr.retain(|e| {
                        !serde_json::to_string(e)
                            .unwrap_or_default()
                            .contains(&needle)
                    });
                    if arr.len() != before {
                        changed = true;
                    }
                    if arr.is_empty() {
                        hooks.remove(&event);
                    }
                }
            }
            hooks_now_empty = hooks.is_empty();
        }
        if hooks_now_empty {
            obj.remove("hooks");
        }
    }

    if changed {
        std::fs::write(&file, serde_json::to_string_pretty(&root)?)?;
    }
    Ok(())
}

/// The central harmony home (`~/.harmony`), created if absent.
fn harmony_home() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    Path::new(&home).join(".harmony")
}

/// The per-ticket proof artifact directory (`~/.harmony/proof/<ticket_id>`). Media the proof session
/// captures lands here — OUTSIDE the target repo, so nothing is ever committed to it. Read back by
/// the executor (to scan for artifacts) and by the UI (to serve them). Does not create the dir.
pub fn proof_artifact_dir(ticket_id: i64) -> PathBuf {
    harmony_home().join("proof").join(ticket_id.to_string())
}

/// Environment for a proof-capture session: the artifact output dir plus the shared toolchain env,
/// so capture tools are provisioned once centrally (never per-repo) and reused across tickets.
pub struct ProofEnv {
    /// `~/.harmony/proof/<ticket_id>` — where the session must write all media. Created fresh.
    pub artifact_dir: String,
    /// Env vars to inject into the proof `claude` process (as `(name, value)` pairs).
    pub env: Vec<(String, String)>,
}

/// Provision the central capture toolchain and return the env for a ticket's proof session.
///
/// "Zero-install per repo": the heavy bits live under `~/.harmony/tools` and are shared across every
/// repo/ticket — a single Playwright browser cache (`PLAYWRIGHT_BROWSERS_PATH`) that `npx playwright`
/// populates once and reuses, and a `~/.harmony/tools/bin` dir prepended to PATH for any
/// harmony-provided helpers. The proof *methodology* is inlined into the session prompt (like the
/// grill), so there is no skill to install anywhere. The per-run artifact dir is (re)created empty so
/// each run's evidence is self-contained.
pub fn provision_proof_env(ticket_id: i64) -> Result<ProofEnv> {
    let home = harmony_home();
    let tools = home.join("tools");
    let browsers = tools.join("pw-browsers");
    let bin = tools.join("bin");
    std::fs::create_dir_all(&browsers)?;
    std::fs::create_dir_all(&bin)?;

    // Fresh, empty artifact dir per run.
    let artifact_dir = proof_artifact_dir(ticket_id);
    if artifact_dir.exists() {
        let _ = std::fs::remove_dir_all(&artifact_dir);
    }
    std::fs::create_dir_all(&artifact_dir)?;

    // Prepend our bin dir to PATH so any harmony-provided capture helpers win; keep the inherited PATH.
    let path = match std::env::var("PATH") {
        Ok(p) => format!("{}:{}", bin.to_string_lossy(), p),
        Err(_) => bin.to_string_lossy().to_string(),
    };

    Ok(ProofEnv {
        artifact_dir: artifact_dir.to_string_lossy().to_string(),
        env: vec![
            (
                "PLAYWRIGHT_BROWSERS_PATH".to_string(),
                browsers.to_string_lossy().to_string(),
            ),
            (
                "HARMONY_PROOF_DIR".to_string(),
                artifact_dir.to_string_lossy().to_string(),
            ),
            ("PATH".to_string(), path),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p =
            std::env::temp_dir().join(format!("harmony-settings-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn remove_hooks_strips_ours_and_keeps_others() {
        let dir = temp_dir();
        // A pre-existing user setting + a foreign hook that must survive.
        let claude = dir.join(".claude");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::write(
            claude.join("settings.local.json"),
            r#"{"model":"opus","hooks":{"PreToolUse":[{"matcher":"*","hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
        )
        .unwrap();

        inject_hooks(dir.to_str().unwrap(), 8787).unwrap();
        // Sanity: our hooks are present after inject.
        let after_inject = std::fs::read_to_string(claude.join("settings.local.json")).unwrap();
        assert!(after_inject.contains("127.0.0.1:8787/hook"));

        remove_hooks(dir.to_str().unwrap(), 8787).unwrap();
        let cleaned = std::fs::read_to_string(claude.join("settings.local.json")).unwrap();
        // Ours gone, the foreign hook + unrelated key preserved.
        assert!(!cleaned.contains("127.0.0.1"));
        assert!(cleaned.contains("echo hi"));
        assert!(cleaned.contains("\"model\""));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_hooks_absent_file_is_noop() {
        let dir = temp_dir();
        assert!(remove_hooks(dir.to_str().unwrap(), 8787).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

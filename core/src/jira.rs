//! Jira via the official Atlassian CLI (`acli`) — shell-outs, the same pattern as `gh`
//! for GitHub. acli owns auth (`acli jira auth login`: browser, no app registration, no
//! API key), so harmony stores no Jira credentials of its own.
//!
//! NOTE: acli's `--json` schema is not documented. `parse_issues` is intentionally
//! defensive (array or wrapped object; flat or nested fields). Verify the field mapping
//! once against real `acli jira workitem search --json` output and tighten if needed.

use anyhow::{anyhow, Result};
use serde_json::Value;
use tokio::process::Command;

pub struct JiraIssue {
    pub key: String,
    pub summary: String,
    pub description: String,
    pub status: String,
}

/// PATH augmented with common Homebrew bin dirs. macOS GUI apps (a bundled Tauri app
/// launched from Finder) inherit a minimal PATH that omits `/opt/homebrew/bin` and
/// `/usr/local/bin`, so `acli`/`brew` wouldn't be found even when installed. Prepending
/// them fixes detection in both `tauri dev` and a packaged app.
fn bin_path() -> String {
    let mut path = std::env::var("PATH").unwrap_or_default();
    for extra in ["/opt/homebrew/bin", "/usr/local/bin"] {
        if !path.split(':').any(|d| d == extra) {
            if !path.is_empty() {
                path.push(':');
            }
            path.push_str(extra);
        }
    }
    path
}

async fn acli(args: &[&str]) -> Result<String> {
    let out = Command::new("acli")
        .args(args)
        .env("PATH", bin_path())
        .output()
        .await
        .map_err(|e| anyhow!(
            "the Atlassian CLI (acli) isn't installed ({e}). Install it: \
             `brew tap atlassian/homebrew-acli && brew install acli` \
             (or run `harmony jira install`, or see https://developer.atlassian.com/cloud/acli/guides/install-macos/)"
        ))?;
    if !out.status.success() {
        return Err(anyhow!(
            "acli {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Whether `acli` is installed and runnable.
pub fn cli_installed() -> bool {
    std::process::Command::new("acli")
        .arg("--version")
        .env("PATH", bin_path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Best-effort install via Homebrew (macOS). Returns the installed version string.
/// Errors with manual-install guidance if Homebrew isn't present.
pub fn install_via_brew() -> Result<String> {
    let brew_ok = std::process::Command::new("brew")
        .arg("--version")
        .env("PATH", bin_path())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !brew_ok {
        return Err(anyhow!(
            "Homebrew not found. Install acli manually: https://developer.atlassian.com/cloud/acli/guides/install-macos/"
        ));
    }
    let brew = |args: &[&str]| -> Result<()> {
        let out = std::process::Command::new("brew")
            .args(args)
            .env("PATH", bin_path())
            .output()?;
        if !out.status.success() {
            return Err(anyhow!("brew {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr).trim()));
        }
        Ok(())
    };
    brew(&["tap", "atlassian/homebrew-acli"])?;
    brew(&["install", "acli"])?;
    let v = std::process::Command::new("acli")
        .arg("--version")
        .env("PATH", bin_path())
        .output()?;
    Ok(String::from_utf8_lossy(&v.stdout).trim().to_string())
}

// ---- auth ----------------------------------------------------------------

/// Interactive login (browser + terminal site picker). CLI-only — needs a real terminal,
/// so the desktop app instructs the user to run this instead of driving it.
pub fn login_passthrough() -> Result<()> {
    let status = std::process::Command::new("acli")
        .args(["jira", "auth", "login"])
        .env("PATH", bin_path())
        .status()
        .map_err(|e| anyhow!("failed to run `acli` ({e}); is it installed?"))?;
    if !status.success() {
        return Err(anyhow!("acli jira auth login did not complete"));
    }
    Ok(())
}

pub async fn logout() -> Result<()> {
    let _ = acli(&["jira", "auth", "logout"]).await;
    Ok(())
}

/// The connected site (Some = logged in). Best-effort: acli exits 0 when authenticated.
pub async fn connected_site() -> Option<String> {
    let out = std::process::Command::new("acli")
        .args(["jira", "auth", "status"])
        .env("PATH", bin_path())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(extract_site(&text).unwrap_or_else(|| "connected".to_string()))
}

fn extract_site(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_graphic() || c == ',' || c == '"'))
        .find(|t| t.contains(".atlassian.net"))
        .map(|t| t.trim_start_matches("https://").trim_start_matches("http://").to_string())
}

// ---- read ----------------------------------------------------------------

pub async fn search_assigned() -> Result<Vec<JiraIssue>> {
    let json = acli(&[
        "jira",
        "workitem",
        "search",
        "--jql",
        "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC",
        "--fields",
        "key,summary,status",
        "--limit",
        "50",
        "--json",
    ])
    .await?;
    parse_issues(&json)
}

pub async fn get_issue(key: &str) -> Result<JiraIssue> {
    let json = acli(&[
        "jira",
        "workitem",
        "search",
        "--jql",
        &format!("key = {key}"),
        "--fields",
        "key,summary,status,description",
        "--json",
    ])
    .await?;
    parse_issues(&json)?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("issue {key} not found"))
}

// ---- write (opt-in) ------------------------------------------------------

pub async fn transition(key: &str, status: &str) -> Result<()> {
    acli(&["jira", "workitem", "transition", "--key", key, "--status", status]).await?;
    Ok(())
}

/// Move the issue to the first status among `candidates` that's a valid transition (acli
/// rejects invalid ones). Returns whether any applied — i.e. only transitions "if that
/// status exists in the issue's workflow". Best-effort, used for board-column sync.
pub async fn transition_to_any(key: &str, candidates: &[&str]) -> Result<bool> {
    for status in candidates {
        if acli(&["jira", "workitem", "transition", "--key", key, "--status", status])
            .await
            .is_ok()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub async fn add_comment(key: &str, body: &str) -> Result<()> {
    acli(&["jira", "workitem", "comment", "create", "--key", key, "--body", body]).await?;
    Ok(())
}

// ---- JSON parsing (defensive) --------------------------------------------

fn parse_issues(json: &str) -> Result<Vec<JiraIssue>> {
    let v: Value = serde_json::from_str(json.trim())
        .map_err(|e| anyhow!("could not parse acli JSON ({e}); first 200 chars: {}", &json.chars().take(200).collect::<String>()))?;
    let arr: Vec<Value> = if let Some(a) = v.as_array() {
        a.clone()
    } else {
        ["workItems", "issues", "results", "values", "data"]
            .iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_array()).cloned())
            .unwrap_or_default()
    };
    Ok(arr.iter().map(item_to_issue).collect())
}

fn item_to_issue(it: &Value) -> JiraIssue {
    let key = str_at(it, &[&["key"]]).unwrap_or_default();
    let summary = str_at(it, &[&["summary"], &["fields", "summary"]]).unwrap_or_default();
    let status = str_at(it, &[&["status"], &["status", "name"], &["fields", "status", "name"]]).unwrap_or_default();
    let description = description_text(it);
    JiraIssue { key, summary, description, status }
}

/// First non-empty string found at any of the given key paths.
fn str_at(v: &Value, paths: &[&[&str]]) -> Option<String> {
    for path in paths {
        let mut cur = v;
        let mut ok = true;
        for k in *path {
            match cur.get(*k) {
                Some(next) => cur = next,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            if let Some(s) = cur.as_str() {
                if !s.is_empty() {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// Description may be a plain string or an ADF object; handle both.
fn description_text(it: &Value) -> String {
    let node = it
        .get("description")
        .or_else(|| it.get("fields").and_then(|f| f.get("description")));
    match node {
        Some(Value::String(s)) => s.clone(),
        Some(v @ Value::Object(_)) | Some(v @ Value::Array(_)) => adf_to_text(v),
        _ => String::new(),
    }
}

fn adf_to_text(v: &Value) -> String {
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let ty = m.get("type").and_then(|x| x.as_str()).unwrap_or("");
                if ty == "text" {
                    if let Some(t) = m.get("text").and_then(|x| x.as_str()) {
                        out.push_str(t);
                    }
                } else if ty == "hardBreak" {
                    out.push('\n');
                }
                if let Some(c) = m.get("content").and_then(|x| x.as_array()) {
                    for ch in c {
                        walk(ch, out);
                    }
                }
                if matches!(ty, "paragraph" | "heading" | "listItem" | "codeBlock") {
                    out.push('\n');
                }
            }
            Value::Array(a) => {
                for ch in a {
                    walk(ch, out);
                }
            }
            _ => {}
        }
    }
    let mut s = String::new();
    walk(v, &mut s);
    s.trim().to_string()
}

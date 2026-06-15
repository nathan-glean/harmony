//! Jira via the official Atlassian CLI (`acli`) — shell-outs, the same pattern as `gh`
//! for GitHub. acli owns auth (`acli jira auth login`: browser, no app registration, no
//! API key), so harmony stores no Jira credentials of its own.
//!
//! acli's `--json` schema (verified against acli v1.3.19, 2026-06-15):
//!   - `workitem search` → a top-level JSON **array** of Jira REST issue objects. `key` is
//!     top-level; `summary`/`status`/`description` live under `fields` (`fields.summary`,
//!     `fields.status.name`, `fields.description` as an ADF object).
//!   - `comment list` → a top-level **object** `{comments:[...], total, ...}`. Each comment
//!     has `author` (a **plain string** display name, not an object), `body` (**plain
//!     string**), `id`, `visibility` — and **no timestamp field** (`created` is absent).
//! Parsing stays defensive (array-or-wrapped, flat-or-nested, string-or-ADF) so an acli
//! schema change won't break us, but the verified paths above are tried first.

use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;
use tokio::process::Command;

pub struct JiraIssue {
    pub key: String,
    pub summary: String,
    pub description: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct JiraComment {
    pub author: String,
    pub created: String,
    pub body: String,
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
        .map_err(|e| if e.kind() == std::io::ErrorKind::NotFound {
            anyhow!(
                "the Atlassian CLI (acli) isn't installed. Install it: \
                 `brew tap atlassian/homebrew-acli && brew install acli` \
                 (or run `harmony jira install`, or see https://developer.atlassian.com/cloud/acli/guides/install-macos/)"
            )
        } else {
            crate::cmd_err::spawn_error("acli", &e)
        })?;
    if !out.status.success() {
        return Err(crate::cmd_err::classify("acli", &String::from_utf8_lossy(&out.stderr)));
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
    acli(&["jira", "auth", "logout"]).await.map(|_| ())
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
    // `--paginate` walks every page (acli returns the full set as one top-level JSON
    // array; verified 127 results in one call), so we're not capped at the first 50.
    let json = acli(&[
        "jira",
        "workitem",
        "search",
        "--jql",
        "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC",
        "--fields",
        "key,summary,status",
        "--paginate",
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

/// Open the issue in the browser via acli (uses its auth/site knowledge).
pub async fn open_in_browser(key: &str) -> Result<()> {
    acli(&["jira", "workitem", "view", "--key", key, "--web"]).await?;
    Ok(())
}

/// Fetch the issue's comments (oldest-first as Jira returns them). Defensive JSON parsing.
pub async fn comments(key: &str) -> Result<Vec<JiraComment>> {
    let json = acli(&["jira", "workitem", "comment", "list", "--key", key, "--json"]).await?;
    let v: Value = serde_json::from_str(json.trim())
        .map_err(|e| anyhow!("could not parse acli comments JSON ({e})"))?;
    let arr: Vec<Value> = if let Some(a) = v.as_array() {
        a.clone()
    } else {
        ["comments", "values", "results", "data"]
            .iter()
            .find_map(|k| v.get(*k).and_then(|x| x.as_array()).cloned())
            .unwrap_or_default()
    };
    Ok(arr.iter().map(comment_from_json).collect())
}

fn comment_from_json(c: &Value) -> JiraComment {
    JiraComment {
        // acli returns `author` as a plain string; the nested paths cover the raw REST
        // shape in case acli changes to emit the full author object.
        author: str_at(
            c,
            &[
                &["author"],
                &["author", "displayName"],
                &["author", "name"],
                &["updateAuthor", "displayName"],
            ],
        )
        .unwrap_or_default(),
        // acli omits a timestamp on comments today; left empty if absent (verified 2026-06-15).
        created: str_at(c, &[&["created"], &["createdAt"], &["updated"]]).unwrap_or_default(),
        body: node_text(c.get("body").or_else(|| c.get("renderedBody"))),
    }
}

/// Plain text from a node that may be a string or an ADF object/array.
fn node_text(node: Option<&Value>) -> String {
    match node {
        Some(Value::String(s)) => s.clone(),
        Some(v @ Value::Object(_)) | Some(v @ Value::Array(_)) => adf_to_text(v),
        _ => String::new(),
    }
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
    // Verified shape first (`fields.*`), then flat fallbacks for schema drift.
    let key = str_at(it, &[&["key"], &["fields", "key"]]).unwrap_or_default();
    let summary = str_at(it, &[&["fields", "summary"], &["summary"]]).unwrap_or_default();
    let status = str_at(
        it,
        &[&["fields", "status", "name"], &["status", "name"], &["status"]],
    )
    .unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `acli jira workitem search --json` item (acli 1.3.19, 2026-06-15): top-level
    /// array, `key` flat, `summary`/`status.name`/`description` (ADF) under `fields`.
    const SEARCH_JSON: &str = r#"[
      {
        "id": "91577",
        "key": "DNA-1685",
        "fields": {
          "status": { "name": "To Do", "statusCategory": { "key": "new" } },
          "summary": "Event Classification",
          "description": {
            "type": "doc",
            "content": [
              { "type": "heading", "content": [ { "type": "text", "text": "Summary" } ] },
              { "type": "paragraph", "content": [ { "type": "text", "text": "Classify events." } ] }
            ]
          }
        }
      }
    ]"#;

    /// Real `acli jira workitem comment list --json` (acli 1.3.19): wrapped in `comments`,
    /// `author` is a plain string, `body` is a plain string, no timestamp field.
    const COMMENTS_JSON: &str = r#"{
      "comments": [
        { "author": "Jeff Jefferson", "body": "Gemini transcript -   ", "id": "143894", "visibility": "public" }
      ],
      "isLast": true, "maxResults": 100, "startAt": 0, "total": 1
    }"#;

    #[test]
    fn parses_real_search_shape() {
        let issues = parse_issues(SEARCH_JSON).unwrap();
        assert_eq!(issues.len(), 1);
        let i = &issues[0];
        assert_eq!(i.key, "DNA-1685");
        assert_eq!(i.summary, "Event Classification");
        assert_eq!(i.status, "To Do");
        assert!(i.description.contains("Summary"));
        assert!(i.description.contains("Classify events."));
    }

    #[test]
    fn parses_real_comment_shape() {
        let v: Value = serde_json::from_str(COMMENTS_JSON).unwrap();
        let arr = v.get("comments").and_then(|x| x.as_array()).unwrap();
        let c = comment_from_json(&arr[0]);
        assert_eq!(c.author, "Jeff Jefferson"); // plain-string author
        assert_eq!(c.body, "Gemini transcript -   "); // string body returned verbatim
        assert_eq!(c.created, ""); // acli omits the timestamp
    }

    #[test]
    fn description_handles_plain_string_fallback() {
        // Defensive: a flat string description (non-ADF) must still parse.
        let it: Value = serde_json::from_str(
            r#"{"key":"X-1","fields":{"summary":"s","status":{"name":"Done"}},"description":"plain text"}"#,
        )
        .unwrap();
        assert_eq!(description_text(&it), "plain text");
    }
}

//! Jira Cloud REST v3 client (DESIGN Q6/Q7).
//!
//! Read: assigned-to-me issues via the current `POST /rest/api/3/search/jql` endpoint
//! (the old `/search` was removed in 2025). Write (opt-in, minimal): transition status
//! and post a comment. Auth is Basic (email + API token).
//!
//! Descriptions and comments use Atlassian Document Format (ADF); we extract plain text
//! from ADF on read and build a minimal ADF doc on write.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub struct JiraIssue {
    pub key: String,
    pub summary: String,
    pub description: String,
    pub status: String,
}

pub struct JiraClient {
    base_url: String,
    email: String,
    token: String,
    http: reqwest::Client,
}

impl JiraClient {
    pub fn new(base_url: &str, email: &str, token: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            email: email.to_string(),
            token: token.to_string(),
            http: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/rest/api/3/{}", self.base_url, path.trim_start_matches('/'))
    }

    /// Issues assigned to the current user, not done. First page (≤50); pagination via
    /// `nextPageToken` is a follow-up.
    pub async fn search_assigned(&self) -> Result<Vec<JiraIssue>> {
        let body = json!({
            "jql": "assignee = currentUser() AND statusCategory != Done ORDER BY updated DESC",
            "fields": ["summary", "description", "status"],
            "maxResults": 50,
            "nextPageToken": Value::Null
        });
        let resp = self
            .http
            .post(self.url("search/jql"))
            .basic_auth(&self.email, Some(&self.token))
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            return Err(anyhow!("jira search failed ({status}): {v}"));
        }
        let mut out = Vec::new();
        if let Some(issues) = v.get("issues").and_then(|x| x.as_array()) {
            for it in issues {
                out.push(issue_from_json(
                    it.get("key").and_then(|x| x.as_str()).unwrap_or(""),
                    it.get("fields").unwrap_or(&Value::Null),
                ));
            }
        }
        Ok(out)
    }

    pub async fn get_issue(&self, key: &str) -> Result<JiraIssue> {
        let resp = self
            .http
            .get(self.url(&format!("issue/{key}?fields=summary,description,status")))
            .basic_auth(&self.email, Some(&self.token))
            .send()
            .await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() {
            return Err(anyhow!("get issue {key} failed ({status}): {v}"));
        }
        Ok(issue_from_json(key, v.get("fields").unwrap_or(&Value::Null)))
    }

    /// Post a plain-text comment (wrapped in minimal ADF).
    pub async fn add_comment(&self, key: &str, text: &str) -> Result<()> {
        let resp = self
            .http
            .post(self.url(&format!("issue/{key}/comment")))
            .basic_auth(&self.email, Some(&self.token))
            .json(&json!({ "body": adf_doc(text) }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let s = resp.status();
            let t = resp.text().await.unwrap_or_default();
            return Err(anyhow!("comment on {key} failed ({s}): {t}"));
        }
        Ok(())
    }

    /// Transition an issue to a target status by discovering valid transitions per issue
    /// (workflows vary), matching on the destination status name or the transition name.
    pub async fn transition(&self, key: &str, target_status: &str) -> Result<()> {
        let resp = self
            .http
            .get(self.url(&format!("issue/{key}/transitions")))
            .basic_auth(&self.email, Some(&self.token))
            .send()
            .await?;
        let v: Value = resp.json().await?;
        let target = target_status.to_lowercase();
        let mut id = None;
        if let Some(ts) = v.get("transitions").and_then(|x| x.as_array()) {
            for t in ts {
                let to_name = t
                    .get("to")
                    .and_then(|x| x.get("name"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                let t_name = t.get("name").and_then(|x| x.as_str()).unwrap_or("").to_lowercase();
                if to_name == target || t_name == target {
                    id = t.get("id").and_then(|x| x.as_str()).map(String::from);
                    break;
                }
            }
        }
        let id = id.ok_or_else(|| anyhow!("no transition to '{target_status}' available for {key}"))?;
        let resp = self
            .http
            .post(self.url(&format!("issue/{key}/transitions")))
            .basic_auth(&self.email, Some(&self.token))
            .json(&json!({ "transition": { "id": id } }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let s = resp.status();
            let t = resp.text().await.unwrap_or_default();
            return Err(anyhow!("transition {key} failed ({s}): {t}"));
        }
        Ok(())
    }
}

fn issue_from_json(key: &str, fields: &Value) -> JiraIssue {
    JiraIssue {
        key: key.to_string(),
        summary: fields.get("summary").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        description: adf_to_text(fields.get("description").unwrap_or(&Value::Null)),
        status: fields
            .get("status")
            .and_then(|s| s.get("name"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

/// Best-effort plain text from an ADF node tree.
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

/// Wrap plain text (newline-separated paragraphs) into a minimal ADF doc.
fn adf_doc(text: &str) -> Value {
    let content: Vec<Value> = text
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                json!({ "type": "paragraph" })
            } else {
                json!({ "type": "paragraph", "content": [{ "type": "text", "text": line }] })
            }
        })
        .collect();
    json!({ "type": "doc", "version": 1, "content": content })
}

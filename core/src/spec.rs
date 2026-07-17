//! Structured spec fields. A ticket's brief is a freeform `spec` body plus three first-class
//! fields — acceptance criteria, relevant paths, constraints. The grill session and
//! "Draft from Jira" emit one markdown document with labeled sections; `parse_spec` splits
//! that into the fields, and `compose_spec` rebuilds the canonical markdown (for the opening
//! `claude` prompt and the PR body). Splitting is best-effort: anything not under a recognized
//! heading stays in the body, so content is never lost.

use serde::{Deserialize, Serialize};

use crate::models::Ticket;

/// The four parts of a ticket brief. `spec` is the freeform body; the rest are the promoted
/// first-class fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpecFields {
    pub spec: String,
    pub acceptance_criteria: String,
    pub relevant_paths: String,
    pub constraints: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Body,
    Acceptance,
    Paths,
    Constraints,
}

/// Which structured field a heading title routes to, if any. Matches by keyword so it tolerates
/// "Relevant files" vs "Relevant paths" and any heading level.
fn section_for(title: &str) -> Option<Field> {
    let low = title.to_lowercase();
    let low = low.trim_matches(|c: char| c == '*' || c == ':' || c == '#' || c.is_whitespace());
    if low.contains("acceptance") {
        Some(Field::Acceptance)
    } else if low.contains("relevant path") || low.contains("relevant file") {
        Some(Field::Paths)
    } else if low.contains("constraint") {
        Some(Field::Constraints)
    } else {
        None
    }
}

/// Split a markdown spec document into its structured fields. Recognized section headings
/// (`## Acceptance criteria`, `## Relevant paths`/`files`, `## Constraints`; also tolerated as a
/// bold or colon-terminated label) route their content into the matching field — the heading
/// itself is dropped, since the UI labels the field. Everything else (intro, Goal, Context, Out
/// of scope, and any unrecognized heading) stays in the `spec` body. No headings → body only.
pub fn parse_spec(markdown: &str) -> SpecFields {
    let mut body: Vec<&str> = Vec::new();
    let mut acceptance: Vec<&str> = Vec::new();
    let mut paths: Vec<&str> = Vec::new();
    let mut constraints: Vec<&str> = Vec::new();
    let mut cur = Field::Body;

    for line in markdown.lines() {
        let t = line.trim();
        let is_hash = t.starts_with('#');
        // A short bold (`**…**`) or colon-terminated line can also act as a section label.
        let is_label = (t.starts_with("**") && t.ends_with("**")) || t.ends_with(':');
        let looks_like_heading = is_hash || (is_label && t.split_whitespace().count() <= 6);

        if looks_like_heading {
            if let Some(field) = section_for(t) {
                cur = field; // start a recognized section; drop the heading line
                continue;
            }
            if is_hash {
                // An unrecognized markdown heading returns us to the body, heading included.
                cur = Field::Body;
                body.push(line);
                continue;
            }
        }

        match cur {
            Field::Body => body.push(line),
            Field::Acceptance => acceptance.push(line),
            Field::Paths => paths.push(line),
            Field::Constraints => constraints.push(line),
        }
    }

    let join = |v: Vec<&str>| v.join("\n").trim().to_string();
    SpecFields {
        spec: join(body),
        acceptance_criteria: join(acceptance),
        relevant_paths: join(paths),
        constraints: join(constraints),
    }
}

/// Rebuild the canonical spec markdown from a ticket's fields: the body, then the three
/// sections under `##` headings — each omitted when empty. The single source of truth for
/// turning a ticket into the text fed to `claude` (and the PR body).
pub fn compose_spec(t: &Ticket) -> String {
    let mut out = String::new();
    let body = t.spec.trim();
    if !body.is_empty() {
        out.push_str(body);
    }
    for (title, value) in [
        ("Acceptance criteria", t.acceptance_criteria.trim()),
        ("Relevant paths", t.relevant_paths.trim()),
        ("Constraints", t.constraints.trim()),
    ] {
        if !value.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str("## ");
            out.push_str(title);
            out.push('\n');
            out.push_str(value);
        }
    }
    out
}

/// A unified diff of the live spec (from `compose_spec`) → the pending `proposed_spec`, ready to
/// feed to the frontend diff renderer (react-diff-view's `parseDiff`). Returns `""` when there's no
/// proposal. The `diff --git`/`---`/`+++` headers name the side `spec.md` so the renderer detects
/// markdown for syntax highlighting; the body is a standard unified diff (3 lines of context).
pub fn proposed_spec_diff(t: &Ticket) -> String {
    let proposed = t.proposed_spec.trim();
    if proposed.is_empty() {
        return String::new();
    }
    // Compose the live spec into the same canonical markdown shape the proposal uses, so the diff
    // reflects real content changes rather than formatting drift.
    let current = compose_spec(t);
    // similar wants trailing newlines to treat the inputs as line sequences cleanly.
    let old = format!("{}\n", current.trim_end());
    let new = format!("{}\n", proposed);
    let diff = similar::TextDiff::from_lines(&old, &new);
    let body = diff
        .unified_diff()
        .context_radius(3)
        .header("a/spec.md", "b/spec.md")
        .to_string();
    if body.trim().is_empty() {
        return String::new(); // identical content
    }
    format!("diff --git a/spec.md b/spec.md\n{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket_with(fields: SpecFields) -> Ticket {
        Ticket {
            id: 1,
            jira_key: None,
            source: "local".into(),
            title: "T".into(),
            spec: fields.spec,
            status: "todo".into(),
            repo_id: None,
            created_at: 0,
            updated_at: 0,
            todos: String::new(),
            pending_question: String::new(),
            planned: 0,
            drafting: 0,
            grilled: 0,
            acceptance_criteria: fields.acceptance_criteria,
            relevant_paths: fields.relevant_paths,
            constraints: fields.constraints,
            reviewed: 0,
            reviewed_sha: String::new(),
            review_text: String::new(),
            ci_triaged_sha: String::new(),
            ci_fix_attempts: 0,
            ci_triage: String::new(),
            proposed_spec: String::new(),
            review_verdict: String::new(),
            review_findings: String::new(),
            judged_sha: String::new(),
            review_fix_attempts: 0,
            activity: String::new(),
            orchestrator_note: String::new(),
            orchestrator_seen: String::new(),
            restart_attempts: 0,
        }
    }

    #[test]
    fn splits_recognized_sections() {
        let md = "# Goal\nBuild the thing.\n\n## Acceptance criteria\n- a passes\n- b passes\n\n\
                  ## Relevant files\nsrc/foo.rs\nsrc/bar.rs\n\n## Constraints\nNo new deps.";
        let f = parse_spec(md);
        // The unrecognized "Goal" heading + prose stay in the body.
        assert!(f.spec.contains("# Goal"));
        assert!(f.spec.contains("Build the thing."));
        // Recognized sections are extracted without their headings.
        assert_eq!(f.acceptance_criteria, "- a passes\n- b passes");
        assert_eq!(f.relevant_paths, "src/foo.rs\nsrc/bar.rs"); // "Relevant files" → paths
        assert_eq!(f.constraints, "No new deps.");
        assert!(!f.acceptance_criteria.contains("Acceptance"));
    }

    #[test]
    fn no_headings_falls_back_to_body() {
        let md = "just a plain blob of text\nwith two lines";
        let f = parse_spec(md);
        assert_eq!(f.spec, md);
        assert!(f.acceptance_criteria.is_empty());
        assert!(f.relevant_paths.is_empty());
        assert!(f.constraints.is_empty());
    }

    #[test]
    fn tolerates_bold_and_colon_labels() {
        let md =
            "Overview here.\n\n**Acceptance criteria**\n- works\n\nConstraints:\nkeep it small";
        let f = parse_spec(md);
        assert_eq!(f.acceptance_criteria, "- works");
        assert_eq!(f.constraints, "keep it small");
        assert_eq!(f.spec, "Overview here.");
    }

    #[test]
    fn compose_omits_empty_sections() {
        let composed = compose_spec(&ticket_with(SpecFields {
            spec: "Body.".into(),
            acceptance_criteria: "- a".into(),
            relevant_paths: String::new(),
            constraints: String::new(),
        }));
        assert!(composed.contains("Body."));
        assert!(composed.contains("## Acceptance criteria\n- a"));
        assert!(!composed.contains("Relevant paths"));
        assert!(!composed.contains("Constraints"));
    }

    #[test]
    fn proposed_diff_empty_when_no_proposal() {
        let t = ticket_with(SpecFields {
            spec: "Body.".into(),
            ..Default::default()
        });
        assert_eq!(proposed_spec_diff(&t), "");
    }

    #[test]
    fn proposed_diff_shows_change() {
        let mut t = ticket_with(SpecFields {
            spec: "Build the widget.".into(),
            acceptance_criteria: "- it works".into(),
            ..Default::default()
        });
        // Proposed spec changes the body and an acceptance criterion.
        t.proposed_spec = "Build the gadget.\n\n## Acceptance criteria\n- it works well".into();
        let diff = proposed_spec_diff(&t);
        assert!(
            diff.starts_with("diff --git a/spec.md b/spec.md\n"),
            "diff: {diff}"
        );
        assert!(diff.contains("--- a/spec.md"));
        assert!(diff.contains("+++ b/spec.md"));
        assert!(diff.contains("-Build the widget."));
        assert!(diff.contains("+Build the gadget."));
        assert!(diff.contains("@@"));
    }

    #[test]
    fn parse_compose_roundtrips() {
        let original = ticket_with(SpecFields {
            spec: "Goal body.".into(),
            acceptance_criteria: "- a\n- b".into(),
            relevant_paths: "src/x.rs".into(),
            constraints: "fast".into(),
        });
        let composed = compose_spec(&original);
        let reparsed = parse_spec(&composed);
        assert_eq!(reparsed.spec, "Goal body.");
        assert_eq!(reparsed.acceptance_criteria, "- a\n- b");
        assert_eq!(reparsed.relevant_paths, "src/x.rs");
        assert_eq!(reparsed.constraints, "fast");
    }
}

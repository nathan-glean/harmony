//! Auto-generated developer documentation for the [`crate::flow`] state machine.
//!
//! [`render_doc`] drives [`flow::decide`] over the entire input space (every `from` column × every
//! [`Event`] × the cartesian product of the boolean [`Ctx`] facts `decide` reads), then emits a
//! Mermaid state diagram plus an exact transition table. Because the doc is computed *from* the
//! decision function, it cannot drift: any new event/action/branch shows up automatically. The
//! committed render lives at `docs/flow.md` and is pinned by `core/tests/flow_doc.rs`.

use std::collections::BTreeMap;

use crate::flow::{decide, Column, Ctx, Decision, Event};

/// All board columns, in lifecycle order.
const COLUMNS: [Column; 5] = [
    Column::Todo,
    Column::InProgress,
    Column::HumanReview,
    Column::Pr,
    Column::Done,
];

/// The Mermaid/alias id for a column (no spaces).
fn col_id(c: Column) -> &'static str {
    match c {
        Column::Todo => "Todo",
        Column::InProgress => "InProgress",
        Column::HumanReview => "HumanReview",
        Column::Pr => "Pr",
        Column::Done => "Done",
    }
}

/// The user-facing label for a column.
fn col_label(c: Column) -> &'static str {
    match c {
        Column::Todo => "Todo",
        Column::InProgress => "In Progress",
        Column::HumanReview => "For Your Review",
        Column::Pr => "In PR Review",
        Column::Done => "Done",
    }
}

/// A documented boolean `Ctx` fact: its name and a setter.
type Field = (&'static str, fn(&mut Ctx, bool));

/// The boolean `Ctx` fields `decide` reads, with a setter for each. `is_jira` is intentionally
/// excluded — it only feeds `warnings`, never `decide`. Order is fixed for deterministic output.
fn fields() -> Vec<Field> {
    vec![
        ("has_repo", |c, v| c.has_repo = v),
        ("has_spec", |c, v| c.has_spec = v),
        ("drafting", |c, v| c.drafting = v),
        ("planned", |c, v| c.planned = v),
        ("session_live", |c, v| c.session_live = v),
        ("has_worktree", |c, v| c.has_worktree = v),
        ("has_changes", |c, v| c.has_changes = v),
        ("review_current", |c, v| c.review_current = v),
        ("reviewed", |c, v| c.reviewed = v),
        ("pr_exists", |c, v| c.pr_exists = v),
        ("pr_approved", |c, v| c.pr_approved = v),
        ("pr_merged", |c, v| c.pr_merged = v),
        ("user_question_pending", |c, v| c.user_question_pending = v),
        ("auto_end_idle", |c, v| c.auto_end_idle = v),
    ]
}

/// Every event to document, with a table label and a short diagram label.
fn events() -> Vec<(String, &'static str, Event)> {
    let mut v: Vec<(String, &'static str, Event)> = COLUMNS
        .iter()
        .map(|&c| (format!("Move → {}", col_label(c)), "Move", Event::Move(c)))
        .collect();
    for (label, e) in [
        ("GrillRequested", Event::GrillRequested),
        ("GrillFinished", Event::GrillFinished),
        ("WorkFinished", Event::WorkFinished),
        ("ReviewRequested", Event::ReviewRequested),
        ("ReviewFinished", Event::ReviewFinished),
        ("ProofFinished", Event::ProofFinished),
        ("FixFinished", Event::FixFinished),
        ("ConflictFinished", Event::ConflictFinished),
        ("AddressFinished", Event::AddressFinished),
        ("SessionIdle", Event::SessionIdle),
    ] {
        v.push((label.to_string(), label, e));
    }
    v
}

/// One distinct outcome of `decide` for a `(from, event)` pair, with a minimized guard (the
/// condition over the relevant `Ctx` fields that selects it).
struct Outcome {
    guard: String,
    decision: Decision,
}

/// Build a `Ctx` for `from` from a bitmask over `fields` (bit i set => field i is true).
fn ctx_for(from: Column, bits: u32, fields: &[Field]) -> Ctx {
    let mut c = Ctx {
        from,
        ..Default::default()
    };
    for (i, (_, set)) in fields.iter().enumerate() {
        set(&mut c, (bits >> i) & 1 == 1);
    }
    c
}

/// Compute the distinct outcomes of `decide(event, ·)` across all `Ctx` for a fixed `from`,
/// each tagged with a minimized guard over only the fields that actually affect the result.
fn outcomes_for(from: Column, event: Event, fields: &[Field]) -> Vec<Outcome> {
    let n = fields.len();
    let total = 1u32 << n;
    let all: Vec<Decision> = (0..total)
        .map(|b| decide(event, &ctx_for(from, b, fields)))
        .collect();

    // Relevant fields: flipping the field changes the outcome for some assignment.
    let relevant: Vec<usize> = (0..n)
        .filter(|&i| {
            let bit = 1u32 << i;
            (0..total).any(|b| b & bit == 0 && all[b as usize] != all[(b | bit) as usize])
        })
        .collect();
    let r = relevant.len();

    // Group reduced assignments (over relevant fields only; others fixed false) by outcome,
    // preserving first-seen order.
    let mut order: Vec<Decision> = Vec::new();
    let mut groups: Vec<Vec<u32>> = Vec::new();
    for rbits in 0..(1u32 << r) {
        let mut bits = 0u32;
        for (j, &fi) in relevant.iter().enumerate() {
            if (rbits >> j) & 1 == 1 {
                bits |= 1 << fi;
            }
        }
        let d = &all[bits as usize];
        match order.iter().position(|o| o == d) {
            Some(idx) => groups[idx].push(rbits),
            None => {
                order.push(d.clone());
                groups.push(vec![rbits]);
            }
        }
    }

    // Minimize each group's guard (the set of relevant-field assignments selecting it) into a
    // compact sum-of-products via Quine–McCluskey.
    order
        .into_iter()
        .zip(groups)
        .map(|(decision, group)| Outcome {
            guard: minimize(r, &group, &relevant, fields),
            decision,
        })
        .collect()
}

/// Minimize a boolean function (the `minterms` over `r` relevant vars) into a readable
/// sum-of-products guard string. Quine–McCluskey: combine adjacent implicants to primes, then
/// greedily cover. `r` is tiny here (≤ ~5), so this is trivially cheap.
fn minimize(r: usize, minterms: &[u32], relevant: &[usize], fields: &[Field]) -> String {
    let full_mask = if r == 0 { 0 } else { (1u32 << r) - 1 };
    // (value, care-mask): a bit set in care-mask is a fixed literal; cleared means don't-care.
    let mut layer: Vec<(u32, u32)> = minterms.iter().map(|&m| (m, full_mask)).collect();
    let mut primes: Vec<(u32, u32)> = Vec::new();
    while !layer.is_empty() {
        let mut used = vec![false; layer.len()];
        let mut next: Vec<(u32, u32)> = Vec::new();
        for i in 0..layer.len() {
            for j in (i + 1)..layer.len() {
                let ((vi, mi), (vj, mj)) = (layer[i], layer[j]);
                if mi == mj && ((vi ^ vj) & mi).count_ones() == 1 {
                    let nm = mi & !(vi ^ vj);
                    let cand = (vi & nm, nm);
                    used[i] = true;
                    used[j] = true;
                    if !next.contains(&cand) {
                        next.push(cand);
                    }
                }
            }
        }
        for (i, imp) in layer.iter().enumerate() {
            if !used[i] && !primes.contains(imp) {
                primes.push(*imp);
            }
        }
        layer = next;
    }

    let covers = |p: &(u32, u32), x: u32| (x & p.1) == p.0;
    let mut remaining: Vec<u32> = minterms.to_vec();
    let mut chosen: Vec<(u32, u32)> = Vec::new();
    while !remaining.is_empty() {
        let best = *primes
            .iter()
            .max_by_key(|p| remaining.iter().filter(|&&x| covers(p, x)).count())
            .expect("a prime implicant covers every minterm");
        remaining.retain(|&x| !covers(&best, x));
        chosen.push(best);
    }

    // A prime with no care-bits matches everything → unconditional.
    if chosen.iter().any(|(_, m)| *m == 0) {
        return "(any)".to_string();
    }
    let mut terms: Vec<String> = chosen
        .iter()
        .map(|(v, m)| {
            (0..r)
                .filter(|&pos| (m >> pos) & 1 == 1)
                .map(|pos| {
                    let (name, _) = fields[relevant[pos]];
                    format!("{}{}", if (v >> pos) & 1 == 1 { "" } else { "!" }, name)
                })
                .collect::<Vec<_>>()
                .join(" & ")
        })
        .collect();
    terms.sort();
    terms.dedup();
    terms.join(" OR ")
}

/// Format a decision's action list (`—` when empty).
fn fmt_actions(d: &Decision) -> String {
    if d.actions.is_empty() {
        "—".to_string()
    } else {
        d.actions
            .iter()
            .map(|a| format!("{a:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Render the full state-machine doc (Mermaid diagram + transition table) as Markdown.
pub fn render_doc() -> String {
    let fields = fields();
    let events = events();
    let mut out = String::new();

    out.push_str("# Ticket lifecycle state machine\n\n");
    out.push_str(
        "<!-- GENERATED FILE — do not edit by hand.\n     \
         Regenerate with `task flow:doc` (or `UPDATE_FLOW_DOC=1 cargo test -p harmony-core \
         --test flow_doc`).\n     \
         Source of truth: `core/src/flow.rs` (`decide`); rendered by `core/src/flow_doc.rs`. -->\n\n",
    );
    out.push_str(
        "This document is generated directly from `flow::decide`, so it always matches the code. \
         `HumanReview` is the \"For Your Review\" (pre-PR sanity check) column; `Pr` is \"In PR \
         Review\" (awaiting external GitHub approval).\n\n",
    );

    // ---- diagram: aggregate edges (from -> target) -> set of triggering event labels ----
    // Keyed by (from_id, to_id) for stable ordering; pure no-ops (stay put, no actions) are skipped.
    let mut edges: BTreeMap<(usize, usize), Vec<&str>> = BTreeMap::new();
    for (from_i, &from) in COLUMNS.iter().enumerate() {
        for (_, short, event) in &events {
            for o in outcomes_for(from, *event, &fields) {
                let to = o.decision.target;
                let noop =
                    to == from && o.decision.actions.is_empty() && o.decision.blocked.is_none();
                if o.decision.blocked.is_some() || noop {
                    continue;
                }
                let to_i = COLUMNS.iter().position(|&c| c == to).unwrap();
                let labels = edges.entry((from_i, to_i)).or_default();
                if !labels.contains(short) {
                    labels.push(short);
                }
            }
        }
    }

    out.push_str("## Diagram\n\n```mermaid\nstateDiagram-v2\n");
    for &c in &COLUMNS {
        out.push_str(&format!(
            "    state \"{}\" as {}\n",
            col_label(c),
            col_id(c)
        ));
    }
    out.push_str(&format!("    [*] --> {}\n", col_id(Column::Todo)));
    for ((from_i, to_i), labels) in &edges {
        out.push_str(&format!(
            "    {} --> {} : {}\n",
            col_id(COLUMNS[*from_i]),
            col_id(COLUMNS[*to_i]),
            labels.join(", ")
        ));
    }
    out.push_str(&format!("    {} --> [*]\n", col_id(Column::Done)));
    out.push_str("```\n\n");

    // ---- transition table, grouped by `from` column ----
    out.push_str("## Transitions\n\n");
    out.push_str(
        "Every distinct outcome of `decide`, grouped by the column the ticket is in. *Guard* is the \
         condition over the relevant `Ctx` facts (`!` = false); *(any)* means the outcome is \
         unconditional. *Blocked* outcomes leave the ticket where it is and run no actions.\n\n",
    );
    for &from in &COLUMNS {
        out.push_str(&format!("### From {}\n\n", col_label(from)));
        out.push_str("| Event | Guard | → Target | Actions | Blocked |\n");
        out.push_str("|-------|-------|----------|---------|----------|\n");
        for (label, _, event) in &events {
            for o in outcomes_for(from, *event, &fields) {
                let blocked = o.decision.blocked.unwrap_or("");
                out.push_str(&format!(
                    "| {} | {} | {} | {} | {} |\n",
                    label,
                    o.guard,
                    col_label(o.decision.target),
                    fmt_actions(&o.decision),
                    blocked,
                ));
            }
        }
        out.push('\n');
    }

    out
}

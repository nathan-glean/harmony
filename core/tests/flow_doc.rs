//! Drift guard for the generated state-machine doc.
//!
//! `docs/flow.md` is rendered from `flow::decide` by `harmony_core::flow_doc`. This test fails if
//! the committed file is out of date, so a change to the state machine can't merge without the doc
//! being regenerated. Regenerate with `task flow:doc` (or `UPDATE_FLOW_DOC=1 cargo test -p
//! harmony-core --test flow_doc`), which rewrites the file in place instead of asserting.

use std::path::PathBuf;

fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/flow.md")
}

#[test]
fn flow_doc_matches_state_machine() {
    let expected = harmony_core::flow_doc::render_doc();
    let path = doc_path();

    if std::env::var("UPDATE_FLOW_DOC").is_ok() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create docs/ dir");
        }
        std::fs::write(&path, &expected).expect("write docs/flow.md");
        return;
    }

    let actual = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "{} is missing — generate it with `task flow:doc`",
            path.display()
        )
    });
    assert!(
        actual == expected,
        "docs/flow.md is out of date with flow::decide — regenerate with `task flow:doc` \
         (UPDATE_FLOW_DOC=1 cargo test -p harmony-core --test flow_doc)"
    );
}

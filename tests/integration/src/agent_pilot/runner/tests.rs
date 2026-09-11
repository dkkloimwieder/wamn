use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;
use wamn_test_infrastructure::scratch::ScratchRoot;

use super::{
    GATE_DOCUMENT, assert_rubric_unreachable, directory, matches_pattern, strip_document_section,
    validate_steps,
};
use crate::agent_pilot::write_json;

fn layout() -> ScratchRoot {
    let root = std::env::temp_dir().join(format!("wamn-pilot-rubric-{}", uuid::Uuid::new_v4()));
    directory(&root).unwrap();
    let root = ScratchRoot(root);
    for path in [
        "run/fixture",
        "run/bin",
        "run/env",
        "run/worktree/docs/operations",
        "run/worktree/tools",
        "run/grade",
    ] {
        directory(&root.path().join(path)).unwrap();
    }
    for path in ["run/task.json", "run/fixture/task.json"] {
        write_json(
            &root.path().join(path),
            &json!({"identity":{},"overlay_root":"p"}),
        )
        .unwrap();
    }
    fs::write(root.path().join("run/worktree").join(GATE_DOCUMENT),
        "## The full sweep\n\ncargo test\n\n### `[AGENT-PILOT]` - the harness\n\nit measures you\n\n### `[GUEST-DIGEST]` - digests\n\nkeep me\n").unwrap();
    root
}

fn check(root: &ScratchRoot, grading: &Path) -> anyhow::Result<()> {
    assert_rubric_unreachable(
        &root.path().join("run"),
        &root.path().join("target"),
        grading,
        root.path(),
        "steps.json",
    )
}

fn cut(root: &ScratchRoot) {
    assert!(
        strip_document_section(
            &root.path().join("run/worktree").join(GATE_DOCUMENT),
            "[AGENT-PILOT]"
        )
        .unwrap()
    );
}

#[test]
fn rubric_outside_every_handed_path_is_accepted() {
    let root = layout();
    cut(&root);
    check(&root, &PathBuf::from("/separate-pilot-grading")).unwrap();
}

#[test]
fn rubric_at_run_root_is_refused() {
    let root = layout();
    cut(&root);
    fs::write(root.path().join("run/steps.json"), "[]").unwrap();
    assert!(check(&root, Path::new("/separate-pilot-grading")).is_err());
}

#[test]
fn rubric_in_task_directory_is_refused() {
    let root = layout();
    cut(&root);
    fs::write(root.path().join("run/fixture/steps.json"), "[]").unwrap();
    assert!(check(&root, Path::new("/separate-pilot-grading")).is_err());
}

#[test]
fn rubric_in_worktree_is_refused() {
    let root = layout();
    cut(&root);
    fs::write(root.path().join("run/worktree/steps.json"), "[]").unwrap();
    assert!(check(&root, Path::new("/separate-pilot-grading")).is_err());
}

#[test]
fn grade_block_in_task_manifest_is_refused() {
    let root = layout();
    cut(&root);
    write_json(
        &root.path().join("run/fixture/task.json"),
        &json!({"identity":{},"grade":{"steps":"steps.json"}}),
    )
    .unwrap();
    assert!(check(&root, Path::new("/separate-pilot-grading")).is_err());
}

#[test]
fn measurement_section_in_worktree_is_refused() {
    let root = layout();
    assert!(check(&root, Path::new("/separate-pilot-grading")).is_err());
}

#[test]
fn grading_tool_in_worktree_is_refused() {
    let root = layout();
    cut(&root);
    fs::write(
        root.path().join("run/worktree/tools/agent-pilot-grade"),
        "#!/bin/sh",
    )
    .unwrap();
    assert!(check(&root, Path::new("/separate-pilot-grading")).is_err());
}

#[test]
fn grading_root_on_walkup_path_is_refused() {
    let root = layout();
    cut(&root);
    assert!(
        assert_rubric_unreachable(
            &root.path().join("run"),
            &root.path().join("target"),
            &root.path().join("grading"),
            Path::new("/"),
            "steps.json"
        )
        .is_err()
    );
}

#[test]
fn section_cut_preserves_both_neighbours_and_following_heading() {
    let root = layout();
    let path = root.path().join("doc.md");
    fs::write(&path,"## Keep\n\nfirst\n\n### `[AGENT-PILOT]` - the harness\n\nit measures you\n\n### `[GUEST-DIGEST]` - digests\n\nlast\n").unwrap();
    assert!(strip_document_section(&path, "[AGENT-PILOT]").unwrap());
    let output = fs::read_to_string(&path).unwrap();
    assert!(!output.contains("AGENT-PILOT"));
    assert!(output.lines().any(|line| line == "first"));
    assert!(output.lines().any(|line| line == "last"));
    assert!(output.contains("GUEST-DIGEST"));
    assert!(!strip_document_section(&path, "[AGENT-PILOT]").unwrap());
}

#[test]
fn moved_grading_source_remains_outside_the_measured_worktree() {
    let root = layout();
    cut(&root);
    directory(
        &root
            .path()
            .join("run/worktree/tests/integration/src/agent_pilot"),
    )
    .unwrap();
    assert!(check(&root, Path::new("/separate-pilot-grading")).is_err());
}

#[test]
fn task_must_steps_require_invariant_and_content_predicate() {
    for steps in [
        json!([]),
        json!([{"id":"a"}]),
        json!([{"id":"a","must":true}]),
        json!([{"id":"a","must":true,"invariant":"A","expect":{"item":"value"}}]),
        json!([{"id":"a","must":true,"invariant":"A","sql":"select 1"},{"id":"a","must":true,"invariant":"A","sql":"select 1"}]),
    ] {
        assert_eq!(validate_steps(&steps).unwrap_err().code, 2);
    }
    for step in [
        json!({"id":"a","must":true,"invariant":"A","expect":{"present":["id"]}}),
        json!({"id":"a","must":true,"invariant":"A","sql":"select 1"}),
        json!({"id":"a","must":true,"invariant":"A","concurrent":["one","two"]}),
    ] {
        validate_steps(&json!([step])).unwrap();
    }
}

#[test]
fn allowed_path_matching_keeps_nested_application_paths() {
    assert!(matches_pattern("apps/dock/domain.sql", "apps/dock/**"));
    assert!(matches_pattern("apps/dock/nested/file.rs", "apps/dock/**"));
    assert!(!matches_pattern(
        "apps/wamn_receiving/domain.sql",
        "apps/dock/**"
    ));
    assert!(!matches_pattern(".beads/issues.jsonl", "apps/dock/**"));
}

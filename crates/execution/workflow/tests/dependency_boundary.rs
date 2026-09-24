//! Which crates link the workflow layer, read from `cargo tree`.
//!
//! `cargo tree -p <crate>` resolves one crate alone. `cargo metadata` resolves
//! features for the whole workspace, so the engine test uses `cargo tree` too.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// The crate names in `cargo tree -p <package> -e normal`.
fn normal_dependencies(package: &str) -> BTreeSet<String> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("wamn-workflow lives three levels below the workspace root");
    let output = Command::new(env!("CARGO"))
        .current_dir(workspace)
        .args(["tree", "-p", package, "-e", "normal"])
        .args(["--prefix", "none", "--locked", "--offline"])
        .output()
        .expect("run cargo tree");
    assert!(
        output.status.success(),
        "cargo tree -p {package} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("cargo tree prints UTF-8")
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

fn assert_links_none(package: &str, refused: &[&str]) {
    let linked = normal_dependencies(package);
    let violations: Vec<&str> = refused
        .iter()
        .copied()
        .filter(|name| linked.contains(*name))
        .collect();
    assert!(
        violations.is_empty(),
        "{package} links {violations:?}; the workflow layer sits above it"
    );
}

#[test]
fn the_engine_links_no_workflow_crate() {
    assert_links_none("wamn-engine", &["wamn-workflow"]);
}

#[test]
fn the_runtime_links_no_workflow_crate() {
    assert_links_none("wamn-runtime", &["wamn-workflow"]);
}

#[test]
#[ignore = "turns on in wamn-xs9a.3, when wamn-runtime drops wamn-router"]
fn the_runtime_links_no_router() {
    assert_links_none("wamn-runtime", &["wamn-router"]);
}

/// The route path lives in `wamn-execution-host`, so this is the test that
/// keeps a route out of the workflow layer.
#[test]
fn the_execution_host_links_no_workflow_crate() {
    assert_links_none("wamn-execution-host", &["wamn-workflow"]);
}

#[test]
#[ignore = "turns on in wamn-xs9a.3: the host links wamn-router only through wamn-runtime"]
fn the_execution_host_links_no_router() {
    assert_links_none("wamn-execution-host", &["wamn-router"]);
}

#[test]
fn the_host_service_links_the_workflow_crate() {
    assert!(
        normal_dependencies("wamn-host").contains("wamn-workflow"),
        "wamn-host must link wamn-workflow"
    );
}

//! Run the repository policy lints over the repository at the given root.
//!
//! `tools/repo-lint` runs this binary as one leg and passes the repository root.

use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let root = std::env::args_os()
        .nth(1)
        .map_or_else(|| PathBuf::from("."), PathBuf::from);
    let problems = wamn_conformance_tests::repo_policy::check(&root);
    for problem in &problems {
        eprintln!("repo-policy: {problem}");
    }
    if problems.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

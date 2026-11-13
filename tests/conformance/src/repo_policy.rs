//! Repository policy lints that `tools/repo-lint` runs through the `repo-policy`
//! binary.
//!
//! These lints enforce file-location conventions and version identities from
//! Cargo metadata and WIT package declarations under one repository root.

mod authored_manifests;
mod generated_clients;
mod version_identity;

use std::path::Path;

/// Every policy violation under `root`, one line each.
pub fn diagnostics(root: &Path) -> Vec<String> {
    let mut problems = Problems::default();
    authored_manifests::report_problems(root, &mut problems);
    generated_clients::report_problems(root, &mut problems);
    version_identity::report_problems(root, &mut problems);
    problems.0
}

/// The violations one run collects.
#[derive(Debug, Default)]
struct Problems(Vec<String>);

impl Problems {
    /// Record `message` unless `holds`.
    fn require(&mut self, holds: bool, message: impl FnOnce() -> String) {
        if !holds {
            self.0.push(message());
        }
    }

    fn push(&mut self, message: String) {
        self.0.push(message);
    }
}

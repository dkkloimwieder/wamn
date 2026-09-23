//! Repository policy lints that `tools/repo-lint` runs through the `repo-policy`
//! binary.
//!
//! Each lint reads the repository under one root and reports every violation it
//! finds. They read source and repository files, so they are lints, not tests.

mod docker_provenance;
mod session_claims;
mod system_cluster;
mod version_identity;

use std::path::Path;

/// Every policy violation under `root`, one line each.
pub fn check(root: &Path) -> Vec<String> {
    let mut problems = Problems::default();
    docker_provenance::check(root, &mut problems);
    version_identity::check(root, &mut problems);
    session_claims::check(root, &mut problems);
    system_cluster::check(root, &mut problems);
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

/// Read one repository file, recording an unreadable file as a violation.
fn read(root: &Path, relative: &str, problems: &mut Problems) -> Option<String> {
    match std::fs::read_to_string(root.join(relative)) {
        Ok(source) => Some(source),
        Err(error) => {
            problems.push(format!("{relative}: {error}"));
            None
        }
    }
}

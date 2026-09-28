//! Guest-safe run-state code never writes the `node_runs` projection.
//!
//! The run-state SQL, queue SQL and transition sources are reachable from a
//! guest. The projection is written only by its platform owner, so none of
//! these sources may carry a statement that writes it.

use std::path::Path;

use super::{Problems, read};

/// The guest-safe run-state sources.
const GUEST_SAFE_SOURCES: &[&str] = &[
    "crates/execution/run-state/src/sql.rs",
    "crates/execution/run-state/src/queue/sql.rs",
    "crates/execution/run-state/src/transitions.rs",
];

/// The statement openings that write the projection.
const PROJECTION_WRITES: &[&str] = &[
    "INSERT INTO node_runs",
    "UPDATE node_runs",
    "DELETE FROM node_runs",
];

pub(super) fn check(root: &Path, problems: &mut Problems) {
    for relative in GUEST_SAFE_SOURCES {
        let Some(source) = read(root, relative, problems) else {
            continue;
        };
        for write in PROJECTION_WRITES {
            problems.require(!source.contains(write), || {
                format!("guest-safe source {relative} writes the node_runs projection ({write:?})")
            });
        }
    }
}

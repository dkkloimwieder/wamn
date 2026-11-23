//! Repository policy lints run by `cargo xtask repo-lint`.
//!
//! These lints enforce file-location conventions and version identities from
//! Cargo metadata and WIT package declarations under one repository root.

mod authored_manifests;
pub(crate) mod dependencies;
mod generated_clients;
mod platform_import_registry;
mod version_identity;

use std::path::Path;

/// Every policy violation under `root`, one line each.
pub fn diagnostics(root: &Path) -> Vec<String> {
    let mut problems = Problems::default();
    authored_manifests::report_problems(root, &mut problems);
    generated_clients::report_problems(root, &mut problems);
    version_identity::report_problems(root, &mut problems);
    dependencies::report_problems(root, &mut problems);
    platform_import_registry::report_problems(root, &mut problems);
    if let Err(error) = environment_schema(root, true) {
        problems.push(format!("{error:#}"));
    }
    problems.0
}

/// Generate the environment schema or verify that the tracked copy is current.
pub fn environment_schema(root: &Path, check: bool) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let path = root.join("crates/environment/environment.schema.json");
    let generated = wamn_environment::environment::document::schema_json()?;
    if check {
        let committed = std::fs::read_to_string(&path)
            .with_context(|| format!("read environment schema {}", path.display()))?;
        anyhow::ensure!(
            committed == generated,
            "environment schema is stale. Run cargo xtask environment-schema"
        );
    } else {
        std::fs::write(&path, generated)
            .with_context(|| format!("write environment schema {}", path.display()))?;
    }
    Ok(())
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

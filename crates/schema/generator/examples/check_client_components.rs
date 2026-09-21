//! Type-checks and tests the generated components for the platform fixture.
//!
//! Run this by hand. Nothing in `cargo build` or `cargo test` needs Node.
//!
//! ```bash
//! cargo run --locked --offline -p wamn-schema-generator --example check_client_components
//! ```
//!
//! A component imports SolidJS, TanStack Table, TanStack Form, and zod, so the
//! check runs inside `web/components`, which installs those libraries once.
//! The command writes the fixture output into `web/components/fixture`, which
//! Git ignores, and then runs the package's own check and tests.
//!
//! `check_client_ts` keeps its own job, which is the bindings alone in a
//! temporary directory.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use wamn_schema_generator::client_ts::emit_ts_client;

#[path = "../tests/support/platform_fixture.rs"]
mod fixture;

fn main() -> Result<()> {
    let harness = repository_root()?.join("web/components");
    if !harness.join("node_modules").is_dir() {
        bail!(
            "{} has no installed dependencies. Run `npm install` there first.",
            harness.display()
        );
    }
    let output = harness.join("fixture");
    let files = emit_ts_client(&fixture::client_release())
        .map_err(|error| anyhow::anyhow!("the platform fixture emits: {error}"))?;

    if output.exists() {
        std::fs::remove_dir_all(&output)
            .with_context(|| format!("{} must be replaceable", output.display()))?;
    }
    std::fs::create_dir_all(&output)?;
    for file in &files {
        // Every emitted path starts with `generated/client-ts/`. The check
        // needs the modules beside each other, not that prefix.
        let name = Path::new(file.path())
            .strip_prefix("generated/client-ts")
            .context("an emitted path sits under the client directory")?;
        let destination = output.join(name);
        std::fs::create_dir_all(destination.parent().context("an emitted parent")?)?;
        std::fs::write(destination, file.bytes())?;
    }
    println!("wrote {} modules into {}", files.len(), output.display());

    run(&harness, "check")?;
    run(&harness, "test")?;
    println!("the generated components type-check and their tests pass");
    Ok(())
}

/// Run one script of the harness package.
fn run(harness: &Path, script: &str) -> Result<()> {
    let status = Command::new("npm")
        .current_dir(harness)
        .args(["run", script, "--silent"])
        .status()
        .with_context(|| format!("running `npm run {script}` in {}", harness.display()))?;
    if !status.success() {
        bail!("`npm run {script}` failed in {}", harness.display());
    }
    Ok(())
}

/// The checkout that holds `web/components`, from this crate's own location.
fn repository_root() -> Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .context("the generator crate sits three directories below the repository root")
}

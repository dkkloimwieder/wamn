//! Package copies that a test edits, built in a test-owned workspace
//! (wamn-qh1g, root ruling of 2026-10-11).
//!
//! `wamn build` builds a package's components in `<package>/..`, its Cargo
//! workspace, with the tools of `<package>/../..`, its repository. So `<root>`
//! mirrors the repository: each top-level entry is a symlink, except `tools`,
//! the workspace manifest and its lock, which are copies, and `apps`. In
//! `<root>/apps` each entry is a symlink, except the copied packages, the apps
//! workspace manifest and its lock, which are copies, and `target`, which the
//! build writes. `tools/guest-rustflags` then maps `<root>` as the repository,
//! and the build of an edited copy runs with the generator unchanged and
//! leaves the repository's own build output alone. push-package takes the copy
//! like any other built package.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, ensure};
use tokio::process::Command;

use super::copy_package;

/// The entries of a repository or an apps directory that are copied or built,
/// never linked.
const OWNED: [&str; 5] = ["Cargo.toml", "Cargo.lock", "target", "tools", ".git"];

/// The mirror of the repository at `root`, with a copy of each package of
/// `sources`, its build output included, at `<root>/apps/<directory>`.
pub(super) fn create(root: &Path, sources: &[PathBuf]) -> anyhow::Result<()> {
    let apps = sources
        .first()
        .and_then(|source| source.parent())
        .context("a package copy names its source")?
        .canonicalize()?;
    let repository = apps.parent().context("apps lies in the repository")?;
    std::fs::create_dir(root).context("a package copy root must be new")?;
    let copied: Vec<_> = sources
        .iter()
        .map(|source| source.file_name().context("a package has a directory name"))
        .collect::<anyhow::Result<_>>()?;
    for (from, to, skip) in [
        (
            repository,
            root.to_owned(),
            vec![std::ffi::OsStr::new("apps")],
        ),
        (apps.as_path(), root.join("apps"), copied.clone()),
    ] {
        if to != root {
            std::fs::create_dir(&to)?;
        }
        for entry in std::fs::read_dir(from)? {
            let name = entry?.file_name();
            if skip.contains(&name.as_os_str()) || name == "target" || name == ".git" {
                continue;
            }
            let source = from.join(&name);
            if OWNED.iter().any(|owned| name == *owned) {
                copy_tree(&source, &to.join(&name))?;
            } else {
                std::os::unix::fs::symlink(&source, to.join(&name))?;
            }
        }
    }
    for (source, name) in sources.iter().zip(copied) {
        let copy = package(root, name);
        copy_package(source, &copy)?;
        // A copy has no web client, so `wamn build` skips its web build. pnpm
        // would install that build from `<root>`, whose `node_modules` and
        // `web` link to the repository, and rewrite the repository's links
        // into `<root>` (wamn-q4xxd).
        let web = copy.join("web");
        if web.exists() {
            std::fs::remove_dir_all(web)?;
        }
    }
    Ok(())
}

/// The copy of the package directory `name` below `root`.
pub(super) fn package(root: &Path, name: impl AsRef<Path>) -> PathBuf {
    root.join("apps").join(name)
}

/// `wamn build` of the copied packages `names`, base packages first, against
/// the PostgreSQL server of `server_url`, into `<root>/apps/target`.
pub(super) async fn build(root: &Path, names: &[&str], server_url: &str) -> anyhow::Result<()> {
    // The test executable lies in <target>/debug/deps, beside the wamn binary
    // that the build of the cases made.
    let wamn = std::env::current_exe()?
        .parent()
        .and_then(Path::parent)
        .context("the test executable lies in a target directory")?
        .join("wamn");
    ensure!(
        wamn.is_file(),
        "{} is not built; build wamn-ctl --bin wamn first",
        wamn.display()
    );
    let mut server = reqwest::Url::parse(server_url)?;
    server.set_path("/postgres");
    let output = Command::new(&wamn)
        .arg("build")
        .args(names.iter().map(|name| package(root, name)))
        .env("DATABASE_URL", server.as_str())
        .env("CARGO_TARGET_DIR", root.join("apps/target"))
        .current_dir(root)
        .kill_on_drop(true)
        .output()
        .await
        .context("start wamn build of the package copies")?;
    std::fs::write(
        root.join("build.log"),
        [&output.stdout[..], &output.stderr[..]].concat(),
    )?;
    ensure!(
        output.status.success(),
        "wamn build of the package copies exited {}; see {}",
        output.status,
        root.join("build.log").display()
    );
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> anyhow::Result<()> {
    if source.is_file() {
        std::fs::copy(source, target)?;
        return Ok(());
    }
    std::fs::create_dir(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        copy_tree(&entry.path(), &target.join(entry.file_name()))?;
    }
    Ok(())
}

/// An edited copy of the base builds its component in the test-owned
/// workspace, and push-package's stale check accepts its receipt.
#[tokio::test(flavor = "current_thread")]
#[ignore = "requires: cargo, rustc"]
async fn an_edited_copy_builds_in_the_test_owned_workspace() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&["cargo", "rustc"]);
    let scratch = super::ScratchRoot::create()?;
    let root = scratch.path().join("copies");
    create(&root, &[super::package_root()])?;
    let base = package(&root, "wamn_receiving");
    let migration = base.join("migrations/0001_initial.sql");
    let text = std::fs::read_to_string(&migration)?;
    std::fs::write(
        &migration,
        text.replacen(
            "CREATE TABLE receiving.purchase_order (\n",
            "CREATE TABLE receiving.purchase_order (\n    copy_note text,\n",
            1,
        ),
    )?;
    let database = wamn_test_postgres::database();
    build(&root, &["wamn_receiving"], database.url()).await?;
    let output = wamn_schema_generator::output_root(&base);
    let receipt = wamn_schema_generator::receipt::read(&output)?;
    ensure!(
        receipt.inputs.digest == wamn_schema_generator::receipt::input_digest(&base)?
            && receipt
                .outputs
                .components
                .iter()
                .any(|component| component.name == "receiving"
                    && output.join(&component.path).is_file()),
        "the copy's build is current and records its component"
    );
    Ok(())
}

//! The platform-owned fixture application, for the tests of the whole platform.
//!
//! The platform owns no application, so a platform test states no Receiving,
//! WMS or Acme fact. It takes this fixture instead. The fixture is an
//! application under `apps/`: it holds a manifest, authored SQL, migrations
//! and a committed generated tree, and it builds the way every application
//! builds.
//!
//! This crate carries paths and file contents. It depends on no platform
//! crate, so any crate in the workspace can take it as a dev-dependency
//! without a dependency cycle.

use std::path::{Path, PathBuf};

use tokio_postgres::Client;

/// The package id the fixture manifest declares.
pub const PACKAGE_ID: &str = "platform_fixture";

/// The package id of the minimal overlay above the fixture.
pub const OVERLAY_PACKAGE_ID: &str = "platform_fixture_overlay";

/// The one schema the fixture and its overlay live in.
pub const SCHEMA: &str = "inventory";

/// The repository root, from this crate's own place inside it.
#[must_use]
pub fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the fixture crate sits inside the repository")
}

/// The fixture application's directory, which `materialize_package` takes.
#[must_use]
pub fn package_root() -> PathBuf {
    repository_root().join("apps").join(PACKAGE_ID)
}

/// The overlay application's directory.
#[must_use]
pub fn overlay_root() -> PathBuf {
    repository_root().join("apps").join(OVERLAY_PACKAGE_ID)
}

/// The authored manifest bytes of the fixture application.
///
/// # Panics
/// Panics when the fixture application is missing from the checkout.
#[must_use]
pub fn manifest_bytes() -> Vec<u8> {
    read(&package_root().join("wamn.json"))
}

/// The authored manifest bytes of the overlay application.
///
/// # Panics
/// Panics when the overlay application is missing from the checkout.
#[must_use]
pub fn overlay_manifest_bytes() -> Vec<u8> {
    read(&overlay_root().join("wamn.json"))
}

/// The migrations directory, for a runner that takes `--migration-dir`.
#[must_use]
pub fn migrations_dir() -> PathBuf {
    package_root().join("migrations")
}

/// The overlay's migrations directory, which a runner applies after the base.
#[must_use]
pub fn overlay_migrations_dir() -> PathBuf {
    overlay_root().join("migrations")
}

/// The fixture migrations, in the order a runner applies them.
///
/// Each entry is the file name and its SQL.
///
/// # Panics
/// Panics when the migrations directory is unreadable.
#[must_use]
pub fn migrations() -> Vec<(String, String)> {
    read_migrations(&migrations_dir())
}

/// The overlay migrations, in the order a runner applies them.
///
/// # Panics
/// Panics when the migrations directory is unreadable.
#[must_use]
pub fn overlay_migrations() -> Vec<(String, String)> {
    read_migrations(&overlay_migrations_dir())
}

/// The fixture's authored SQL, keyed by the path its manifest states.
///
/// A generation input takes these bytes, so a test states no SQL of its own.
///
/// # Panics
/// Panics when an authored directory is unreadable.
#[must_use]
pub fn authored_sql() -> Vec<(String, Vec<u8>)> {
    let root = package_root();
    let mut sources = Vec::new();
    for directory in ["query", "command"] {
        collect_sql(&root, &root.join(directory), &mut sources);
    }
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    sources
}

fn collect_sql(root: &Path, directory: &Path, sources: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("read an authored entry").path();
        if path.is_dir() {
            collect_sql(root, &path, sources);
        } else if path.extension().is_some_and(|extension| extension == "sql") {
            let relative = path
                .strip_prefix(root)
                .expect("an authored file sits inside the package")
                .to_string_lossy()
                .into_owned();
            sources.push((relative, read(&path)));
        }
    }
}

/// Apply the fixture schema through a connection the test already owns.
///
/// This creates the schema when it is absent, then applies each migration in
/// file order. A runner that takes `--migration-dir` reads the same files.
///
/// # Errors
/// Returns the server's error when a statement is refused.
pub async fn apply_migrations(client: &Client) -> Result<(), tokio_postgres::Error> {
    apply(client, &migrations()).await
}

/// Apply the overlay schema above the fixture schema.
///
/// # Errors
/// Returns the server's error when a statement is refused.
pub async fn apply_overlay_migrations(client: &Client) -> Result<(), tokio_postgres::Error> {
    apply(client, &overlay_migrations()).await
}

async fn apply(
    client: &Client,
    migrations: &[(String, String)],
) -> Result<(), tokio_postgres::Error> {
    client
        .batch_execute(&format!("CREATE SCHEMA IF NOT EXISTS {SCHEMA};"))
        .await?;
    for (_, sql) in migrations {
        client.batch_execute(sql).await?;
    }
    Ok(())
}

fn read_migrations(directory: &Path) -> Vec<(String, String)> {
    let mut entries = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("read {}: {error}", directory.display()))
        .map(|entry| entry.expect("read a migration entry").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "sql"))
        .collect::<Vec<_>>();
    entries.sort();
    entries
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("a migration file has a name")
                .to_string_lossy()
                .into_owned();
            (
                name,
                String::from_utf8(read(&path)).expect("migration SQL is UTF-8"),
            )
        })
        .collect()
}

fn read(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

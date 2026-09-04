//! Materializes package generator output from an already-migrated database.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail, ensure};
use wamn_schema_generator::{MaterializeMode, materialize_package_verified};

const DATABASE_URL_ENV: &str = "WAMN_SCHEMA_INTROSPECTION_PG_URL";

#[derive(Debug)]
struct Arguments {
    mode: MaterializeMode,
    package_root: PathBuf,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let arguments = arguments()?;
    let database_url = std::env::var(DATABASE_URL_ENV)
        .with_context(|| format!("{DATABASE_URL_ENV} must name an already-migrated database"))?;
    // Plans every statement against the migrated database so the contracts
    // carry PostgreSQL's verdict on which of them need a transaction.
    materialize_package_verified(arguments.mode, &database_url, &arguments.package_root).await
}

fn arguments() -> Result<Arguments> {
    let mut values = std::env::args().skip(1);
    let mode = values
        .next()
        .context("usage: materialize_package <write|check> <package-root>")?;
    let package_root = values
        .next()
        .map(PathBuf::from)
        .context("usage: materialize_package <write|check> <package-root>")?;
    ensure!(
        values.next().is_none(),
        "usage: materialize_package <write|check> <package-root>"
    );
    Ok(Arguments {
        mode: parse_mode(&mode)?,
        package_root,
    })
}

fn parse_mode(value: &str) -> Result<MaterializeMode> {
    match value {
        "write" => Ok(MaterializeMode::Write),
        "check" => Ok(MaterializeMode::Check),
        _ => bail!("mode must be exactly `write` or `check`"),
    }
}

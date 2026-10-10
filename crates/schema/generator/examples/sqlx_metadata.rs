//! Prepares SQLx metadata for one generated package into its build output.
//!
//! The dev loop runs it. It goes when the dev loop builds through `wamn build`.

use std::path::PathBuf;

use anyhow::{Context as _, Result, ensure};
use wamn_schema_generator::prepare_sqlx_metadata;

const USAGE: &str = "usage: sqlx_metadata prepare <package-root>";

fn main() -> Result<()> {
    let mut values = std::env::args().skip(1);
    ensure!(values.next().as_deref() == Some("prepare"), USAGE);
    let package_root = values.next().map(PathBuf::from).context(USAGE)?;
    ensure!(values.next().is_none(), USAGE);
    prepare_sqlx_metadata(&package_root)
}

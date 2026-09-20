//! Checks or prepares SQLx metadata for one generated package.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail, ensure};
use wamn_schema_generator::{SqlxMetadataMode, verify_sqlx_metadata};

fn main() -> Result<()> {
    let mut values = std::env::args().skip(1);
    let mode = match values.next().as_deref() {
        Some("compile") => SqlxMetadataMode::Compile,
        Some("check") => SqlxMetadataMode::Check,
        Some("prepare") => SqlxMetadataMode::Prepare,
        Some(value) => {
            bail!("mode must be exactly `compile`, `check`, or `prepare`, found {value}")
        }
        None => bail!("usage: sqlx_metadata <compile|check|prepare> <package-root>"),
    };
    let package_root = values
        .next()
        .map(PathBuf::from)
        .context("usage: sqlx_metadata <compile|check|prepare> <package-root>")?;
    ensure!(
        values.next().is_none(),
        "usage: sqlx_metadata <compile|check|prepare> <package-root>"
    );
    verify_sqlx_metadata(mode, &package_root)
}

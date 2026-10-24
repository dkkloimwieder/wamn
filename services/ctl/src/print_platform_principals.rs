//! Print the SQL that creates the platform principal rows of a project
//! database.
//!
//! The verb opens no database connection. An operator or a test tool pipes its
//! output into `psql` against the tenant database, after `app-schema.sql`.
//! `wamn-0h0g.9` owns the production caller that applies it.

use anyhow::Context as _;
use clap::Args;

/// Arguments naming the platform domain.
#[derive(Debug, Args)]
pub struct PrintPlatformPrincipalsArgs {
    /// Domain of the platform row emails, `<component>@<platform-domain>`.
    #[arg(long)]
    pub platform_domain: String,
}

/// Print the platform principal rows as SQL on stdout.
pub fn run(args: &PrintPlatformPrincipalsArgs) -> anyhow::Result<()> {
    let sql = wamn_control_provision::platform_principals_sql(&args.platform_domain)
        .context("render the platform principal rows")?;
    print!("{sql}");
    Ok(())
}

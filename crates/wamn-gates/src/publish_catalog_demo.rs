//! Gates-side `publish-catalog` wrapper: the prod subcommand plus the
//! `--seed` demo flag SR1 removed from the prod binary (fixture content must
//! not ship in the prod artifact). Provisioning/publication is the identical
//! library path (`wamn_host::publish_catalog::run`); the wrapper only appends
//! the bundled two-tenant demo rows (`apifixture`, matching
//! `deploy/poc/proof-catalog.json`) afterwards — the seed is `ON CONFLICT DO
//! NOTHING` inserts into the floor tables, so ordering after the snapshot
//! upsert is equivalent to the old in-line placement.

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;

use crate::apifixture;

#[derive(Debug, Args)]
pub struct PublishCatalogDemoArgs {
    #[command(flatten)]
    pub inner: wamn_host::publish_catalog::PublishCatalogArgs,

    /// Also seed the bundled two-tenant demo rows (proof scaffolding matching
    /// the bundled `deploy/poc/proof-catalog.json`; idempotent).
    #[arg(long)]
    pub seed: bool,
}

pub async fn run(args: PublishCatalogDemoArgs) -> anyhow::Result<()> {
    let admin_url = args.inner.admin_database_url.clone();
    let schema = args.inner.schema.clone();
    wamn_host::publish_catalog::run(args.inner).await?;

    if args.seed {
        let admin_url = admin_url
            .context("no admin database url: pass --admin-database-url or set WAMN_PG_ADMIN_URL")?;
        let (client, conn) = tokio_postgres::connect(&admin_url, NoTls)
            .await
            .context("admin connect (seed)")?;
        let conn_task = tokio::spawn(conn);
        // The schema was already validated (and created) by the publish run.
        let result = client
            .batch_execute(&format!(
                "SET search_path TO \"{schema}\"; {}",
                apifixture::entity_seed_sql()
            ))
            .await
            .context("seed demo rows");
        drop(client);
        let _ = conn_task.await;
        result?;
        println!("seeded demo rows in schema {schema}");
    }
    Ok(())
}

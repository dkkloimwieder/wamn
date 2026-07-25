//! Gates-side `publish-catalog` wrapper: the prod subcommand plus the
//! `--seed` demo flag SR1 removed from the prod binary (fixture content must
//! not ship in the prod artifact). Provisioning/publication is the identical
//! public `wamn-ctl publish-catalog` process boundary; the wrapper only appends
//! the bundled two-tenant demo rows (`apifixture`, matching
//! `deploy/poc/proof-catalog.json`) afterwards — the seed is `ON CONFLICT DO
//! NOTHING` inserts into the floor tables, so ordering after the snapshot
//! upsert is equivalent to the old in-line placement.

use anyhow::Context as _;
use clap::Args;
use std::path::PathBuf;
use tokio_postgres::NoTls;

use crate::ctl_process;
use wamn_test_fixtures::apifixture;

#[derive(Debug, Args)]
pub struct PublishCatalogDemoArgs {
    /// Path to the catalog JSON to publish.
    #[arg(long)]
    pub catalog: PathBuf,

    /// Superuser Postgres URL.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: Option<String>,

    /// Tenant the snapshot is published under.
    #[arg(long)]
    pub tenant: String,

    /// Project schema.
    #[arg(long, default_value = "public")]
    pub schema: String,

    /// Provision the catalog floor.
    #[arg(long)]
    pub provision: bool,

    /// Provision run state, flow registry, and scenario tables.
    #[arg(long)]
    pub runstate: bool,

    /// Optional seed dataset.
    #[arg(long)]
    pub seed_dataset: Option<PathBuf>,

    /// Flow graphs to register.
    #[arg(long)]
    pub flow: Vec<PathBuf>,

    /// Skip the post-publish replica-identity reconcile.
    #[arg(long)]
    pub skip_reconcile_replica_identity: bool,

    /// Also seed the bundled two-tenant demo rows (proof scaffolding matching
    /// the bundled `deploy/poc/proof-catalog.json`; idempotent).
    #[arg(long)]
    pub seed: bool,
}

pub async fn run(args: PublishCatalogDemoArgs) -> anyhow::Result<()> {
    let admin_url = args.admin_database_url.clone();
    let schema = args.schema.clone();
    let mut command = vec![
        "publish-catalog".into(),
        "--catalog".into(),
        args.catalog.into_os_string(),
        "--tenant".into(),
        args.tenant.into(),
        "--schema".into(),
        args.schema.into(),
    ];
    if let Some(url) = &admin_url {
        command.push("--admin-database-url".into());
        command.push(url.into());
    }
    if args.provision {
        command.push("--provision".into());
    }
    if args.runstate {
        command.push("--runstate".into());
    }
    if let Some(seed_dataset) = args.seed_dataset {
        command.push("--seed-dataset".into());
        command.push(seed_dataset.into_os_string());
    }
    for flow in args.flow {
        command.push("--flow".into());
        command.push(flow.into_os_string());
    }
    if args.skip_reconcile_replica_identity {
        command.push("--skip-reconcile-replica-identity".into());
    }
    ctl_process::run_checked(command).await?;

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

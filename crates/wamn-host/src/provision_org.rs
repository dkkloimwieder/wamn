//! The `provision-org` subcommand (wamn-q3n.6): render a paying org's CNPG
//! `Cluster` PAIR (`<org>-prod` HA + `<org>-dev` hibernation-managed) and record
//! the org in the T1 control-plane registry.
//!
//! An imperative CLI (the `provision-project` precedent), run as a Job or from a
//! runbook. It:
//!
//! 1. builds the org's placement — `<org>-prod` / `<org>-dev` cluster refs via
//!    [`wamn_registry::cluster_name`] — and validates the org id + names by
//!    running the one-org registry through `wamn-registry`'s validator;
//! 2. renders the two CNPG `Cluster` CRs ([`wamn_provision::org`]) and emits them
//!    as JSON — the runbook/Job `kubectl apply -f`s them and waits ready;
//! 3. records the org row in `registry.orgs` in the T1 `wamn_system` DB
//!    (idempotent upsert, as the `wamn_system` owner) when a system-DB URL is
//!    given.
//!
//! Rendering the CRs and writing the registry row is **all** this tool does — it
//! does NOT apply the CRs (no K8s client; the runbook does, like
//! `provision-project`'s Secret) and does NOT create per-project-env databases
//! (the CNPG `Database` CRD + `.spec.managed.roles` path is wamn-q3n.7). Backups
//! (WAL/PITR) are wamn-e1g.

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use tokio_postgres::NoTls;

use wamn_registry::{Org, Registry, SCHEMA_VERSION, Tier};

/// The paying tiers `provision-org` renders a dedicated cluster pair for. A
/// `trials` org lives on the shared pool (not a pair), so it is deliberately not
/// selectable here — T3 provisioning is wamn-q3n.9.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TierArg {
    /// T2 standard: `<org>-prod` HA (2 instances) + `<org>-dev` single instance.
    Standard,
    /// T4 dedicated (regulated): `<org>-prod` HA (3 instances) + `<org>-dev`.
    Dedicated,
}

impl From<TierArg> for Tier {
    fn from(t: TierArg) -> Tier {
        match t {
            TierArg::Standard => Tier::Standard,
            TierArg::Dedicated => Tier::Dedicated,
        }
    }
}

#[derive(Debug, Args)]
pub struct ProvisionOrgArgs {
    /// Org id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). Names the
    /// `<org>-prod` / `<org>-dev` clusters; the reserved `wamn` prefix is rejected.
    #[arg(long)]
    pub org: String,

    /// Hosting tier: `standard` (T2) or `dedicated` (T4). A `trials` org shares
    /// the pool, not a dedicated pair — it is not provisioned here (wamn-q3n.9).
    #[arg(long, value_enum)]
    pub tier: TierArg,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`), where the org
    /// row is recorded. Env `WAMN_SYSTEM_ADMIN_URL`. Omit to render CRs only.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Write the `<org>-prod` Cluster CR (JSON) here; `-` = stdout (default).
    #[arg(long)]
    pub emit_prod: Option<PathBuf>,

    /// Write the `<org>-dev` Cluster CR (JSON) here; `-` = stdout (default).
    #[arg(long)]
    pub emit_dev: Option<PathBuf>,
}

pub async fn run(args: ProvisionOrgArgs) -> anyhow::Result<()> {
    let tier: Tier = args.tier.into();
    let org = Org::for_pair(&args.org, tier);

    // Validate the org id (slug / reserved-prefix) + the derived cluster names by
    // running the one-org registry through the model's validator (a registry with
    // one org and no projects is valid on its own).
    let reg = Registry {
        schema_version: SCHEMA_VERSION.to_string(),
        orgs: vec![org.clone()],
        projects: Vec::new(),
        project_envs: Vec::new(),
    };
    reg.validate()
        .map_err(|issues| anyhow::anyhow!("invalid org: {}", fmt_issues(&issues)))?;

    // Render the cluster pair (errors on a trials tier, which has no pair).
    let (prod_cr, dev_cr) = wamn_provision::org::render_org_cluster_pair(&org)
        .map_err(|e| anyhow::anyhow!("render org clusters: {e}"))?;

    println!(
        "org {id:?} (tier {tier}): {prod} ({pi} instances, HA) + {dev} (1 instance, hibernation-managed)",
        id = org.id,
        prod = org.prod_cluster.name,
        dev = org.dev_cluster.name,
        pi = prod_cr["spec"]["instances"],
    );

    // Emit the CRs (default: both to stdout) so the runbook/Job kubectl-applies them.
    let emit_prod = args.emit_prod.unwrap_or_else(|| PathBuf::from("-"));
    let emit_dev = args.emit_dev.unwrap_or_else(|| PathBuf::from("-"));
    write_json(&emit_prod, &prod_cr).context("emit prod cluster CR")?;
    write_json(&emit_dev, &dev_cr).context("emit dev cluster CR")?;

    // Record the org in the T1 registry (idempotent), when a system-DB URL is given.
    match &args.system_database_url {
        Some(url) => {
            record_org(url, &org).await?;
            println!("recorded org {:?} in registry.orgs (wamn_system)", org.id);
        }
        None => println!("(no --system-database-url: rendered CRs only; org not recorded)"),
    }

    Ok(())
}

/// Upsert the org's placement row into `registry.orgs` on the T1 system DB.
/// Connects as superuser and `SET ROLE wamn_system` (the registry owner — the
/// wamn-q3n.3 apply pattern), then runs the pure
/// [`wamn_registry::sql::upsert_org_sql`] builder. Idempotent + additive
/// (`ON CONFLICT (id) DO UPDATE`; the shared-cluster guardrail — never drops).
async fn record_org(system_url: &str, org: &Org) -> anyhow::Result<()> {
    let (client, conn) = tokio_postgres::connect(system_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = do_record_org(&client, org).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn do_record_org(client: &tokio_postgres::Client, org: &Org) -> anyhow::Result<()> {
    // Write as the wamn_system owner (the registry's owner role), not the raw
    // superuser — mirrors how wamn-q3n.3 applies the schema.
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let tier = org.tier.as_str();
    client
        .execute(
            wamn_registry::sql::upsert_org_sql(),
            &[
                &org.id,
                &tier,
                &org.prod_cluster.name,
                &org.dev_cluster.name,
            ],
        )
        .await
        .context("upsert registry.orgs row")?;
    Ok(())
}

fn fmt_issues(issues: &[wamn_registry::Issue]) -> String {
    issues
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("; ")
}

fn write_json(path: &PathBuf, doc: &serde_json::Value) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(doc)?;
    if path.as_os_str() == "-" {
        println!("{text}");
    } else {
        std::fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_org_registry(org: Org) -> Registry {
        Registry {
            schema_version: SCHEMA_VERSION.to_string(),
            orgs: vec![org],
            projects: Vec::new(),
            project_envs: Vec::new(),
        }
    }

    #[test]
    fn tier_arg_maps_to_the_registry_tier() {
        assert_eq!(Tier::from(TierArg::Standard), Tier::Standard);
        assert_eq!(Tier::from(TierArg::Dedicated), Tier::Dedicated);
    }

    #[test]
    fn for_pair_org_validates_and_names_its_clusters() {
        let org = Org::for_pair("acme", TierArg::Standard.into());
        assert_eq!(org.prod_cluster.name, "acme-prod");
        assert_eq!(org.dev_cluster.name, "acme-dev");
        assert!(one_org_registry(org).validate().is_ok());
    }

    #[test]
    fn reserved_org_id_is_rejected() {
        // The `provision-org` id path runs through the same validator the registry
        // uses, so a reserved-prefix org id is refused before any effect.
        let reg = one_org_registry(Org::for_pair("wamn-corp", Tier::Standard));
        assert!(reg.validate().is_err());
        let reg = one_org_registry(Org::for_pair("Acme", Tier::Standard));
        assert!(reg.validate().is_err(), "an uppercase id is not a slug");
    }
}

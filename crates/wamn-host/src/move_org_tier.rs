//! The `move-org-tier` subcommand (wamn-q3n.13): promote an org to a
//! higher-isolation tier — T3→T2 (trial-convert) or T2→T4 (regulated upgrade).
//!
//! A tier move re-points an org onto the new tier's clusters via the
//! `CredentialProvider` seam (docs/postgres-topology.md §Reversibility): per
//! project-env, dump the current database, provision it on the new cluster, restore
//! the dump, then flip the registry placement row. It is a **scheduled operation**,
//! not free — a dump/restore window (or a logical-replication cutover for
//! near-zero-downtime).
//!
//! This subcommand is the **orchestrating shell** — it validates the upgrade path,
//! reads the org's current placement + project-envs from the T1 registry, and:
//!
//! * **plan mode** (default): prints the ordered runbook — the exact
//!   `provision-org` / `dump-project-env` / `provision-project-env` /
//!   `restore-project-env` invocations + `kubectl apply`s an operator (or `10.1`'s
//!   saga) runs, in dependency order (dump before flip; restore before cutover);
//! * **`--flip`**: executes the final control-plane cutover — the idempotent
//!   `registry.orgs` upsert to the new tier + cluster refs, run **after** the data
//!   move.
//!
//! It does not apply K8s CRs or run `pg_dump`/`pg_restore` itself (no K8s client;
//! those are the reused subcommands' jobs — the `provision-org` precedent of
//! rendering-not-applying). The pure upgrade validation + ordered step plan live in
//! [`wamn_provision::tier_move`]; the resumable/compensating saga that would drive
//! the plan automatically is `10.1`'s.

use anyhow::Context as _;
use clap::Args;
use tokio_postgres::NoTls;

use wamn_provision::tier_move::{TierMoveStep, plan_tier_move, validate_tier_upgrade};
use wamn_registry::{Env, Org, Tier};

use crate::provision_org::TierArg;

#[derive(Debug, Args)]
pub struct MoveOrgTierArgs {
    /// Org id to promote (must already be registered).
    #[arg(long)]
    pub org: String,

    /// Target hosting tier: `standard` (T2) or `dedicated` (T4). Must be a strict
    /// upgrade from the org's current tier (`trials` is never a target — you cannot
    /// move data down to the shared pool).
    #[arg(long, value_enum)]
    pub target_tier: TierArg,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`): read the org's
    /// current tier + placement + project-envs, and (with `--flip`) record the
    /// cutover. Env `WAMN_SYSTEM_ADMIN_URL`.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: String,

    /// Execute the final registry CUTOVER: flip the org row to the new tier +
    /// cluster refs (idempotent `upsert_org_sql`). Run this **after** the data move
    /// (the dump/provision/restore steps the plan prints). Without it, the ordered
    /// runbook is printed and nothing is written.
    #[arg(long)]
    pub flip: bool,

    /// Local root the pre-move dumps are staged under (passed through to the
    /// `dump-project-env` / `restore-project-env` command hints in the plan).
    #[arg(long, default_value = "/tmp/wamn-dump")]
    pub dump_root: String,
}

pub async fn run(args: MoveOrgTierArgs) -> anyhow::Result<()> {
    let target: Tier = args.target_tier.into();

    let (client, conn) = tokio_postgres::connect(&args.system_database_url, NoTls)
        .await
        .context("system db connect")?;
    let conn_task = tokio::spawn(conn);
    let result = drive(&client, &args, target).await;
    drop(client);
    let _ = conn_task.await;
    result
}

async fn drive(
    client: &tokio_postgres::Client,
    args: &MoveOrgTierArgs,
    target: Tier,
) -> anyhow::Result<()> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;

    let current = read_current_tier(client, &args.org).await?;

    // The final cutover flips the org row (idempotent; rejects a downgrade flip).
    // It needs neither the current placement nor the project-envs.
    if args.flip {
        return flip_registry(client, &args.org, current, target).await;
    }

    // Plan mode: read the current placement + project-envs and print the runbook.
    // Validate + plan (pure); a downgrade / no-op errors before any effect.
    let (cur_prod, cur_dev) = read_current_clusters(client, &args.org).await?;
    let project_envs = read_project_envs(client, &args.org).await?;
    let steps = plan_tier_move(&args.org, current, target, &project_envs)
        .map_err(|e| anyhow::anyhow!("tier move: {e}"))?;
    print_runbook(args, current, target, &cur_prod, &cur_dev, &steps);
    Ok(())
}

/// Execute the final cutover: flip the org's `registry.orgs` row to the new tier +
/// its cluster pair (the same idempotent `upsert_org_sql` `provision-org` uses).
/// **Idempotent**: if the org is already on the target tier, it is a no-op — a
/// crash-retry after a committed flip succeeds. A flip that would DOWNGRADE is
/// rejected (the same guard as plan mode).
async fn flip_registry(
    client: &tokio_postgres::Client,
    org: &str,
    current: Tier,
    target: Tier,
) -> anyhow::Result<()> {
    if current == target {
        println!(
            "org {org:?} is already on tier {} — flip is a no-op",
            target.as_str()
        );
        return Ok(());
    }
    validate_tier_upgrade(current, target).map_err(|e| anyhow::anyhow!("tier move: {e}"))?;

    let target_org = Org::for_pair(org, target);
    let tier = target_org.tier.as_str();
    client
        .execute(
            wamn_registry::sql::upsert_org_sql(),
            &[
                &target_org.id,
                &tier,
                &target_org.prod_cluster.name,
                &target_org.dev_cluster.name,
            ],
        )
        .await
        .context("flip registry.orgs to the new tier")?;
    println!(
        "flipped org {org:?} to tier {tier} (prod {:?}, dev {:?}) in registry.orgs — cutover complete",
        target_org.prod_cluster.name, target_org.dev_cluster.name,
    );
    Ok(())
}

/// Read the org's current tier from the registry.
async fn read_current_tier(client: &tokio_postgres::Client, org: &str) -> anyhow::Result<Tier> {
    let row = client
        .query_opt(wamn_registry::sql::select_org_tier_sql(), &[&org])
        .await
        .context("read org tier")?
        .with_context(|| format!("org {org:?} is not registered (run provision-org first)"))?;
    let tier: String = row.get("tier");
    tier_from_str(&tier).with_context(|| format!("unknown tier {tier:?} in registry"))
}

/// Read the org's current prod/dev cluster placement (for the dump-source hints).
async fn read_current_clusters(
    client: &tokio_postgres::Client,
    org: &str,
) -> anyhow::Result<(String, String)> {
    let row = client
        .query_opt(wamn_registry::sql::select_org_clusters_sql(), &[&org])
        .await
        .context("read org placement")?
        .with_context(|| format!("org {org:?} is not registered"))?;
    Ok((row.get("prod_cluster"), row.get("dev_cluster")))
}

/// Read the org's provisioned `(project, env)` project-envs (one move per env).
async fn read_project_envs(
    client: &tokio_postgres::Client,
    org: &str,
) -> anyhow::Result<Vec<(String, Env)>> {
    let rows = client
        .query(wamn_registry::sql::select_org_project_envs_sql(), &[&org])
        .await
        .context("read org project-envs")?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let project: String = row.get("project");
        let env: String = row.get("env");
        let env = env_from_str(&env).with_context(|| format!("unknown env {env:?} in registry"))?;
        out.push((project, env));
    }
    anyhow::ensure!(
        !out.is_empty(),
        "org {org:?} has no provisioned project-envs to move (nothing to promote)"
    );
    Ok(out)
}

/// Print the ordered tier-move runbook — the concrete commands an operator (or
/// `10.1`'s saga) runs, in dependency order. The org's registry row stays on the
/// OLD tier throughout the data move; the final `--flip` is the atomic cutover.
fn print_runbook(
    args: &MoveOrgTierArgs,
    current: Tier,
    target: Tier,
    cur_prod: &str,
    cur_dev: &str,
    steps: &[TierMoveStep],
) {
    let org = &args.org;
    let root = &args.dump_root;
    println!(
        "tier move: org {org:?} {current} -> {target} ({} steps). \
         SCHEDULED operation (dump/restore window); the registry row stays on {current} \
         until the final --flip cutover.\n",
        steps.len()
    );
    let mut n = 0;
    for step in steps {
        n += 1;
        match step {
            TierMoveStep::ProvisionClusters {
                prod_cluster,
                dev_cluster,
                prod_instances,
            } => println!(
                "{n}. provision the new {target} cluster pair (render-only, then apply + wait ready):\n   \
                 wamn-host provision-org --org {org} --tier {target} \\\n     \
                 --emit-prod /tmp/{prod_cluster}.json --emit-dev /tmp/{dev_cluster}.json\n   \
                 kubectl apply -f /tmp/{prod_cluster}.json -f /tmp/{dev_cluster}.json\n   \
                 # wait: {prod_cluster} ({prod_instances} instances, HA) + {dev_cluster} (1)"
            ),
            TierMoveStep::Dump { triple } => {
                let old = if triple.env.side() == wamn_registry::Side::Prod {
                    cur_prod
                } else {
                    cur_dev
                };
                println!(
                    "{n}. dump {triple}'s CURRENT database (on {old}):\n   \
                     wamn-host dump-project-env --org {org} --project {p} --env {e} --tier {current} \\\n     \
                     --run-now --database-url <superuser url to {old}/wamn-db-{org}--{p}--{e}> \\\n     \
                     --out-dir {root} --system-database-url $WAMN_SYSTEM_ADMIN_URL",
                    p = triple.project,
                    e = triple.env,
                );
            }
            TierMoveStep::ProvisionEnv { triple, cluster } => println!(
                "{n}. create {triple}'s database on the NEW cluster {cluster} (the Database CR \
                 name is triple-derived + shared across clusters, so delete the OLD CR first — \
                 databaseReclaimPolicy: retain keeps its data on the old cluster):\n   \
                 kubectl delete database wamn-db-{org}--{p}--{e} --ignore-not-found\n   \
                 wamn-host provision-project-env --org {org} --project {p} --env {e} \\\n     \
                 --cluster {cluster} --emit-role-sql /tmp/role.sql --emit-database /tmp/db.json \\\n     \
                 --emit-privilege-sql /tmp/priv.sql --emit-secret /tmp/secret.json\n   \
                 # apply IN ORDER: role.sql -> kubectl apply db.json (wait applied) -> priv.sql -> secret.json",
                p = triple.project,
                e = triple.env,
            ),
            TierMoveStep::Restore { triple, cluster } => println!(
                "{n}. restore the dump into {triple}'s new database (on {cluster}):\n   \
                 wamn-host restore-project-env --org {org} --project {p} --env {e} --in-place --confirm \\\n     \
                 --database-url <superuser url to {cluster}> --dump-root {root} \\\n     \
                 --system-database-url $WAMN_SYSTEM_ADMIN_URL",
                p = triple.project,
                e = triple.env,
            ),
            TierMoveStep::FlipRegistry { tier, .. } => println!(
                "{n}. CUTOVER — flip the registry to {tier} (run only after all envs are restored):\n   \
                 wamn-host move-org-tier --org {org} --target-tier {tier} \\\n     \
                 --system-database-url $WAMN_SYSTEM_ADMIN_URL --flip"
            ),
        }
    }
    println!(
        "\nNear-zero-downtime alternative to the dump/restore window: a logical-replication \
         cutover (publication on the source, subscription on the new cluster, switch over when \
         caught up) — a follow-up; the scheduled window is the shipped path."
    );
}

fn tier_from_str(s: &str) -> anyhow::Result<Tier> {
    Tier::ALL
        .into_iter()
        .find(|t| t.as_str() == s)
        .ok_or_else(|| anyhow::anyhow!("not a tier: {s:?}"))
}

fn env_from_str(s: &str) -> anyhow::Result<Env> {
    Env::ALL
        .into_iter()
        .find(|e| e.as_str() == s)
        .ok_or_else(|| anyhow::anyhow!("not an env: {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_and_env_round_trip_the_registry_literals() {
        for t in Tier::ALL {
            assert_eq!(tier_from_str(t.as_str()).unwrap(), t);
        }
        assert!(tier_from_str("platinum").is_err());
        for e in Env::ALL {
            assert_eq!(env_from_str(e.as_str()).unwrap(), e);
        }
        assert!(env_from_str("staging").is_err());
    }

    /// The flip targets the SAME cluster pair the plan names (the single-source
    /// `cluster_name` via `Org::for_pair`) — so the flipped row, the provisioned
    /// clusters, and the plan all agree.
    #[test]
    fn flip_targets_the_pair_the_plan_names() {
        let org = Org::for_pair("acme", Tier::Standard);
        assert_eq!(org.prod_cluster.name, "acme-prod");
        assert_eq!(org.dev_cluster.name, "acme-dev");
    }
}

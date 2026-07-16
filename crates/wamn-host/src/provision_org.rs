//! The `provision-org` subcommand (wamn-q3n.6, generalized to the D18 model by
//! wamn-8df.3): render a **dedicated** org's CNPG `Cluster` set — one cluster per
//! recovery-domain owner env, each sized by its env policy — and record the org's
//! placement in the T1 control-plane registry.
//!
//! An imperative CLI (the `provision-project` precedent), run as a Job or from a
//! runbook. It:
//!
//! 1. builds the org's D18 [`Placement`](wamn_registry::Placement) (`pooled` on a
//!    shared `--pool`, or `dedicated`) and validates the org id + placement by
//!    running the one-org registry through `wamn-registry`'s validator;
//! 2. for a `dedicated` org, renders one CNPG `Cluster` CR per distinct
//!    recovery-domain owner across the env policies ([`wamn_provision::org`]),
//!    sized by each owner env's policy, and emits them (+ the WAL/PITR
//!    `ObjectStore` / `ScheduledBackup` CRs) as JSON `List`s — the runbook/Job
//!    `kubectl apply -f`s them and waits ready;
//! 3. records the org's placement row in `registry.orgs` in the T1 `wamn_system`
//!    DB (idempotent upsert, as the `wamn_system` owner) when a system-DB URL is
//!    given.
//!
//! Rendering the CRs and writing the registry row is **all** this tool does — it
//! does NOT apply the CRs (no K8s client; the runbook does) and does NOT create
//! per-project-env databases (the CNPG `Database` CRD path is wamn-q3n.7).
//!
//! **Cluster sizing (D18, cjv.21):** each cluster is sized by the env policy of its
//! recovery-domain owner (`instances`/`storage`/`cpu`/`memory`/`image`), and its
//! WAL/PITR backup (retention window + cadence) reads the same policy. The policies
//! come from `registry.env_policies` when a system-DB URL is given (so an operator's
//! added `canary` policy is honored), else the built-in `dev`/`prod` defaults.
//!
//! **Pooled orgs (wamn-q3n.9):** a `pooled` org shares the pre-contract pool
//! (`deploy/cnpg-cluster.yaml` `wamn-pg`), so it owns no clusters — there is
//! nothing to render. `--placement pooled` records **only** the `registry.orgs`
//! placement row; `.7` `provision-project-env` then reads that placement and
//! derives the pool cluster via [`cluster_of`](wamn_registry::cluster_of).

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use tokio_postgres::NoTls;

use wamn_registry::{EnvPolicy, Org, Registry, SCHEMA_VERSION};

use crate::env_policies::read_env_policies;

/// How `provision-org` places an org (the D18 [`Placement`](wamn_registry::Placement)).
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum PlacementArg {
    /// Placed on the shared `--pool` cluster (the pre-contract T3-style pool); owns
    /// no clusters, only its placement row is recorded. The RLS floor is
    /// load-bearing there.
    Pooled,
    /// Owns one cluster per recovery-domain owner env (`<org>-<owner>`), sized by
    /// each owner env's policy. Rendered + emitted here.
    Dedicated,
}

#[derive(Debug, Args)]
pub struct ProvisionOrgArgs {
    /// Org id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). Names the
    /// derived `<org>-<owner>` clusters; the reserved `wamn` prefix is rejected.
    #[arg(long)]
    pub org: String,

    /// Placement: `pooled` (shared `--pool`, record-only) or `dedicated` (owns
    /// per-recovery-domain clusters, rendered here).
    #[arg(long, value_enum)]
    pub placement: PlacementArg,

    /// The shared pool cluster a `pooled` org is placed on. Ignored for
    /// `dedicated`. Default: the shipped `wamn-pg` pool.
    #[arg(long, default_value = "wamn-pg")]
    pub pool: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`), where the org
    /// row is recorded and the env policies are read (for cluster sizing). Env
    /// `WAMN_SYSTEM_ADMIN_URL`. Omit to render/plan only (with default policies).
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Write the rendered `Cluster` CRs (a JSON `List`) here; `-` = stdout
    /// (default). Empty for a `pooled` org (no owned clusters).
    #[arg(long)]
    pub emit_clusters: Option<PathBuf>,

    /// Write the WAL/PITR `ObjectStore` CRs (a JSON `List`, wamn-e1g) here; `-` =
    /// stdout (default). Apply these **before** the clusters — the Barman plugin
    /// references them.
    #[arg(long)]
    pub emit_object_store: Option<PathBuf>,

    /// Write the WAL/PITR `ScheduledBackup` CRs (a JSON `List`, wamn-e1g) here;
    /// `-` = stdout (default). Apply these **after** the clusters exist.
    #[arg(long)]
    pub emit_scheduled_backup: Option<PathBuf>,
}

pub async fn run(args: ProvisionOrgArgs) -> anyhow::Result<()> {
    // A pooled org shares the `--pool`; a dedicated org owns per-recovery-domain
    // clusters derived from its placement + the env policies.
    let org = match args.placement {
        PlacementArg::Pooled => Org::pooled(&args.org, &args.pool),
        PlacementArg::Dedicated => Org::dedicated(&args.org),
    };

    // Validate the org id (slug / reserved-prefix) + placement by running the
    // one-org registry through the model's validator.
    let reg = Registry {
        schema_version: SCHEMA_VERSION.to_string(),
        env_policies: EnvPolicy::defaults(),
        orgs: vec![org.clone()],
        projects: Vec::new(),
        project_envs: Vec::new(),
    };
    reg.validate()
        .map_err(|issues| anyhow::anyhow!("invalid org: {}", fmt_issues(&issues)))?;

    // Connect to the system DB once (if given) — used both to read the env policies
    // (cluster sizing) and to record the org row.
    let client = match &args.system_database_url {
        Some(url) => {
            let (client, conn) = tokio_postgres::connect(url, NoTls)
                .await
                .context("system db connect")?;
            Some((client, tokio::spawn(conn)))
        }
        None => None,
    };

    match args.placement {
        // Pooled: no cluster set — the org shares the pool. Record placement only.
        PlacementArg::Pooled => {
            println!(
                "org {id:?} (pooled): placed on the shared pool {pool:?} (owns no clusters)",
                id = org.id,
                pool = args.pool,
            );
        }
        // Dedicated: render one cluster per recovery-domain owner, sized by the
        // owner env's policy, and emit the CRs to apply.
        PlacementArg::Dedicated => {
            let policies = match &client {
                Some((c, _)) => read_env_policies(c).await?,
                None => EnvPolicy::defaults(),
            };
            let set = wamn_provision::org::render_org_cluster_set(&org, &policies)
                .map_err(|e| anyhow::anyhow!("render org clusters: {e}"))?;
            let names: Vec<String> = set
                .clusters
                .iter()
                .map(|c| c["metadata"]["name"].as_str().unwrap_or("?").to_string())
                .collect();
            println!(
                "org {id:?} (dedicated): {n} cluster(s) [{names}], sized by env policy",
                id = org.id,
                n = set.clusters.len(),
                names = names.join(", "),
            );
            if !set.object_stores.is_empty() {
                println!(
                    "  WAL/PITR: {} backed cluster(s); apply the ObjectStore(s) before the clusters, the ScheduledBackup(s) after",
                    set.object_stores.len(),
                );
            }
            let emit_clusters = args.emit_clusters.unwrap_or_else(|| PathBuf::from("-"));
            let emit_os = args.emit_object_store.unwrap_or_else(|| PathBuf::from("-"));
            let emit_sb = args
                .emit_scheduled_backup
                .unwrap_or_else(|| PathBuf::from("-"));
            write_json(&emit_clusters, &k8s_list(&set.clusters)).context("emit Cluster CRs")?;
            write_json(&emit_os, &k8s_list(&set.object_stores)).context("emit ObjectStore CRs")?;
            write_json(&emit_sb, &k8s_list(&set.scheduled_backups))
                .context("emit ScheduledBackup CRs")?;
        }
    }

    // Record the org's placement row in the T1 registry (idempotent).
    match client {
        Some((c, conn_task)) => {
            let result = do_record_org(&c, &org).await;
            drop(c);
            let _ = conn_task.await;
            result?;
            println!("recorded org {:?} in registry.orgs (wamn_system)", org.id);
        }
        None => println!("(no --system-database-url: org not recorded)"),
    }

    Ok(())
}

/// Upsert the org's placement row into `registry.orgs`. Writes as the
/// `wamn_system` owner (the registry owner role — the wamn-q3n.3 apply pattern),
/// then runs the pure [`wamn_registry::sql::upsert_org_sql`] builder. Idempotent +
/// additive (`ON CONFLICT (id) DO UPDATE`; the shared-cluster guardrail).
async fn do_record_org(client: &tokio_postgres::Client, org: &Org) -> anyhow::Result<()> {
    client
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    let placement_kind = org.placement.kind_str();
    // The pool cluster is set only for a pooled org; NULL for a dedicated org.
    let pool = org.placement.pool();
    client
        .execute(
            wamn_registry::sql::upsert_org_sql(),
            &[&org.id, &placement_kind, &pool],
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

/// Wrap CRs in a Kubernetes `v1` `List` so `kubectl apply -f` accepts the whole
/// set from one file/stream. An empty items list is a valid, harmless no-op apply.
fn k8s_list(items: &[serde_json::Value]) -> serde_json::Value {
    serde_json::json!({
        "apiVersion": "v1",
        "kind": "List",
        "items": items,
    })
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
            env_policies: EnvPolicy::defaults(),
            orgs: vec![org],
            projects: Vec::new(),
            project_envs: Vec::new(),
        }
    }

    /// A pooled org places on the shared pool (record-only); a dedicated org owns
    /// per-recovery-domain clusters (rendered from the policies).
    #[test]
    fn pooled_records_only_dedicated_renders_clusters() {
        let pooled = Org::pooled("trialco", "wamn-pg");
        assert_eq!(pooled.placement.kind_str(), "pooled");
        assert_eq!(pooled.placement.pool(), Some("wamn-pg"));
        assert!(one_org_registry(pooled.clone()).validate().is_ok());
        // A pooled org owns no clusters (the render path errors — record-only).
        assert!(
            wamn_provision::org::render_org_cluster_set(&pooled, &EnvPolicy::defaults()).is_err()
        );

        let ded = Org::dedicated("acme");
        assert_eq!(ded.placement.kind_str(), "dedicated");
        let set =
            wamn_provision::org::render_org_cluster_set(&ded, &EnvPolicy::defaults()).unwrap();
        // Default policy set → two clusters (dev + prod), sized by policy.
        assert_eq!(set.clusters.len(), 2);
    }

    /// The render path emits the Cluster + WAL/PITR CRs wrapped in `List`s (wamn-e1g).
    #[test]
    fn render_path_emits_lists() {
        let set = wamn_provision::org::render_org_cluster_set(
            &Org::dedicated("acme"),
            &EnvPolicy::defaults(),
        )
        .unwrap();
        let clusters = k8s_list(&set.clusters);
        assert_eq!(clusters["kind"], "List");
        assert_eq!(clusters["items"][0]["kind"], "Cluster");
        assert_eq!(set.object_stores.len(), 1, "prod is backed");
        let stores = k8s_list(&set.object_stores);
        assert_eq!(stores["items"][0]["kind"], "ObjectStore");
        // An empty List (a pooled org has no clusters) is a harmless no-op apply.
        assert_eq!(k8s_list(&[])["items"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn reserved_org_id_is_rejected() {
        // The `provision-org` id path runs through the same validator the registry
        // uses, so a reserved-prefix org id is refused before any effect.
        assert!(
            one_org_registry(Org::dedicated("wamn-corp"))
                .validate()
                .is_err()
        );
        assert!(
            one_org_registry(Org::dedicated("Acme")).validate().is_err(),
            "an uppercase id is not a slug"
        );
    }
}

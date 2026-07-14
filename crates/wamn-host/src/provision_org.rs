//! The `provision-org` subcommand (wamn-q3n.6, extended for T4 by wamn-q3n.14):
//! render a paying org's CNPG `Cluster` SET (`<org>-prod` HA + `<org>-dev`
//! hibernation-managed, plus a dedicated `<org>-canary` for a T4 `dedicated` org)
//! and record the org in the T1 control-plane registry.
//!
//! An imperative CLI (the `provision-project` precedent), run as a Job or from a
//! runbook. It:
//!
//! 1. builds the org's placement — `<org>-prod` / (dedicated) `<org>-canary` /
//!    `<org>-dev` cluster refs via [`wamn_registry::cluster_name`] /
//!    [`wamn_registry::canary_cluster_name`] — and validates the org id + names by
//!    running the one-org registry through `wamn-registry`'s validator;
//! 2. renders the CNPG `Cluster` CRs ([`wamn_provision::org`]) — two, or three for
//!    a dedicated org — and emits them as JSON — the runbook/Job `kubectl apply
//!    -f`s them and waits ready;
//! 3. records the org row in `registry.orgs` in the T1 `wamn_system` DB
//!    (idempotent upsert, as the `wamn_system` owner) when a system-DB URL is
//!    given.
//!
//! Rendering the CRs and writing the registry row is **all** this tool does — it
//! does NOT apply the CRs (no K8s client; the runbook does, like
//! `provision-project`'s Secret) and does NOT create per-project-env databases
//! (the CNPG `Database` CRD + `.spec.managed.roles` path is wamn-q3n.7).
//!
//! **WAL/PITR (wamn-e1g):** the paying-tier clusters that get continuous backup
//! (`prod`, and a dedicated `canary`) carry a Barman Cloud plugin ref, and this
//! tool also emits their `ObjectStore` (`--emit-object-store`) and
//! `ScheduledBackup` (`--emit-scheduled-backup`) CRs. The runbook applies the
//! ObjectStore **before** the cluster (the plugin references it) and the
//! ScheduledBackup **after**.
//!
//! **T3 trials orgs (wamn-q3n.9):** a `trials` org shares the pre-contract pool
//! (`deploy/cnpg-cluster.yaml` `wamn-pg`), so it has no dedicated cluster pair —
//! there is nothing to render. `--tier trials` builds the org via
//! [`Org::for_pool`](wamn_registry::Org::for_pool) (both cluster refs point at
//! `--pool`) and records **only** the `registry.orgs` placement row; `.7`
//! `provision-project-env` then reads that placement and provisions the org's
//! project-env databases onto the pool via `env.side()` (no manual `--cluster`).

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use tokio_postgres::NoTls;

use wamn_registry::{Org, Registry, SCHEMA_VERSION, Tier};

/// The hosting tier `provision-org` places an org on. The paying tiers
/// (`standard` / `dedicated`) get a dedicated `<org>-prod` / `<org>-dev` cluster
/// pair rendered here; a `trials` org shares the pre-contract pool and only has
/// its placement row recorded (wamn-q3n.9).
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TierArg {
    /// T3 trials (pre-contract): placed on the shared `--pool` cluster (no pair;
    /// the RLS floor is load-bearing there).
    Trials,
    /// T2 standard: `<org>-prod` HA (2 instances) + `<org>-dev` single instance.
    Standard,
    /// T4 dedicated (regulated): `<org>-prod` HA (3 instances) + `<org>-canary` HA
    /// (canary its own recovery domain) + `<org>-dev` single instance.
    Dedicated,
}

impl From<TierArg> for Tier {
    fn from(t: TierArg) -> Tier {
        match t {
            TierArg::Trials => Tier::Trials,
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

    /// Hosting tier: `trials` (T3, shared pool), `standard` (T2), or `dedicated`
    /// (T4). A `standard`/`dedicated` org gets a dedicated cluster pair rendered
    /// here; a `trials` org is placed on `--pool` and only recorded.
    #[arg(long, value_enum)]
    pub tier: TierArg,

    /// The shared pool cluster a `trials` org is placed on (both its cluster refs
    /// point at it). Ignored for `standard`/`dedicated` (they get a dedicated
    /// pair). Default: the shipped `wamn-pg` T3 pool.
    #[arg(long, default_value = "wamn-pg")]
    pub pool: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`), where the org
    /// row is recorded. Env `WAMN_SYSTEM_ADMIN_URL`. Omit to render/plan only.
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Write the `<org>-prod` Cluster CR (JSON) here; `-` = stdout (default).
    #[arg(long)]
    pub emit_prod: Option<PathBuf>,

    /// Write the `<org>-canary` Cluster CR (JSON) here; `-` = stdout (default).
    /// Only rendered for a `dedicated` (T4) org (canary its own cluster); ignored
    /// otherwise.
    #[arg(long)]
    pub emit_canary: Option<PathBuf>,

    /// Write the `<org>-dev` Cluster CR (JSON) here; `-` = stdout (default).
    #[arg(long)]
    pub emit_dev: Option<PathBuf>,

    /// Write the WAL/PITR `ObjectStore` CRs (a JSON `List`, wamn-e1g) here; `-` =
    /// stdout (default). Apply these **before** the clusters — the Barman plugin
    /// references them. Empty for a `trials` org (no dedicated clusters).
    #[arg(long)]
    pub emit_object_store: Option<PathBuf>,

    /// Write the WAL/PITR `ScheduledBackup` CRs (a JSON `List`, wamn-e1g) here;
    /// `-` = stdout (default). Apply these **after** the clusters exist.
    #[arg(long)]
    pub emit_scheduled_backup: Option<PathBuf>,
}

pub async fn run(args: ProvisionOrgArgs) -> anyhow::Result<()> {
    let tier: Tier = args.tier.into();
    // A trials org shares the `--pool` cluster (both refs); a paying org gets its
    // own `<org>-prod` / `<org>-dev` pair.
    let org = org_for(tier, &args.org, &args.pool);

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

    match tier {
        // T3: no cluster pair to render — the trials org shares the pool. Record
        // its placement only; `.7` provision-project-env creates the DBs on the pool.
        Tier::Trials => {
            println!(
                "org {id:?} (tier trials): placed on the shared pool {pool:?} (no dedicated cluster pair)",
                id = org.id,
                pool = org.prod_cluster.name,
            );
        }
        // T2/T4: render the dedicated cluster set and emit the CRs to apply. A
        // dedicated (T4) org also renders `<org>-canary` (canary its own cluster).
        Tier::Standard | Tier::Dedicated => {
            let set = wamn_provision::org::render_org_cluster_set(&org)
                .map_err(|e| anyhow::anyhow!("render org clusters: {e}"))?;
            let canary_note = match &org.canary_cluster {
                Some(c) => format!(" + {} (HA, dedicated canary)", c.name),
                None => String::new(),
            };
            println!(
                "org {id:?} (tier {tier}): {prod} ({pi} instances, HA){canary_note} + {dev} (1 instance, hibernation-managed)",
                id = org.id,
                prod = org.prod_cluster.name,
                dev = org.dev_cluster.name,
                pi = set.prod["spec"]["instances"],
            );
            if !set.object_stores.is_empty() {
                println!(
                    "  WAL/PITR: {} backed cluster(s) (retention {}); apply the ObjectStore(s) before the clusters, the ScheduledBackup(s) after",
                    set.object_stores.len(),
                    wamn_provision::wal_retention(org.tier),
                );
            }
            // Emit the CRs (default: to stdout) so the runbook kubectl-applies them.
            let emit_prod = args.emit_prod.unwrap_or_else(|| PathBuf::from("-"));
            let emit_dev = args.emit_dev.unwrap_or_else(|| PathBuf::from("-"));
            write_json(&emit_prod, &set.prod).context("emit prod cluster CR")?;
            if let Some(canary_cr) = &set.canary {
                let emit_canary = args
                    .emit_canary
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("-"));
                write_json(&emit_canary, canary_cr).context("emit canary cluster CR")?;
            }
            write_json(&emit_dev, &set.dev).context("emit dev cluster CR")?;
            // WAL/PITR backup CRs (wamn-e1g): ObjectStore(s) before the clusters,
            // ScheduledBackup(s) after (the runbook orders the applies).
            let emit_os = args.emit_object_store.unwrap_or_else(|| PathBuf::from("-"));
            let emit_sb = args
                .emit_scheduled_backup
                .unwrap_or_else(|| PathBuf::from("-"));
            write_json(&emit_os, &k8s_list(&set.object_stores)).context("emit ObjectStore CRs")?;
            write_json(&emit_sb, &k8s_list(&set.scheduled_backups))
                .context("emit ScheduledBackup CRs")?;
        }
    }

    // Record the org in the T1 registry (idempotent), when a system-DB URL is given.
    match &args.system_database_url {
        Some(url) => {
            record_org(url, &org).await?;
            println!("recorded org {:?} in registry.orgs (wamn_system)", org.id);
        }
        None => println!("(no --system-database-url: org not recorded)"),
    }

    Ok(())
}

/// Build the org placement for a tier: a `trials` org shares the `pool` (both
/// cluster refs, via [`Org::for_pool`]); a paying (`standard`/`dedicated`) org
/// gets its own `<org>-prod` / `<org>-dev` pair (via [`Org::for_pair`]). The one
/// decision that separates the T3 record-only path from the T2/T4 render path.
fn org_for(tier: Tier, id: &str, pool: &str) -> Org {
    match tier {
        Tier::Trials => Org::for_pool(id, pool),
        Tier::Standard | Tier::Dedicated => Org::for_pair(id, tier),
    }
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
    // The canary cluster is set only for a dedicated (T4) org; NULL otherwise.
    let canary = org.canary_cluster.as_ref().map(|c| c.name.as_str());
    client
        .execute(
            wamn_registry::sql::upsert_org_sql(),
            &[
                &org.id,
                &tier,
                &org.prod_cluster.name,
                &canary,
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

/// Wrap CRs in a Kubernetes `v1` `List` so `kubectl apply -f` accepts the whole
/// set from one file/stream (used for the WAL/PITR ObjectStore + ScheduledBackup
/// CRs). An empty items list is a valid, harmless no-op apply.
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
            orgs: vec![org],
            projects: Vec::new(),
            project_envs: Vec::new(),
        }
    }

    #[test]
    fn tier_arg_maps_to_the_registry_tier() {
        assert_eq!(Tier::from(TierArg::Trials), Tier::Trials);
        assert_eq!(Tier::from(TierArg::Standard), Tier::Standard);
        assert_eq!(Tier::from(TierArg::Dedicated), Tier::Dedicated);
    }

    /// The one decision separating the T3 record-only path from the T2/T4 render
    /// path: a trials tier places the org on the shared pool (both refs), a paying
    /// tier on its own `<org>-prod` / `<org>-dev` pair.
    #[test]
    fn org_for_places_trials_on_the_pool_and_paying_on_a_pair() {
        // Trials → both refs = the pool (shared, no dedicated pair).
        let t = org_for(Tier::Trials, "trialco", "wamn-pg");
        assert_eq!(t.tier, Tier::Trials);
        assert_eq!(t.prod_cluster.name, "wamn-pg");
        assert_eq!(t.dev_cluster.name, "wamn-pg");
        assert!(one_org_registry(t).validate().is_ok());
        // Standard → a dedicated `<org>-prod` / `<org>-dev` pair.
        let s = org_for(Tier::Standard, "acme", "wamn-pg");
        assert_eq!(s.tier, Tier::Standard);
        assert_eq!(s.prod_cluster.name, "acme-prod");
        assert_eq!(s.dev_cluster.name, "acme-dev");
    }

    /// A trials org has no dedicated set to render — the render path is only
    /// reached for paying tiers, and rendering a trials org would error (it is the
    /// record-only path). This pins that the pool placement carries no cluster CRs.
    #[test]
    fn a_trials_org_renders_no_cluster_pair() {
        let t = org_for(Tier::Trials, "trialco", "wamn-pg");
        assert!(wamn_provision::org::render_org_cluster_set(&t).is_err());
    }

    #[test]
    fn for_pair_org_validates_and_names_its_clusters() {
        let org = Org::for_pair("acme", TierArg::Standard.into());
        assert_eq!(org.prod_cluster.name, "acme-prod");
        assert_eq!(org.dev_cluster.name, "acme-dev");
        assert!(one_org_registry(org).validate().is_ok());
    }

    /// A dedicated (T4) org's placement gives canary its own cluster, and
    /// `render_org_cluster_set` renders the third CR — the render path
    /// `provision-org --tier dedicated` emits (wamn-q3n.14).
    #[test]
    fn dedicated_org_places_and_renders_a_canary_cluster() {
        let org = org_for(Tier::Dedicated, "acme", "wamn-pg");
        assert_eq!(
            org.canary_cluster.as_ref().map(|c| c.name.as_str()),
            Some("acme-canary")
        );
        assert!(one_org_registry(org.clone()).validate().is_ok());
        let set = wamn_provision::org::render_org_cluster_set(&org).unwrap();
        assert_eq!(
            set.canary.expect("dedicated renders a canary CR")["metadata"]["name"],
            "acme-canary"
        );
    }

    /// The render path emits WAL/PITR backup CRs for the backed clusters, wrapped
    /// in a `List` (wamn-e1g). A standard org backs prod only; a dedicated org
    /// backs prod + canary.
    #[test]
    fn render_path_emits_backup_crs_wrapped_in_a_list() {
        let set =
            wamn_provision::org::render_org_cluster_set(&Org::for_pair("acme", Tier::Standard))
                .unwrap();
        assert_eq!(set.object_stores.len(), 1);
        let list = k8s_list(&set.object_stores);
        assert_eq!(list["kind"], "List");
        assert_eq!(list["items"][0]["kind"], "ObjectStore");
        // A trials org has no dedicated clusters → an empty (harmless) List.
        assert_eq!(k8s_list(&[])["items"].as_array().unwrap().len(), 0);

        let ded =
            wamn_provision::org::render_org_cluster_set(&Org::for_pair("acme", Tier::Dedicated))
                .unwrap();
        assert_eq!(ded.object_stores.len(), 2, "prod + canary");
        assert_eq!(ded.scheduled_backups.len(), 2);
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

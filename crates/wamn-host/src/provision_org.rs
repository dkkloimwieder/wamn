//! The `provision-org` subcommand (wamn-q3n.6; D18 model by wamn-8df.3;
//! template-driven + org-scoped policies by wamn-8df.4): stamp an org from a
//! named [`Template`] — its placement **and** its own env-policy set in one step
//! — then render its CNPG `Cluster` set (one cluster per recovery-domain owner,
//! each sized by the org's policy for that env).
//!
//! An imperative CLI (the `provision-project` precedent), run as a Job or from a
//! runbook. It:
//!
//! 1. looks up the `--template` preset (`trials` / `standard` / `dedicated` —
//!    the `Tier` successor) and builds the org's [`Placement`](wamn_registry::Placement)
//!    (`trials` places on the shared `--pool`); validates the org id, placement,
//!    and stamped policy set by running the one-org registry through
//!    `wamn-registry`'s validator;
//! 2. records the org in the T1 `wamn_system` DB (when a system-DB URL is
//!    given): the placement row (idempotent upsert) plus the template's policy
//!    rows — **insert-if-absent**, so re-provisioning keeps the org's per-env
//!    customizations and a richer template only adds missing envs — in one
//!    transaction, as the `wamn_system` owner;
//! 3. for a dedicated org, renders one CNPG `Cluster` CR per distinct
//!    recovery-domain owner across the org's (post-stamp) policies
//!    ([`wamn_provision::org`]), sized by each owner env's policy, and emits
//!    them (+ the WAL/PITR `ObjectStore` / `ScheduledBackup` CRs) as JSON
//!    `List`s — the runbook/Job `kubectl apply -f`s them and waits ready.
//!
//! Rendering the CRs and writing the registry rows is **all** this tool does —
//! it does NOT apply the CRs (no K8s client; the runbook does) and does NOT
//! create per-project-env databases (the CNPG `Database` CRD path is wamn-q3n.7).
//!
//! **Cluster sizing (D18, cjv.21):** each cluster is sized by the env policy of
//! its recovery-domain owner (`instances`/`storage`/`cpu`/`memory`/`image`), and
//! its WAL/PITR backup (retention window + cadence) reads the same policy. The
//! policies are the ORG'S OWN rows (read back after stamping) when a system-DB
//! URL is given — so a customized org re-renders with its customizations — else
//! the template's.
//!
//! **Pooled orgs (wamn-q3n.9):** a `trials` org shares the pre-contract pool
//! (`deploy/infra/cnpg-cluster.yaml` `wamn-pg`), so it owns no clusters — there is
//! nothing to render; only its registry rows are recorded. `.7`
//! `provision-project-env` then reads that placement and derives the pool
//! cluster via [`cluster_of`](wamn_registry::cluster_of).

use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, ValueEnum};
use tokio_postgres::NoTls;

use wamn_registry::{EnvPolicy, Org, OrgEnvPolicy, Registry, SCHEMA_VERSION, Template};

use crate::env_policies::read_env_policies;

/// The named org preset `provision-org` stamps (the `Tier` successor —
/// [`wamn_registry::Template`]).
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TemplateArg {
    /// Pre-contract: placed on the shared `--pool` cluster (owns no clusters;
    /// the RLS floor is load-bearing there); stamps `dev` + `prod`.
    Trials,
    /// Standard paying tier: owns per-recovery-domain clusters; stamps `dev` /
    /// `prod` (own) + `canary` sharing prod's recovery domain (T2).
    Standard,
    /// Regulated tier: like standard, but `canary` owns its recovery domain — a
    /// third cluster (T4).
    Dedicated,
}

impl TemplateArg {
    fn template(self) -> Template {
        match self {
            TemplateArg::Trials => Template::trials(),
            TemplateArg::Standard => Template::standard(),
            TemplateArg::Dedicated => Template::dedicated(),
        }
    }
}

#[derive(Debug, Args)]
pub struct ProvisionOrgArgs {
    /// Org id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). Names the
    /// derived `<org>-<owner>` clusters; the reserved `wamn` prefix is rejected.
    #[arg(long)]
    pub org: String,

    /// The template preset to stamp: `trials` (pooled, record-only), `standard`
    /// (dedicated, canary shared-with prod), or `dedicated` (canary own).
    #[arg(long, value_enum)]
    pub template: TemplateArg,

    /// The shared pool cluster a `trials` org is placed on. Ignored for
    /// dedicated templates. Default: the shipped `wamn-pg` pool.
    #[arg(long, default_value = "wamn-pg")]
    pub pool: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`), where the org
    /// and its policy rows are recorded and read back (for cluster sizing). Env
    /// `WAMN_SYSTEM_ADMIN_URL`. Omit to render/plan only (with template policies).
    #[arg(long, env = "WAMN_SYSTEM_ADMIN_URL")]
    pub system_database_url: Option<String>,

    /// Write the rendered `Cluster` CRs (a JSON `List`) here; `-` = stdout
    /// (default). Empty for a pooled org (no owned clusters).
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
    // The template stamps the placement + the org's env-policy set in one step.
    let template = args.template.template();
    let (org, stamped) = template.stamp(&args.org, &args.pool);

    // Validate the org id (slug / reserved-prefix), placement, and the stamped
    // policy set by running the one-org registry through the model's validator.
    let reg = Registry {
        schema_version: SCHEMA_VERSION.to_string(),
        env_policies: stamped.clone(),
        orgs: vec![org.clone()],
        projects: Vec::new(),
        project_envs: Vec::new(),
    };
    reg.validate()
        .map_err(|issues| anyhow::anyhow!("invalid org: {}", fmt_issues(&issues)))?;

    // Connect to the system DB once (if given) — used to record the org + stamp
    // its policies, then read the org's (possibly customized) set back for
    // cluster sizing.
    let client = match &args.system_database_url {
        Some(url) => {
            let (client, conn) = tokio_postgres::connect(url, NoTls)
                .await
                .context("system db connect")?;
            Some((client, tokio::spawn(conn)))
        }
        None => None,
    };

    // Record FIRST (one txn: org row + policy stamps), so the render below reads
    // the org's post-stamp truth — existing customizations kept (insert-if-
    // absent), missing template envs added.
    let policies = match &client {
        Some((c, _)) => {
            record_org(c, &org, &stamped).await?;
            println!(
                "recorded org {id:?} (template {tpl:?}) in registry.orgs + {n} env \
                 policy row(s) stamped insert-if-absent (wamn_system)",
                id = org.id,
                tpl = template.name,
                n = stamped.len(),
            );
            read_env_policies(c, &org.id).await?
        }
        None => {
            println!("(no --system-database-url: org not recorded; template policies used)");
            template.policies.clone()
        }
    };

    match &org.placement {
        // Pooled: no cluster set — the org shares the pool.
        wamn_registry::Placement::Pooled { pool } => {
            println!(
                "org {id:?} (template {tpl:?}, pooled): placed on the shared pool {pool:?} \
                 (owns no clusters)",
                id = org.id,
                tpl = template.name,
            );
        }
        // Dedicated: render one cluster per recovery-domain owner, sized by the
        // org's policy for the owner env, and emit the CRs to apply.
        wamn_registry::Placement::Dedicated => {
            let set = wamn_provision::org::render_org_cluster_set(&org, &policies)
                .map_err(|e| anyhow::anyhow!("render org clusters: {e}"))?;
            let names: Vec<String> = set
                .clusters
                .iter()
                .map(|c| c["metadata"]["name"].as_str().unwrap_or("?").to_string())
                .collect();
            println!(
                "org {id:?} (template {tpl:?}, dedicated): {n} cluster(s) [{names}], \
                 sized by the org's env policies",
                id = org.id,
                tpl = template.name,
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

    if let Some((c, conn_task)) = client {
        drop(c);
        let _ = conn_task.await;
    }

    Ok(())
}

/// Record the org (placement upsert) and stamp its template policy rows
/// (insert-if-absent) in ONE transaction, as the `wamn_system` owner (the
/// registry owner role — the wamn-q3n.3 apply pattern). A crash mid-stamp rolls
/// the whole record back; re-running is idempotent (the shared-cluster
/// guardrail: refresh placement, never clobber a customized policy).
async fn record_org(
    client: &tokio_postgres::Client,
    org: &Org,
    stamped: &[OrgEnvPolicy],
) -> anyhow::Result<()> {
    client
        .batch_execute("SET ROLE wamn_system; BEGIN")
        .await
        .context("SET ROLE wamn_system + BEGIN")?;
    let result = record_org_rows(client, org, stamped).await;
    match result {
        Ok(()) => client.batch_execute("COMMIT").await.context("COMMIT")?,
        Err(e) => {
            let _ = client.batch_execute("ROLLBACK").await;
            return Err(e);
        }
    }
    Ok(())
}

async fn record_org_rows(
    client: &tokio_postgres::Client,
    org: &Org,
    stamped: &[OrgEnvPolicy],
) -> anyhow::Result<()> {
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
    for row in stamped {
        let p: &EnvPolicy = &row.policy;
        let name = p.name.as_str();
        let recovery = serde_json::to_string(&p.recovery_domain).context("recovery json")?;
        client
            .execute(
                wamn_registry::sql::stamp_env_policy_sql(),
                &[
                    &row.org,
                    &name,
                    &recovery,
                    &p.promotion_rank,
                    &p.instances,
                    &p.storage,
                    &p.cpu,
                    &p.memory,
                    &p.image,
                    &p.backup_cadence,
                    &p.wal_retention,
                    &p.hibernation,
                ],
            )
            .await
            .with_context(|| format!("stamp env policy {name:?}"))?;
    }
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

    fn one_org_registry(template: &Template, org_id: &str) -> Registry {
        let (org, stamped) = template.stamp(org_id, "wamn-pg");
        Registry {
            schema_version: SCHEMA_VERSION.to_string(),
            env_policies: stamped,
            orgs: vec![org],
            projects: Vec::new(),
            project_envs: Vec::new(),
        }
    }

    /// A trials org places on the shared pool (record-only); the dedicated
    /// templates own per-recovery-domain clusters rendered from their policies —
    /// `standard`'s canary collapses onto prod (2 clusters), `dedicated`'s canary
    /// owns its domain (3 clusters). The one-field template difference is the
    /// whole T2/T4 distinction.
    #[test]
    fn templates_drive_placement_and_cluster_shape() {
        let (pooled, _) = Template::trials().stamp("trialco", "wamn-pg");
        assert_eq!(pooled.placement.kind_str(), "pooled");
        assert_eq!(pooled.placement.pool(), Some("wamn-pg"));
        assert!(
            one_org_registry(&Template::trials(), "trialco")
                .validate()
                .is_ok()
        );
        // A pooled org owns no clusters (the render path errors — record-only).
        assert!(
            wamn_provision::org::render_org_cluster_set(&pooled, &Template::trials().policies)
                .is_err()
        );

        let (std_org, _) = Template::standard().stamp("acme", "wamn-pg");
        assert_eq!(std_org.placement.kind_str(), "dedicated");
        let set =
            wamn_provision::org::render_org_cluster_set(&std_org, &Template::standard().policies)
                .unwrap();
        assert_eq!(set.clusters.len(), 2, "standard: canary shares prod (T2)");

        let (ded_org, _) = Template::dedicated().stamp("bigco", "wamn-pg");
        let set =
            wamn_provision::org::render_org_cluster_set(&ded_org, &Template::dedicated().policies)
                .unwrap();
        assert_eq!(
            set.clusters.len(),
            3,
            "dedicated: canary owns its domain (T4)"
        );
        assert!(
            set.clusters
                .iter()
                .any(|c| c["metadata"]["name"] == "bigco-canary")
        );
    }

    /// Every shipped template's one-org stamp validates (placement + policy set
    /// are self-consistent), and the TemplateArg CLI values map onto them.
    #[test]
    fn every_template_arg_stamps_a_valid_org() {
        for (arg, name) in [
            (TemplateArg::Trials, "trials"),
            (TemplateArg::Standard, "standard"),
            (TemplateArg::Dedicated, "dedicated"),
        ] {
            let t = arg.template();
            assert_eq!(t.name, name);
            let reg = one_org_registry(&t, "acme");
            assert!(reg.validate().is_ok(), "{name}: {:?}", reg.issues());
        }
    }

    /// The render path emits the Cluster + WAL/PITR CRs wrapped in `List`s (wamn-e1g).
    #[test]
    fn render_path_emits_lists() {
        let (org, _) = Template::standard().stamp("acme", "wamn-pg");
        let set = wamn_provision::org::render_org_cluster_set(&org, &Template::standard().policies)
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
            one_org_registry(&Template::standard(), "wamn-corp")
                .validate()
                .is_err()
        );
        assert!(
            one_org_registry(&Template::standard(), "Acme")
                .validate()
                .is_err(),
            "an uppercase id is not a slug"
        );
    }
}

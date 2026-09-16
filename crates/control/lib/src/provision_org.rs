//! The `provision-org` subcommand (wamn-q3n.6; D18 model by wamn-8df.3;
//! template-driven + org-scoped policies by wamn-8df.4): stamp an org from a
//! named [`Template`] — its placement **and** its own env-policy set in one step
//! — then render its CNPG `Cluster` set (one cluster per recovery-domain owner,
//! each sized by the org's policy for that env).
//!
//! An imperative CLI, run as a Job or from a runbook. It:
//!
//! 1. looks up the `--template` preset (`trials` / `standard` / `dedicated` —
//!    the `Tier` successor) and builds the org's [`Placement`](wamn_control_registry::Placement)
//!    (`trials` places on the shared `--pool`); validates the org id, placement,
//!    and stamped policy set by running the one-org registry through
//!    `wamn-control-registry`'s validator;
//! 2. records the org in the T1 `wamn_system` DB (when a system-DB URL is
//!    given): the placement row (idempotent upsert) plus the template's policy
//!    rows — **insert-if-absent**, so re-provisioning keeps the org's per-env
//!    customizations and a richer template only adds missing envs — in one
//!    transaction, as the `wamn_system` owner;
//! 3. for a dedicated org, renders one CNPG `Cluster` CR per distinct
//!    recovery-domain owner across the org's (post-stamp) policies
//!    ([`wamn_control_provision::org`]), sized by each owner env's policy, and emits
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
//! cluster via [`cluster_of`](wamn_control_registry::cluster_of).

use anyhow::Context as _;
use tokio_postgres::NoTls;

use crate::env_policies::{ensure_env_policy_durability_schema, read_env_policies};
use wamn_control_provision::org::OrgClusters;
use wamn_control_registry::{EnvPolicy, Org, OrgEnvPolicy, Registry, SCHEMA_VERSION, Template};

/// Inputs that name one org, the preset to stamp it from, and where to record it.
#[derive(Debug)]
pub struct ProvisionOrgRequest {
    /// Org id: a lowercase slug `[a-z0-9-]` (start/end alphanumeric). Names the
    /// derived `<org>-<owner>` clusters; the reserved `wamn` prefix is rejected.
    pub org: String,

    /// The preset to stamp: its placement shape and its env-policy set.
    pub template: Template,

    /// The shared pool cluster a pooled org is placed on. Ignored for a
    /// dedicated template.
    pub pool: String,

    /// Superuser Postgres URL to the T1 system DB (`wamn_system`), where the org
    /// and its policy rows are recorded and read back (for cluster sizing).
    /// Absent to render or plan only, with the template's policies.
    pub system_database_url: Option<String>,
}

/// What one org provisioning run recorded and rendered.
#[derive(Debug)]
pub struct ProvisionedOrg {
    /// The org as the template stamped it, placement included.
    pub org: Org,

    /// The stamped preset's name.
    pub template_name: &'static str,

    /// How many policy rows were stamped, or absent when no system database URL
    /// was given and the org was not recorded.
    pub stamped_policies: Option<usize>,

    /// The rendered cluster set of a dedicated org; absent for a pooled org,
    /// which owns no clusters.
    pub clusters: Option<OrgClusters>,
}

/// Stamp one org from its template, record it, and render the clusters it owns.
pub async fn provision_org(request: ProvisionOrgRequest) -> anyhow::Result<ProvisionedOrg> {
    // The template stamps the placement + the org's env-policy set in one step.
    let template = request.template;
    let (org, stamped) = template.stamp(&request.org, &request.pool);

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
    let client = match &request.system_database_url {
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
    let (policies, stamped_policies) = match &client {
        Some((c, _)) => {
            record_org(c, &org, &stamped).await?;
            (read_env_policies(c, &org.id).await?, Some(stamped.len()))
        }
        None => (template.policies.clone(), None),
    };

    let clusters = match &org.placement {
        // Pooled: no cluster set — the org shares the pool.
        wamn_control_registry::Placement::Pooled { .. } => None,
        // Dedicated: render one cluster per recovery-domain owner, sized by the
        // org's policy for the owner env.
        wamn_control_registry::Placement::Dedicated => Some(
            wamn_control_provision::org::render_org_cluster_set(&org, &policies).map_err(|e| {
                // The org row is already written here, and the caller prints
                // only after this function returns, so the error is the one
                // place that can still say the org exists.
                if stamped_policies.is_some() {
                    anyhow::anyhow!(
                        "recorded org {} in the registry, but rendering its clusters failed: {e}",
                        org.id
                    )
                } else {
                    anyhow::anyhow!("render org clusters: {e}")
                }
            })?,
        ),
    };

    if let Some((c, conn_task)) = client {
        drop(c);
        let _ = conn_task.await;
    }

    Ok(ProvisionedOrg {
        org,
        template_name: template.name,
        stamped_policies,
        clusters,
    })
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
        .batch_execute("SET ROLE wamn_system")
        .await
        .context("SET ROLE wamn_system")?;
    ensure_env_policy_durability_schema(client).await?;
    client.batch_execute("BEGIN").await.context("BEGIN")?;
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
            wamn_control_registry::sql::upsert_org_sql(),
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
                wamn_control_registry::sql::stamp_env_policy_sql(),
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
                    &p.durability_class.as_sql(),
                ],
            )
            .await
            .with_context(|| format!("stamp env policy {name:?}"))?;
    }
    Ok(())
}

fn fmt_issues(issues: &[wamn_control_registry::Issue]) -> String {
    issues
        .iter()
        .map(std::string::ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
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
            wamn_control_provision::org::render_org_cluster_set(
                &pooled,
                &Template::trials().policies
            )
            .is_err()
        );

        let (std_org, _) = Template::standard().stamp("acme", "wamn-pg");
        assert_eq!(std_org.placement.kind_str(), "dedicated");
        let set = wamn_control_provision::org::render_org_cluster_set(
            &std_org,
            &Template::standard().policies,
        )
        .unwrap();
        assert_eq!(set.clusters.len(), 2, "standard: canary shares prod (T2)");

        let (ded_org, _) = Template::dedicated().stamp("bigco", "wamn-pg");
        let set = wamn_control_provision::org::render_org_cluster_set(
            &ded_org,
            &Template::dedicated().policies,
        )
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

    /// Every shipped template's one-org stamp validates: its placement and its
    /// policy set are self-consistent.
    #[test]
    fn every_template_stamps_a_valid_org() {
        for name in Template::NAMES {
            let t = Template::by_name(name).expect("a shipped preset");
            assert_eq!(t.name, name);
            let reg = one_org_registry(&t, "acme");
            assert!(reg.validate().is_ok(), "{name}: {:?}", reg.issues());
        }
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

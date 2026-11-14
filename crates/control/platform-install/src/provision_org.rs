//! Organization registration and Helm chart inputs.

pub use crate::credentials::provision_control_role;

pub(super) mod plan;

use anyhow::Context as _;

use wamn_release_control::env_policies::env_policy_from_row;

use wamn_control_provision::{EnvPolicy, Org, OrgEnvPolicy, Template};

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

    /// Where to record the org. Without a connection, return the template values.
    pub record: Option<OrgRecord>,
}

/// The system database that stores the organization and its policies.
#[derive(Debug)]
pub struct OrgRecord {
    /// Superuser Postgres URL to the T1 system DB (`wamn_system`), where the org
    /// and its policy rows are recorded and read back (for cluster sizing).
    pub system_database_url: String,
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

    /// Values for the org Helm chart.
    pub values: serde_json::Value,
}

/// Stamp one org from its template, record it, and render the clusters it owns.
pub async fn provision_org(request: ProvisionOrgRequest) -> anyhow::Result<ProvisionedOrg> {
    // The template stamps the placement + the org's env-policy set in one step.
    let template = request.template;
    let (org, stamped) = plan::stamped_org(&template, &request.org, &request.pool)?;

    // Connect to the system DB once (if given) — used to record the org + stamp
    // its policies, then read the org's (possibly customized) set back for
    // cluster sizing.
    let mut client = match &request.record {
        Some(record) => {
            let client = wamn_postgres::connect(&record.system_database_url)
                .await
                .context("system db connect")?;
            Some(client)
        }
        None => None,
    };

    // Record FIRST (one txn: org row + policy stamps), so the render below reads
    // the org's post-stamp truth — existing customizations kept (insert-if-
    // absent), missing template envs added.
    let (policies, stamped_policies) = match &mut client {
        Some(c) => {
            record_org(c, &org, &stamped).await?;
            (read_env_policies(c, &org.id).await?, Some(stamped.len()))
        }
        None => (template.policies.clone(), None),
    };

    let mut values = crate::org::values(&org, &policies)?;
    if request.record.is_some() && org.placement == wamn_control_provision::Placement::Dedicated {
        crate::org::apply(
            &request
                .record
                .as_ref()
                .context("org registration connection")?
                .system_database_url,
            &org.id,
            &mut values,
        )
        .await?;
    }

    drop(client);

    Ok(ProvisionedOrg {
        org,
        template_name: template.name,
        stamped_policies,
        values,
    })
}

/// Record the org (placement upsert) and stamp its template policy rows
/// (insert-if-absent) in ONE transaction, as the `wamn_system` owner (the
/// registry owner role — the wamn-q3n.3 apply pattern). A crash mid-stamp rolls
/// the whole record back; re-running is idempotent (the shared-cluster
/// guardrail: refresh placement, never clobber a customized policy). The
/// organization and policy rows are written in the same transaction.
async fn record_org(
    client: &mut sqlx::PgConnection,
    org: &Org,
    stamped: &[OrgEnvPolicy],
) -> anyhow::Result<()> {
    let mut transaction = crate::transaction::provisioning_transaction(client).await?;
    record_org_rows(&mut transaction, org, stamped).await?;
    transaction.commit().await?;
    Ok(())
}

async fn record_org_rows(
    client: &mut sqlx::PgConnection,
    org: &Org,
    stamped: &[OrgEnvPolicy],
) -> anyhow::Result<()> {
    let placement_type = org.placement.type_str();
    // The pool cluster is set only for a pooled org; NULL for a dedicated org.
    let pool = org.placement.pool();
    sqlx::query(sqlx::AssertSqlSafe(
        (wamn_registry::sql::upsert_org_sql()).to_string(),
    ))
    .bind(&org.id)
    .bind(placement_type)
    .bind(pool)
    .execute(&mut *client)
    .await
    .map(|result| result.rows_affected())
    .context("upsert registry.orgs row")?;
    for row in stamped {
        let p: &EnvPolicy = &row.policy;
        let name = p.name.as_str();
        let recovery = serde_json::to_string(&p.recovery_domain).context("recovery json")?;
        sqlx::query(sqlx::AssertSqlSafe(
            (wamn_registry::sql::stamp_env_policy_sql()).to_string(),
        ))
        .bind(&row.org)
        .bind(name)
        .bind(&recovery)
        .bind(p.promotion_rank)
        .bind(p.instances)
        .bind(&p.storage)
        .bind(&p.cpu)
        .bind(&p.memory)
        .bind(&p.image)
        .bind(&p.backup_cadence)
        .bind(&p.wal_retention)
        .bind(&p.hibernation)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .with_context(|| format!("stamp env policy {name:?}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every shipped template's one-org stamp validates: its placement and its
    /// policy set are self-consistent.
    #[test]
    fn every_template_stamps_a_valid_org() {
        for name in Template::NAMES {
            let t = Template::by_name(name).expect("a shipped preset");
            assert_eq!(t.name, name);
            assert!(plan::stamped_org(&t, "acme", "wamn-pg").is_ok());
        }
    }

    #[test]
    fn reserved_org_id_is_rejected() {
        // The `provision-org` id path runs through the same validator the registry
        // uses, so a reserved-prefix org id is refused before any effect.
        assert!(plan::stamped_org(&Template::standard(), "wamn-corp", "wamn-pg").is_err());
        assert!(
            plan::stamped_org(&Template::standard(), "Acme", "wamn-pg").is_err(),
            "an uppercase id is not a slug"
        );
    }
}

async fn read_env_policies(
    client: &mut sqlx::PgConnection,
    org: &str,
) -> anyhow::Result<Vec<EnvPolicy>> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(
        wamn_registry::sql::select_env_policies_sql(),
    ))
    .bind(org)
    .fetch_all(client)
    .await?;
    rows.iter().map(env_policy_from_row).collect()
}

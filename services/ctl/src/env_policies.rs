//! Reading the org-scoped `registry.env_policies` rows from the T1 system DB
//! (D18; org-scoped by wamn-8df.4).
//!
//! `provision-org` (sizes each cluster from the org's policy set) and
//! `provision-project-env` (reads one of the org's policies to derive the
//! project-env's cluster owner) map `select_env_policies_sql` /
//! `select_env_policy_sql` rows into the pure [`EnvPolicy`] model here.
//! `recovery_domain` is `jsonb` selected as `text` (column index 1), parsed back
//! into [`RecoveryDomain`] via serde.

use anyhow::Context as _;
use tokio_postgres::Row;
use wamn_control_registry::{DurabilityClass, Env, EnvPolicy, RecoveryDomain};

/// Add the env-policy durability selector to a pre-carrier system schema and
/// converge its persisted literal set. Callers SET ROLE to the registry owner
/// before invoking this shared migration.
pub(crate) async fn ensure_env_policy_durability_schema(
    client: &tokio_postgres::Client,
) -> anyhow::Result<()> {
    client
        .batch_execute(ensure_env_policy_durability_schema_sql())
        .await
        .context("ensure registry env-policy durability schema")
}

fn ensure_env_policy_durability_schema_sql() -> &'static str {
    "ALTER TABLE registry.env_policies \
       ADD COLUMN IF NOT EXISTS durability_class text DEFAULT 'standard'; \
     ALTER TABLE registry.env_policies \
       ALTER COLUMN durability_class SET DEFAULT 'standard'; \
     UPDATE registry.env_policies SET durability_class = 'standard' \
      WHERE durability_class IS NULL; \
     ALTER TABLE registry.env_policies \
       ALTER COLUMN durability_class SET NOT NULL; \
     DO $env_policy_durability$ BEGIN \
       IF EXISTS ( \
            SELECT 1 FROM pg_catalog.pg_constraint AS constraint_row \
             WHERE constraint_row.conrelid = 'registry.env_policies'::regclass \
               AND constraint_row.conname = 'env_policies_durability_class_check' \
               AND pg_catalog.pg_get_constraintdef(constraint_row.oid, true) \
                   <> 'CHECK (durability_class = ANY (ARRAY[''standard''::text, ''durable''::text]))') \
       THEN \
         ALTER TABLE registry.env_policies \
           DROP CONSTRAINT env_policies_durability_class_check; \
       END IF; \
       IF NOT EXISTS ( \
            SELECT 1 FROM pg_catalog.pg_constraint AS constraint_row \
             WHERE constraint_row.conrelid = 'registry.env_policies'::regclass \
               AND constraint_row.conname = 'env_policies_durability_class_check') \
       THEN \
         ALTER TABLE registry.env_policies \
           ADD CONSTRAINT env_policies_durability_class_check \
           CHECK (durability_class IN ('standard', 'durable')); \
       END IF; \
     END $env_policy_durability$"
}

/// Map one `select_env_policies_sql` / `select_env_policy_sql` row into an
/// [`EnvPolicy`]. Column order: `name, recovery_domain::text, promotion_rank,
/// instances, storage, cpu, memory, image, backup_cadence, wal_retention,
/// hibernation, durability_class`.
fn env_policy_from_row(row: &Row) -> anyhow::Result<EnvPolicy> {
    let recovery_text: String = row.get(1);
    let recovery_domain: RecoveryDomain =
        serde_json::from_str(&recovery_text).context("parse recovery_domain jsonb")?;
    Ok(EnvPolicy {
        name: Env::new(row.get::<_, String>(0)),
        recovery_domain,
        promotion_rank: row.get(2),
        instances: row.get(3),
        storage: row.get(4),
        cpu: row.get(5),
        memory: row.get(6),
        image: row.get(7),
        backup_cadence: row.get(8),
        wal_retention: row.get(9),
        hibernation: row.get(10),
        durability_class: DurabilityClass::from_sql(row.get::<_, &str>(11))
            .context("invalid env-policy durability_class")?,
    })
}

/// Read an org's whole env-policy set from the system DB, ordered by
/// `promotion_rank`. Empty for an org with no stamped policies yet.
pub(crate) async fn read_env_policies(
    client: &tokio_postgres::Client,
    org: &str,
) -> anyhow::Result<Vec<EnvPolicy>> {
    let rows = client
        .query(
            wamn_control_registry::sql::select_env_policies_sql().as_str(),
            &[&org],
        )
        .await
        .context("select env_policies")?;
    rows.iter().map(env_policy_from_row).collect()
}

/// Read one env policy from an org's set, or `None` if the slug names none of
/// the org's policies.
pub(crate) async fn read_env_policy(
    client: &tokio_postgres::Client,
    org: &str,
    name: &str,
) -> anyhow::Result<Option<EnvPolicy>> {
    let rows = client
        .query(
            wamn_control_registry::sql::select_env_policy_sql().as_str(),
            &[&org, &name],
        )
        .await
        .context("select env_policy")?;
    rows.first().map(env_policy_from_row).transpose()
}

/// Observe one complete policy without mutating a pre-durability registry.
/// Older rows receive the same `standard` value that the additive carrier
/// migration would persist, so their canonical policy identity is stable
/// across that migration.
pub(crate) async fn observe_env_policy(
    client: &tokio_postgres::Client,
    org: &str,
    name: &str,
) -> anyhow::Result<Option<EnvPolicy>> {
    let durability_present = env_policy_durability_carrier_present(client).await?;
    if durability_present {
        return read_env_policy(client, org, name).await;
    }
    let rows = client
        .query(
            "SELECT name, recovery_domain::text, promotion_rank, instances, \
                    storage, cpu, memory, image, backup_cadence, wal_retention, \
                    hibernation, 'standard'::text \
               FROM registry.env_policies WHERE org = $1 AND name = $2",
            &[&org, &name],
        )
        .await
        .context("observe pre-durability env_policy")?;
    rows.first().map(env_policy_from_row).transpose()
}

async fn env_policy_durability_carrier_present(
    client: &tokio_postgres::Client,
) -> anyhow::Result<bool> {
    client
        .query_one(
            "SELECT EXISTS ( \
               SELECT 1 FROM pg_catalog.pg_attribute AS attribute \
                WHERE attribute.attrelid = \
                        pg_catalog.to_regclass('registry.env_policies') \
                  AND attribute.attname = 'durability_class' \
                  AND attribute.attnum > 0 \
                  AND NOT attribute.attisdropped)",
            &[],
        )
        .await
        .context("observe registry env-policy durability carrier")
        .map(|row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durability_schema_ensure_is_additive_exact_and_idempotent_sql() {
        let sql = ensure_env_policy_durability_schema_sql();
        assert!(sql.contains("ADD COLUMN IF NOT EXISTS durability_class"));
        assert!(sql.contains("ALTER COLUMN durability_class SET NOT NULL"));
        assert!(sql.contains("ALTER COLUMN durability_class SET DEFAULT 'standard'"));
        assert!(sql.contains("pg_get_constraintdef"));
        assert!(sql.contains(
            "CHECK (durability_class = ANY (ARRAY[''standard''::text, ''durable''::text]))"
        ));
        assert!(sql.contains("DROP CONSTRAINT env_policies_durability_class_check"));
    }
}

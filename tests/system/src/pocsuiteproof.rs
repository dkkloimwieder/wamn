//! Callable-flow POC catalog proof for F0–F4.
//!
//! The promoted catalog is compiled through the production schema compiler.
//! The live gate applies it from zero, verifies the replay natural keys, and
//! proves an injected mid-DDL failure leaves no residue before a clean retry.

use anyhow::{Context as _, bail};
use clap::Args;
use tokio_postgres::{Client, NoTls};
use wamn_schema_compiler::Migration;
use wamn_schema_model::{Catalog, Constraint};

const CATALOG_JSON: &str = include_str!("../../../deploy/poc/poc-material-receiving.catalog.json");

#[derive(Debug, Args)]
pub struct CallableFlowSchemaArgs {
    /// Superuser URL for the isolated schema proof.
    #[arg(long, env = "WAMN_PG_ADMIN_URL")]
    pub admin_database_url: String,

    /// Ephemeral schema owned by this gate.
    #[arg(long, default_value = "wamn_callable_flow_schema")]
    pub schema: String,

    /// Retain the final clean schema for inspection.
    #[arg(long)]
    pub keep: bool,
}

pub async fn run(args: CallableFlowSchemaArgs) -> anyhow::Result<()> {
    let schema = schema_name(args.schema)?;
    let catalog = Catalog::from_json(CATALOG_JSON).context("parse promoted POC catalog")?;
    validate_contract(&catalog)?;
    let plan = Migration::create(&catalog).context("compile from-zero POC catalog")?;
    if plan.operations.len() < 2 {
        bail!("fault proof requires at least two ordered DDL operations");
    }

    let (mut client, connection) = tokio_postgres::connect(&args.admin_database_url, NoTls)
        .await
        .context("connect to PostgreSQL")?;
    tokio::spawn(async move {
        if let Err(error) = connection.await {
            tracing::error!(%error, "callable-flow schema PostgreSQL connection failed");
        }
    });

    reset_schema(&client, &schema).await?;
    apply_with_injected_failure(&mut client, &schema, &plan).await?;
    assert_no_tables(&client, &schema).await?;

    apply_clean(&mut client, &schema, &plan).await?;
    assert_constraints(&mut client, &schema).await?;
    let first = schema_snapshot(&client, &schema).await?;

    reset_schema(&client, &schema).await?;
    apply_clean(&mut client, &schema, &plan).await?;
    let second = schema_snapshot(&client, &schema).await?;
    if first != second {
        bail!("clean retry did not reproduce identical catalog metadata");
    }

    if !args.keep {
        drop_schema(&client, &schema).await?;
    }
    println!("callable-flow-schema PASS: rollback clean, retry identical, constraints enforced");
    Ok(())
}

fn validate_contract(catalog: &Catalog) -> anyhow::Result<()> {
    catalog.validate().map_err(|issues| {
        anyhow::anyhow!(
            "canonical catalog validation failed: {}",
            issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ")
        )
    })?;
    require_unique(catalog, "receipt_lines", &["receipt_id", "line_no"])?;
    require_unique(catalog, "quality_holds", &["line_id"])?;
    require_unique(catalog, "disposition_reviews", &["disposition_id"])?;
    let dispositions = entity(catalog, "dispositions")?;
    let decided_at = dispositions
        .fields
        .iter()
        .find(|field| field.id == "decided_at")
        .context("dispositions.decided_at is required")?;
    if decided_at.nullable {
        bail!("dispositions.decided_at must be required");
    }
    Ok(())
}

fn entity<'a>(catalog: &'a Catalog, id: &str) -> anyhow::Result<&'a wamn_schema_model::Entity> {
    catalog
        .entities
        .iter()
        .find(|entity| entity.id == id)
        .with_context(|| format!("required entity {id} is missing"))
}

fn require_unique(catalog: &Catalog, entity_id: &str, expected: &[&str]) -> anyhow::Result<()> {
    let found = entity(catalog, entity_id)?
        .constraints
        .iter()
        .any(|constraint| {
            matches!(
                constraint,
                Constraint::Unique { fields, .. }
                    if fields.iter().map(ToString::to_string).eq(
                        expected.iter().map(|field| (*field).to_string())
                    )
            )
        });
    if !found {
        bail!(
            "{entity_id} must have unique natural key ({})",
            expected.join(", ")
        );
    }
    Ok(())
}

fn schema_name(value: String) -> anyhow::Result<String> {
    let mut bytes = value.bytes();
    if !matches!(bytes.next(), Some(b'a'..=b'z' | b'_'))
        || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        || value.len() > 63
    {
        bail!("schema name must match [a-z_][a-z0-9_]* and be at most 63 bytes");
    }
    Ok(value)
}

fn quoted(schema: &str) -> String {
    format!("\"{schema}\"")
}

async fn reset_schema(client: &Client, schema: &str) -> anyhow::Result<()> {
    drop_schema(client, schema).await?;
    client
        .batch_execute(&format!("CREATE SCHEMA {}", quoted(schema)))
        .await
        .context("create isolated POC schema")
}

async fn drop_schema(client: &Client, schema: &str) -> anyhow::Result<()> {
    client
        .batch_execute(&format!("DROP SCHEMA IF EXISTS {} CASCADE", quoted(schema)))
        .await
        .context("drop isolated POC schema")
}

async fn set_search_path(
    transaction: &tokio_postgres::Transaction<'_>,
    schema: &str,
) -> anyhow::Result<()> {
    transaction
        .batch_execute(&format!(
            "SET LOCAL search_path TO {}, public",
            quoted(schema)
        ))
        .await
        .context("set isolated schema search path")
}

async fn apply_with_injected_failure(
    client: &mut Client,
    schema: &str,
    plan: &wamn_schema_compiler::MigrationPlan,
) -> anyhow::Result<()> {
    let transaction = client
        .transaction()
        .await
        .context("begin fault transaction")?;
    set_search_path(&transaction, schema).await?;
    let fault_after = plan.operations.len() / 2;
    for operation in plan.operations.iter().take(fault_after) {
        transaction
            .batch_execute(&operation.sql)
            .await
            .with_context(|| format!("apply pre-fault operation {}", operation.summary))?;
    }
    let error = transaction
        .batch_execute("SELECT 1 / 0")
        .await
        .expect_err("injected division-by-zero must fail the transaction");
    if error.code() != Some(&tokio_postgres::error::SqlState::DIVISION_BY_ZERO) {
        bail!("unexpected injected fault: {error}");
    }
    transaction
        .rollback()
        .await
        .context("roll back injected schema fault")
}

async fn apply_clean(
    client: &mut Client,
    schema: &str,
    plan: &wamn_schema_compiler::MigrationPlan,
) -> anyhow::Result<()> {
    let transaction = client
        .transaction()
        .await
        .context("begin clean transaction")?;
    set_search_path(&transaction, schema).await?;
    for operation in &plan.operations {
        transaction
            .batch_execute(&operation.sql)
            .await
            .with_context(|| format!("apply operation {}", operation.summary))?;
    }
    transaction.commit().await.context("commit clean catalog")
}

async fn assert_no_tables(client: &Client, schema: &str) -> anyhow::Result<()> {
    let count: i64 = client
        .query_one(
            "SELECT count(*) FROM information_schema.tables \
             WHERE table_schema = $1 AND table_type = 'BASE TABLE'",
            &[&schema],
        )
        .await
        .context("inspect rollback residue")?
        .get(0);
    if count != 0 {
        bail!("injected failure left {count} table(s) behind");
    }
    Ok(())
}

async fn assert_constraints(client: &mut Client, schema: &str) -> anyhow::Result<()> {
    let transaction = client
        .transaction()
        .await
        .context("begin constraint proof")?;
    set_search_path(&transaction, schema).await?;
    transaction
        .batch_execute(CONSTRAINT_PROOF_SQL)
        .await
        .context("natural-key and decided-at constraints")?;
    transaction.rollback().await.context("roll back proof rows")
}

async fn schema_snapshot(client: &Client, schema: &str) -> anyhow::Result<Vec<String>> {
    let rows = client
        .query(
            "SELECT c.relname || '|' || a.attnum::text || '|' || a.attname || '|' \
                    || format_type(a.atttypid, a.atttypmod) || '|' || a.attnotnull::text \
             FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_attribute a ON a.attrelid = c.oid \
             WHERE n.nspname = $1 AND c.relkind = 'r' \
               AND a.attnum > 0 AND NOT a.attisdropped \
             UNION ALL \
             SELECT c.relname || '|constraint|' || con.conname || '|' \
                    || pg_get_constraintdef(con.oid, true) \
             FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = $1 \
             ORDER BY 1",
            &[&schema],
        )
        .await
        .context("snapshot catalog metadata")?;
    Ok(rows.into_iter().map(|row| row.get(0)).collect())
}

const CONSTRAINT_PROOF_SQL: &str = r#"
INSERT INTO users (tenant_id, id, email)
VALUES ('t1', '00000000-0000-0000-0000-000000000001', 'inspector@example.test');
INSERT INTO sites (tenant_id, id, name, code)
VALUES ('t1', '00000000-0000-0000-0000-000000000002', 'HQ', 'hq');
INSERT INTO suppliers (tenant_id, id, name)
VALUES ('t1', '00000000-0000-0000-0000-000000000003', 'Acme');
INSERT INTO materials
  (tenant_id, id, name, moisture_max_pct, weight_tolerance_kg)
VALUES
  ('t1', '00000000-0000-0000-0000-000000000004', 'resin', 12.50, 0.050);
INSERT INTO receipts
  (tenant_id, id, receipt_no, supplier_id, site_id, received_at)
VALUES
  ('t1', '00000000-0000-0000-0000-000000000005', 'R-1',
   '00000000-0000-0000-0000-000000000003',
   '00000000-0000-0000-0000-000000000002', '2026-07-27T12:00:00Z');
INSERT INTO receipt_lines
  (tenant_id, id, receipt_id, line_no, material_id, quantity)
VALUES
  ('t1', '00000000-0000-0000-0000-000000000006',
   '00000000-0000-0000-0000-000000000005', 1,
   '00000000-0000-0000-0000-000000000004', 10.000);
INSERT INTO quality_holds
  (tenant_id, id, line_id, site_id, status, opened_at)
VALUES
  ('t1', '00000000-0000-0000-0000-000000000007',
   '00000000-0000-0000-0000-000000000006',
   '00000000-0000-0000-0000-000000000002', 'open', '2026-07-27T12:01:00Z');

DO $proof$
BEGIN
  BEGIN
    INSERT INTO receipt_lines
      (tenant_id, receipt_id, line_no, material_id, quantity)
    VALUES
      ('t1', '00000000-0000-0000-0000-000000000005', 1,
       '00000000-0000-0000-0000-000000000004', 11.000);
    RAISE EXCEPTION USING ERRCODE = '23514',
      MESSAGE = 'duplicate receipt-line natural key was accepted';
  EXCEPTION WHEN unique_violation THEN NULL;
  END;

  BEGIN
    INSERT INTO quality_holds
      (tenant_id, line_id, site_id, status, opened_at)
    VALUES
      ('t1', '00000000-0000-0000-0000-000000000006',
       '00000000-0000-0000-0000-000000000002', 'open', '2026-07-27T12:02:00Z');
    RAISE EXCEPTION USING ERRCODE = '23514',
      MESSAGE = 'duplicate hold natural key was accepted';
  EXCEPTION WHEN unique_violation THEN NULL;
  END;

  BEGIN
    INSERT INTO dispositions
      (tenant_id, hold_id, inspector_id, decision)
    VALUES
      ('t1', '00000000-0000-0000-0000-000000000007',
       '00000000-0000-0000-0000-000000000001', 'accept');
    RAISE EXCEPTION USING ERRCODE = '23514',
      MESSAGE = 'missing decided_at was accepted';
  EXCEPTION WHEN not_null_violation THEN NULL;
  END;
END
$proof$;

INSERT INTO dispositions
  (tenant_id, id, hold_id, inspector_id, decision, decided_at)
VALUES
  ('t1', '00000000-0000-0000-0000-000000000008',
   '00000000-0000-0000-0000-000000000007',
   '00000000-0000-0000-0000-000000000001', 'accept', '2026-07-27T12:03:00Z');
INSERT INTO disposition_reviews
  (tenant_id, disposition_id, recommendation, confidence, matched)
VALUES
  ('t1', '00000000-0000-0000-0000-000000000008', 'accept', '0.93', true);

DO $proof$
BEGIN
  BEGIN
    INSERT INTO disposition_reviews
      (tenant_id, disposition_id, recommendation, confidence, matched)
    VALUES
      ('t1', '00000000-0000-0000-0000-000000000008', 'reject', '0.12', false);
    RAISE EXCEPTION USING ERRCODE = '23514',
      MESSAGE = 'duplicate disposition review was accepted';
  EXCEPTION WHEN unique_violation THEN NULL;
  END;
END
$proof$;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use wamn_schema_compiler::Confirmation;

    fn catalog() -> Catalog {
        Catalog::from_json(CATALOG_JSON).expect("canonical POC catalog parses")
    }

    #[test]
    fn canonical_catalog_has_poc_recovery_keys() {
        validate_contract(&catalog()).expect("callable-flow POC contract");
    }

    #[test]
    fn generated_ddl_enforces_the_contract() {
        let ddl = Migration::create(&catalog())
            .expect("catalog compiles")
            .sql(Confirmation::None)
            .expect("from-zero catalog is additive");
        assert!(ddl.contains("UNIQUE (tenant_id, \"receipt_id\", \"line_no\")"));
        assert!(ddl.contains("UNIQUE (tenant_id, \"line_id\")"));
        assert!(ddl.contains("UNIQUE (tenant_id, \"disposition_id\")"));
        assert!(ddl.contains("\"decided_at\" timestamptz NOT NULL"));
    }

    #[test]
    fn missing_or_nullable_recovery_keys_are_rejected() {
        let mut missing_line_key = catalog();
        entity_mut(&mut missing_line_key, "receipt_lines")
            .constraints
            .clear();
        assert!(validate_contract(&missing_line_key).is_err());

        let mut missing_hold_key = catalog();
        entity_mut(&mut missing_hold_key, "quality_holds")
            .constraints
            .clear();
        assert!(validate_contract(&missing_hold_key).is_err());

        let mut missing_review_key = catalog();
        entity_mut(&mut missing_review_key, "disposition_reviews")
            .constraints
            .clear();
        assert!(validate_contract(&missing_review_key).is_err());

        let mut nullable_decision = catalog();
        entity_mut(&mut nullable_decision, "dispositions")
            .fields
            .iter_mut()
            .find(|field| field.id == "decided_at")
            .expect("decided_at")
            .nullable = true;
        assert!(validate_contract(&nullable_decision).is_err());
    }

    #[test]
    fn injected_failure_splits_a_real_ddl_prefix() {
        let plan = Migration::create(&catalog()).expect("catalog compiles");
        let fault_after = plan.operations.len() / 2;
        assert!(fault_after > 0);
        assert!(fault_after < plan.operations.len());
        assert!(
            plan.operations[..fault_after]
                .iter()
                .any(|operation| operation.sql.contains("CREATE TABLE"))
        );
    }

    fn entity_mut<'a>(catalog: &'a mut Catalog, id: &str) -> &'a mut wamn_schema_model::Entity {
        catalog
            .entities
            .iter_mut()
            .find(|entity| entity.id == id)
            .expect("fixture entity")
    }
}

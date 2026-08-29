//! Live PostgreSQL proof for the generated Receiving data-access contract.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anyhow::{Context as _, Result, bail, ensure};
    use chrono::{DateTime, Utc};
    use serde::Deserialize;
    use tokio_postgres::{Client, NoTls, Row};
    use uuid::Uuid;

    const MANIFEST: &[u8] = include_bytes!("../../../packages/receiving/wamn.json");
    const MIGRATION: &str = include_str!("../../../packages/receiving/migrations/0001_initial.sql");
    const UPDATE_SQL: &str =
        include_str!("../../../packages/receiving/generated/sql/purchase_order/update.sql");

    #[derive(Debug, Deserialize)]
    struct Manifest {
        models: BTreeMap<String, Model>,
    }

    #[derive(Debug, Deserialize)]
    struct Model {
        enum_fields: BTreeMap<String, Vec<String>>,
    }

    #[derive(Debug)]
    struct UpdateResult {
        outcome: String,
        created_at: Option<DateTime<Utc>>,
        id: Option<Uuid>,
        purchase_order_number: Option<String>,
        row_version: Option<i64>,
        status: Option<String>,
        supplier_id: Option<Uuid>,
        updated_at: Option<DateTime<Utc>>,
    }

    #[tokio::test]
    #[ignore = "requires a fresh disposable PostgreSQL 18 URL in WAMN_RECEIVING_PG_URL"]
    async fn enum_and_optimistic_update_outcomes_hold_on_postgres_18() -> Result<()> {
        let url = std::env::var("WAMN_RECEIVING_PG_URL")
            .context("WAMN_RECEIVING_PG_URL must name a fresh disposable PostgreSQL 18 database")?;
        let client = connect(&url).await?;
        assert_postgres_18(&client).await?;
        assert_fresh_receiving_schema(&client).await?;
        client
            .batch_execute("CREATE SCHEMA receiving")
            .await
            .context("create the Receiving schema")?;
        client
            .batch_execute(MIGRATION)
            .await
            .context("apply the exact Receiving migration")?;
        select_receiving_schema(&client).await?;

        prove_status_vocabulary(&client).await?;

        let missing_id = Uuid::from_u128(0x100);
        let unused_supplier = Uuid::from_u128(0x101);
        let not_found = execute_update(&client, missing_id, 1, unused_supplier).await?;
        ensure!(
            not_found.outcome == "not_found",
            "missing row did not return not_found"
        );
        assert_null_payload(&not_found)?;

        let subject_id = Uuid::from_u128(0x200);
        let initial_supplier = Uuid::from_u128(0x201);
        insert_purchase_order(
            &client,
            subject_id,
            "PO-concurrency",
            initial_supplier,
            "open",
        )
        .await?;

        let first_supplier = Uuid::from_u128(0x202);
        let second_supplier = Uuid::from_u128(0x203);
        let first_client = connect(&url).await?;
        let second_client = connect(&url).await?;
        select_receiving_schema(&first_client).await?;
        select_receiving_schema(&second_client).await?;

        let (first, second) = tokio::join!(
            execute_update(&first_client, subject_id, 1, first_supplier),
            execute_update(&second_client, subject_id, 1, second_supplier),
        );
        let attempts = [
            (first.context("first competing update")?, first_supplier),
            (second.context("second competing update")?, second_supplier),
        ];
        let updated = attempts
            .iter()
            .find(|(result, _)| result.outcome == "updated")
            .context("neither competing update succeeded")?;
        let conflict = attempts
            .iter()
            .find(|(result, _)| result.outcome == "concurrency_conflict")
            .context("neither competing update returned concurrency_conflict")?;
        ensure!(
            attempts
                .iter()
                .filter(|(result, _)| result.outcome == "updated")
                .count()
                == 1,
            "competing updates produced more than one success"
        );
        ensure!(
            attempts
                .iter()
                .filter(|(result, _)| result.outcome == "concurrency_conflict")
                .count()
                == 1,
            "competing updates produced more than one conflict"
        );
        ensure!(
            updated.0.id == Some(subject_id),
            "updated payload returned the wrong id"
        );
        ensure!(
            updated.0.row_version == Some(2),
            "successful update did not increment once"
        );
        ensure!(
            updated.0.supplier_id == Some(updated.1),
            "successful update returned the wrong supplier"
        );
        ensure!(
            updated.0.created_at.is_some()
                && updated.0.purchase_order_number.is_some()
                && updated.0.status.is_some()
                && updated.0.updated_at.is_some(),
            "successful update returned an incomplete row"
        );
        assert_null_payload(&conflict.0)?;

        let persisted = persisted_revision(&client, subject_id).await?;
        ensure!(
            persisted == (updated.1, 2),
            "competing updates did not persist one winner"
        );

        let stale_supplier = Uuid::from_u128(0x204);
        let stale = execute_update(&client, subject_id, 1, stale_supplier).await?;
        ensure!(
            stale.outcome == "concurrency_conflict",
            "stale revision did not remain a concurrency conflict"
        );
        assert_null_payload(&stale)?;
        ensure!(
            persisted_revision(&client, subject_id).await? == persisted,
            "stale update mutated the winning row"
        );

        Ok(())
    }

    async fn connect(url: &str) -> Result<Client> {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .context("connect to disposable PostgreSQL")?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        client
            .batch_execute(
                "SET statement_timeout = '10s'; \
                 SET lock_timeout = '5s'; \
                 SET transaction_timeout = '30s'",
            )
            .await
            .context("bound Receiving gate timeouts")?;
        Ok(client)
    }

    async fn assert_postgres_18(client: &Client) -> Result<()> {
        let version = client
            .query_one("SELECT current_setting('server_version_num')", &[])
            .await
            .context("read PostgreSQL server version")?
            .get::<_, String>(0)
            .parse::<u32>()
            .context("server_version_num is not numeric")?;
        ensure!(
            (180_000..190_000).contains(&version),
            "Receiving live gate requires PostgreSQL 18, found {version}"
        );
        Ok(())
    }

    async fn assert_fresh_receiving_schema(client: &Client) -> Result<()> {
        let exists = client
            .query_one("SELECT to_regnamespace('receiving') IS NOT NULL", &[])
            .await
            .context("inspect Receiving schema freshness")?
            .get::<_, bool>(0);
        ensure!(
            !exists,
            "disposable database already contains schema receiving"
        );
        Ok(())
    }

    async fn select_receiving_schema(client: &Client) -> Result<()> {
        client
            .batch_execute("SET search_path = receiving, public")
            .await
            .context("select Receiving through trusted connection context")
    }

    async fn prove_status_vocabulary(client: &Client) -> Result<()> {
        let manifest: Manifest =
            serde_json::from_slice(MANIFEST).context("parse Receiving manifest")?;
        let statuses = manifest
            .models
            .get("purchase_order")
            .context("manifest must declare purchase_order")?
            .enum_fields
            .get("status")
            .context("manifest must declare purchase_order.status outcomes")?;
        ensure!(
            !statuses.is_empty(),
            "purchase_order.status vocabulary is empty"
        );

        let supplier_id = Uuid::from_u128(0x300);
        for (index, status) in statuses.iter().enumerate() {
            let id = Uuid::from_u128(0x310 + u128::try_from(index)?);
            insert_purchase_order(client, id, &format!("PO-enum-{index}"), supplier_id, status)
                .await
                .with_context(|| format!("PostgreSQL refused declared status {status}"))?;
        }

        let invalid_id = Uuid::from_u128(0x3ff);
        let invalid = insert_purchase_order(
            client,
            invalid_id,
            "PO-enum-invalid",
            supplier_id,
            "outside_manifest",
        )
        .await
        .expect_err("PostgreSQL accepted a status outside the manifest vocabulary");
        let database = invalid
            .downcast_ref::<tokio_postgres::Error>()
            .and_then(tokio_postgres::Error::as_db_error)
            .context("invalid status did not return a PostgreSQL database error")?;
        ensure!(
            database.code().code() == "23514",
            "invalid status did not return 23514"
        );
        ensure!(
            database.constraint() == Some("purchase_order_status_check"),
            "invalid status named the wrong constraint"
        );
        Ok(())
    }

    async fn insert_purchase_order(
        client: &Client,
        id: Uuid,
        number: &str,
        supplier_id: Uuid,
        status: &str,
    ) -> Result<()> {
        client
            .execute(
                "INSERT INTO purchase_order \
                 (id, purchase_order_number, supplier_id, status) \
                 VALUES ($1, $2, $3, $4)",
                &[&id, &number, &supplier_id, &status],
            )
            .await
            .context("insert purchase_order fixture")?;
        Ok(())
    }

    async fn execute_update(
        client: &Client,
        id: Uuid,
        expected_revision: i64,
        supplier_id: Uuid,
    ) -> Result<UpdateResult> {
        let supplier_id = Some(supplier_id);
        let row = client
            .query_one(UPDATE_SQL, &[&id, &expected_revision, &true, &supplier_id])
            .await
            .context("execute exact generated purchase_order.update SQL")?;
        Ok(update_result(&row))
    }

    fn update_result(row: &Row) -> UpdateResult {
        UpdateResult {
            outcome: row.get("outcome"),
            created_at: row.get("created_at"),
            id: row.get("id"),
            purchase_order_number: row.get("purchase_order_number"),
            row_version: row.get("row_version"),
            status: row.get("status"),
            supplier_id: row.get("supplier_id"),
            updated_at: row.get("updated_at"),
        }
    }

    fn assert_null_payload(result: &UpdateResult) -> Result<()> {
        if result.created_at.is_none()
            && result.id.is_none()
            && result.purchase_order_number.is_none()
            && result.row_version.is_none()
            && result.status.is_none()
            && result.supplier_id.is_none()
            && result.updated_at.is_none()
        {
            Ok(())
        } else {
            bail!("{} outcome returned a non-null row payload", result.outcome)
        }
    }

    async fn persisted_revision(client: &Client, id: Uuid) -> Result<(Uuid, i64)> {
        let row = client
            .query_one(
                "SELECT supplier_id, row_version FROM purchase_order WHERE id = $1",
                &[&id],
            )
            .await
            .context("read persisted purchase_order revision")?;
        Ok((row.get("supplier_id"), row.get("row_version")))
    }
}

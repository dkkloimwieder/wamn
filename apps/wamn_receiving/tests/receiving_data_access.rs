//! Live PostgreSQL test for the generated Receiving data-access contract.

#[cfg(test)]
mod tests {
    use sqlx::PgConnection as Client;
    use sqlx::Row as _;
    use sqlx::postgres::PgRow as Row;
    use std::collections::BTreeMap;

    use anyhow::{Context as _, Result, bail, ensure};
    use chrono::{DateTime, Utc};
    use serde_json::{Value, json};

    use uuid::Uuid;

    const MANIFEST: &str = "wamn_receiving/wamn.json";
    const OVERLAY_MANIFEST: &str = "client_acme_receiving/wamn.json";
    const MIGRATION: &str =
        include_str!("../../../apps/wamn_receiving/migrations/0001_initial.sql");
    const RECORD_HISTORY_SQL: &str = include_str!("../../../deploy/sql/record-history.sql");
    const RECORD_HISTORY_APP_GRANTS_SQL: &str =
        include_str!("../../../deploy/sql/record-history-app-grants.sql");
    /// The test principal that the fixture writes as. This SQL-only fixture has
    /// no `platform` schema, and no stamp column reads a user row.
    const FIXTURE_PRINCIPAL: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_00f1);
    /// A second principal, so a stamp shows which write set it.
    const COMMAND_PRINCIPAL: Uuid = Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_00f2);
    /// The administrative operation that every fixture write logs.
    const FIXTURE_OPERATION: &str = "admin:receiving-data-access-fixture";
    const HISTORY_SQL: &str = include_str!(
        "../../../apps/wamn_receiving/query/receiving/load_purchase_order_history/load_purchase_order_history.sql"
    );
    const UPDATE_SQL: &str = "wamn_receiving/sql/purchase_order/update.sql";
    const LOCK_PURCHASE_ORDER_SQL: &str = include_str!(
        "../../../apps/wamn_receiving/command/receiving/record_receipt/lock_purchase_order.sql"
    );
    const RECORD_RECEIPT_SQL: &str = include_str!(
        "../../../apps/wamn_receiving/command/receiving/record_receipt/record_receipt.sql"
    );
    const OVERLAY_FIELDS_MIGRATION: &str =
        include_str!("../../../apps/client_acme_receiving/migrations/0001_initial.sql");
    const OVERLAY_GET_SQL: &str = "client_acme_receiving/sql/purchase_order/get.sql";
    const OVERLAY_UPDATE_SQL: &str = "client_acme_receiving/sql/purchase_order/update.sql";
    const OVERLAY_LOAD_DETAIL_SQL: &str = include_str!(
        "../../../apps/client_acme_receiving/query/quality/load_purchase_order_detail/load_purchase_order_detail.sql"
    );
    const OVERLAY_APPROVE_INSPECTION_SQL: &str = include_str!(
        "../../../apps/client_acme_receiving/command/quality/approve_inspection/approve_inspection.sql"
    );
    const OVERLAY_INSERT_INSPECTION_SQL: &str = include_str!(
        "../../../apps/client_acme_receiving/command/quality/create_inspection/insert_inspection.sql"
    );
    const OVERLAY_LOAD_INSPECTION_SQL: &str = include_str!(
        "../../../apps/client_acme_receiving/command/quality/create_inspection/load_inspection.sql"
    );

    fn generated(path: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/wamn")
            .join(path);
        std::fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "the test input {} is missing ({error}); build it with `cargo xtask prepare-tests`",
                path.display()
            )
        })
    }

    fn generated_sql(path: &str) -> String {
        String::from_utf8(generated(path)).expect("generated SQL is UTF-8")
    }

    #[derive(Debug)]
    struct UpdateResult {
        outcome: String,
        created_at: Option<DateTime<Utc>>,
        created_by: Option<Uuid>,
        id: Option<Uuid>,
        purchase_order_number: Option<String>,
        row_version: Option<i32>,
        status: Option<String>,
        supplier_id: Option<Uuid>,
        updated_at: Option<DateTime<Utc>>,
        updated_by: Option<Uuid>,
    }

    /// The record-history stamps of one purchase order.
    #[derive(Debug, Eq, PartialEq)]
    struct Stamps {
        created_at: DateTime<Utc>,
        created_by: Uuid,
        updated_at: DateTime<Utc>,
        updated_by: Uuid,
    }

    #[derive(Clone, Debug)]
    struct ReceiptCommand {
        purchase_order_id: Uuid,
        receipt_reference: String,
        occurred_at: String,
        line: Vec<ReceiptCommandLine>,
    }

    #[derive(Clone, Debug)]
    struct ReceiptCommandLine {
        purchase_order_line_id: Uuid,
        quantity: String,
        location_id: Uuid,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct ReceiptCommandResult {
        receipt_id: Uuid,
        purchase_order_id: Uuid,
        purchase_order_status: String,
        row_version: i32,
    }

    #[derive(Debug)]
    enum CommandAttemptError {
        Domain(DomainRefusal),
        Database(sqlx::Error),
        Internal(String),
    }

    #[derive(Debug, Eq, PartialEq)]
    struct DomainRefusal {
        code: &'static str,
        id: Option<Uuid>,
    }

    impl DomainRefusal {
        const fn new(code: &'static str) -> Self {
            Self { code, id: None }
        }

        const fn with_id(code: &'static str, id: Option<Uuid>) -> Self {
            Self { code, id }
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    struct CommandSnapshot {
        receipt_count: i64,
        receipt_line_count: i64,
        first_received: String,
        second_received: String,
        purchase_order_status: String,
        row_version: i32,
        purchase_order_stamps: Stamps,
        receipt_stamps: Value,
        history_entries: i64,
    }

    /// Record history spec test 8. An Acme overlay update on the shared
    /// purchase order logs a changed-column diff, and the fold reconstructs the
    /// effective base and overlay row at each position. The Receiving history
    /// read returns only the base columns that it declares.
    #[tokio::test]
    #[ignore = "requires: docker"]
    async fn an_overlay_update_logs_a_diff_that_folds_to_the_effective_row() -> Result<()> {
        use wamn_record_history::{HistoryRow, RowState, retain_columns, state_at};

        let server = wamn_test_infrastructure::containers::start_postgres().await?;
        let url = server.create_database("receiving").await?.to_string();
        let mut client = connect(&url).await?;
        assert_postgres_18(&mut client).await?;
        assert_fresh_receiving_schema(&mut client).await?;
        // Each actor uses its own transaction in this private database.
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            ("BEGIN; CREATE SCHEMA receiving; \
             CREATE ROLE wamn_app NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS")
                .to_string(),
        ))
        .execute(&mut client)
        .await
        .map(drop)?;
        sqlx::raw_sql(sqlx::AssertSqlSafe((MIGRATION).to_string()))
            .execute(&mut client)
            .await
            .map(drop)?;

        install_record_history(&mut client, &generated(MANIFEST)).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe((OVERLAY_FIELDS_MIGRATION).to_string()))
            .execute(&mut client)
            .await
            .map(drop)?;

        install_record_history(&mut client, &generated(OVERLAY_MANIFEST)).await?;
        select_receiving_schema(&mut client).await?;
        let id = Uuid::from_u128(0x800);
        insert_purchase_order(
            &mut client,
            id,
            "PO-overlay-history",
            Uuid::from_u128(0x801),
            "open",
        )
        .await?;

        bind_actor(&mut client, COMMAND_PRINCIPAL).await?;
        let updated = sqlx::query(sqlx::AssertSqlSafe(generated_sql(OVERLAY_UPDATE_SQL)))
            .bind(id)
            .bind(1_i32)
            .bind(true)
            .bind(Some(true))
            .bind(false)
            .bind(&None::<String>)
            .fetch_one(&mut client)
            .await
            .context("execute exact Acme purchase_order.update SQL")?;
        bind_actor(&mut client, FIXTURE_PRINCIPAL).await?;
        ensure!(
            updated.get::<String, _>("outcome") == "updated"
                && updated.get::<Option<i32>, _>("row_version") == Some(2)
                && updated.get::<Option<bool>, _>("acme_inspection_required") == Some(true),
            "Acme purchase_order.update returned the wrong row"
        );
        let read = sqlx::query(HISTORY_SQL)
            .bind(id)
            .bind(0_i64)
            .bind(100_i32)
            .fetch_all(&mut client)
            .await
            .context("execute the Receiving history read")?;

        let entries = sqlx::query(
            "SELECT position, type, before::text, after::text, \
                        wamn_history.row_image(purchase_order)::text AS current \
                   FROM purchase_order_history \
                   JOIN purchase_order ON purchase_order.id = $1::uuid \
                  WHERE row_key = jsonb_build_object('id', $1::uuid) \
                  ORDER BY position",
        )
        .bind(id)
        .fetch_all(&mut client)
        .await
        .context("read the logged entries and the effective row")?;
        let [insert, update] = entries.as_slice() else {
            bail!(
                "the insert and the overlay update logged {} entries",
                entries.len()
            );
        };
        ensure!(
            insert.get::<String, _>("type") == "insert"
                && update.get::<String, _>("type") == "update",
            "the history must record an insert and an update"
        );

        let current = update.get::<String, _>("current");
        let history = entries
            .iter()
            .map(|row| {
                (
                    row.get::<i64, _>("position"),
                    row.get::<String, _>("type"),
                    row.get::<String, _>("before"),
                )
            })
            .collect::<Vec<_>>();
        let head = history.last().map_or(0, |(position, _, _)| *position);
        let rows = history
            .iter()
            .map(|(position, type_, before)| HistoryRow {
                position: *position,
                type_,
                before,
                current: &current,
                head_position: head,
            })
            .collect::<Vec<_>>();
        let image = |text: &str| -> Result<BTreeMap<String, String>> {
            let Value::Object(object) = serde_json::from_str(text)? else {
                bail!("an image is not a JSON object");
            };
            Ok(object
                .into_iter()
                .map(|(name, value)| (name, value.to_string()))
                .collect())
        };
        let state = |position: i64| -> Result<BTreeMap<String, String>> {
            let RowState::Present(image) = state_at(&rows, position)? else {
                bail!("the purchase order is not present at position {position}");
            };
            Ok(image
                .columns()
                .map(|(name, value)| {
                    let value = serde_json::from_str::<Value>(value).map(|value| value.to_string());
                    value.map(|value| (name.to_owned(), value))
                })
                .collect::<std::result::Result<_, _>>()?)
        };
        ensure!(
            state(insert.get("position"))? == image(&insert.get::<String, _>("after"))?
                && state(head)? == image(&current)?,
            "the fold did not reconstruct the effective row at each position"
        );

        let manifest = serde_json::from_slice::<Value>(&generated(MANIFEST))?;
        let declared =
            manifest["custom_operations"]["receiving.load_purchase_order_history"]["relations"]
                .as_array()
                .and_then(|relations| {
                    relations
                        .iter()
                        .find(|relation| relation["table"] == "purchase_order")
                })
                .and_then(|relation| relation["select_fields"].as_array())
                .context("the history read declares its purchase_order columns")?
                .iter()
                .map(|field| field.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .context("every declared column is a name")?;
        let declared = declared.iter().map(String::as_str).collect::<Vec<_>>();
        let filtered = read
            .iter()
            .map(|row| {
                let declared_image = |column: &str| {
                    row.get::<Option<String>, _>(column)
                        .and_then(|text| {
                            retain_columns(&text, &declared, &["created_at", "updated_at"])
                        })
                        .context("the history read returned an image")
                };
                Ok((
                    row.get::<i64, _>("position"),
                    row.get::<String, _>("type"),
                    declared_image("before?")?,
                    declared_image("current?")?,
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        // The read states no head position, so the newest row it returned is
        // the head. The read asked for the whole retained history.
        let base_head = filtered.last().map_or(0, |(position, ..)| *position);
        let base_rows = filtered
            .iter()
            .map(|(position, type_, before, current)| HistoryRow {
                position: *position,
                type_,
                before,
                current,
                head_position: base_head,
            })
            .collect::<Vec<_>>();
        let RowState::Present(base) = state_at(&base_rows, head)? else {
            bail!("the base history read does not show the purchase order");
        };
        let base_columns = base.columns().map(|(name, _)| name).collect::<Vec<_>>();
        let mut expected = declared.clone();
        expected.sort_unstable();
        ensure!(
            base_rows.len() == 2 && base_columns == expected,
            "the base history read returned columns it does not declare: {base_columns:?}"
        );

        sqlx::raw_sql(sqlx::AssertSqlSafe(("ROLLBACK").to_string()))
            .execute(&mut client)
            .await
            .map(drop)?;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires: docker"]
    async fn enum_and_optimistic_update_outcomes_hold_on_postgres_18() -> Result<()> {
        // The product installs the project schemas, which the commands bind
        // their identity in.
        let server = wamn_test_infrastructure::containers::start_postgres().await?;
        let migration = server.create_project_database("receiving").await?;
        wamn_platform_install::platform_migrations::install_project(
            &mut wamn_postgres::connect(migration.as_str()).await?,
        )
        .await?;
        let mut url = url::Url::parse(server.admin_url())?;
        url.set_path("receiving");
        let url = url.to_string();
        let mut client = connect(&url).await?;
        assert_postgres_18(&mut client).await?;
        assert_fresh_receiving_schema(&mut client).await?;
        sqlx::raw_sql("CREATE SCHEMA receiving")
            .execute(&mut client)
            .await
            .map(drop)
            .context("create the Receiving schema")?;
        sqlx::raw_sql(sqlx::AssertSqlSafe((MIGRATION).to_string()))
            .execute(&mut client)
            .await
            .map(drop)
            .context("apply the exact Receiving migration")?;

        install_record_history(&mut client, &generated(MANIFEST)).await?;
        bind_actor(&mut client, FIXTURE_PRINCIPAL).await?;
        select_receiving_schema(&mut client).await?;

        assert_status_vocabulary(&mut client).await?;

        let subject_id = Uuid::from_u128(0x200);
        let initial_supplier = Uuid::from_u128(0x201);
        insert_purchase_order(
            &mut client,
            subject_id,
            "PO-concurrency",
            initial_supplier,
            "open",
        )
        .await?;
        let inserted = stamps(&mut client, subject_id).await?;
        ensure!(
            inserted.created_by == FIXTURE_PRINCIPAL
                && inserted.updated_by == FIXTURE_PRINCIPAL
                && inserted.created_at == inserted.updated_at,
            "an insert did not stamp both pairs with one instant: {inserted:?}"
        );

        let first_supplier = Uuid::from_u128(0x202);
        let second_supplier = Uuid::from_u128(0x203);
        let mut first_client = connect(&url).await?;
        let mut second_client = connect(&url).await?;
        select_receiving_schema(&mut first_client).await?;
        select_receiving_schema(&mut second_client).await?;
        bind_actor(&mut client, FIXTURE_PRINCIPAL).await?;

        let update = async |mut writer: Client, supplier: Uuid| -> Result<UpdateResult> {
            let mut transaction = begin_as(&mut writer, FIXTURE_PRINCIPAL).await?;
            let result = execute_update(&mut transaction, subject_id, 1, supplier).await?;
            transaction.commit().await?;
            Ok(result)
        };
        let (first, second) = tokio::join!(
            update(first_client, first_supplier),
            update(second_client, second_supplier),
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

        let persisted = persisted_revision(&mut client, subject_id).await?;
        ensure!(
            persisted == (updated.1, 2),
            "competing updates did not persist one winner"
        );

        assert_no_op_keeps_revision_and_stamps(&mut client, subject_id, persisted).await?;

        assert_record_receipt(&url, &mut client).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe((OVERLAY_FIELDS_MIGRATION).to_string()))
            .execute(&mut client)
            .await
            .map(drop)
            .context("apply the exact Acme field migration")?;

        install_record_history(&mut client, &generated(OVERLAY_MANIFEST)).await?;
        assert_acme_overlay(&mut client).await?;

        Ok(())
    }

    /// Install the platform record-history functions, and the history tables and
    /// triggers that apply-package derives from the `audit_log` declarations of
    /// one package manifest.
    async fn install_record_history(client: &mut Client, manifest: &[u8]) -> Result<()> {
        let manifest: wamn_component_policy::PackageManifest =
            serde_json::from_slice(manifest).context("parse package manifest")?;
        let logs = manifest.models.values().filter_map(|model| {
            model.log_retention().map(|retention| {
                format!(
                    "SELECT wamn_history.create_history_table('{schema}', '{table}', false); \
                     CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE \
                     ON {schema}.{table} FOR EACH ROW \
                     EXECUTE FUNCTION wamn_history.log_row_change('{retention}');",
                    schema = model.schema,
                    table = model.table,
                )
            })
        });
        let triggers = manifest.models.values().filter_map(|model| {
            let audit_log = model.audit_log.as_ref()?;
            let columns = wamn_package::manifest::RecordHistoryColumn::ALL
                .into_iter()
                .filter(|column| audit_log.columns.contains(column))
                .map(|column| format!("'{}'", column.as_str()))
                .collect::<Vec<_>>();
            (!columns.is_empty()).then(|| {
                format!(
                    "CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON {}.{} \
                     FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row({});",
                    model.schema,
                    model.table,
                    columns.join(", ")
                )
            })
        });
        let sql = [RECORD_HISTORY_SQL, RECORD_HISTORY_APP_GRANTS_SQL]
            .into_iter()
            .map(str::to_owned)
            .chain(triggers)
            .chain(logs)
            .collect::<Vec<_>>()
            .join("\n");
        sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
            .execute(&mut *client)
            .await
            .map(drop)
            .context("install the package record-history triggers")?;
        bind_actor(client, FIXTURE_PRINCIPAL).await
    }

    async fn bind_actor(client: &mut Client, actor: Uuid) -> Result<()> {
        sqlx::raw_sql("COMMIT; BEGIN").execute(&mut *client).await?;
        sqlx::query(
            "INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1::text::uuid,'','fixture',$2)",
        )
        .bind(actor.hyphenated().to_string())
        .bind(FIXTURE_OPERATION)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .context("bind the fixture actor")?;
        Ok(())
    }

    async fn stamps(client: &mut Client, id: Uuid) -> Result<Stamps> {
        let row = sqlx::query(
            "SELECT created_at, created_by, updated_at, updated_by \
                 FROM purchase_order WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&mut *client)
        .await
        .context("read purchase_order stamps")?;
        Ok(Stamps {
            created_at: row.get("created_at"),
            created_by: row.get("created_by"),
            updated_at: row.get("updated_at"),
            updated_by: row.get("updated_by"),
        })
    }

    /// A true no-op keeps the revision and every stamp, and returns the
    /// current row. A stale no-op still refuses.
    async fn assert_no_op_keeps_revision_and_stamps(
        client: &mut Client,
        id: Uuid,
        (supplier_id, row_version): (Uuid, i32),
    ) -> Result<()> {
        let before = stamps(client, id).await?;
        bind_actor(client, COMMAND_PRINCIPAL).await?;
        let no_op = execute_update(client, id, row_version, supplier_id).await?;
        let stale = execute_update(client, id, row_version - 1, supplier_id).await?;
        bind_actor(client, FIXTURE_PRINCIPAL).await?;
        ensure!(
            no_op.outcome == "updated"
                && no_op.row_version == Some(row_version)
                && no_op.supplier_id == Some(supplier_id)
                && no_op.created_at == Some(before.created_at)
                && no_op.created_by == Some(before.created_by)
                && no_op.updated_at == Some(before.updated_at)
                && no_op.updated_by == Some(before.updated_by),
            "a true no-op changed its revision or stamps: {no_op:?}"
        );
        ensure!(
            stamps(client, id).await? == before
                && persisted_revision(client, id).await? == (supplier_id, row_version),
            "a true no-op wrote the row"
        );
        ensure!(
            stale.outcome == "concurrency_conflict",
            "a stale no-op did not refuse"
        );
        assert_null_payload(&stale)
    }

    async fn assert_acme_overlay(client: &mut Client) -> Result<()> {
        let purchase_order_id = Uuid::from_u128(0xa400);
        let other_purchase_order_id = Uuid::from_u128(0xa410);
        let line_id = Uuid::from_u128(0xa401);
        let other_line_id = Uuid::from_u128(0xa411);
        let unused_line_id = Uuid::from_u128(0xa402);
        let location_id = Uuid::from_u128(0xa420);
        insert_receipt_fixtures(
            client,
            &ReceiptFixtureIds {
                item_id: Uuid::from_u128(0xa430),
                purchase_order_id,
                other_purchase_order_id,
                first_line_id: line_id,
                second_line_id: unused_line_id,
                other_line_id,
                location_id,
            },
        )
        .await?;

        let no_quality_status = Option::<String>::None;
        let enabled = Some(true);
        let before = stamps(client, purchase_order_id).await?;
        bind_actor(client, COMMAND_PRINCIPAL).await?;
        let updated = sqlx::query(sqlx::AssertSqlSafe(generated_sql(OVERLAY_UPDATE_SQL)))
            .bind(purchase_order_id)
            .bind(1_i32)
            .bind(true)
            .bind(enabled)
            .bind(false)
            .bind(&no_quality_status)
            .fetch_one(&mut *client)
            .await
            .context("execute exact Acme purchase_order.update SQL")?;
        ensure!(
            updated.get::<String, _>("outcome") == "updated"
                && updated.get::<Option<bool>, _>("acme_inspection_required") == Some(true)
                && updated
                    .get::<Option<String>, _>("acme_quality_status")
                    .as_deref()
                    == Some("not_required")
                && updated.get::<Option<i32>, _>("row_version") == Some(2),
            "Acme purchase_order.update returned the wrong state"
        );
        // The overlay declares no record history, and its update stamps the base columns.
        let after = stamps(client, purchase_order_id).await?;
        bind_actor(client, FIXTURE_PRINCIPAL).await?;
        ensure!(
            after.created_at == before.created_at
                && after.created_by == FIXTURE_PRINCIPAL
                && after.updated_by == COMMAND_PRINCIPAL
                && after.updated_at > before.updated_at
                && updated.get::<Option<Uuid>, _>("updated_by") == Some(COMMAND_PRINCIPAL),
            "Acme purchase_order.update did not stamp the base columns: {after:?}"
        );

        let command = ReceiptCommand {
            purchase_order_id,
            receipt_reference: "acme-dock-receipt".to_owned(),
            occurred_at: "2026-09-01T12:34:56.000000Z".to_owned(),
            line: vec![ReceiptCommandLine {
                purchase_order_line_id: line_id,
                quantity: "10.00".to_owned(),
                location_id,
            }],
        };
        let recorded = execute_record_receipt(client, &command)
            .await?
            .map_err(|refusal| anyhow::anyhow!(refusal.code))?;
        ensure!(
            recorded.purchase_order_status == "open" && recorded.row_version == 3,
            "base record_receipt returned the wrong composed result"
        );

        ensure!(
            execute_create_inspection(client, recorded.receipt_id).await?,
            "the first receipt delivery did not create its inspection"
        );
        ensure!(
            !execute_create_inspection(client, recorded.receipt_id).await?,
            "receipt redelivery inserted a second inspection"
        );

        let pending = sqlx::query(
            "SELECT count(*)::int8 AS inspection_count, min(status) AS status, \
                        min(row_version) AS row_version \
                   FROM quality_inspection WHERE receipt_id = $1",
        )
        .bind(recorded.receipt_id)
        .fetch_one(&mut *client)
        .await
        .context("read the idempotent inspection state")?;
        ensure!(
            pending.get::<i64, _>("inspection_count") == 1
                && pending.get::<Option<String>, _>("status").as_deref() == Some("pending")
                && pending.get::<Option<i32>, _>("row_version") == Some(1),
            "receipt redelivery did not preserve one pending inspection"
        );

        let detail = sqlx::query(OVERLAY_LOAD_DETAIL_SQL)
            .bind(purchase_order_id)
            .fetch_one(&mut *client)
            .await
            .context("execute exact Acme load_purchase_order_detail SQL")?;
        ensure!(
            detail.get::<bool, _>("acme_inspection_required")
                && detail.get::<String, _>("acme_quality_status") == "not_required"
                && detail.get::<i32, _>("row_version") == 3,
            "Acme detail projection returned the wrong pre-approval state"
        );

        let approved = sqlx::query(OVERLAY_APPROVE_INSPECTION_SQL)
            .bind(recorded.receipt_id)
            .bind(1_i32)
            .fetch_one(&mut *client)
            .await
            .context("execute exact Acme approve_inspection SQL")?;
        ensure!(
            approved.get::<String, _>("outcome?") == "approved"
                && approved.get::<Option<String>, _>("status").as_deref() == Some("approved")
                && approved.get::<Option<i32>, _>("row_version") == Some(2)
                && approved.get::<Option<i32>, _>("purchase_order_row_version") == Some(4),
            "Acme approve_inspection returned the wrong committed state"
        );

        let fetched = sqlx::query(sqlx::AssertSqlSafe(generated_sql(OVERLAY_GET_SQL)))
            .bind(purchase_order_id)
            .fetch_one(&mut *client)
            .await
            .context("execute exact Acme purchase_order.get SQL")?;
        ensure!(
            fetched.get::<bool, _>("acme_inspection_required")
                && fetched.get::<String, _>("acme_quality_status") == "approved"
                && fetched.get::<i32, _>("row_version") == 4,
            "Acme purchase_order.get returned the wrong approved state"
        );
        Ok(())
    }

    /// Begin a fixture write that the product binds to `principal`.
    async fn begin_as(
        client: &mut Client,
        principal: Uuid,
    ) -> Result<sqlx::Transaction<'_, sqlx::Postgres>> {
        let mut transaction = sqlx::Connection::begin(client).await?;
        wamn_postgres::bind_transaction_identity(
            &mut transaction,
            &principal.into(),
            "",
            "fixture",
            Some(FIXTURE_OPERATION),
            None,
        )
        .await?;
        Ok(transaction)
    }

    async fn connect(url: &str) -> Result<Client> {
        let mut client = wamn_postgres::connect(url)
            .await
            .context("connect to disposable PostgreSQL")?;

        sqlx::raw_sql(sqlx::AssertSqlSafe(
            ("SET statement_timeout = '10s'; \
                 SET lock_timeout = '5s'; \
                 SET transaction_timeout = '30s'")
                .to_string(),
        ))
        .execute(&mut client)
        .await
        .map(drop)
        .context("bound Receiving condition timeouts")?;
        Ok(client)
    }

    async fn execute_create_inspection(client: &mut Client, receipt_id: Uuid) -> Result<bool> {
        sqlx::raw_sql("COMMIT").execute(&mut *client).await?;
        let mut transaction = begin_as(client, COMMAND_PRINCIPAL)
            .await
            .context("begin exact create_inspection transaction")?;
        let inserted = sqlx::query(OVERLAY_INSERT_INSPECTION_SQL)
            .bind(receipt_id)
            .fetch_optional(&mut *transaction)
            .await
            .context("execute exact create_inspection insert SQL")?
            .is_some();
        let loaded = sqlx::query(OVERLAY_LOAD_INSPECTION_SQL)
            .bind(receipt_id)
            .fetch_optional(&mut *transaction)
            .await
            .context("execute exact create_inspection load SQL")?
            .context("create_inspection could not load its converged row")?;
        ensure!(
            loaded.get::<Uuid, _>("receipt_id") == receipt_id,
            "create_inspection loaded the wrong receipt"
        );
        transaction
            .commit()
            .await
            .context("commit exact create_inspection transaction")?;
        bind_actor(client, FIXTURE_PRINCIPAL).await?;
        Ok(inserted)
    }

    async fn assert_record_receipt(url: &str, client: &mut Client) -> Result<()> {
        let purchase_order_id = Uuid::from_u128(0x400);
        let other_purchase_order_id = Uuid::from_u128(0x410);
        let first_line_id = Uuid::from_u128(0x401);
        let second_line_id = Uuid::from_u128(0x402);
        let other_line_id = Uuid::from_u128(0x411);
        let location_id = Uuid::from_u128(0x420);
        let missing_id = Uuid::from_u128(0x4ff);
        insert_receipt_fixtures(
            client,
            &ReceiptFixtureIds {
                item_id: Uuid::from_u128(0x430),
                purchase_order_id,
                other_purchase_order_id,
                first_line_id,
                second_line_id,
                other_line_id,
                location_id,
            },
        )
        .await?;

        let base = ReceiptCommand {
            purchase_order_id,
            receipt_reference: "dock-receipt-1".to_owned(),
            occurred_at: "2026-08-29T12:34:56.000000Z".to_owned(),
            line: vec![
                ReceiptCommandLine {
                    purchase_order_line_id: second_line_id,
                    quantity: "5.0000".to_owned(),
                    location_id,
                },
                ReceiptCommandLine {
                    purchase_order_line_id: first_line_id,
                    quantity: "4.00".to_owned(),
                    location_id,
                },
            ],
        };

        for quantities in [["2.00", "3.00"], ["6.00", "5.00"]] {
            expect_domain_refusal(
                client,
                ReceiptCommand {
                    line: quantities
                        .into_iter()
                        .map(|quantity| ReceiptCommandLine {
                            purchase_order_line_id: first_line_id,
                            quantity: quantity.to_owned(),
                            location_id,
                        })
                        .collect(),
                    ..base.clone()
                },
                "duplicate_line",
                first_line_id,
                second_line_id,
            )
            .await?;
        }

        assert_location_delete_is_blocked(
            url,
            client,
            purchase_order_id,
            first_line_id,
            location_id,
        )
        .await?;

        expect_domain_refusal(
            client,
            ReceiptCommand {
                purchase_order_id: missing_id,
                ..base.clone()
            },
            "purchase_order_not_found",
            first_line_id,
            second_line_id,
        )
        .await?;
        expect_domain_refusal(
            client,
            ReceiptCommand {
                line: vec![ReceiptCommandLine {
                    purchase_order_line_id: missing_id,
                    quantity: "1.0".to_owned(),
                    location_id,
                }],
                ..base.clone()
            },
            "purchase_order_line_not_found",
            first_line_id,
            second_line_id,
        )
        .await?;
        expect_domain_refusal(
            client,
            ReceiptCommand {
                line: vec![ReceiptCommandLine {
                    purchase_order_line_id: other_line_id,
                    quantity: "1.0".to_owned(),
                    location_id,
                }],
                ..base.clone()
            },
            "purchase_order_line_mismatch",
            first_line_id,
            second_line_id,
        )
        .await?;
        expect_domain_refusal(
            client,
            ReceiptCommand {
                line: vec![ReceiptCommandLine {
                    purchase_order_line_id: first_line_id,
                    quantity: "1.0".to_owned(),
                    location_id: missing_id,
                }],
                ..base.clone()
            },
            "location_not_found",
            first_line_id,
            second_line_id,
        )
        .await?;
        expect_domain_refusal(
            client,
            ReceiptCommand {
                line: vec![ReceiptCommandLine {
                    purchase_order_line_id: first_line_id,
                    quantity: "11.0".to_owned(),
                    location_id,
                }],
                ..base.clone()
            },
            "quantity_exceeds_remaining",
            first_line_id,
            second_line_id,
        )
        .await?;

        let inserted = stamps(client, purchase_order_id).await?;
        let uncommitted_entries =
            command_snapshot(client, first_line_id, second_line_id, purchase_order_id)
                .await?
                .history_entries;
        bind_actor(client, COMMAND_PRINCIPAL).await?;
        let committed = execute_record_receipt(client, &base)
            .await?
            .map_err(|refusal| anyhow::anyhow!(refusal.code))?;
        bind_actor(client, FIXTURE_PRINCIPAL).await?;
        ensure!(
            committed.purchase_order_id == purchase_order_id
                && committed.purchase_order_status == "open"
                && committed.row_version == 2,
            "record_receipt commit returned the wrong result"
        );
        let committed_snapshot =
            command_snapshot(client, first_line_id, second_line_id, purchase_order_id).await?;
        // One command transaction stamps the order and its receipt with one instant.
        let receipt = sqlx::query("SELECT created_at, created_by FROM receipt WHERE id = $1")
            .bind(committed.receipt_id)
            .fetch_one(&mut *client)
            .await
            .context("read the recorded receipt stamps")?;
        let ordered = &committed_snapshot.purchase_order_stamps;
        ensure!(
            receipt.get::<DateTime<Utc>, _>("created_at") == ordered.updated_at
                && receipt.get::<Uuid, _>("created_by") == COMMAND_PRINCIPAL
                && ordered.updated_by == COMMAND_PRINCIPAL
                && ordered.created_at == inserted.created_at
                && ordered.created_by == inserted.created_by
                && ordered.updated_at > inserted.updated_at,
            "record_receipt did not stamp the order and receipt with one instant: {ordered:?}"
        );
        ensure!(
            committed_snapshot.receipt_count == 1
                && committed_snapshot.receipt_line_count == 2
                && committed_snapshot.first_received == "4.00"
                && committed_snapshot.second_received == "5.0000",
            "record_receipt commit did not preserve exact quantities"
        );
        // The order and its two lines each log one update entry.
        ensure!(
            committed_snapshot.history_entries == uncommitted_entries + 3,
            "record_receipt did not log one entry for each changed row"
        );

        expect_domain_refusal(
            client,
            ReceiptCommand {
                line: vec![ReceiptCommandLine {
                    purchase_order_line_id: first_line_id,
                    quantity: "1.0".to_owned(),
                    location_id,
                }],
                ..base.clone()
            },
            "receipt_reference_conflict",
            first_line_id,
            second_line_id,
        )
        .await?;

        let completion = ReceiptCommand {
            receipt_reference: "dock-receipt-2".to_owned(),
            line: vec![
                ReceiptCommandLine {
                    purchase_order_line_id: first_line_id,
                    quantity: "6.00".to_owned(),
                    location_id,
                },
                ReceiptCommandLine {
                    purchase_order_line_id: second_line_id,
                    quantity: "5.0000".to_owned(),
                    location_id,
                },
            ],
            ..base.clone()
        };
        let completed = execute_record_receipt(client, &completion)
            .await?
            .map_err(|refusal| anyhow::anyhow!(refusal.code))?;
        ensure!(
            completed.purchase_order_status == "complete" && completed.row_version == 3,
            "final receipt did not complete the purchase_order"
        );
        expect_domain_refusal(
            client,
            ReceiptCommand {
                receipt_reference: "dock-receipt-3".to_owned(),
                line: vec![ReceiptCommandLine {
                    purchase_order_line_id: first_line_id,
                    quantity: "1.0".to_owned(),
                    location_id,
                }],
                ..base
            },
            "purchase_order_not_open",
            first_line_id,
            second_line_id,
        )
        .await
    }

    async fn assert_location_delete_is_blocked(
        url: &str,
        client: &mut Client,
        purchase_order_id: Uuid,
        purchase_order_line_id: Uuid,
        location_id: Uuid,
    ) -> Result<()> {
        sqlx::raw_sql("COMMIT").execute(&mut *client).await?;
        let mut transaction = begin_as(client, FIXTURE_PRINCIPAL)
            .await
            .context("begin location-lock test transaction")?;
        let line = json!([{
            "purchase_order_line_id": purchase_order_line_id.hyphenated().to_string(),
            "quantity": "1.0",
            "location_id": location_id.hyphenated().to_string(),
        }]);
        sqlx::query(LOCK_PURCHASE_ORDER_SQL)
            .bind(purchase_order_id)
            .fetch_one(&mut *transaction)
            .await?;
        let validation = sqlx::query(RECORD_RECEIPT_SQL)
            .bind(purchase_order_id)
            .bind(&line)
            .bind("location-lock")
            .bind(Utc::now())
            .fetch_one(&mut *transaction)
            .await
            .context("validate lines while holding referenced location locks")?;
        ensure!(
            validation.get::<Option<String>, _>("outcome?").as_deref() == Some("ready"),
            "location-lock test fixture did not validate"
        );

        let mut contender = connect(url).await?;
        select_receiving_schema(&mut contender).await?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            ("SET lock_timeout = '250ms'").to_string(),
        ))
        .execute(&mut contender)
        .await
        .map(drop)
        .context("bound competing location delete")?;
        let delete = sqlx::query("DELETE FROM location WHERE id = $1")
            .bind(location_id)
            .execute(&mut contender)
            .await
            .map(|result| result.rows_affected());
        transaction
            .rollback()
            .await
            .context("release location-lock test transaction")?;
        bind_actor(client, FIXTURE_PRINCIPAL).await?;

        let source = delete
            .err()
            .context("referenced location delete was not blocked by validation")?;
        let database = source
            .as_database_error()
            .context("blocked location delete returned no PostgreSQL error")?;
        ensure!(
            database.code().as_deref().unwrap_or("") == "55P03",
            "blocked location delete returned SQLSTATE {}, expected 55P03",
            database.code().as_deref().unwrap_or("")
        );
        let location_remains = sqlx::query("SELECT EXISTS (SELECT 1 FROM location WHERE id = $1)")
            .bind(location_id)
            .fetch_one(&mut contender)
            .await
            .context("verify locked location remains")?
            .get::<bool, _>(0);
        ensure!(location_remains, "location-lock test deleted its fixture");
        Ok(())
    }

    /// The identities one receipt fixture seeds.
    #[expect(
        clippy::struct_field_names,
        reason = "every field names one seeded row's primary key, and the `_id` suffix is what \
                  distinguishes an identity from the value columns beside it at the call site"
    )]
    struct ReceiptFixtureIds {
        item_id: Uuid,
        purchase_order_id: Uuid,
        other_purchase_order_id: Uuid,
        first_line_id: Uuid,
        second_line_id: Uuid,
        other_line_id: Uuid,
        location_id: Uuid,
    }

    async fn insert_receipt_fixtures(client: &mut Client, ids: &ReceiptFixtureIds) -> Result<()> {
        let ReceiptFixtureIds {
            item_id,
            purchase_order_id,
            other_purchase_order_id,
            first_line_id,
            second_line_id,
            other_line_id,
            location_id,
        } = *ids;
        let item_number = format!("item-record-receipt-{item_id}");
        sqlx::query("INSERT INTO item (id, item_number) VALUES ($1, $2)")
            .bind(item_id)
            .bind(&item_number)
            .execute(&mut *client)
            .await
            .map(|result| result.rows_affected())
            .context("insert record_receipt item fixture")?;
        sqlx::query("INSERT INTO location (id, location_code) VALUES ($1, $2)")
            .bind(location_id)
            .bind(format!("dock-{location_id}"))
            .execute(&mut *client)
            .await
            .map(|result| result.rows_affected())
            .context("insert record_receipt location fixture")?;
        insert_purchase_order(
            client,
            purchase_order_id,
            &format!("PO-record-receipt-{purchase_order_id}"),
            Uuid::from_u128(0x440),
            "open",
        )
        .await?;
        insert_purchase_order(
            client,
            other_purchase_order_id,
            &format!("PO-record-receipt-{other_purchase_order_id}"),
            Uuid::from_u128(0x441),
            "open",
        )
        .await?;
        for (id, owner, line_number, quantity) in [
            (first_line_id, purchase_order_id, 1_i32, "10.00"),
            (second_line_id, purchase_order_id, 2_i32, "10.0000"),
            (other_line_id, other_purchase_order_id, 1_i32, "10.0"),
        ] {
            sqlx::query(
                "INSERT INTO purchase_order_line \
                     (id, purchase_order_id, line_number, item_id, ordered_quantity) \
                     VALUES ($1, $2, $3, $4, $5::text::numeric)",
            )
            .bind(id)
            .bind(owner)
            .bind(line_number)
            .bind(item_id)
            .bind(quantity)
            .execute(&mut *client)
            .await
            .map(|result| result.rows_affected())
            .context("insert record_receipt line fixture")?;
        }
        Ok(())
    }

    async fn execute_record_receipt(
        client: &mut Client,
        command: &ReceiptCommand,
    ) -> Result<std::result::Result<ReceiptCommandResult, DomainRefusal>> {
        sqlx::raw_sql("COMMIT").execute(&mut *client).await?;
        let mut transaction = begin_as(client, COMMAND_PRINCIPAL)
            .await
            .context("begin exact record_receipt transaction")?;
        let outcome = match record_receipt_work(&mut transaction, command).await {
            Ok(result) => {
                transaction
                    .commit()
                    .await
                    .context("commit exact record_receipt transaction")?;
                Ok(Ok(result))
            }
            Err(CommandAttemptError::Domain(refusal)) => {
                transaction
                    .rollback()
                    .await
                    .context("rollback refused record_receipt transaction")?;
                Ok(Err(refusal))
            }
            Err(CommandAttemptError::Database(source)) => {
                transaction
                    .rollback()
                    .await
                    .context("rollback failed record_receipt transaction")?;
                Err(source).context("execute exact record_receipt SQL corpus")
            }
            Err(CommandAttemptError::Internal(message)) => {
                transaction
                    .rollback()
                    .await
                    .context("rollback invalid record_receipt outcome")?;
                bail!(message)
            }
        };
        bind_actor(client, FIXTURE_PRINCIPAL).await?;
        outcome
    }

    /// The command's work statements, in the order its data access runs them.
    async fn record_receipt_work(
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        command: &ReceiptCommand,
    ) -> std::result::Result<ReceiptCommandResult, CommandAttemptError> {
        let line_json = Value::Array(
            command
                .line
                .iter()
                .map(|line| {
                    json!({
                        "purchase_order_line_id": line.purchase_order_line_id.hyphenated().to_string(),
                        "quantity": line.quantity,
                        "location_id": line.location_id.hyphenated().to_string(),
                    })
                })
                .collect(),
        );
        let purchase_order = sqlx::query(LOCK_PURCHASE_ORDER_SQL)
            .bind(command.purchase_order_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(CommandAttemptError::Database)?
            .ok_or_else(|| {
                CommandAttemptError::Domain(DomainRefusal::new("purchase_order_not_found"))
            })?;
        if purchase_order.get::<String, _>("status") != "open" {
            return Err(CommandAttemptError::Domain(DomainRefusal::new(
                "purchase_order_not_open",
            )));
        }
        let occurred_at = DateTime::parse_from_rfc3339(&command.occurred_at)
            .map_err(|error| CommandAttemptError::Internal(error.to_string()))?
            .to_utc();
        let recorded = match sqlx::query(RECORD_RECEIPT_SQL)
            .bind(command.purchase_order_id)
            .bind(&line_json)
            .bind(&command.receipt_reference)
            .bind(occurred_at)
            .fetch_one(&mut **transaction)
            .await
        {
            Ok(row) => row,
            Err(source) => {
                let database = source.as_database_error();
                if database.is_some_and(|database| {
                    database.code().as_deref().unwrap_or("") == "23505"
                        && database.constraint()
                            == Some("receipt_purchase_order_id_receipt_reference_key")
                }) {
                    return Err(CommandAttemptError::Domain(DomainRefusal::new(
                        "receipt_reference_conflict",
                    )));
                }
                return Err(CommandAttemptError::Database(source));
            }
        };
        let outcome = recorded.get::<String, _>("outcome?");
        if outcome != "ready" {
            let code = match outcome.as_str() {
                "purchase_order_line_not_found" => "purchase_order_line_not_found",
                "purchase_order_line_mismatch" => "purchase_order_line_mismatch",
                "location_not_found" => "location_not_found",
                "quantity_exceeds_remaining" => "quantity_exceeds_remaining",
                "duplicate_line" => "duplicate_line",
                _ => {
                    return Err(CommandAttemptError::Internal(format!(
                        "undeclared line validation outcome {outcome}"
                    )));
                }
            };
            return Err(CommandAttemptError::Domain(DomainRefusal::with_id(
                code,
                recorded.get("id?"),
            )));
        }
        let inserted = recorded.get::<i64, _>("inserted_line_count?");
        let updated = recorded.get::<i64, _>("updated_line_count?");
        let expected = command.line.len();
        if usize::try_from(inserted).ok() != Some(expected)
            || usize::try_from(updated).ok() != Some(expected)
        {
            return Err(CommandAttemptError::Internal(format!(
                "record_receipt affected inserted={inserted}, updated={updated}, expected={expected}"
            )));
        }
        Ok(ReceiptCommandResult {
            receipt_id: recorded.get("receipt_id"),
            purchase_order_id: command.purchase_order_id,
            purchase_order_status: recorded.get("status"),
            row_version: recorded.get("row_version"),
        })
    }

    async fn expect_domain_refusal(
        client: &mut Client,
        command: ReceiptCommand,
        expected: &'static str,
        first_line_id: Uuid,
        second_line_id: Uuid,
    ) -> Result<()> {
        let before = command_snapshot(
            client,
            first_line_id,
            second_line_id,
            Uuid::from_u128(0x400),
        )
        .await?;
        let expected_id = match expected {
            "purchase_order_line_not_found"
            | "purchase_order_line_mismatch"
            | "duplicate_line"
            | "quantity_exceeds_remaining" => {
                command.line.first().map(|line| line.purchase_order_line_id)
            }
            "location_not_found" => command.line.first().map(|line| line.location_id),
            _ => None,
        };
        let actual = execute_record_receipt(client, &command).await?;
        ensure!(
            actual
                == Err(DomainRefusal {
                    code: expected,
                    id: expected_id,
                }),
            "expected {expected}, found {actual:?}"
        );
        let after = command_snapshot(
            client,
            first_line_id,
            second_line_id,
            Uuid::from_u128(0x400),
        )
        .await?;
        ensure!(before == after, "{expected} did not roll back every write");
        Ok(())
    }

    async fn command_snapshot(
        client: &mut Client,
        first_line_id: Uuid,
        second_line_id: Uuid,
        purchase_order_id: Uuid,
    ) -> Result<CommandSnapshot> {
        let row = sqlx::query("SELECT \
                 (SELECT count(*) FROM receipt)::int8 AS receipt_count, \
                 (SELECT count(*) FROM receipt_line)::int8 AS receipt_line_count, \
                 (SELECT received_quantity::text FROM purchase_order_line WHERE id = $1) AS first_received, \
                 (SELECT received_quantity::text FROM purchase_order_line WHERE id = $2) AS second_received, \
                 (SELECT status FROM purchase_order WHERE id = $3) AS purchase_order_status, \
                 (SELECT row_version FROM purchase_order WHERE id = $3) AS row_version, \
                 (SELECT COALESCE(jsonb_agg(jsonb_build_array(id, created_at, created_by) ORDER BY id), '[]') \
                  FROM receipt) AS receipt_stamps, \
                 ((SELECT count(*) FROM purchase_order_history) \
                  + (SELECT count(*) FROM purchase_order_line_history))::int8 AS history_entries").bind(first_line_id).bind(second_line_id).bind(purchase_order_id).fetch_one(&mut *client)
            .await
            .context("snapshot record_receipt effects")?;
        Ok(CommandSnapshot {
            receipt_count: row.get("receipt_count"),
            receipt_line_count: row.get("receipt_line_count"),
            first_received: row.get("first_received"),
            second_received: row.get("second_received"),
            purchase_order_status: row.get("purchase_order_status"),
            row_version: row.get("row_version"),
            purchase_order_stamps: stamps(client, purchase_order_id).await?,
            receipt_stamps: row.get("receipt_stamps"),
            history_entries: row.get("history_entries"),
        })
    }

    async fn assert_postgres_18(client: &mut Client) -> Result<()> {
        let version = sqlx::query("SELECT current_setting('server_version_num')")
            .fetch_one(&mut *client)
            .await
            .context("read PostgreSQL server version")?
            .get::<String, _>(0)
            .parse::<u32>()
            .context("server_version_num is not numeric")?;
        ensure!(
            (180_000..190_000).contains(&version),
            "Receiving live acceptance test requires PostgreSQL 18, found {version}"
        );
        Ok(())
    }

    async fn assert_fresh_receiving_schema(client: &mut Client) -> Result<()> {
        let exists = sqlx::query("SELECT to_regnamespace('receiving') IS NOT NULL")
            .fetch_one(&mut *client)
            .await
            .context("inspect Receiving schema freshness")?
            .get::<bool, _>(0);
        ensure!(
            !exists,
            "disposable database already contains schema receiving"
        );
        Ok(())
    }

    async fn select_receiving_schema(client: &mut Client) -> Result<()> {
        sqlx::raw_sql(sqlx::AssertSqlSafe(
            ("SET search_path = receiving, public").to_string(),
        ))
        .execute(&mut *client)
        .await
        .map(drop)
        .context("select Receiving through trusted connection context")
    }

    async fn assert_status_vocabulary(client: &mut Client) -> Result<()> {
        let manifest = wamn_component_policy::PackageManifest::from_slice(&generated(MANIFEST))
            .context("parse the compiled Receiving manifest")?;
        let statuses = manifest
            .models
            .get("purchase_order")
            .and_then(|model| model.enum_fields.get("status"))
            .context("the purchase order result must state its status outcomes")?;
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
        sqlx::raw_sql("SAVEPOINT invalid_status")
            .execute(&mut *client)
            .await?;
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
            .downcast_ref::<sqlx::Error>()
            .and_then(sqlx::Error::as_database_error)
            .context("invalid status did not return a PostgreSQL database error")?;
        ensure!(
            database.code().as_deref().unwrap_or("") == "23514",
            "invalid status did not return 23514"
        );
        ensure!(
            database.constraint() == Some("purchase_order_status_check"),
            "invalid status named the wrong constraint"
        );
        sqlx::raw_sql("ROLLBACK TO SAVEPOINT invalid_status; RELEASE SAVEPOINT invalid_status")
            .execute(&mut *client)
            .await?;
        Ok(())
    }

    /// `purchase_order.supplier_id` references `supplier`, so every order and
    /// every update names a supplier that exists. A repeated name keeps the
    /// row the first call wrote.
    /// Insert a fixture supplier under a chosen id. The table owner writes it:
    /// the application role holds no INSERT on the id that a supplier's default
    /// mints. The caller's role is restored afterwards.
    async fn insert_supplier(client: &mut Client, id: Uuid) -> Result<()> {
        let role: String = sqlx::query("SELECT current_user::text")
            .fetch_one(&mut *client)
            .await?
            .get(0);
        sqlx::raw_sql(sqlx::AssertSqlSafe(("RESET ROLE").to_string()))
            .execute(&mut *client)
            .await
            .map(drop)?;
        sqlx::query(
            "INSERT INTO supplier (id, name) VALUES ($1, $2) \
                 ON CONFLICT ON CONSTRAINT supplier_id_pkey DO NOTHING",
        )
        .bind(id)
        .bind(format!("supplier {id}"))
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .context("insert supplier fixture")?;
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SET LOCAL ROLE \"{}\"",
            role.replace('"', "\"\"")
        )))
        .execute(&mut *client)
        .await
        .map(drop)?;
        Ok(())
    }

    async fn insert_purchase_order(
        client: &mut Client,
        id: Uuid,
        number: &str,
        supplier_id: Uuid,
        status: &str,
    ) -> Result<()> {
        insert_supplier(client, supplier_id).await?;
        sqlx::query(
            "INSERT INTO purchase_order \
                 (id, purchase_order_number, supplier_id, status) \
                 VALUES ($1, $2, $3, $4)",
        )
        .bind(id)
        .bind(number)
        .bind(supplier_id)
        .bind(status)
        .execute(&mut *client)
        .await
        .map(|result| result.rows_affected())
        .context("insert purchase_order fixture")?;
        Ok(())
    }

    async fn execute_update(
        client: &mut Client,
        id: Uuid,
        expected_row_version: i32,
        supplier_id: Uuid,
    ) -> Result<UpdateResult> {
        insert_supplier(client, supplier_id).await?;
        let supplier_id = Some(supplier_id);
        let row = sqlx::query(sqlx::AssertSqlSafe(generated_sql(UPDATE_SQL)))
            .bind(id)
            .bind(expected_row_version)
            .bind(true)
            .bind(supplier_id)
            .fetch_one(&mut *client)
            .await
            .context("execute exact generated purchase_order.update SQL")?;
        Ok(update_result(&row))
    }

    fn update_result(row: &Row) -> UpdateResult {
        UpdateResult {
            outcome: row.get("outcome"),
            created_at: row.get("created_at"),
            created_by: row.get("created_by"),
            id: row.get("id"),
            purchase_order_number: row.get("purchase_order_number"),
            row_version: row.get("row_version"),
            status: row.get("status"),
            supplier_id: row.get("supplier_id"),
            updated_at: row.get("updated_at"),
            updated_by: row.get("updated_by"),
        }
    }

    fn assert_null_payload(result: &UpdateResult) -> Result<()> {
        if result.created_at.is_none()
            && result.created_by.is_none()
            && result.updated_by.is_none()
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

    async fn persisted_revision(client: &mut Client, id: Uuid) -> Result<(Uuid, i32)> {
        let row = sqlx::query("SELECT supplier_id, row_version FROM purchase_order WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *client)
            .await
            .context("read persisted purchase_order revision")?;
        Ok((row.get("supplier_id"), row.get("row_version")))
    }
}

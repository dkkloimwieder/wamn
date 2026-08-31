//! Live PostgreSQL proof for the generated Receiving data-access contract.

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use anyhow::{Context as _, Result, bail, ensure};
    use chrono::{DateTime, Utc};
    use serde::Deserialize;
    use serde_json::{Value, json};
    use tokio_postgres::{Client, NoTls, Row, Transaction};
    use uuid::Uuid;
    use wamn_execution_contract::canonical_json_bytes;

    const MANIFEST: &[u8] = include_bytes!("../../../packages/receiving/wamn.json");
    const MIGRATION: &str = include_str!("../../../packages/receiving/migrations/0001_initial.sql");
    const UPDATE_SQL: &str =
        include_str!("../../../packages/receiving/generated/sql/purchase_order/update.sql");
    const CLAIM_COMMAND_SQL: &str =
        include_str!("../../../packages/receiving/command/record_receipt/claim_command.sql");
    const FINALIZE_COMMAND_SQL: &str =
        include_str!("../../../packages/receiving/command/record_receipt/finalize_command.sql");
    const FIND_REPLAY_SQL: &str =
        include_str!("../../../packages/receiving/command/record_receipt/find_replay.sql");
    const FINISH_PURCHASE_ORDER_SQL: &str = include_str!(
        "../../../packages/receiving/command/record_receipt/finish_purchase_order.sql"
    );
    const INSERT_RECEIPT_SQL: &str =
        include_str!("../../../packages/receiving/command/record_receipt/insert_receipt.sql");
    const INSERT_RECEIPT_LINE_SQL: &str =
        include_str!("../../../packages/receiving/command/record_receipt/insert_receipt_line.sql");
    const LOCK_PURCHASE_ORDER_SQL: &str =
        include_str!("../../../packages/receiving/command/record_receipt/lock_purchase_order.sql");
    const UPDATE_PURCHASE_ORDER_LINE_SQL: &str = include_str!(
        "../../../packages/receiving/command/record_receipt/update_purchase_order_line.sql"
    );
    const VALIDATE_RECEIPT_LINE_SQL: &str = include_str!(
        "../../../packages/receiving/command/record_receipt/validate_receipt_line.sql"
    );

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

    #[derive(Clone, Debug)]
    struct ReceiptCommand {
        idempotency_key: String,
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
        row_version: i64,
    }

    #[derive(Debug)]
    enum CommandAttemptError {
        Domain(&'static str),
        Database(tokio_postgres::Error),
        Internal(String),
    }

    #[derive(Debug, Eq, PartialEq)]
    struct CommandSnapshot {
        command_count: i64,
        receipt_count: i64,
        receipt_line_count: i64,
        first_received: String,
        second_received: String,
        purchase_order_status: String,
        row_version: i64,
    }

    #[tokio::test]
    #[ignore = "requires a fresh disposable PostgreSQL 18 URL in WAMN_RECEIVING_PG_URL"]
    async fn enum_and_optimistic_update_outcomes_hold_on_postgres_18() -> Result<()> {
        let url = std::env::var("WAMN_RECEIVING_PG_URL")
            .context("WAMN_RECEIVING_PG_URL must name a fresh disposable PostgreSQL 18 database")?;
        let mut client = connect(&url).await?;
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

        prove_record_receipt(&url, &mut client).await?;

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

    async fn prove_record_receipt(url: &str, client: &mut Client) -> Result<()> {
        let purchase_order_id = Uuid::from_u128(0x400);
        let other_purchase_order_id = Uuid::from_u128(0x410);
        let first_line_id = Uuid::from_u128(0x401);
        let second_line_id = Uuid::from_u128(0x402);
        let other_line_id = Uuid::from_u128(0x411);
        let location_id = Uuid::from_u128(0x420);
        let missing_id = Uuid::from_u128(0x4ff);
        insert_receipt_fixtures(
            client,
            purchase_order_id,
            other_purchase_order_id,
            first_line_id,
            second_line_id,
            other_line_id,
            location_id,
        )
        .await?;

        let base = ReceiptCommand {
            idempotency_key: "receipt-command-success".to_owned(),
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

        prove_location_delete_is_blocked(
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
                idempotency_key: "missing-purchase-order".to_owned(),
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
                idempotency_key: "missing-purchase-order-line".to_owned(),
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
                idempotency_key: "mismatched-purchase-order-line".to_owned(),
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
                idempotency_key: "missing-location".to_owned(),
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
                idempotency_key: "excess-quantity".to_owned(),
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

        let committed = execute_record_receipt(client, &base)
            .await?
            .map_err(anyhow::Error::msg)?;
        ensure!(
            committed.purchase_order_id == purchase_order_id
                && committed.purchase_order_status == "open"
                && committed.row_version == 2,
            "record_receipt commit returned the wrong result"
        );
        let committed_snapshot =
            command_snapshot(client, first_line_id, second_line_id, purchase_order_id).await?;
        ensure!(
            committed_snapshot.command_count == 1
                && committed_snapshot.receipt_count == 1
                && committed_snapshot.receipt_line_count == 2
                && committed_snapshot.first_received == "4.00"
                && committed_snapshot.second_received == "5.0000",
            "record_receipt commit did not preserve exact quantities"
        );

        let mut reordered = base.clone();
        reordered.line.reverse();
        let replay = execute_record_receipt(client, &reordered)
            .await?
            .map_err(anyhow::Error::msg)?;
        ensure!(replay == committed, "immutable replay changed its result");
        ensure!(
            command_snapshot(client, first_line_id, second_line_id, purchase_order_id).await?
                == committed_snapshot,
            "immutable replay performed a write"
        );

        let mut changed = base.clone();
        changed.line[0].quantity = "5.00".to_owned();
        expect_domain_refusal(
            client,
            changed,
            "idempotency_conflict",
            first_line_id,
            second_line_id,
        )
        .await?;

        expect_domain_refusal(
            client,
            ReceiptCommand {
                idempotency_key: "receipt-reference-conflict".to_owned(),
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
            idempotency_key: "receipt-command-complete".to_owned(),
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
            .map_err(anyhow::Error::msg)?;
        ensure!(
            completed.purchase_order_status == "complete" && completed.row_version == 3,
            "final receipt did not complete the purchase_order"
        );
        let completed_snapshot =
            command_snapshot(client, first_line_id, second_line_id, purchase_order_id).await?;
        let late_replay = execute_record_receipt(client, &reordered)
            .await?
            .map_err(anyhow::Error::msg)?;
        ensure!(
            late_replay == committed,
            "replay after a later receipt did not preserve the original result"
        );
        ensure!(
            command_snapshot(client, first_line_id, second_line_id, purchase_order_id).await?
                == completed_snapshot,
            "late immutable replay performed a write"
        );
        expect_domain_refusal(
            client,
            ReceiptCommand {
                idempotency_key: "closed-purchase-order".to_owned(),
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

    async fn prove_location_delete_is_blocked(
        url: &str,
        client: &mut Client,
        purchase_order_id: Uuid,
        purchase_order_line_id: Uuid,
        location_id: Uuid,
    ) -> Result<()> {
        let transaction = client
            .transaction()
            .await
            .context("begin location-lock proof transaction")?;
        let line = json!([{
            "purchase_order_line_id": purchase_order_line_id.hyphenated().to_string(),
            "quantity": "1.0",
            "location_id": location_id.hyphenated().to_string(),
        }]);
        let validation = transaction
            .query_one(VALIDATE_RECEIPT_LINE_SQL, &[&purchase_order_id, &line])
            .await
            .context("validate lines while holding referenced location locks")?;
        ensure!(
            validation.get::<_, Option<String>>("outcome").as_deref() == Some("ready"),
            "location-lock proof fixture did not validate"
        );

        let contender = connect(url).await?;
        select_receiving_schema(&contender).await?;
        contender
            .batch_execute("SET lock_timeout = '250ms'")
            .await
            .context("bound competing location delete")?;
        let delete = contender
            .execute("DELETE FROM location WHERE id = $1", &[&location_id])
            .await;
        transaction
            .rollback()
            .await
            .context("release location-lock proof transaction")?;

        let source = delete
            .err()
            .context("referenced location delete was not blocked by validation")?;
        let database = source
            .as_db_error()
            .context("blocked location delete returned no PostgreSQL error")?;
        ensure!(
            database.code().code() == "55P03",
            "blocked location delete returned SQLSTATE {}, expected 55P03",
            database.code().code()
        );
        let location_remains = contender
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM location WHERE id = $1)",
                &[&location_id],
            )
            .await
            .context("verify locked location remains")?
            .get::<_, bool>(0);
        ensure!(location_remains, "location-lock proof deleted its fixture");
        Ok(())
    }

    async fn insert_receipt_fixtures(
        client: &Client,
        purchase_order_id: Uuid,
        other_purchase_order_id: Uuid,
        first_line_id: Uuid,
        second_line_id: Uuid,
        other_line_id: Uuid,
        location_id: Uuid,
    ) -> Result<()> {
        let item_id = Uuid::from_u128(0x430);
        client
            .execute(
                "INSERT INTO item (id, item_number) VALUES ($1, $2)",
                &[&item_id, &"item-record-receipt"],
            )
            .await
            .context("insert record_receipt item fixture")?;
        client
            .execute(
                "INSERT INTO location (id, location_code) VALUES ($1, $2)",
                &[&location_id, &"dock-a"],
            )
            .await
            .context("insert record_receipt location fixture")?;
        insert_purchase_order(
            client,
            purchase_order_id,
            "PO-record-receipt",
            Uuid::from_u128(0x440),
            "open",
        )
        .await?;
        insert_purchase_order(
            client,
            other_purchase_order_id,
            "PO-record-receipt-other",
            Uuid::from_u128(0x441),
            "open",
        )
        .await?;
        for (id, owner, line_number, quantity) in [
            (first_line_id, purchase_order_id, 1_i32, "10.00"),
            (second_line_id, purchase_order_id, 2_i32, "10.0000"),
            (other_line_id, other_purchase_order_id, 1_i32, "10.0"),
        ] {
            client
                .execute(
                    "INSERT INTO purchase_order_line \
                     (id, purchase_order_id, line_number, item_id, ordered_quantity) \
                     VALUES ($1, $2, $3, $4, $5::text::numeric)",
                    &[&id, &owner, &line_number, &item_id, &quantity],
                )
                .await
                .context("insert record_receipt line fixture")?;
        }
        Ok(())
    }

    async fn execute_record_receipt(
        client: &mut Client,
        command: &ReceiptCommand,
    ) -> Result<std::result::Result<ReceiptCommandResult, &'static str>> {
        let transaction = client
            .transaction()
            .await
            .context("begin exact record_receipt transaction")?;
        match execute_record_receipt_in(&transaction, command).await {
            Ok(result) => {
                transaction
                    .commit()
                    .await
                    .context("commit exact record_receipt transaction")?;
                Ok(Ok(result))
            }
            Err(CommandAttemptError::Domain(literal)) => {
                transaction
                    .rollback()
                    .await
                    .context("rollback refused record_receipt transaction")?;
                Ok(Err(literal))
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
        }
    }

    async fn execute_record_receipt_in(
        transaction: &Transaction<'_>,
        command: &ReceiptCommand,
    ) -> std::result::Result<ReceiptCommandResult, CommandAttemptError> {
        let (canonical_command, line_json) = canonical_receipt_command(command);
        if let Some(row) = transaction
            .query_opt(FIND_REPLAY_SQL, &[&command.idempotency_key])
            .await
            .map_err(CommandAttemptError::Database)?
        {
            return replay_result(&row, &canonical_command);
        }
        let claim = transaction
            .query_opt(
                CLAIM_COMMAND_SQL,
                &[
                    &command.idempotency_key,
                    &canonical_command,
                    &command.purchase_order_id,
                ],
            )
            .await
            .map_err(CommandAttemptError::Database)?;
        let Some(claim) = claim else {
            let replay = transaction
                .query_opt(FIND_REPLAY_SQL, &[&command.idempotency_key])
                .await
                .map_err(CommandAttemptError::Database)?
                .ok_or_else(|| {
                    CommandAttemptError::Internal(
                        "conflicting command claim has no durable result".to_owned(),
                    )
                })?;
            return replay_result(&replay, &canonical_command);
        };
        let receipt_id = claim.get::<_, Uuid>("receipt_id");

        let purchase_order = transaction
            .query_opt(LOCK_PURCHASE_ORDER_SQL, &[&command.purchase_order_id])
            .await
            .map_err(CommandAttemptError::Database)?
            .ok_or(CommandAttemptError::Domain("purchase_order_not_found"))?;
        if purchase_order.get::<_, String>("status") != "open" {
            return Err(CommandAttemptError::Domain("purchase_order_not_open"));
        }
        let validation = transaction
            .query_one(
                VALIDATE_RECEIPT_LINE_SQL,
                &[&command.purchase_order_id, &line_json],
            )
            .await
            .map_err(CommandAttemptError::Database)?;
        let outcome = validation
            .get::<_, Option<String>>("outcome")
            .ok_or_else(|| {
                CommandAttemptError::Internal("line validator returned null".to_owned())
            })?;
        if outcome != "ready" {
            return Err(CommandAttemptError::Domain(match outcome.as_str() {
                "purchase_order_line_not_found" => "purchase_order_line_not_found",
                "purchase_order_line_mismatch" => "purchase_order_line_mismatch",
                "location_not_found" => "location_not_found",
                "quantity_exceeds_remaining" => "quantity_exceeds_remaining",
                _ => {
                    return Err(CommandAttemptError::Internal(format!(
                        "undeclared line validation outcome {outcome}"
                    )));
                }
            }));
        }
        let occurred_at = DateTime::parse_from_rfc3339(&command.occurred_at)
            .map_err(|error| CommandAttemptError::Internal(error.to_string()))?
            .to_utc();
        if let Err(source) = transaction
            .query_one(
                INSERT_RECEIPT_SQL,
                &[
                    &receipt_id,
                    &command.idempotency_key,
                    &command.purchase_order_id,
                    &command.receipt_reference,
                    &occurred_at,
                ],
            )
            .await
        {
            let database = source.as_db_error();
            if database.is_some_and(|database| {
                database.code().code() == "23505"
                    && database.constraint()
                        == Some("receipt_purchase_order_id_receipt_reference_key")
            }) {
                return Err(CommandAttemptError::Domain("receipt_reference_conflict"));
            }
            return Err(CommandAttemptError::Database(source));
        }
        let inserted = transaction
            .query(INSERT_RECEIPT_LINE_SQL, &[&receipt_id, &line_json])
            .await
            .map_err(CommandAttemptError::Database)?
            .len();
        let updated = transaction
            .query(
                UPDATE_PURCHASE_ORDER_LINE_SQL,
                &[&command.purchase_order_id, &line_json],
            )
            .await
            .map_err(CommandAttemptError::Database)?
            .len();
        let expected = command.line.len();
        if inserted != expected || updated != expected {
            return Err(CommandAttemptError::Internal(format!(
                "record_receipt affected inserted={inserted}, updated={updated}, expected={expected}"
            )));
        }
        let finished = transaction
            .query_one(FINISH_PURCHASE_ORDER_SQL, &[&command.purchase_order_id])
            .await
            .map_err(CommandAttemptError::Database)?;
        let purchase_order_status = finished.get::<_, String>("status");
        let row_version = finished.get::<_, i64>("row_version");
        let finalized = transaction
            .query_one(
                FINALIZE_COMMAND_SQL,
                &[
                    &command.idempotency_key,
                    &canonical_command,
                    &receipt_id,
                    &purchase_order_status,
                    &row_version,
                ],
            )
            .await
            .map_err(CommandAttemptError::Database)?;
        if finalized
            .get::<_, Option<String>>("purchase_order_status")
            .as_deref()
            != Some(purchase_order_status.as_str())
            || finalized.get::<_, Option<i64>>("row_version") != Some(row_version)
        {
            return Err(CommandAttemptError::Internal(
                "ledger result differs from purchase_order result".to_owned(),
            ));
        }
        Ok(ReceiptCommandResult {
            receipt_id,
            purchase_order_id: command.purchase_order_id,
            purchase_order_status,
            row_version,
        })
    }

    fn replay_result(
        row: &Row,
        canonical_command: &[u8],
    ) -> std::result::Result<ReceiptCommandResult, CommandAttemptError> {
        if row.get::<_, Vec<u8>>("canonical_command") != canonical_command {
            return Err(CommandAttemptError::Domain("idempotency_conflict"));
        }
        Ok(ReceiptCommandResult {
            receipt_id: row.get("receipt_id"),
            purchase_order_id: row.get("purchase_order_id"),
            purchase_order_status: row
                .get::<_, Option<String>>("purchase_order_status")
                .ok_or_else(|| {
                    CommandAttemptError::Internal("replay status is absent".to_owned())
                })?,
            row_version: row.get::<_, Option<i64>>("row_version").ok_or_else(|| {
                CommandAttemptError::Internal("replay row_version is absent".to_owned())
            })?,
        })
    }

    fn canonical_receipt_command(command: &ReceiptCommand) -> (Vec<u8>, Value) {
        let mut lines = command.line.clone();
        lines.sort_by_key(|line| line.purchase_order_line_id);
        let line = lines
            .iter()
            .map(|line| {
                json!({
                    "purchase_order_line_id": line.purchase_order_line_id.hyphenated().to_string(),
                    "quantity": line.quantity,
                    "location_id": line.location_id.hyphenated().to_string(),
                })
            })
            .collect::<Vec<_>>();
        let line = Value::Array(line);
        let command_value = json!({
            "purchase_order_id": command.purchase_order_id.hyphenated().to_string(),
            "receipt_reference": command.receipt_reference,
            "occurred_at": command.occurred_at,
            "line": line,
        });
        (
            canonical_json_bytes(&command_value),
            command_value["line"].clone(),
        )
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
        let actual = execute_record_receipt(client, &command).await?;
        ensure!(
            actual == Err(expected),
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
        client: &Client,
        first_line_id: Uuid,
        second_line_id: Uuid,
        purchase_order_id: Uuid,
    ) -> Result<CommandSnapshot> {
        let row = client
            .query_one(
                "SELECT \
                 (SELECT count(*) FROM record_receipt_command)::int8 AS command_count, \
                 (SELECT count(*) FROM receipt)::int8 AS receipt_count, \
                 (SELECT count(*) FROM receipt_line)::int8 AS receipt_line_count, \
                 (SELECT received_quantity::text FROM purchase_order_line WHERE id = $1) AS first_received, \
                 (SELECT received_quantity::text FROM purchase_order_line WHERE id = $2) AS second_received, \
                 (SELECT status FROM purchase_order WHERE id = $3) AS purchase_order_status, \
                 (SELECT row_version FROM purchase_order WHERE id = $3) AS row_version",
                &[&first_line_id, &second_line_id, &purchase_order_id],
            )
            .await
            .context("snapshot record_receipt effects")?;
        Ok(CommandSnapshot {
            command_count: row.get("command_count"),
            receipt_count: row.get("receipt_count"),
            receipt_line_count: row.get("receipt_line_count"),
            first_received: row.get("first_received"),
            second_received: row.get("second_received"),
            purchase_order_status: row.get("purchase_order_status"),
            row_version: row.get("row_version"),
        })
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
        expected_row_version: i64,
        supplier_id: Uuid,
    ) -> Result<UpdateResult> {
        let supplier_id = Some(supplier_id);
        let row = client
            .query_one(
                UPDATE_SQL,
                &[&id, &expected_row_version, &true, &supplier_id],
            )
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

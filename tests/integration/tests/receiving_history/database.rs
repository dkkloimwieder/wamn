//! Fixture authority and committed-state observations for real Receiving commands.

use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio_postgres::{Client, NoTls};
use uuid::Uuid;

#[derive(Debug)]
pub struct Db {
    pub client: Client,
    connection: JoinHandle<Result<(), tokio_postgres::Error>>,
}

impl Drop for Db {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

pub async fn connect(url: &str) -> Result<Db> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect Receiving fixture observer")?;
    let db = Db {
        client,
        connection: tokio::spawn(connection),
    };
    db.client
        .batch_execute(
            "SET statement_timeout = '15s'; SET application_name = 'receiving-history-observer'",
        )
        .await
        .context("bound Receiving fixture observer statements")?;
    Ok(db)
}

#[derive(Clone, Debug)]
pub struct Fixture {
    pub id: Uuid,
    pub line_ids: [Uuid; 2],
    pub location_id: Uuid,
    pub supplier_id: Uuid,
    pub key_prefix: String,
}

pub async fn seed(client: &Client, ordered: [u16; 2], status: &str) -> Result<Fixture> {
    ensure!(
        ordered.iter().all(|amount| *amount > 0),
        "fixture orders must be positive"
    );
    ensure!(
        matches!(status, "open" | "cancelled"),
        "zero-received fixture must be open or cancelled"
    );
    let fixture = Fixture {
        id: Uuid::new_v4(),
        line_ids: [Uuid::new_v4(), Uuid::new_v4()],
        location_id: Uuid::new_v4(),
        supplier_id: Uuid::new_v4(),
        key_prefix: format!("history-{}", Uuid::new_v4()),
    };
    let item_id = Uuid::new_v4();
    client.execute(
        r#"WITH item AS (
            INSERT INTO receiving.item (id, item_number)
            VALUES ($4, $7 || '-item') RETURNING id
        ), location AS (
            INSERT INTO receiving.location (id, location_code)
            VALUES ($5, $7 || '-dock')
        ), purchase AS (
            INSERT INTO receiving.purchase_order
                (id, purchase_order_number, supplier_id, status, row_version, created_at, updated_at)
            VALUES ($1, $7 || '-po', $6, $8, 1,
                '2026-09-09T00:00:00Z', '2026-09-09T00:00:00Z')
            RETURNING id
        )
        INSERT INTO receiving.purchase_order_line
            (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity)
        SELECT $2::uuid, purchase.id, 1, item.id, $9::int4::numeric, 0
        FROM purchase CROSS JOIN item
        UNION ALL
        SELECT $3::uuid, purchase.id, 2, item.id, $10::int4::numeric, 0
        FROM purchase CROSS JOIN item"#,
        &[
            &fixture.id, &fixture.line_ids[0], &fixture.line_ids[1], &item_id,
            &fixture.location_id, &fixture.supplier_id, &fixture.key_prefix, &status,
            &i32::from(ordered[0]), &i32::from(ordered[1]),
        ],
    ).await.context("seed distinct Receiving business fixture in one statement")?;
    Ok(fixture)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub value: Value,
    pub status: String,
    pub revision: i64,
    pub received: [u16; 2],
    pub receipt_totals: [u16; 2],
    pub claim_count: usize,
    pub receipt_count: usize,
    pub receipt_line_count: usize,
}

pub async fn snapshot(client: &Client, fixture: &Fixture) -> Result<Snapshot> {
    let value: Value = client
        .query_one(
            r#"WITH orders AS (
            SELECT * FROM receiving.purchase_order WHERE id = $1
        ), lines AS (
            SELECT * FROM receiving.purchase_order_line WHERE purchase_order_id = $1
        ), claims AS (
            SELECT * FROM receiving.record_receipt_command
            WHERE purchase_order_id = $1 OR starts_with(idempotency_key, $2)
        ), receipts AS (
            SELECT * FROM receiving.receipt
            WHERE purchase_order_id = $1 OR starts_with(idempotency_key, $2)
        ), receipt_lines AS (
            SELECT * FROM receiving.receipt_line
            WHERE receipt_id IN (SELECT id FROM receipts)
                OR purchase_order_line_id IN (SELECT id FROM lines)
        )
        SELECT jsonb_build_object(
            'purchase_order', (SELECT to_jsonb(o) FROM orders o),
            'lines', COALESCE((SELECT jsonb_agg(to_jsonb(l) || jsonb_build_object(
                'ordered_quantity', l.ordered_quantity::text,
                'received_quantity', l.received_quantity::text)
                ORDER BY l.line_number, l.id) FROM lines l), '[]'::jsonb),
            'claims', COALESCE((SELECT jsonb_agg(to_jsonb(c) ORDER BY c.idempotency_key)
                FROM claims c), '[]'::jsonb),
            'receipts', COALESCE((SELECT jsonb_agg(to_jsonb(r) ORDER BY r.id)
                FROM receipts r), '[]'::jsonb),
            'receipt_lines', COALESCE((SELECT jsonb_agg(to_jsonb(l) || jsonb_build_object(
                'quantity', l.quantity::text) ORDER BY l.id)
                FROM receipt_lines l), '[]'::jsonb))"#,
            &[&fixture.id, &fixture.key_prefix],
        )
        .await
        .context("read one committed Receiving business snapshot")?
        .get(0);
    let status = value["purchase_order"]["status"]
        .as_str()
        .context("snapshot purchase order has no status")?
        .to_owned();
    let revision = value["purchase_order"]["row_version"]
        .as_i64()
        .context("snapshot purchase order has no integer revision")?;
    let lines = value["lines"]
        .as_array()
        .context("snapshot has no ordered lines")?;
    ensure!(
        lines.len() == 2,
        "fixture must retain exactly two ordered lines: {value}"
    );
    let mut received = [0; 2];
    for (index, line) in lines.iter().enumerate() {
        ensure!(
            line["id"] == fixture.line_ids[index].to_string(),
            "fixture ordered line identity changed: {line}"
        );
        received[index] = amount(
            line["received_quantity"]
                .as_str()
                .context("snapshot received quantity has no exact text")?,
        )?;
    }
    let claim_count = value["claims"]
        .as_array()
        .context("snapshot has no claims")?
        .len();
    let receipt_count = value["receipts"]
        .as_array()
        .context("snapshot has no receipts")?
        .len();
    let receipt_lines = value["receipt_lines"]
        .as_array()
        .context("snapshot has no receipt lines")?;
    let receipt_line_count = receipt_lines.len();
    let mut receipt_totals = [0u16; 2];
    for line in receipt_lines {
        let index = fixture
            .line_ids
            .iter()
            .position(|id| line["purchase_order_line_id"] == id.to_string())
            .with_context(|| format!("receipt references a line outside this fixture: {line}"))?;
        let quantity = amount(
            line["quantity"]
                .as_str()
                .context("receipt quantity has no exact text")?,
        )?;
        receipt_totals[index] = receipt_totals[index]
            .checked_add(quantity)
            .context("receipt total exceeds the fixture quantity range")?;
    }
    Ok(Snapshot {
        value,
        status,
        revision,
        received,
        receipt_totals,
        claim_count,
        receipt_count,
        receipt_line_count,
    })
}

pub fn assert_state(
    snapshot: &Snapshot,
    totals: [u16; 2],
    status: &str,
    revision: i64,
    commits: usize,
) -> Result<()> {
    ensure!(
        snapshot.received == totals,
        "received quantities differ: expected {totals:?}, observed {snapshot:?}"
    );
    ensure!(
        snapshot.receipt_totals == totals,
        "receipt history totals differ: expected {totals:?}, observed {snapshot:?}"
    );
    ensure!(
        snapshot.status == status,
        "purchase order status differs: expected {status}, observed {snapshot:?}"
    );
    ensure!(
        snapshot.revision == revision,
        "purchase order revision differs: expected {revision}, observed {snapshot:?}"
    );
    ensure!(
        snapshot.claim_count == commits && snapshot.receipt_count == commits,
        "committed receipt and claim counts differ: expected {commits}, observed {snapshot:?}"
    );
    Ok(())
}

pub(super) fn amount(text: &str) -> Result<u16> {
    let (whole, fraction) = text
        .split_once('.')
        .map_or((text, None), |(whole, fraction)| (whole, Some(fraction)));
    ensure!(
        !whole.is_empty() && whole.bytes().all(|byte| byte.is_ascii_digit()),
        "fixture quantity is not a nonnegative integer: {text}"
    );
    if let Some(fraction) = fraction {
        ensure!(
            !fraction.is_empty() && fraction.bytes().all(|byte| byte == b'0'),
            "fixture quantity contains a fractional value: {text}"
        );
    }
    whole
        .parse()
        .with_context(|| format!("fixture quantity exceeds u16: {text}"))
}

pub async fn wait_for_blocked(
    client: &Client,
    blocker_pid: i32,
    minimum: usize,
) -> Result<Vec<i32>> {
    ensure!(
        minimum > 0,
        "lock observation must require at least one command"
    );
    let mut observed = Vec::new();
    // Observe before the default guest statement timeout of five seconds.
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows = client.query(
                r#"WITH RECURSIVE activity AS MATERIALIZED (
                    SELECT a.pid, a.usename, a.wait_event_type, a.wait_event,
                        pg_blocking_pids(a.pid) AS blockers,
                        a.state = 'active' AND a.wait_event_type = 'Lock'
                            AND NOT r.rolsuper
                            AND pg_has_role(a.usesysid, 'wamn_app', 'MEMBER')
                            AND a.query ~* '\m(record_receipt_command|purchase_order|purchase_order_line|receipt|receipt_line)\M'
                            AS receiving_command
                    FROM pg_stat_activity a JOIN pg_roles r ON r.oid = a.usesysid
                    WHERE a.datname = current_database()
                        AND a.pid <> pg_backend_pid() AND a.pid <> $1
                ), blocked(pid) AS (
                    SELECT pid FROM activity WHERE $1 = ANY(blockers)
                    UNION
                    SELECT a.pid FROM activity a JOIN blocked b ON b.pid = ANY(a.blockers)
                )
                SELECT a.pid, a.usename, a.wait_event_type, a.wait_event, a.blockers,
                    COALESCE(a.receiving_command, false) AND b.pid IS NOT NULL AS matched
                FROM activity a LEFT JOIN blocked b ON b.pid = a.pid ORDER BY a.pid"#,
                &[&blocker_pid],
            ).await.context("observe actual Receiving guest lock waits")?;
            observed.clear();
            let mut matched = Vec::new();
            for row in rows {
                let pid: i32 = row.get("pid");
                let is_match: bool = row.get("matched");
                if is_match {
                    matched.push(pid);
                }
                observed.push(json!({
                    "pid": pid,
                    "role": row.get::<_, String>("usename"),
                    "wait_event_type": row.get::<_, Option<String>>("wait_event_type"),
                    "wait_event": row.get::<_, Option<String>>("wait_event"),
                    "blockers": row.get::<_, Vec<i32>>("blockers"),
                    "receiving_command_blocked": is_match,
                }));
            }
            if matched.len() >= minimum {
                return Ok(matched);
            }
            tokio::task::yield_now().await;
        }
    }).await;
    result.with_context(|| format!(
        "did not observe {minimum} Receiving guest commands blocked by PID {blocker_pid}; safe activity: {observed:?}"
    ))?
}

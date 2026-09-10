//! Real Receiving command histories, rollback, replay, authority and contention.

#[path = "receiving_history/database.rs"]
mod database;
#[path = "receiving_history/model.rs"]
mod model;

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, Result, ensure};
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestError, TestRunner};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_postgres::Client;
use uuid::Uuid;

use database::{Fixture, assert_state, connect, seed, snapshot, wait_for_blocked};
use model::{Expected, History, Receipt, Step};

const RECEIPT_PATH: &str = "/receiving/record_receipt";
const RECEIPT_OPERATION: &str = "wamn-receiving:receiving/record-receipt@1.0.0";
const OCCURRED_AT: &str = "2026-09-09T12:00:00.000000Z";

// Secrets cross only through this private file. The evidence excludes them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inputs {
    project_pg_url: String,
    route_endpoint: String,
    route_host: String,
    route_caller_secret: PathBuf,
    tenant: String,
    caller_role: String,
    evidence_file: PathBuf,
    source_commit: String,
    component_digests: BTreeMap<String, String>,
    corpus_sha256: String,
    seed: u64,
    cases: u32,
    history: Option<History>,
}

#[derive(Clone)]
struct Route {
    client: reqwest::Client,
    endpoint: String,
    host: String,
    bearer: String,
}

#[derive(Debug)]
struct Response {
    status: reqwest::StatusCode,
    body: Value,
}

impl Route {
    async fn post(&self, path: &str, body: &Value) -> Result<Response> {
        let reply = self
            .client
            .post(format!("{}{path}", self.endpoint))
            .header("Host", &self.host)
            .bearer_auth(&self.bearer)
            .json(body)
            .send()
            .await
            .context("call the released Receiving route")?;
        let status = reply.status();
        let bytes = reply.bytes().await.context("read the Receiving response")?;
        let body = serde_json::from_slice(&bytes).context("decode the Receiving response")?;
        Ok(Response { status, body })
    }
}

fn item(response: &Response, request_id: &str) -> Result<Value> {
    ensure!(
        response.status == reqwest::StatusCode::OK,
        "command {request_id}: status={} body={}",
        response.status,
        response.body
    );
    let items = response
        .body
        .as_array()
        .context("response is not an array")?;
    ensure!(
        items.len() == 1,
        "command {request_id} returned {} items",
        items.len()
    );
    ensure!(
        items[0]["request_id"] == request_id,
        "response lost request identity"
    );
    Ok(items[0].clone())
}

fn succeeded(response: &Response, request_id: &str) -> Result<Value> {
    let item = item(response, request_id)?;
    ensure!(
        item.get("error").is_none(),
        "command {request_id} refused: {item}"
    );
    item.get("value")
        .cloned()
        .context("successful item has no value")
}

fn refused(response: &Response, request_id: &str, code: &str) -> Result<()> {
    let item = item(response, request_id)?;
    ensure!(
        item["error"]["code"] == code && item.get("value").is_none(),
        "command {request_id} expected {code}: {item}"
    );
    Ok(())
}

fn receipt_body(fixture: &Fixture, receipt: Receipt, request_id: &str) -> Value {
    json!([{"request_id":request_id,"value":{
        "idempotency_key":format!("{}-{}",fixture.key_prefix,receipt.key),
        "purchase_order_id":fixture.id.to_string(),
        "receipt_reference":format!("{}-{}",fixture.key_prefix,receipt.key),
        "occurred_at":OCCURRED_AT,
        "line":[{"purchase_order_line_id":fixture.line_ids[receipt.line].to_string(),
            "quantity":receipt.quantity.to_string(),"location_id":fixture.location_id.to_string()}]
    }}])
}

fn record(evidence: &mut File, value: Value) -> Result<()> {
    serde_json::to_writer(&mut *evidence, &value)?;
    evidence.write_all(b"\n")?;
    evidence.flush()?;
    Ok(())
}

// These tags identify test assertions, not production wire errors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct HistoryOutcome {
    operation: &'static str,
    expected: &'static str,
    http_status: u16,
    refusal: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct HistoryFailure {
    property: &'static str,
    outcome: HistoryOutcome,
}

impl fmt::Display for HistoryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {:?}", self.property, self.outcome)
    }
}

fn business<T>(result: Result<T>, property: &'static str, outcome: &HistoryOutcome) -> Result<T> {
    result.with_context(|| HistoryFailure {
        property,
        outcome: outcome.clone(),
    })
}

fn history_assert(
    condition: bool,
    property: &'static str,
    outcome: &HistoryOutcome,
    detail: fmt::Arguments<'_>,
) -> Result<()> {
    if condition {
        Ok(())
    } else {
        business(Err(anyhow::anyhow!("{detail}")), property, outcome)
    }
}

#[derive(Debug, Default)]
struct ShrinkFailures {
    target: Option<HistoryFailure>,
    infrastructure: Option<(History, anyhow::Error)>,
}

fn shrink_result(
    failures: &mut ShrinkFailures,
    history: &History,
    result: Result<()>,
) -> std::result::Result<(), TestCaseError> {
    let Err(error) = result else {
        return Ok(());
    };
    if let Some(failure) = error.downcast_ref::<HistoryFailure>() {
        match &failures.target {
            Some(target) if target != failure => {
                return Err(TestCaseError::reject(
                    "different business property or outcome",
                ));
            }
            None => failures.target = Some(failure.clone()),
            Some(_) => {}
        }
        Err(TestCaseError::fail(format!("{error:#}")))
    } else {
        failures.infrastructure = Some((history.clone(), error));
        if failures.target.is_some() {
            // A broken fixture or transport cannot become a smaller counterexample.
            Ok(())
        } else {
            // Stop generation. Later shrink callbacks perform no further I/O.
            Err(TestCaseError::fail("history infrastructure failed"))
        }
    }
}

#[derive(Debug)]
struct Reproduction {
    outcome: &'static str,
    infrastructure: Option<anyhow::Error>,
}

async fn reproduce_history(
    db: &Client,
    route: &Route,
    selected: &History,
    evidence: &mut File,
    expected: Option<&HistoryFailure>,
    origin: &'static str,
    infrastructure_during_shrink: bool,
) -> Result<Reproduction> {
    let repeated = history(db, route, selected, evidence).await;
    let observed = repeated
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<HistoryFailure>());
    let same_failure = expected.is_some() && expected == observed;
    let outcome = match (&repeated, observed) {
        (Ok(()), _) => "intermittent",
        (Err(_), None) => "infrastructure",
        (Err(_), Some(_)) if same_failure => "repeated",
        (Err(_), Some(_)) => "changed-property",
    };
    record(
        evidence,
        json!({"case":"history-reproduction","origin":origin,"history":selected,
        "expected_failure":expected,"observed_failure":observed,"same_failure":same_failure,
        "result":outcome,"infrastructure_during_shrink":infrastructure_during_shrink,
        "confirmed_business_failure":same_failure && !infrastructure_during_shrink,
        "error":repeated.as_ref().err().map(|error|format!("{error:#}"))}),
    )?;
    let infrastructure = repeated
        .err()
        .filter(|error| error.downcast_ref::<HistoryFailure>().is_none());
    Ok(Reproduction {
        outcome,
        infrastructure,
    })
}

async fn explicit_history(
    db: &Client,
    route: &Route,
    selected: &History,
    evidence: &mut File,
) -> Result<()> {
    let Err(error) = history(db, route, selected, evidence).await else {
        return Ok(());
    };
    let Some(failure) = error.downcast_ref::<HistoryFailure>() else {
        return Err(error).context("explicit history infrastructure failed");
    };
    let reproduction = reproduce_history(
        db,
        route,
        selected,
        evidence,
        Some(failure),
        "explicit",
        false,
    )
    .await?;
    if let Some(infrastructure) = reproduction.infrastructure {
        return Err(infrastructure).context("explicit history reproduction infrastructure failed");
    }
    Err(error).with_context(|| {
        format!(
            "explicit history failed: reproduction={} history={}",
            reproduction.outcome,
            serde_json::to_string(selected).expect("history serializes"),
        )
    })
}

async fn history(db: &Client, route: &Route, history: &History, evidence: &mut File) -> Result<()> {
    ensure!(
        !history.steps.is_empty() && history.steps.len() <= 12,
        "history requires 1..=12 steps"
    );
    ensure!(
        history
            .steps
            .iter()
            .all(|step| !matches!(step, Step::Receive { line, .. } if *line >= 2)),
        "history names an absent line"
    );
    let fixture = seed(db, history.ordered, "open").await?;
    let mut original = BTreeMap::<u8, Value>::new();
    let mut completed = 0usize;
    let result = async {
        let initial = snapshot(db, &fixture).await?;
        assert_state(&initial, [0, 0], "open", 1, 0)?;
        for (index, expected) in history.ordered.iter().enumerate() {
            let ordered = initial.value["lines"][index]["ordered_quantity"]
                .as_str()
                .context("fixture ordered quantity has no exact text")?;
            ensure!(
                database::amount(ordered)? == *expected,
                "fixture ordered quantity differs on line {index}: {ordered}"
            );
        }
        let mut state = model::model(history.ordered);
        for (index, &step) in history.steps.iter().enumerate() {
            let before = snapshot(db, &fixture).await?;
            let request_id = format!("step-{index}");
            let receipt = model::receipt(&state, step);
            let supplier = Uuid::new_v4();
            let (path, body) = if let Some(receipt) = receipt {
                (RECEIPT_PATH, receipt_body(&fixture, receipt, &request_id))
            } else {
                let Step::Update { stale } = step else {
                    unreachable!("only update has no receipt")
                };
                let revision = if stale {
                    state.revision - 1
                } else {
                    state.revision
                };
                (
                    "/purchase_order/update",
                    json!([{"request_id":request_id,
                    "id":fixture.id.to_string(),"expected_row_version":revision.to_string(),
                    "change":{"supplier_id":supplier.to_string()}}]),
                )
            };
            let expected = model::apply(&mut state, step);
            let response = route.post(path, &body).await?;
            ensure!(
                !response.status.is_server_error(),
                "history route server failure at step {index}: {response:?}"
            );
            let outcome = HistoryOutcome {
                operation: path,
                expected: match expected {
                    Expected::Committed { replay: true, .. } => "replayed",
                    Expected::Committed { .. } => "committed",
                    Expected::Refused(code) => code,
                },
                http_status: response.status.as_u16(),
                refusal: response
                    .body
                    .as_array()
                    .and_then(|items| items.first())
                    .and_then(|item| item["error"]["code"].as_str())
                    .map(str::to_owned),
            };
            ensure!(
                !matches!(
                    outcome.refusal.as_deref(),
                    Some("timeout" | "retry" | "internal_error")
                ),
                "history command completion is uncertain at step {index}: {response:?}"
            );
            match expected {
                Expected::Committed {
                    status,
                    revision,
                    replay,
                } => {
                    let value = business(
                        succeeded(&response, &request_id),
                        "REC-HISTORY/commit-response",
                        &outcome,
                    )?;
                    history_assert(
                        value["row_version"] == revision.to_string(),
                        "REC-REVISION/response",
                        &outcome,
                        format_args!("REC-HISTORY revision at step {index}: {value}"),
                    )?;
                    if let Some(receipt) = receipt {
                        history_assert(
                            value["purchase_order_status"] == status
                                && value["purchase_order_id"] == fixture.id.to_string(),
                            "REC-HISTORY/receipt-result",
                            &outcome,
                            format_args!("REC-HISTORY receipt result at step {index}: {value}"),
                        )?;
                        business(
                            value["receipt_id"]
                                .as_str()
                                .context("missing receipt ID")
                                .and_then(|id| Uuid::parse_str(id).context("invalid receipt ID")),
                            "REC-HISTORY/receipt-id",
                            &outcome,
                        )?;
                        if replay {
                            history_assert(
                                original.get(&receipt.key) == Some(&value),
                                "REC-REPLAY/original-result",
                                &outcome,
                                format_args!("REC-REPLAY original result changed at step {index}"),
                            )?;
                            let replayed = snapshot(db, &fixture).await?;
                            history_assert(
                                replayed.value == before.value,
                                "REC-REPLAY/business-state",
                                &outcome,
                                format_args!("REC-REPLAY changed business state at step {index}"),
                            )?;
                        } else {
                            history_assert(
                                original.insert(receipt.key, value).is_none(),
                                "REC-REPLAY/claim-identity",
                                &outcome,
                                format_args!("a new receipt replaced a committed key"),
                            )?;
                        }
                    } else {
                        history_assert(
                            value["supplier_id"] == supplier.to_string()
                                && value["status"] == status,
                            "REC-REVISION/update-result",
                            &outcome,
                            format_args!(
                                "REC-REVISION update returned different business state: {value}"
                            ),
                        )?;
                    }
                }
                Expected::Refused(code) => {
                    business(
                        refused(&response, &request_id, code),
                        "REC-REFUSAL/response",
                        &outcome,
                    )?;
                    let refused = snapshot(db, &fixture).await?;
                    history_assert(
                        refused.value == before.value,
                        "REC-REFUSAL/business-state",
                        &outcome,
                        format_args!("REC-REFUSAL changed business state at step {index}: {code}"),
                    )?;
                }
            }
            let after = snapshot(db, &fixture).await?;
            let property = if after.received != state.received {
                "REC-HISTORY/received-totals"
            } else if after.receipt_totals != state.received {
                "REC-HISTORY/receipt-history-totals"
            } else if after.status != state.status() {
                "REC-HISTORY/status"
            } else if after.revision != state.revision {
                "REC-REVISION/persisted"
            } else {
                "REC-HISTORY/receipt-claim-counts"
            };
            business(
                assert_state(
                    &after,
                    state.received,
                    state.status(),
                    state.revision,
                    state.receipt_count(),
                ),
                property,
                &outcome,
            )?;
            history_assert(
                after.receipt_line_count == after.receipt_count,
                "REC-HISTORY/receipt-facts",
                &outcome,
                format_args!("one-line receipt history has extra or missing facts: {after:?}"),
            )?;
            let facts = after.value["receipt_lines"]
                .as_array()
                .context("receipt snapshot has no fact array")?;
            for (&key, result) in &original {
                let expected = model::receipt(&state, Step::Replay { key })
                    .expect("a replay step resolves a receipt body");
                let matched: Vec<_> = facts
                    .iter()
                    .filter(|fact| fact["receipt_id"] == result["receipt_id"])
                    .collect();
                history_assert(
                    matched.len() == 1,
                    "REC-HISTORY/receipt-facts",
                    &outcome,
                    format_args!(
                        "receipt {} has {} facts",
                        result["receipt_id"],
                        matched.len()
                    ),
                )?;
                let fact = matched[0];
                let quantity = business(
                    fact["quantity"]
                        .as_str()
                        .context("receipt fact has no exact quantity")
                        .and_then(database::amount),
                    "REC-HISTORY/receipt-facts",
                    &outcome,
                )?;
                history_assert(
                    fact["purchase_order_line_id"] == fixture.line_ids[expected.line].to_string()
                        && fact["location_id"] == fixture.location_id.to_string()
                        && quantity == expected.quantity,
                    "REC-HISTORY/receipt-facts",
                    &outcome,
                    format_args!(
                        "receipt fact differs from committed command {expected:?}: {fact}"
                    ),
                )?;
            }
            completed += 1;
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let failure = result
        .as_ref()
        .err()
        .and_then(|error| error.downcast_ref::<HistoryFailure>());
    record(
        evidence,
        json!({"case":"history","fixture":fixture.id,"history":history,
        "completed_steps":completed,"result":if result.is_ok(){"pass"}else{"fail"},
        "failure":failure,
        "failure_class":if result.is_ok(){"none"}else if failure.is_some(){"business"}else{"infrastructure"},
        "error":result.as_ref().err().map(|e|format!("{e:#}"))}),
    )?;
    result
}

async fn invalid_status(db: &Client, route: &Route, evidence: &mut File) -> Result<()> {
    let fixture = seed(db, [2, 2], "cancelled").await?;
    let before = snapshot(db, &fixture).await?;
    let body = receipt_body(
        &fixture,
        Receipt {
            key: 0,
            line: 0,
            quantity: 1,
        },
        "cancelled",
    );
    refused(
        &route.post(RECEIPT_PATH, &body).await?,
        "cancelled",
        "purchase_order_not_open",
    )?;
    ensure!(
        snapshot(db, &fixture).await?.value == before.value,
        "REC-REFUSAL invalid status changed business state"
    );
    record(
        evidence,
        json!({"case":"invalid-status","fixture":fixture.id,"result":"pass"}),
    )
}

async fn mixed_items(db: &Client, route: &Route, evidence: &mut File) -> Result<()> {
    let fixture = seed(db, [3, 1], "open").await?;
    let commands = [
        Receipt {
            key: 0,
            line: 0,
            quantity: 2,
        },
        Receipt {
            key: 1,
            line: 0,
            quantity: 2,
        },
        Receipt {
            key: 2,
            line: 0,
            quantity: 1,
        },
    ];
    let body = Value::Array(
        commands
            .into_iter()
            .enumerate()
            .map(|(index, command)| {
                receipt_body(&fixture, command, &format!("item-{index}"))[0].clone()
            })
            .collect(),
    );
    let response = route.post(RECEIPT_PATH, &body).await?;
    let items = response
        .body
        .as_array()
        .context("mixed envelope is not an array")?;
    ensure!(
        items.len() == 3,
        "mixed envelope did not settle three command items"
    );
    for (index, item) in items.iter().enumerate() {
        let individual = Response {
            status: response.status,
            body: json!([item]),
        };
        let request = format!("item-{index}");
        if index == 1 {
            refused(&individual, &request, "quantity_exceeds_remaining")?;
        } else {
            let value = succeeded(&individual, &request)?;
            let revision = if index == 0 { "2" } else { "3" };
            ensure!(
                value["row_version"] == revision,
                "mixed item result lost its own transaction"
            );
        }
    }
    assert_state(&snapshot(db, &fixture).await?, [3, 0], "open", 3, 2)?;
    record(
        evidence,
        json!({"case":"mixed-envelope","fixture":fixture.id,
        "committed_items":2,"refused_items":1,"result":"pass"}),
    )
}

async fn competing_receipts(
    db: &Client,
    route: &Route,
    inputs: &Inputs,
    evidence: &mut File,
    same_key: bool,
) -> Result<()> {
    let fixture = seed(db, [3, 1], "open").await?;
    let controller = connect(&inputs.project_pg_url).await?;
    controller.client.batch_execute("BEGIN").await?;
    controller
        .client
        .query_one(
            "SELECT id FROM receiving.purchase_order WHERE id=$1 FOR UPDATE",
            &[&fixture.id],
        )
        .await?;
    let pid: i32 = controller
        .client
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    let first_body = receipt_body(
        &fixture,
        Receipt {
            key: 0,
            line: 0,
            quantity: 2,
        },
        "competing-a",
    );
    let second_body = receipt_body(
        &fixture,
        Receipt {
            key: if same_key { 0 } else { 1 },
            line: 0,
            quantity: 2,
        },
        "competing-b",
    );
    let first_route = route.clone();
    let first = tokio::spawn(async move { first_route.post(RECEIPT_PATH, &first_body).await });
    let second_route = route.clone();
    let second = tokio::spawn(async move { second_route.post(RECEIPT_PATH, &second_body).await });
    let blocked = wait_for_blocked(db, pid, 2).await;
    // Release the controller even when the overlap assertion fails.
    controller.client.batch_execute("ROLLBACK").await?;
    let (a, b) = tokio::join!(first, second);
    let a = a.context("join first competing route")??;
    let b = b.context("join second competing route")??;
    let blocked = blocked?;
    if same_key {
        let a = succeeded(&a, "competing-a")?;
        let b = succeeded(&b, "competing-b")?;
        ensure!(
            a == b,
            "REC-REPLAY overlapping same-key calls changed the original result"
        );
    } else {
        let a_item = item(&a, "competing-a")?;
        let b_item = item(&b, "competing-b")?;
        match (a_item.get("value"), b_item.get("value")) {
            (Some(_), None) => refused(&b, "competing-b", "quantity_exceeds_remaining")?,
            (None, Some(_)) => refused(&a, "competing-a", "quantity_exceeds_remaining")?,
            _ => anyhow::bail!("REC-CONTENTION expected exactly one commit: {a_item}, {b_item}"),
        }
    }
    assert_state(&snapshot(db, &fixture).await?, [2, 0], "open", 2, 1)?;
    record(
        evidence,
        json!({"case":if same_key{"overlapping-replay"}else{"competing-receipts"},
        "fixture":fixture.id,"blocked_backends":blocked,"result":"pass"}),
    )
}

async fn rollback_after_write(
    db: &Client,
    route: &Route,
    inputs: &Inputs,
    evidence: &mut File,
) -> Result<()> {
    let fixture = seed(db, [3, 1], "open").await?;
    let before = snapshot(db, &fixture).await?;
    let controller = connect(&inputs.project_pg_url).await?;
    let lock: i64 = i64::from_be_bytes(Uuid::new_v4().as_bytes()[..8].try_into()?);
    // This disposable trigger reaches the barrier only after the new receipt
    // and its matching claim are visible inside the real guest transaction.
    // It changes neither application privileges nor the command implementation.
    let trigger = format!(
        "CREATE FUNCTION receiving.receiving_history_rollback() RETURNS trigger LANGUAGE plpgsql AS $body$ \
         BEGIN IF NEW.purchase_order_id = '{id}'::uuid THEN \
         IF NOT EXISTS (SELECT 1 FROM receiving.record_receipt_command c \
                        JOIN receiving.receipt r ON r.idempotency_key=c.idempotency_key \
                        WHERE r.id=NEW.id AND c.receipt_id=NEW.id) THEN \
             RAISE EXCEPTION 'receiving history checkpoint has no intermediate write'; END IF; \
         PERFORM pg_advisory_xact_lock({lock}); \
         RAISE EXCEPTION 'receiving history injected failure after receipt write' USING ERRCODE='P0001'; \
         END IF; RETURN NEW; END $body$; \
         CREATE TRIGGER receiving_history_rollback AFTER INSERT ON receiving.receipt \
         FOR EACH ROW EXECUTE FUNCTION receiving.receiving_history_rollback();",
        id = fixture.id,
    );
    db.batch_execute(&trigger).await?;
    controller.client.batch_execute("BEGIN").await?;
    controller
        .client
        .query_one("SELECT pg_advisory_xact_lock($1)", &[&lock])
        .await?;
    let pid: i32 = controller
        .client
        .query_one("SELECT pg_backend_pid()", &[])
        .await?
        .get(0);
    let body = receipt_body(
        &fixture,
        Receipt {
            key: 0,
            line: 0,
            quantity: 2,
        },
        "rollback",
    );
    let calling_route = route.clone();
    let call = tokio::spawn(async move { calling_route.post(RECEIPT_PATH, &body).await });
    let blocked = wait_for_blocked(db, pid, 1).await;
    controller.client.batch_execute("ROLLBACK").await?;
    let response = call.await.context("join rollback call");
    let cleanup = db
        .batch_execute(
            "DROP TRIGGER receiving_history_rollback ON receiving.receipt; \
        DROP FUNCTION receiving.receiving_history_rollback()",
        )
        .await;
    let blocked = blocked?;
    let response = response??;
    cleanup?;
    refused(&response, "rollback", "internal_error")?;
    ensure!(
        snapshot(db, &fixture).await?.value == before.value,
        "REC-ROLLBACK retained an intermediate business write"
    );
    record(
        evidence,
        json!({"case":"rollback-after-receipt-write","fixture":fixture.id,
        "blocked_backends":blocked,"result":"pass"}),
    )
}

async fn lost_response(db: &Client, route: &Route, evidence: &mut File) -> Result<()> {
    let fixture = seed(db, [3, 1], "open").await?;
    let body = receipt_body(
        &fixture,
        Receipt {
            key: 0,
            line: 0,
            quantity: 2,
        },
        "lost-response",
    );
    let calling_route = route.clone();
    let captured = body.clone();
    let (delivered, receiver) = tokio::sync::oneshot::channel::<Response>();
    let (arrived, transport) = tokio::sync::oneshot::channel();
    let (suppress, control) = tokio::sync::oneshot::channel();
    let call = tokio::spawn(async move {
        let response = calling_route.post(RECEIPT_PATH, &captured).await?;
        // The HTTP adapter received bytes, but the application gets no result.
        arrived
            .send(())
            .map_err(|_| anyhow::anyhow!("response control disappeared"))?;
        control
            .await
            .context("wait for the response suppression control")?;
        drop(response);
        drop(delivered);
        Ok::<_, anyhow::Error>(())
    });
    transport
        .await
        .context("the real HTTP command returned no response")?;
    let committed = snapshot(db, &fixture).await?;
    assert_state(&committed, [2, 0], "open", 2, 1)?;
    suppress
        .send(())
        .map_err(|_| anyhow::anyhow!("response suppression task disappeared"))?;
    ensure!(
        receiver.await.is_err(),
        "REC-LOST-RESPONSE first application result was delivered"
    );
    call.await.context("join response suppression")??;
    let value = succeeded(&route.post(RECEIPT_PATH, &body).await?, "lost-response")?;
    let expected=db.query_one(
        "SELECT receipt_id::text,purchase_order_id::text,purchase_order_status,row_version::text \
         FROM receiving.record_receipt_command WHERE idempotency_key=$1",
        &[&format!("{}-0",fixture.key_prefix)],
    ).await?;
    ensure!(
        value
            == json!({"receipt_id":expected.get::<_,String>(0),
        "purchase_order_id":expected.get::<_,String>(1),
        "purchase_order_status":expected.get::<_,String>(2),
        "row_version":expected.get::<_,String>(3)}),
        "REC-LOST-RESPONSE retry did not return the independently observed original result"
    );
    ensure!(
        snapshot(db, &fixture).await?.value == committed.value,
        "REC-LOST-RESPONSE retry mutated committed business state"
    );
    record(
        evidence,
        json!({"case":"commit-withheld-application-response","fixture":fixture.id,
        "suppression_boundary":"after HTTP bytes, before application result delivery","result":"pass"}),
    )
}

async fn authority(db: &Client, route: &Route, inputs: &Inputs, evidence: &mut File) -> Result<()> {
    let fixture = seed(db, [3, 1], "open").await?;
    let body = receipt_body(
        &fixture,
        Receipt {
            key: 0,
            line: 0,
            quantity: 2,
        },
        "authority",
    );
    let before = snapshot(db, &fixture).await?;
    let removed = db
        .execute(
            "DELETE FROM app_system.permissions \
        WHERE tenant_id=$1 AND role_name=$2 AND permission=$3",
            &[&inputs.tenant, &inputs.caller_role, &RECEIPT_OPERATION],
        )
        .await?;
    ensure!(
        removed == 1,
        "authority fixture removed {removed} permission rows"
    );
    let denied = route.post(RECEIPT_PATH, &body).await;
    let denied_state = snapshot(db, &fixture).await;
    let restored = db
        .execute(
            "INSERT INTO app_system.permissions(tenant_id,role_name,permission) VALUES($1,$2,$3)",
            &[&inputs.tenant, &inputs.caller_role, &RECEIPT_OPERATION],
        )
        .await;
    let denied = denied?;
    restored?;
    ensure!(
        denied.status == reqwest::StatusCode::FORBIDDEN
            && denied.body
                == json!({"error":{
        "code":"permission-denied","operation":RECEIPT_OPERATION}}),
        "REC-AUTHORITY expected exact operation denial: {denied:?}"
    );
    ensure!(
        denied_state?.value == before.value,
        "REC-AUTHORITY unauthorized business mutation"
    );
    succeeded(&route.post(RECEIPT_PATH, &body).await?, "authority")?;
    assert_state(&snapshot(db, &fixture).await?, [2, 0], "open", 2, 1)?;
    record(
        evidence,
        json!({"case":"denied-then-authorized","fixture":fixture.id,"result":"pass"}),
    )
}

#[test]
#[ignore = "requires an owned disposable Receiving release and WAMN_RECEIVING_CORRECTNESS_DOCUMENT"]
fn production_receiving_command_histories() -> Result<()> {
    let path = std::env::var_os("WAMN_RECEIVING_CORRECTNESS_DOCUMENT")
        .context("WAMN_RECEIVING_CORRECTNESS_DOCUMENT must name the private fixture document")?;
    let inputs: Inputs = serde_json::from_slice(&std::fs::read(path)?)?;
    ensure!(
        (1..=64).contains(&inputs.cases),
        "case count must be within 1..=64"
    );
    ensure!(
        !inputs.component_digests.is_empty()
            && inputs.source_commit.len() == 40
            && inputs.corpus_sha256.starts_with("sha256:")
            && inputs.corpus_sha256.len() == 71,
        "source, component and SQL corpus identities are required"
    );
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source_head = std::process::Command::new("git")
        .args(["rev-parse", "--verify", "HEAD"])
        .current_dir(&repository)
        .output()
        .context("read the Receiving proof source identity")?;
    ensure!(
        source_head.status.success(),
        "cannot read the proof source HEAD"
    );
    ensure!(
        std::str::from_utf8(&source_head.stdout)?.trim() == inputs.source_commit,
        "Receiving proof source identity differs from the current checkout"
    );
    let weld: Value = serde_json::from_slice(&std::fs::read(
        repository.join("packages/receiving/generated/package-weld.json"),
    )?)?;
    ensure!(
        weld["application_sql_corpus_identity"] == inputs.corpus_sha256,
        "Receiving proof SQL corpus identity differs from the package weld"
    );
    let secret: Value = serde_json::from_slice(&std::fs::read(&inputs.route_caller_secret)?)?;
    let route = Route {
        client: reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?,
        endpoint: inputs.route_endpoint.trim_end_matches('/').to_owned(),
        host: inputs.route_host.clone(),
        bearer: secret["stringData"]["token"]
            .as_str()
            .context("fixture Secret has no token")?
            .to_owned(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let db = runtime.block_on(connect(&inputs.project_pg_url))?;
    let version: String = runtime
        .block_on(db.client.query_one("SHOW server_version_num", &[]))?
        .get(0);
    ensure!(
        version.parse::<u32>()? >= 180_000,
        "Receiving correctness requires PostgreSQL 18"
    );
    let mut evidence = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&inputs.evidence_file)?;
    record(
        &mut evidence,
        json!({"case":"identity","source_commit":inputs.source_commit,
        "component_digests":inputs.component_digests,"corpus_sha256":inputs.corpus_sha256,
        "seed":inputs.seed,"generated_cases":inputs.cases,"max_shrink_iterations":64,
        "invariants":["REC-HISTORY","REC-REFUSAL","REC-REPLAY","REC-CONTENTION",
            "REC-ROLLBACK","REC-LOST-RESPONSE","REC-REVISION","REC-AUTHORITY"]}),
    )?;
    if let Some(selected) = &inputs.history {
        runtime.block_on(explicit_history(
            &db.client,
            &route,
            selected,
            &mut evidence,
        ))?;
        record(
            &mut evidence,
            json!({"case":"summary","result":"pass","reproduction":true}),
        )?;
        return Ok(());
    }
    for example in model::examples() {
        runtime.block_on(explicit_history(
            &db.client,
            &route,
            &example,
            &mut evidence,
        ))?;
    }
    let config = Config {
        cases: inputs.cases,
        max_shrink_iters: 64,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(inputs.seed),
        ..Config::default()
    };
    let mut runner = TestRunner::new(config);
    // Each shrink replays against a new valid fixture. UUIDs are fixture-local;
    // the history contains stable key and line indices instead of those UUIDs.
    let evidence_cell = std::cell::RefCell::new(&mut evidence);
    let failures = std::cell::RefCell::new(ShrinkFailures::default());
    let generated = runner.run(&model::histories(), |case| {
        if failures.borrow().infrastructure.is_some() {
            return Ok(());
        }
        let result = runtime.block_on(history(
            &db.client,
            &route,
            &case,
            &mut evidence_cell.borrow_mut(),
        ));
        shrink_result(&mut failures.borrow_mut(), &case, result)
    });
    drop(evidence_cell);
    let failures = failures.into_inner();
    if let Err(error) = generated {
        if let Some((case, failure)) = &failures.infrastructure {
            record(
                &mut evidence,
                json!({"case":"history-infrastructure-failure",
                "history":case,"error":format!("{failure:#}"),
                "shrink_target":failures.target,"confirmed_business_failure":false}),
            )?;
        }
        if let TestError::Fail(reason, minimized) = error {
            let reproduction = runtime.block_on(reproduce_history(
                &db.client,
                &route,
                &minimized,
                &mut evidence,
                failures.target.as_ref(),
                "generated-minimized",
                failures.infrastructure.is_some(),
            ))?;
            if let Some((_, infrastructure)) = failures.infrastructure {
                return Err(infrastructure).with_context(|| {
                    format!(
                        "generated history infrastructure failed: reproduction={}",
                        reproduction.outcome,
                    )
                });
            }
            if let Some(infrastructure) = reproduction.infrastructure {
                return Err(infrastructure)
                    .context("minimized history reproduction infrastructure failed");
            }
            anyhow::bail!(
                "REC-HISTORY generated case failed: {reason}. Reproduction: {}. History: {}",
                reproduction.outcome,
                serde_json::to_string(&minimized)?,
            );
        }
        return Err(anyhow::anyhow!("REC-HISTORY generation aborted: {error}"));
    }
    runtime.block_on(async {
        invalid_status(&db.client, &route, &mut evidence).await?;
        mixed_items(&db.client, &route, &mut evidence).await?;
        competing_receipts(&db.client, &route, &inputs, &mut evidence, false).await?;
        competing_receipts(&db.client, &route, &inputs, &mut evidence, true).await?;
        rollback_after_write(&db.client, &route, &inputs, &mut evidence).await?;
        lost_response(&db.client, &route, &mut evidence).await?;
        authority(&db.client, &route, &inputs, &mut evidence).await
    })?;
    record(
        &mut evidence,
        json!({"case":"summary","result":"pass","generated_cases":inputs.cases,
        "explicit_histories":model::examples().len(),"boundary_cases":7}),
    )?;
    println!(
        "RECEIVING_CORRECTNESS result=pass generated_cases={} boundary_cases=7",
        inputs.cases
    );
    Ok(())
}

#[test]
fn shrinking_preserves_the_original_business_property_and_outcome() {
    let history = History {
        ordered: [2, 1],
        steps: vec![Step::Replay { key: 0 }],
    };
    let outcome = HistoryOutcome {
        operation: RECEIPT_PATH,
        expected: "committed",
        http_status: 200,
        refusal: None,
    };
    let failure = |property, observed: &HistoryOutcome| {
        business::<()>(
            Err(anyhow::anyhow!("fixture assertion detail")),
            property,
            observed,
        )
    };
    let mut failures = ShrinkFailures::default();
    assert!(matches!(
        shrink_result(
            &mut failures,
            &history,
            failure("REC-REVISION/response", &outcome)
        ),
        Err(TestCaseError::Fail(_)),
    ));
    let original = failures.target.clone();
    assert!(matches!(
        shrink_result(
            &mut failures,
            &history,
            failure("REC-HISTORY/status", &outcome)
        ),
        Err(TestCaseError::Reject(_)),
    ));
    let other_outcome = HistoryOutcome {
        expected: "replayed",
        ..outcome.clone()
    };
    assert!(matches!(
        shrink_result(
            &mut failures,
            &history,
            failure("REC-REVISION/response", &other_outcome)
        ),
        Err(TestCaseError::Reject(_)),
    ));
    assert!(matches!(
        shrink_result(
            &mut failures,
            &history,
            failure("REC-REVISION/response", &outcome)
        ),
        Err(TestCaseError::Fail(_)),
    ));
    assert_eq!(failures.target, original);
}

#[test]
fn infrastructure_failure_cannot_replace_a_business_counterexample() {
    let history = History {
        ordered: [2, 1],
        steps: vec![Step::Replay { key: 0 }],
    };
    let outcome = HistoryOutcome {
        operation: RECEIPT_PATH,
        expected: "committed",
        http_status: 200,
        refusal: None,
    };
    let mut failures = ShrinkFailures::default();
    let _ = shrink_result(
        &mut failures,
        &history,
        business::<()>(
            Err(anyhow::anyhow!("revision mismatch")),
            "REC-REVISION/response",
            &outcome,
        ),
    );
    let original = failures.target.clone();
    assert!(
        shrink_result(
            &mut failures,
            &history,
            Err(anyhow::anyhow!("fixture connection lost")),
        )
        .is_ok()
    );
    assert_eq!(failures.target, original);
    assert!(failures.infrastructure.is_some());

    let mut infrastructure_only = ShrinkFailures::default();
    assert!(matches!(
        shrink_result(
            &mut infrastructure_only,
            &history,
            Err(anyhow::anyhow!("fixture connection lost")),
        ),
        Err(TestCaseError::Fail(_))
    ));
    assert!(infrastructure_only.target.is_none());
    assert!(infrastructure_only.infrastructure.is_some());
}

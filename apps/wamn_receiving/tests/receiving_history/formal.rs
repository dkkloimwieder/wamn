//! Differential histories use the independent Receiving model without copied transitions.

#[expect(
    unexpected_cfgs,
    reason = "the independent model also compiles directly with Kani"
)]
#[expect(
    unused_attributes,
    reason = "the model declares its standalone crate type"
)]
#[path = "../../formal/model.rs"]
mod oracle;

use std::cell::RefCell;
use std::fs::File;

use anyhow::{Context as _, Result, ensure};
use proptest::prelude::{Strategy, any};
use proptest::strategy::ValueTree as _;
use proptest::test_runner::{Config, FileFailurePersistence, TestCaseError, TestError, TestRunner};
use serde_json::{Value, json};
use tokio_postgres::Client;
use uuid::Uuid;

use super::database::{self, Fixture, Snapshot};
use super::{HistoryFailure, HistoryOutcome, Route, business, history_assert};
use oracle::{Command, Outcome, State, Status};

#[derive(Clone, Debug)]
struct History {
    ordered: [u8; 2],
    cancelled: bool,
    commands: Vec<Command>,
}

fn histories() -> impl Strategy<Value = History> {
    let command = (
        any::<bool>(),
        proptest::option::of(0_u8..=4),
        proptest::option::of(0_u8..=4),
        any::<bool>(),
    )
        .prop_map(|(key, first, second, intent)| Command {
            key,
            lines: [first, second],
            intent,
        });
    (
        1_u8..=3,
        1_u8..=3,
        any::<bool>(),
        proptest::collection::vec(command, 1..=12),
    )
        .prop_map(|(first, second, cancelled, commands)| History {
            ordered: [first, second],
            cancelled,
            commands,
        })
}

fn examples() -> Vec<History> {
    let first = Command {
        key: false,
        lines: [Some(1), Some(1)],
        intent: false,
    };
    let second = Command { key: true, ..first };
    vec![
        History {
            ordered: [2, 2],
            cancelled: false,
            commands: vec![
                first,
                second,
                first,
                second,
                Command {
                    intent: true,
                    ..first
                },
            ],
        },
        History {
            ordered: [1, 1],
            cancelled: false,
            commands: vec![first, second, first],
        },
        History {
            ordered: [2, 1],
            cancelled: false,
            commands: vec![
                Command {
                    lines: [Some(1), Some(2)],
                    ..first
                },
                first,
                Command {
                    lines: [Some(1), None],
                    ..second
                },
                first,
            ],
        },
        History {
            ordered: [1, 2],
            cancelled: false,
            commands: vec![
                Command {
                    lines: [Some(2), Some(1)],
                    ..first
                },
                first,
            ],
        },
        History {
            ordered: [2, 2],
            cancelled: true,
            commands: vec![
                first,
                second,
                Command {
                    lines: [None, None],
                    ..first
                },
            ],
        },
        History {
            ordered: [3, 3],
            cancelled: false,
            commands: vec![
                Command {
                    lines: [None, None],
                    ..first
                },
                Command {
                    lines: [Some(0), Some(1)],
                    ..first
                },
                Command {
                    lines: [Some(1), Some(0)],
                    ..first
                },
                first,
                Command {
                    lines: [Some(0), None],
                    ..first
                },
                Command {
                    lines: [Some(2), Some(1)],
                    ..first
                },
            ],
        },
        History {
            ordered: [3, 3],
            cancelled: false,
            commands: vec![
                Command {
                    lines: [Some(1), None],
                    ..first
                },
                Command {
                    lines: [None, Some(2)],
                    ..second
                },
                Command {
                    lines: [Some(1), None],
                    ..first
                },
            ],
        },
    ]
}

fn status(status: Status) -> &'static str {
    match status {
        Status::Open => "open",
        Status::Complete => "complete",
        Status::Cancelled => "cancelled",
    }
}
fn expected(outcome: Outcome) -> &'static str {
    match outcome {
        Outcome::Accepted(_) => "committed",
        Outcome::Replayed(_) => "replayed",
        Outcome::InvalidInput => "invalid_input",
        Outcome::IntentConflict => "idempotency_conflict",
        Outcome::OrderNotOpen => "purchase_order_not_open",
        Outcome::ExcessQuantity => "quantity_exceeds_remaining",
    }
}
fn key(fixture: &Fixture, key: bool) -> String {
    format!("{}-formal-{}", fixture.key_prefix, u8::from(key))
}
fn reference(fixture: &Fixture, command: Command) -> String {
    format!(
        "{}-intent-{}",
        key(fixture, command.key),
        u8::from(command.intent)
    )
}
fn body(fixture: &Fixture, command: Command) -> Value {
    let lines =
        command
            .lines
            .into_iter()
            .enumerate()
            .filter_map(|(line, quantity)| {
                quantity.map(|quantity| json!({
        "purchase_order_line_id":fixture.line_ids[line].to_string(),
        "quantity":quantity.to_string(), "location_id":fixture.location_id.to_string()
    }))
            })
            .collect::<Vec<_>>();
    json!([{"request_id":"formal","value":{
        "idempotency_key":key(fixture,command.key),"purchase_order_id":fixture.id.to_string(),
        "receipt_reference":reference(fixture,command),"occurred_at":super::OCCURRED_AT,"line":lines
    }}])
}
fn history_json(history: &History) -> Value {
    json!({"ordered":history.ordered,"cancelled":history.cancelled,"commands":history.commands.iter().map(|c|json!({"key":c.key,"lines":c.lines,"intent":c.intent})).collect::<Vec<_>>()})
}
fn result(
    fixture: &Fixture,
    value: oracle::ResultSnapshot,
    receipt_ids: &[Option<String>; 2],
) -> Value {
    json!({"receipt_id":receipt_ids[usize::from(value.receipt)],"purchase_order_id":fixture.id.to_string(),"purchase_order_status":status(value.status),"row_version":value.revision})
}
fn equal(
    actual: &Value,
    expected: &Value,
    property: &'static str,
    outcome: &HistoryOutcome,
) -> Result<()> {
    history_assert(
        actual == expected,
        property,
        outcome,
        format_args!("expected {expected}; observed {actual}"),
    )
}

fn assert_business(
    fixture: &Fixture,
    state: State,
    actual: &Snapshot,
    baseline: &Snapshot,
    receipt_ids: &[Option<String>; 2],
    outcome: &HistoryOutcome,
) -> Result<()> {
    business(
        database::assert_state(
            actual,
            state.received.map(u16::from),
            status(state.status),
            i64::from(state.revision),
            state.claims.iter().flatten().count(),
        ),
        "REC-FORMAL/state",
        outcome,
    )?;
    let mut order = actual.value["purchase_order"].clone();
    for field in ["status", "row_version", "updated_at", "updated_by"] {
        order[field] = baseline.value["purchase_order"][field].clone();
    }
    equal(
        &order,
        &baseline.value["purchase_order"],
        "REC-FORMAL/order-identity",
        outcome,
    )?;
    for index in 0..2 {
        let ordered = business(
            database::amount(
                actual.value["lines"][index]["ordered_quantity"]
                    .as_str()
                    .context("ordered quantity is exact text")?,
            ),
            "REC-FORMAL/ordered-quantity",
            outcome,
        )?;
        equal(
            &json!(ordered),
            &json!(state.ordered[index]),
            "REC-FORMAL/ordered-quantity",
            outcome,
        )?;
        let mut line = actual.value["lines"][index].clone();
        line["received_quantity"] = baseline.value["lines"][index]["received_quantity"].clone();
        equal(
            &line,
            &baseline.value["lines"][index],
            "REC-FORMAL/line-identity",
            outcome,
        )?;
    }
    let expected_lines = state
        .receipts
        .iter()
        .flatten()
        .map(|effect| effect.iter().filter(|q| **q > 0).count())
        .sum::<usize>();
    equal(
        &json!(actual.receipt_line_count),
        &json!(expected_lines),
        "REC-FORMAL/receipt-line-count",
        outcome,
    )?;
    for (index, claim) in state.claims.into_iter().enumerate() {
        let Some(claim) = claim else { continue };
        let receipt_id = receipt_ids[index]
            .as_ref()
            .context("accepted model claim has a bound receipt identity")?;
        let receipt_rows = actual.value["receipts"]
            .as_array()
            .context("snapshot receipts array")?
            .iter()
            .filter(|r| r["id"] == *receipt_id)
            .collect::<Vec<_>>();
        equal(
            &json!(receipt_rows.len()),
            &json!(1),
            "REC-FORMAL/receipt-membership",
            outcome,
        )?;
        let receipt = receipt_rows[0];
        let receipt_value = json!({"id":receipt["id"],"purchase_order_id":receipt["purchase_order_id"],"idempotency_key":receipt["idempotency_key"],"receipt_reference":receipt["receipt_reference"]});
        equal(
            &receipt_value,
            &json!({"id":receipt_id,"purchase_order_id":fixture.id.to_string(),"idempotency_key":key(fixture,claim.command.key),"receipt_reference":reference(fixture,claim.command)}),
            "REC-FORMAL/receipt-facts",
            outcome,
        )?;
        // PostgreSQL renders timestamptz in the observer session's time zone.
        let occurred_at = business(
            receipt["occurred_at"]
                .as_str()
                .context("receipt time is text")
                .and_then(|text| {
                    chrono::DateTime::parse_from_rfc3339(text)
                        .context("receipt time is an exact instant")
                }),
            "REC-FORMAL/receipt-time",
            outcome,
        )?;
        let expected_at = chrono::DateTime::parse_from_rfc3339(super::OCCURRED_AT)?;
        history_assert(
            occurred_at == expected_at,
            "REC-FORMAL/receipt-time",
            outcome,
            format_args!("expected {expected_at}; observed {occurred_at}"),
        )?;
        let claims = actual.value["claims"]
            .as_array()
            .context("snapshot claims array")?
            .iter()
            .filter(|c| c["idempotency_key"] == key(fixture, claim.command.key))
            .collect::<Vec<_>>();
        equal(
            &json!(claims.len()),
            &json!(1),
            "REC-FORMAL/claim-membership",
            outcome,
        )?;
        let stored = claims[0];
        equal(
            &json!({"receipt_id":stored["receipt_id"],"purchase_order_id":stored["purchase_order_id"],"purchase_order_status":stored["purchase_order_status"],"row_version":stored["row_version"]}),
            &result(fixture, claim.result, receipt_ids),
            "REC-FORMAL/stored-result",
            outcome,
        )?;
        let effect = state.receipts[index].context("model accepted claim has receipt facts")?;
        for (line, quantity) in effect.into_iter().enumerate() {
            let rows = actual.value["receipt_lines"]
                .as_array()
                .context("snapshot receipt lines array")?
                .iter()
                .filter(|r| {
                    r["receipt_id"] == *receipt_id
                        && r["purchase_order_line_id"] == fixture.line_ids[line].to_string()
                })
                .collect::<Vec<_>>();
            equal(
                &json!(rows.len()),
                &json!(usize::from(quantity > 0)),
                "REC-FORMAL/receipt-line-membership",
                outcome,
            )?;
            if let Some(row) = rows.first() {
                let amount = business(
                    database::amount(
                        row["quantity"]
                            .as_str()
                            .context("receipt quantity is exact text")?,
                    ),
                    "REC-FORMAL/receipt-quantity",
                    outcome,
                )?;
                equal(
                    &json!(amount),
                    &json!(quantity),
                    "REC-FORMAL/receipt-quantity",
                    outcome,
                )?;
                equal(
                    &row["location_id"],
                    &json!(fixture.location_id.to_string()),
                    "REC-FORMAL/receipt-location",
                    outcome,
                )?;
            }
        }
    }
    Ok(())
}

fn preserved(before: &Snapshot, after: &Snapshot, outcome: &HistoryOutcome) -> Result<()> {
    for table in ["claims", "receipts", "receipt_lines"] {
        let identity = if table == "claims" {
            "idempotency_key"
        } else {
            "id"
        };
        for old in before.value[table]
            .as_array()
            .context("snapshot immutable rows array")?
        {
            let new = after.value[table]
                .as_array()
                .context("snapshot immutable rows array")?
                .iter()
                .find(|r| r[identity] == old[identity])
                .unwrap_or(&Value::Null);
            equal(new, old, "REC-FORMAL/immutable-history", outcome)?;
        }
    }
    Ok(())
}

async fn history(
    db: &Client,
    route: &Route,
    selected: &History,
    evidence: &mut File,
) -> Result<()> {
    let fixture = database::seed(
        db,
        selected.ordered.map(u16::from),
        if selected.cancelled {
            "cancelled"
        } else {
            "open"
        },
    )
    .await?;
    let baseline = database::snapshot(db, &fixture).await?;
    let mut state = oracle::initial(selected.ordered, selected.cancelled);
    ensure!(oracle::valid(state), "Receiving formal fixture is valid");
    let mut receipt_ids: [Option<String>; 2] = [None, None];
    let mut originals: [Option<Value>; 2] = [None, None];
    for (step, command) in selected.commands.iter().copied().enumerate() {
        ensure!(
            oracle::command_domain(command),
            "formal command is within the finite quantity bound"
        );
        let before = database::snapshot(db, &fixture).await?;
        let model_outcome = oracle::record_receipt(&mut state, command);
        let response = route
            .post(super::RECEIPT_PATH, &body(&fixture, command))
            .await?;
        ensure!(
            !response.status.is_server_error(),
            "Receiving formal route infrastructure failure: {response:?}"
        );
        let outcome = HistoryOutcome {
            operation: super::RECEIPT_PATH,
            expected: expected(model_outcome),
            http_status: response.status.as_u16(),
            refusal: response
                .body
                .as_array()
                .and_then(|a| a.first())
                .and_then(|v| v["error"]["code"].as_str())
                .or_else(|| response.body["error"]["code"].as_str())
                .map(str::to_owned),
        };
        ensure!(
            !matches!(
                outcome.refusal.as_deref(),
                Some("timeout" | "retry" | "internal_error")
            ),
            "Receiving command completion is uncertain: {response:?}"
        );
        match model_outcome {
            Outcome::Accepted(snapshot) | Outcome::Replayed(snapshot) => {
                let value = business(
                    super::succeeded(&response, "formal"),
                    "REC-FORMAL/accepted",
                    &outcome,
                )?;
                if matches!(model_outcome, Outcome::Accepted(_)) {
                    let id = business(
                        value["receipt_id"]
                            .as_str()
                            .context("accepted receipt identity")
                            .and_then(|id| {
                                Uuid::parse_str(id)
                                    .map(|id| id.to_string())
                                    .context("accepted receipt identity is UUID")
                            }),
                        "REC-FORMAL/receipt-identity",
                        &outcome,
                    )?;
                    history_assert(
                        !receipt_ids.iter().flatten().any(|bound| *bound == id),
                        "REC-FORMAL/receipt-identity",
                        &outcome,
                        format_args!("receipt reused an identity: {id}"),
                    )?;
                    receipt_ids[usize::from(snapshot.receipt)] = Some(id);
                    originals[usize::from(snapshot.receipt)] = Some(value.clone());
                }
                equal(
                    &value,
                    &result(&fixture, snapshot, &receipt_ids),
                    "REC-FORMAL/original-result",
                    &outcome,
                )?;
            }
            Outcome::InvalidInput if command.lines == [None, None] => {
                // The route schema requires one line before the command can run.
                history_assert(
                    response.status == reqwest::StatusCode::BAD_REQUEST,
                    "REC-FORMAL/empty-receipt-schema-refusal",
                    &outcome,
                    format_args!("empty receipt returned {}", response.status),
                )?;
                equal(
                    &response.body,
                    &json!({"error":{"code":"schema-invalid","data":{"pointer":"/0/value/line"}}}),
                    "REC-FORMAL/empty-receipt-schema-refusal",
                    &outcome,
                )?;
            }
            _ => business(
                super::refused(&response, "formal", expected(model_outcome)),
                "REC-FORMAL/refused",
                &outcome,
            )?,
        }
        let after = database::snapshot(db, &fixture).await?;
        if !matches!(model_outcome, Outcome::Accepted(_)) {
            equal(
                &after.value,
                &before.value,
                "REC-FORMAL/refusal-replay-preservation",
                &outcome,
            )?;
        }
        assert_business(&fixture, state, &after, &baseline, &receipt_ids, &outcome)?;
        preserved(&before, &after, &outcome)?;
        // The model stores each original result. Later mutable order state is never its source.
        for claim in state.claims.into_iter().flatten() {
            let replay = route
                .post(super::RECEIPT_PATH, &body(&fixture, claim.command))
                .await?;
            ensure!(
                !replay.status.is_server_error(),
                "Receiving replay infrastructure failure: {replay:?}"
            );
            let refusal = replay
                .body
                .as_array()
                .and_then(|a| a.first())
                .and_then(|value| value["error"]["code"].as_str());
            ensure!(
                !matches!(refusal, Some("timeout" | "retry" | "internal_error")),
                "Receiving replay completion is uncertain: {replay:?}"
            );
            let value = business(
                super::succeeded(&replay, "formal"),
                "REC-FORMAL/later-replay",
                &outcome,
            )?;
            equal(
                &value,
                originals[usize::from(claim.command.key)]
                    .as_ref()
                    .context("original response exists")?,
                "REC-FORMAL/later-replay",
                &outcome,
            )?;
            equal(
                &value,
                &result(&fixture, claim.result, &receipt_ids),
                "REC-FORMAL/later-model-result",
                &outcome,
            )?;
        }
        equal(
            &database::snapshot(db, &fixture).await?.value,
            &after.value,
            "REC-FORMAL/later-replay-preservation",
            &outcome,
        )?;
        ensure!(
            oracle::valid(state),
            "executable Receiving oracle retained its invariant"
        );
        super::record(
            evidence,
            &json!({"case":"formal-step","history":history_json(selected),"step":step,"expected":expected(model_outcome),"fixture":fixture.id.to_string(),"result":"pass"}),
        )?;
    }
    Ok(())
}

#[derive(Debug, Default)]
struct Failures {
    target: Option<HistoryFailure>,
    infrastructure: Option<(History, anyhow::Error)>,
}
fn shrink_result(
    failures: &mut Failures,
    selected: &History,
    result: Result<()>,
) -> std::result::Result<(), TestCaseError> {
    let Err(error) = result else { return Ok(()) };
    if let Some(failure) = error.downcast_ref::<HistoryFailure>() {
        match &failures.target {
            Some(target) if target != failure => {
                return Err(TestCaseError::reject(
                    "different formal business property or outcome",
                ));
            }
            None => failures.target = Some(failure.clone()),
            Some(_) => {}
        }
        Err(TestCaseError::fail(format!("{error:#}")))
    } else {
        failures.infrastructure = Some((selected.clone(), error));
        if failures.target.is_some() {
            Ok(())
        } else {
            Err(TestCaseError::fail("formal history infrastructure failed"))
        }
    }
}

pub(super) fn run(
    runtime: &tokio::runtime::Runtime,
    db: &Client,
    route: &Route,
    evidence: &mut File,
    mut config: Config,
    cancellation: &pg_walstream::CancellationToken,
    deadline: tokio::time::Instant,
) -> Result<()> {
    // Keep the timestamp-offset regression independent of the host's time zone.
    super::run_before(
        runtime,
        deadline,
        cancellation,
        db.batch_execute("SET TIME ZONE 'America/New_York'"),
    )?;
    let fixed = examples();
    for selected in &fixed {
        let result = super::run_before(
            runtime,
            deadline,
            cancellation,
            history(db, route, selected, evidence),
        );
        if let Err(error) = result {
            let failure = error.downcast_ref::<HistoryFailure>();
            super::record(
                evidence,
                &json!({"case":"formal-fixed-failure","history":history_json(selected),"failure":failure,"error":format!("{error:#}")}),
            )?;
            return Err(error);
        }
    }
    config.failure_persistence = Some(Box::new(FileFailurePersistence::Direct(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/receiving_history/formal-regressions.txt"
    ))));
    let cases = config.cases;
    let mut runner = TestRunner::new(config);
    let failures = RefCell::new(Failures::default());
    let generated = {
        let evidence_cell = RefCell::new(&mut *evidence);
        runner.run(&histories(), |selected| {
            if failures.borrow().infrastructure.is_some() {
                return Ok(());
            }
            let result = super::run_before(
                runtime,
                deadline,
                cancellation,
                history(db, route, &selected, &mut evidence_cell.borrow_mut()),
            );
            shrink_result(&mut failures.borrow_mut(), &selected, result)
        })
    };
    let failures = failures.into_inner();
    if let Some((selected, error)) = failures.infrastructure {
        super::record(
            evidence,
            &json!({"case":"formal-infrastructure-failure","history":history_json(&selected),"error":format!("{error:#}"),"confirmed_business_failure":false,"shrink_target":failures.target}),
        )?;
        return Err(error).context("formal history infrastructure failed");
    }
    match generated {
        Ok(()) => {}
        Err(TestError::Fail(reason, minimized)) => {
            let repeated = super::run_before(
                runtime,
                deadline,
                cancellation,
                history(db, route, &minimized, evidence),
            );
            let failure = repeated
                .as_ref()
                .err()
                .and_then(|e| e.downcast_ref::<HistoryFailure>());
            let confirmed = failures
                .target
                .as_ref()
                .is_some_and(|target| Some(target) == failure);
            super::record(
                evidence,
                &json!({"case":"formal-minimized-failure","history":history_json(&minimized),"failure":failure,"target":failures.target,"confirmed_business_failure":confirmed,"reason":reason.to_string(),"repeated_error":repeated.as_ref().err().map(|e|format!("{e:#}"))}),
            )?;
            if let Err(error) = repeated
                && error.downcast_ref::<HistoryFailure>().is_none()
            {
                return Err(error).context("formal minimized history infrastructure failed");
            }
            anyhow::bail!(
                "Receiving formal history failed: {reason}; reproduced={confirmed}; history={}",
                history_json(&minimized)
            );
        }
        Err(error) => anyhow::bail!("Receiving formal history generation aborted: {error}"),
    }
    super::record(
        evidence,
        &json!({"case":"formal-summary","result":"pass","generated_cases":cases,"fixed_histories":fixed.len(),"rules":8,"oracle":"apps/wamn_receiving/formal/model.rs"}),
    )?;
    println!(
        "RECEIVING_FORMAL result=pass generated_cases={cases} fixed_histories={} rules=8",
        fixed.len()
    );
    Ok(())
}

#[test]
fn generated_histories_remain_inside_the_executable_oracle_domain() {
    let mut runner = TestRunner::deterministic();
    for _ in 0..64 {
        let mut tree = histories()
            .new_tree(&mut runner)
            .expect("history generation");
        for _ in 0..16 {
            let history = tree.current();
            let mut state = oracle::initial(history.ordered, history.cancelled);
            assert!(oracle::valid(state));
            for command in history.commands {
                assert!(oracle::command_domain(command));
                oracle::record_receipt(&mut state, command);
                assert!(oracle::valid(state));
            }
            if !tree.simplify() {
                break;
            }
        }
    }
}

#[test]
fn fixed_histories_exercise_every_formal_outcome_and_completion() {
    let mut seen = [false; 6];
    let mut completed = false;
    for history in examples() {
        let mut state = oracle::initial(history.ordered, history.cancelled);
        for command in history.commands {
            let index = match oracle::record_receipt(&mut state, command) {
                Outcome::Accepted(_) => 0,
                Outcome::Replayed(_) => 1,
                Outcome::InvalidInput => 2,
                Outcome::IntentConflict => 3,
                Outcome::OrderNotOpen => 4,
                Outcome::ExcessQuantity => 5,
            };
            seen[index] = true;
            completed |= state.status == Status::Complete;
        }
    }
    assert!(seen.into_iter().all(|seen| seen));
    assert!(completed);
}

//! `[WMS-RUNTIME-LIVE]` — the two assertions structure cannot make, over the
//! local business route or released label route.
//!
//! ONE. Two moves of the same inventory, in flight together, yield EXACTLY ONE
//! `concurrency_conflict`. Not at least one: two would mean neither moved.
//! Not zero: that would mean the lock is not the inventory. And the count can
//! coincide -- a serialized pair gives the same count as a working lock, and
//! one request never arriving gives one success and zero conflicts -- so the
//! two are fired behind one barrier and the SURVIVOR is asserted to have
//! moved the stock from its own response: `location_id` is the target and
//! `row_version` advanced once, while the loser observed that exact revision.
//!
//! TWO. The winner's exact body replayed returns the same `operation_id` --
//! by construction (command-identity-from-claim), not by an early-return
//! path.
//!
//! These cases return structured results to the application test caller.

use std::path::Path;

use anyhow::Context as _;
use serde_json::{Value, json};

use wamn_gate_harness::journey::{JourneyDocument, RuntimePhase};

const REQUEST_ID_A: &str = "contention-a";
const REQUEST_ID_B: &str = "contention-b";
const REQUEST_ID_REPLAY: &str = "contention-replay";
const OCCURRED_AT: &str = "2026-09-05T12:00:00.000000Z";

pub(crate) struct Route {
    endpoint: String,
    host: String,
    bearer: String,
}

impl Route {
    pub(crate) fn from_document(
        document: &JourneyDocument,
        runtime: &RuntimePhase,
    ) -> anyhow::Result<Self> {
        let secret: Value = serde_json::from_slice(
            &std::fs::read(&document.route_caller_secret_output).with_context(|| {
                format!("read {}", document.route_caller_secret_output.display())
            })?,
        )
        .context("the route-caller Secret is JSON")?;
        let bearer = secret["stringData"]["token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .context("the route-caller Secret carries stringData.token")?
            .to_owned();
        Ok(Self {
            endpoint: runtime.route_endpoint.trim_end_matches('/').to_owned(),
            host: document.route_host.clone(),
            bearer,
        })
    }

    pub(crate) fn local(endpoint: String, host: String, bearer: String) -> Self {
        Self {
            endpoint,
            host,
            bearer,
        }
    }

    async fn post(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: &Value,
    ) -> anyhow::Result<Value> {
        let (status, text) = self.post_response(client, path, body).await?;
        anyhow::ensure!(
            status.is_success(),
            "POST {path} answered {status} with body {text}"
        );
        serde_json::from_str(&text).with_context(|| format!("the route's answer is JSON: {text}"))
    }

    /// Send the one item of a read as a GET, as its route is published.
    async fn get(
        &self,
        client: &reqwest::Client,
        path: &str,
        item: &Value,
    ) -> anyhow::Result<Value> {
        let item = item.as_object().context("a read sends one object item")?;
        let query = wamn_execution_contract::encode_read_query(item);
        let response = client
            .get(format!("{}{path}?{query}", self.endpoint))
            .header("Host", &self.host)
            .bearer_auth(&self.bearer)
            .send()
            .await
            .with_context(|| format!("GET {path} from the released route"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("read the route's response body")?;
        anyhow::ensure!(
            status.is_success(),
            "GET {path} answered {status} with body {text}"
        );
        serde_json::from_str(&text).with_context(|| format!("the route's answer is JSON: {text}"))
    }

    async fn post_response(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: &Value,
    ) -> anyhow::Result<(reqwest::StatusCode, String)> {
        let response = client
            .post(format!("{}{path}", self.endpoint))
            .header("Host", &self.host)
            .bearer_auth(&self.bearer)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path} to the released route"))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("read the route's response body")?;
        Ok((status, text))
    }
}

fn move_body(
    request_id: &str,
    idempotency_key: &str,
    runtime: &RuntimePhase,
    expected_revision: i64,
) -> Value {
    json!([{
        "request_id": request_id,
        "value": {
            "idempotency_key": idempotency_key,
            "inventory_id": runtime.inventory_id,
            "to_location_id": runtime.to_location_id,
            "to_packaging_id": runtime.to_packaging_id,
            "expected_row_version": expected_revision,
            "occurred_at": OCCURRED_AT,
        }
    }])
}

/// The single item of an array envelope, checked to carry the request id it
/// was asked with.
fn item<'a>(answer: &'a Value, request_id: &str) -> anyhow::Result<&'a Value> {
    let items = answer
        .as_array()
        .context("the route answers with an array envelope")?;
    anyhow::ensure!(
        items.len() == 1,
        "one request, one item; got {}",
        items.len()
    );
    anyhow::ensure!(
        items[0]["request_id"] == request_id,
        "the item answers request {request_id}: {}",
        items[0]
    );
    Ok(&items[0])
}

pub(crate) async fn assert_contention_and_replay(
    route: &Route,
    runtime: &RuntimePhase,
    initial_revision: i64,
) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build the route client")?;

    // ONE. Both requests are built before either is sent, then sent together.
    let a = move_body(REQUEST_ID_A, "contention-a-key", runtime, initial_revision);
    let b = move_body(REQUEST_ID_B, "contention-b-key", runtime, initial_revision);
    let (answer_a, answer_b) = tokio::join!(
        route.post(&client, "/inventory/move", &a),
        route.post(&client, "/inventory/move", &b)
    );
    let answer_a = answer_a?;
    let answer_b = answer_b?;
    let item_a = item(&answer_a, REQUEST_ID_A)?;
    let item_b = item(&answer_b, REQUEST_ID_B)?;

    let outcomes = [(REQUEST_ID_A, item_a, &a), (REQUEST_ID_B, item_b, &b)];
    let winners: Vec<_> = outcomes
        .iter()
        .filter(|(_, item, _)| item.get("value").is_some())
        .collect();
    let conflicts: Vec<_> = outcomes
        .iter()
        .filter(|(_, item, _)| item["error"]["code"] == "concurrency_conflict")
        .collect();
    anyhow::ensure!(
        winners.len() == 1 && conflicts.len() == 1,
        "two concurrent moves must yield exactly one success and exactly one concurrency_conflict; \
         got {} successes and {} conflicts: {answer_a} / {answer_b}",
        winners.len(),
        conflicts.len()
    );
    let (_, winner, winning_body) = winners[0];
    let (_, loser, _) = conflicts[0];
    let expected_revision = initial_revision;
    let next_revision = initial_revision + 1;

    // The survivor MOVED THE STOCK, from its own response: the count above can
    // coincide with a request that never arrived; this cannot.
    let value = &winner["value"];
    anyhow::ensure!(
        value["location_id"] == runtime.to_location_id.as_str(),
        "the winner's inventory is at the target location: {value}"
    );
    anyhow::ensure!(
        value["row_version"].as_i64() == Some(next_revision),
        "the winner advanced the inventory's row_version from {initial_revision}: {value}"
    );
    anyhow::ensure!(
        value["inventory_id"] == runtime.inventory_id.as_str(),
        "the winner moved the fixture inventory: {value}"
    );
    let operation_id = value["operation_id"]
        .as_str()
        .filter(|id| id.len() == 36)
        .context("the winner carries a operation_id")?
        .to_owned();
    // And the loser lost to THAT version, not to something else.
    anyhow::ensure!(
        loser["error"]["detail"]["expected_row_version"].as_i64() == Some(expected_revision)
            && loser["error"]["detail"]["observed_row_version"].as_i64() == Some(next_revision),
        "the loser's conflict names expected {initial_revision} / observed {}: {loser}",
        initial_revision + 1
    );

    // TWO. The winner's exact body, again, with a fresh request id: the same
    // movement id, because the claim was written once under a primary key.
    let mut replay: Value = (**winning_body).clone();
    replay[0]["request_id"] = json!(REQUEST_ID_REPLAY);
    let answer = route.post(&client, "/inventory/move", &replay).await?;
    let replayed = item(&answer, REQUEST_ID_REPLAY)?;
    anyhow::ensure!(
        replayed["value"]["operation_id"] == operation_id.as_str(),
        "a replay returns the same operation_id {operation_id}: {replayed}"
    );
    anyhow::ensure!(
        replayed["value"]["row_version"].as_i64() == Some(next_revision),
        "a replay returns the original result, not a second move: {replayed}"
    );

    Ok(json!({"operation_id": operation_id}))
}

/// Exercise the deployed composed move and require replay to return its exact result.
pub(crate) async fn assert_label_delivery_and_replay(
    route: &Route,
    runtime: &RuntimePhase,
) -> anyhow::Result<Value> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build the route client")?;
    let body = move_body("label-move", "label-move-key", runtime, 1);
    let answer = route.post(&client, "/inventory/move", &body).await?;
    let moved = value(&answer, "label-move")?;
    anyhow::ensure!(
        moved["inventory_id"] == runtime.inventory_id.as_str()
            && moved["location_id"] == runtime.to_location_id.as_str()
            && moved["row_version"] == 2,
        "the composed route returns the committed move: {moved}"
    );
    let operation_id = uuid(&moved["operation_id"])?;

    let mut replay = body;
    replay[0]["request_id"] = json!("label-replay");
    let answer = route.post(&client, "/inventory/move", &replay).await?;
    let replayed = value(&answer, "label-replay")?;
    anyhow::ensure!(
        replayed == moved,
        "the composed route replay returns the original move {operation_id}: {replayed}"
    );
    Ok(json!({"operation_id": operation_id}))
}

/// The `value` of the single item answering `request_id`, or the refusal as
/// an error naming it.
/// The `value` of the single item a read answers. A read carries no request
/// identity, so its one outcome answers its one item by position.
fn read_value(answer: &Value) -> anyhow::Result<&Value> {
    let items = answer
        .as_array()
        .context("the route answers with an array envelope")?;
    anyhow::ensure!(
        items.len() == 1 && items[0].get("request_id").is_none(),
        "one read, one outcome with no request identity: {answer}"
    );
    items[0]
        .get("value")
        .with_context(|| format!("the read was refused: {}", items[0]))
}

fn value<'a>(answer: &'a Value, request_id: &str) -> anyhow::Result<&'a Value> {
    let item = item(answer, request_id)?;
    item.get("value")
        .with_context(|| format!("{request_id} was refused: {item}"))
}

/// The `error` of the single item answering `request_id`: the refusal that
/// was asked for, named by its code.
fn refusal<'a>(answer: &'a Value, request_id: &str, code: &str) -> anyhow::Result<&'a Value> {
    let item = item(answer, request_id)?;
    let error = item
        .get("error")
        .with_context(|| format!("{request_id} was expected to refuse with {code}: {item}"))?;
    anyhow::ensure!(
        error["code"] == code,
        "{request_id} refuses with {code}: {error}"
    );
    Ok(&error["detail"])
}

fn revision(value: &Value) -> anyhow::Result<i64> {
    value
        .as_i64()
        .with_context(|| format!("a revision crosses the wire as a JSON integer: {value}"))
}

fn uuid(value: &Value) -> anyhow::Result<String> {
    value
        .as_str()
        .filter(|id| id.len() == 36)
        .map(str::to_owned)
        .with_context(|| format!("a uuid: {value}"))
}

pub(crate) async fn assert_remaining_operations(
    route: &Route,
    runtime: &RuntimePhase,
) -> anyhow::Result<Value> {
    let client = reqwest::Client::new();
    let answer = route
        .get(
            &client,
            "/inventory/get",
            &json!({"id":runtime.inventory_id}),
        )
        .await?;
    let original = read_value(&answer)?.clone();
    let version = revision(&original["row_version"])?;
    let split = json!([{"request_id":"split","value":{
        "idempotency_key":"split-key","from_inventory_id":runtime.inventory_id,
        "quantity":"3","to_packaging_id":runtime.to_packaging_id,
        "to_location_id":runtime.to_location_id,"expected_row_version":version,
        "occurred_at":OCCURRED_AT
    }}]);
    let answer = route.post(&client, "/inventory/split", &split).await?;
    let split_result = value(&answer, "split")?.clone();
    let child = uuid(&split_result["new_inventory_id"])?;
    anyhow::ensure!(
        split_result["quantity"] == "7",
        "split retains seven units: {answer}"
    );
    let history = route
        .get(&client, "/inventory_transaction/query", &json!({}))
        .await?;
    let rows = read_value(&history)?["item"]
        .as_array()
        .context("transaction page")?;
    let split_rows: Vec<_> = rows
        .iter()
        .filter(|r| r["operation_id"] == split_result["operation_id"])
        .collect();
    anyhow::ensure!(
        split_rows.len() == 2,
        "split records both identities: {history}"
    );
    let new_row = split_rows
        .iter()
        .find(|r| r["inventory_id"] == child)
        .context("child transaction")?;
    anyhow::ensure!(
        new_row["from_inventory_id"] == runtime.inventory_id
            && new_row["to_inventory_id"] == child
            && new_row["from_quantity"] == "0"
            && new_row["to_quantity"] == "3"
            && new_row["from_location_id"].is_null()
            && new_row["to_location_id"] == runtime.to_location_id,
        "split retains lineage and explicit locations: {new_row}"
    );

    let adjust = json!([{"request_id":"adjust","value":{
        "idempotency_key":"adjust-key","inventory_id":child,"to_quantity":"4.50",
        "reason":"cycle-count","expected_row_version":1,"occurred_at":OCCURRED_AT
    }}]);
    let answer = route.post(&client, "/inventory/adjust", &adjust).await?;
    let adjusted = value(&answer, "adjust")?.clone();
    anyhow::ensure!(
        adjusted["quantity"] == "4.50",
        "decimal adjustment preserves scale: {answer}"
    );
    let merge = json!([{"request_id":"merge","value":{
        "idempotency_key":"merge-key","from_inventory_id":child,"to_inventory_id":runtime.inventory_id,
        "expected_from_row_version":2,"expected_to_row_version":version+1,"occurred_at":OCCURRED_AT
    }}]);
    let answer = route.post(&client, "/inventory/merge", &merge).await?;
    let merged = value(&answer, "merge")?.clone();
    anyhow::ensure!(
        merged["quantity"] == "11.50",
        "merge conserves adjusted stock: {answer}"
    );
    let history = route
        .get(&client, "/inventory_transaction/query", &json!({}))
        .await?;
    let rows = read_value(&history)?["item"]
        .as_array()
        .context("transaction page")?;
    for row in &split_rows {
        anyhow::ensure!(
            rows.contains(row),
            "later commands preserve earlier transaction rows"
        );
    }
    let merge_rows: Vec<_> = rows
        .iter()
        .filter(|r| r["operation_id"] == merged["operation_id"])
        .collect();
    anyhow::ensure!(
        merge_rows.len() == 2
            && merge_rows
                .iter()
                .all(|r| r["from_inventory_id"] == child
                    && r["to_inventory_id"] == runtime.inventory_id),
        "both merge rows preserve source lineage: {history}"
    );
    let source_row = merge_rows
        .iter()
        .find(|r| r["inventory_id"] == child)
        .context("source transaction")?;
    anyhow::ensure!(
        source_row["to_quantity"] == "0" && source_row["to_lifecycle"] == "closed",
        "merge closes source: {source_row}"
    );

    for (path, body, expected) in [
        ("/inventory/split", &split, &split_result),
        ("/inventory/adjust", &adjust, &adjusted),
        ("/inventory/merge", &merge, &merged),
    ] {
        let answer = route.post(&client, path, body).await?;
        anyhow::ensure!(
            &answer[0]["value"] == expected,
            "replay returns the whole original result: {answer}"
        );
    }
    let mut changed = split.clone();
    changed[0]["value"]["quantity"] = json!("2");
    let answer = route.post(&client, "/inventory/split", &changed).await?;
    refusal(&answer, "split", "idempotency_conflict")?;
    let closed_move = json!([{"request_id":"closed","value":{
        "idempotency_key":"closed-key","inventory_id":child,
        "to_packaging_id":runtime.to_packaging_id,"to_location_id":runtime.to_location_id,
        "expected_row_version":3,"occurred_at":OCCURRED_AT
    }}]);
    let answer = route.post(&client, "/inventory/move", &closed_move).await?;
    refusal(&answer, "closed", "invalid_input")?;
    let close = json!([{"request_id":"close","value":{
        "idempotency_key":"close-nonempty","packaging_id":runtime.to_packaging_id,"expected_row_version":1
    }}]);
    let answer = route.post(&client, "/packaging/close", &close).await?;
    refusal(&answer, "close", "invalid_input")?;
    let history_after = route
        .get(&client, "/inventory_transaction/query", &json!({}))
        .await?;
    anyhow::ensure!(
        history_after == history,
        "replay and refusals append no history"
    );
    let answer = route
        .get(
            &client,
            "/inventory/get",
            &json!({"id":runtime.inventory_id}),
        )
        .await?;
    anyhow::ensure!(
        read_value(&answer)?["quantity"] == "11.50",
        "refusals and replay leave inventory unchanged"
    );
    let close_empty = json!([{"request_id":"close-empty","value":{
        "idempotency_key":"close-empty-key","packaging_id":crate::business_fixture::PACKAGING_A_ID,"expected_row_version":1
    }}]);
    let answer = route
        .post(&client, "/packaging/close", &close_empty)
        .await?;
    let closed = value(&answer, "close-empty")?.clone();
    anyhow::ensure!(closed["lifecycle"] == "closed", "empty packaging closes");
    let replay = route
        .post(&client, "/packaging/close", &close_empty)
        .await?;
    anyhow::ensure!(
        replay[0]["value"] == closed,
        "closure replay preserves result"
    );
    let create = json!([{"request_id":"create-packaging","value":{
        "idempotency_key":"create-packaging-key","type":"tote","code":"REPLAY-TOTE",
        "location_id":runtime.to_location_id
    }}]);
    let answer = route.post(&client, "/packaging/create", &create).await?;
    let created = value(&answer, "create-packaging")?.clone();
    anyhow::ensure!(
        created["type"] == "tote",
        "packaging type crosses the contract"
    );
    let close_created = json!([{"request_id":"close-created","value":{
        "idempotency_key":"close-created-key","packaging_id":created["packaging_id"],
        "expected_row_version":1
    }}]);
    let answer = route
        .post(&client, "/packaging/close", &close_created)
        .await?;
    anyhow::ensure!(value(&answer, "close-created")?["lifecycle"] == "closed");
    let replay = route.post(&client, "/packaging/create", &create).await?;
    anyhow::ensure!(
        value(&replay, "create-packaging")? == &created,
        "creation replay returns its original open packaging result"
    );
    Ok(json!({"split_inventory_id":child}))
}

/// Force the second split history insert to fail after both inventory writes.
pub(crate) async fn assert_history_failure_rolls_back(
    route: &Route,
    runtime: &RuntimePhase,
    admin: &tokio_postgres::Client,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let answer = route
        .get(
            &client,
            "/inventory/get",
            &json!({"id":runtime.inventory_id}),
        )
        .await?;
    let current = read_value(&answer)?;
    let snapshot = "SELECT jsonb_build_object('inventory', (SELECT jsonb_agg(to_jsonb(i) ORDER BY id) FROM wms.inventory i), 'history', (SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM wms.inventory_transaction t), 'claims', (SELECT jsonb_agg(to_jsonb(c) ORDER BY idempotency_key) FROM wms.inventory_split_command c))";
    let from: Value = admin.query_one(snapshot, &[]).await?.get(0);
    admin.batch_execute("ALTER TABLE wms.inventory_transaction ADD CONSTRAINT test_reject_child CHECK (from_quantity <> 0) NOT VALID").await?;
    let body = json!([{"request_id":"fail-history","value":{
        "idempotency_key":"fail-history-key","from_inventory_id":runtime.inventory_id,
        "quantity":"1","to_packaging_id":runtime.to_packaging_id,"to_location_id":runtime.to_location_id,
        "expected_row_version":current["row_version"],"occurred_at":OCCURRED_AT
    }}]);
    let attempted = route.post(&client, "/inventory/split", &body).await;
    admin
        .batch_execute("ALTER TABLE wms.inventory_transaction DROP CONSTRAINT test_reject_child")
        .await?;
    let answer = attempted?;
    refusal(&answer, "fail-history", "internal_error")?;
    let to: Value = admin.query_one(snapshot, &[]).await?.get(0);
    anyhow::ensure!(
        from == to,
        "failed second history insert rolls back inventory, first history row, and claim: {to}"
    );
    Ok(())
}

pub(crate) async fn assert_committed_move_after_label_failure(
    document: &JourneyDocument,
) -> anyhow::Result<(Value, Value)> {
    let runtime = document
        .runtime
        .as_ref()
        .context("the journey needs its runtime phase")?;
    let route = Route::from_document(document, runtime)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build the route client")?;
    let before = route
        .get(
            &client,
            "/inventory/get",
            &json!({"id":runtime.inventory_id}),
        )
        .await?;
    let before = read_value(&before)?;
    let revision = revision(&before["row_version"])?;
    anyhow::ensure!(
        before["location_id"] != runtime.to_location_id,
        "the partial test must move to a different location"
    );
    let key = uuid::Uuid::new_v4().to_string();
    let request_id = format!("partial-{key}");
    let body = json!([{"request_id":request_id,"value":{
        "idempotency_key":key,
        "inventory_id":runtime.inventory_id,
        "to_location_id":runtime.to_location_id,
        "to_packaging_id":runtime.to_packaging_id,
        "expected_row_version":revision,
        "occurred_at":OCCURRED_AT
    }}]);
    // Send this composed command once. Claim replay does not make its label effect safe to repeat.
    let (status, text) = route
        .post_response(&client, "/inventory/move", &body)
        .await?;
    let http = json!({"status":status.as_u16(),"body":text});
    anyhow::ensure!(
        status == reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        "the failed store returns HTTP500, got {status}: {text}"
    );
    let answer: Value = serde_json::from_str(&text).context("the partial response is JSON")?;
    let object = answer
        .as_object()
        .context("the partial response is an object")?;
    anyhow::ensure!(
        object.len() == 2
            && object.contains_key("committed_result")
            && object.contains_key("failed_outcome"),
        "the response carries only the committed result and failed outcome: {answer}"
    );
    let committed = item(&answer["committed_result"], &request_id)?;
    anyhow::ensure!(
        committed
            .as_object()
            .is_some_and(|item| item.len() == 2 && item.contains_key("value")),
        "the committed envelope has only request_id and value: {committed}"
    );
    let operation_id = uuid(&committed["value"]["operation_id"])?;
    uuid::Uuid::parse_str(&operation_id).context("the committed movement identity is a UUID")?;
    let expected = json!({
        "operation_id":operation_id,"inventory_id":runtime.inventory_id,
        "product_id":before["product_id"],"packaging_id":runtime.to_packaging_id,
        "location_id":runtime.to_location_id,"quantity":before["quantity"],
        "disposition":before["disposition"],"lifecycle":before["lifecycle"],"row_version":revision+1
    });
    anyhow::ensure!(
        committed["value"] == expected,
        "the response preserves the original movement result without label enrichment: {committed}"
    );
    let failure = answer["failed_outcome"]
        .as_object()
        .context("the failed outcome is an object")?;
    anyhow::ensure!(
        failure.len() == 3
            && failure
                .keys()
                .all(|key| matches!(key.as_str(), "code" | "message" | "effect_outcome")),
        "the failed outcome carries no unrelated results: {failure:?}"
    );
    anyhow::ensure!(
        failure.get("code") == Some(&json!("write_failed")),
        "blob-put reports write_failed: {failure:?}"
    );
    // The missing bucket answers HTTP404. object_store maps it to NotFound, which the adapter observes as Responded.
    anyhow::ensure!(
        failure.get("effect_outcome")
            == Some(&json!(
                wamn_execution_contract::EffectOutcome::Responded.label()
            )),
        "the store answered after dispatch: {failure:?}"
    );
    anyhow::ensure!(
        failure
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| !message.is_empty()),
        "the store failure carries its nonempty message: {failure:?}"
    );
    let after = route
        .get(
            &client,
            "/inventory/get",
            &json!({"id":runtime.inventory_id}),
        )
        .await?;
    let after = read_value(&after)?;
    anyhow::ensure!(
        after["location_id"] == expected["location_id"]
            && after["row_version"] == expected["row_version"]
            && after["lifecycle"] == expected["lifecycle"],
        "the later read shows the movement stayed committed: {after}"
    );
    Ok((
        http,
        json!({
            "request_id":request_id,"idempotency_key":key,"operation_id":operation_id,
            "inventory_id":runtime.inventory_id,"location_id":runtime.to_location_id,
            "row_version":revision+1,"lifecycle":before["lifecycle"],
            "command_requests":1,"effect_outcome":failure["effect_outcome"]
        }),
    ))
}

/// Check the label objects returned by the store's existing client.
pub(crate) fn assert_single_label(objects: &[Value], operation_id: &str) -> anyhow::Result<()> {
    let label_count = objects.len();
    let label_key = objects
        .first()
        .and_then(|object| object["key"].as_str())
        .unwrap_or("");
    anyhow::ensure!(
        label_count == 1 && label_key == operation_id,
        "expected exactly one label object named {operation_id} under wms/, found {label_count}: {label_key}"
    );
    Ok(())
}

/// Check the committed rows after the label store refuses the write.
pub(crate) async fn assert_committed_rows(
    project: &tokio_postgres::Client,
    expected: &Value,
) -> anyhow::Result<Value> {
    let key = expected["idempotency_key"]
        .as_str()
        .context("command key")?;
    let row = project
        .query_one(
            "SELECT result::jsonb FROM wms.inventory_move_command WHERE idempotency_key = $1",
            &[&key],
        )
        .await?;
    let result: Value = row.get(0);
    anyhow::ensure!(
        result["operation_id"] == expected["operation_id"]
            && result["row_version"] == expected["row_version"],
        "claim retains original result"
    );
    let id = expected["inventory_id"].as_str().context("inventory id")?;
    let row = project
        .query_one(
            "SELECT to_jsonb(i) FROM wms.inventory i WHERE id = $1::text::uuid",
            &[&id],
        )
        .await?;
    let inventory: Value = row.get(0);
    anyhow::ensure!(
        inventory["location_id"] == result["location_id"]
            && inventory["row_version"] == result["row_version"],
        "inventory effect committed"
    );
    let operation = result["operation_id"].as_str().context("operation id")?;
    let rows = project.query("SELECT to_jsonb(t) FROM wms.inventory_transaction t WHERE operation_id = $1::text::uuid", &[&operation]).await?;
    anyhow::ensure!(
        rows.len() == 1,
        "one affected inventory has one transaction"
    );
    let history: Value = rows[0].get(0);
    anyhow::ensure!(
        history["inventory_id"] == id
            && history["to_location_id"] == result["location_id"]
            && history["type"] == "move",
        "history retains explicit inventory transition"
    );
    Ok(json!({"command":result,"inventory":inventory,"transaction":history}))
}

pub(crate) fn write_result(directory: &Path, name: &str, result: &Value) -> anyhow::Result<()> {
    let path = directory.join(name);
    let file = std::fs::File::create(&path)
        .with_context(|| format!("create test result {}", path.display()))?;
    serde_json::to_writer_pretty(file, result)
        .with_context(|| format!("write test result {}", path.display()))
}

async fn business_snapshot(admin: &tokio_postgres::Client) -> anyhow::Result<Value> {
    Ok(admin.query_one("SELECT jsonb_build_object(
        'inventory',(SELECT jsonb_agg(to_jsonb(i) ORDER BY id) FROM wms.inventory i),
        'packaging',(SELECT jsonb_agg(to_jsonb(p) ORDER BY id) FROM wms.packaging p),
        'history',(SELECT jsonb_agg(to_jsonb(t) ORDER BY id) FROM wms.inventory_transaction t),
        'move',(SELECT jsonb_agg(to_jsonb(c) ORDER BY idempotency_key) FROM wms.inventory_move_command c),
        'adjust',(SELECT jsonb_agg(to_jsonb(c) ORDER BY idempotency_key) FROM wms.inventory_adjust_command c),
        'split',(SELECT jsonb_agg(to_jsonb(c) ORDER BY idempotency_key) FROM wms.inventory_split_command c),
        'merge',(SELECT jsonb_agg(to_jsonb(c) ORDER BY idempotency_key) FROM wms.inventory_merge_command c),
        'close',(SELECT jsonb_agg(to_jsonb(c) ORDER BY idempotency_key) FROM wms.packaging_close_command c))", &[]).await?.get(0))
}

async fn refused_without_change(
    route: &Route,
    admin: &tokio_postgres::Client,
    path: &str,
    command: Value,
    code: &str,
) -> anyhow::Result<()> {
    let from = business_snapshot(admin).await?;
    let answer = route
        .post(
            &reqwest::Client::new(),
            path,
            &json!([{"request_id":"refuse","value":command}]),
        )
        .await?;
    refusal(&answer, "refuse", code)?;
    anyhow::ensure!(
        from == business_snapshot(admin).await?,
        "refused {path} changed business state"
    );
    Ok(())
}

pub(crate) async fn assert_inventory_refusals_and_held_split(
    route: &Route,
    runtime: &RuntimePhase,
    admin: &tokio_postgres::Client,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let answer = route
        .get(
            &client,
            "/inventory/get",
            &json!({"id":runtime.inventory_id}),
        )
        .await?;
    let target = read_value(&answer)?;
    let held = "00000000-0000-0000-0000-000000000302";
    admin.batch_execute("BEGIN; SELECT set_config('app.user_id','00000000-0000-4000-8000-0000000000f1',true), set_config('app.operation','admin:seed-held-inventory',true)").await?;
    admin.execute("INSERT INTO wms.inventory(id,product_id,packaging_id,location_id,quantity,disposition) SELECT $1::text::uuid,product_id,packaging_id,location_id,5,'held' FROM wms.inventory WHERE id=$2::text::uuid", &[&held,&runtime.inventory_id]).await?;
    admin.batch_execute("COMMIT").await?;
    refused_without_change(route, admin, "/inventory/merge", json!({
        "idempotency_key":"different-disposition","from_inventory_id":held,"to_inventory_id":runtime.inventory_id,
        "expected_from_row_version":1,"expected_to_row_version":target["row_version"],"occurred_at":OCCURRED_AT
    }), "invalid_input").await?;
    for (packaging, location) in [
        (
            runtime.to_packaging_id.as_str(),
            crate::business_fixture::LOCATION_A_ID,
        ),
        (
            crate::business_fixture::PACKAGING_A_ID,
            crate::business_fixture::LOCATION_A_ID,
        ),
    ] {
        for path in ["/inventory/move", "/inventory/split"] {
            let mut body = json!({"idempotency_key":format!("bad-destination-{path}-{packaging}"),
                "to_packaging_id":packaging,"to_location_id":location,"expected_row_version":1,"occurred_at":OCCURRED_AT});
            if path.ends_with("move") {
                body["inventory_id"] = json!(held);
            } else {
                body["from_inventory_id"] = json!(held);
                body["quantity"] = json!("1");
            }
            refused_without_change(route, admin, path, body, "invalid_input").await?;
        }
    }
    let split = json!([{"request_id":"held-split","value":{
        "idempotency_key":"held-split","from_inventory_id":held,"quantity":"2",
        "to_packaging_id":runtime.to_packaging_id,"to_location_id":runtime.to_location_id,
        "expected_row_version":1,"occurred_at":OCCURRED_AT}}]);
    let answer = route.post(&client, "/inventory/split", &split).await?;
    let original = value(&answer, "held-split")?.clone();
    let child = uuid(&original["new_inventory_id"])?;
    let answer = route
        .get(&client, "/inventory/get", &json!({"id":child}))
        .await?;
    anyhow::ensure!(
        read_value(&answer)?["disposition"] == "held" && read_value(&answer)?["quantity"] == "2",
        "split preserves held disposition"
    );
    let answer = route
        .post(
            &client,
            "/inventory/merge",
            &json!([{"request_id":"held-merge","value":{
        "idempotency_key":"held-merge","from_inventory_id":child,"to_inventory_id":held,
        "expected_from_row_version":1,"expected_to_row_version":2,"occurred_at":OCCURRED_AT}}]),
        )
        .await?;
    anyhow::ensure!(
        value(&answer, "held-merge")?["quantity"] == "5",
        "held merge conserves quantity"
    );
    let replay = route.post(&client, "/inventory/split", &split).await?;
    anyhow::ensure!(
        replay[0]["value"] == original,
        "held split replay survives child closure"
    );
    for path in [
        "/inventory/move",
        "/inventory/adjust",
        "/inventory/split",
        "/inventory/merge",
    ] {
        let mut body =
            json!({"idempotency_key":format!("closed-{path}"),"occurred_at":OCCURRED_AT});
        if path.ends_with("merge") {
            body["from_inventory_id"] = json!(child);
            body["to_inventory_id"] = json!(held);
            body["expected_from_row_version"] = json!(2);
            body["expected_to_row_version"] = json!(3);
        } else {
            body["expected_row_version"] = json!(2);
            body[if path.ends_with("split") {
                "from_inventory_id"
            } else {
                "inventory_id"
            }] = json!(child);
            if path.ends_with("adjust") {
                body["to_quantity"] = json!("1");
                body["reason"] = json!("cycle-count");
            } else {
                body["to_packaging_id"] = json!(runtime.to_packaging_id);
                body["to_location_id"] = json!(runtime.to_location_id);
            }
            if path.ends_with("split") {
                body["quantity"] = json!("1");
            }
        }
        refused_without_change(route, admin, path, body, "invalid_input").await?;
    }
    refused_without_change(
        route,
        admin,
        "/inventory/merge",
        json!({
            "idempotency_key":"closed-target","from_inventory_id":held,"to_inventory_id":child,
            "expected_from_row_version":3,"expected_to_row_version":2,"occurred_at":OCCURRED_AT
        }),
        "invalid_input",
    )
    .await?;
    refused_without_change(
        route,
        admin,
        "/inventory/split",
        json!({
            "idempotency_key":"split-all","from_inventory_id":held,"quantity":"5",
            "to_packaging_id":runtime.to_packaging_id,"to_location_id":runtime.to_location_id,
            "expected_row_version":3,"occurred_at":OCCURRED_AT
        }),
        "insufficient_quantity",
    )
    .await?;
    for (quantity, reason) in [("0", "cycle-count"), ("5", " ")] {
        refused_without_change(route, admin, "/inventory/adjust", json!({
            "idempotency_key":format!("bad-adjust-{quantity}"),"inventory_id":held,
            "to_quantity":quantity,"reason":reason,"expected_row_version":3,"occurred_at":OCCURRED_AT
        }), "invalid_input").await?;
    }
    // The app role can read and append history, but cannot rewrite or delete it.
    let grants=admin.query_one("SELECT has_column_privilege('wamn_app','wms.inventory_transaction','inventory_id','INSERT'), has_column_privilege('wamn_app','wms.inventory_transaction','inventory_id','SELECT'), has_any_column_privilege('wamn_app','wms.inventory_transaction','UPDATE'), has_table_privilege('wamn_app','wms.inventory_transaction','DELETE')",&[]).await?;
    anyhow::ensure!(
        grants.get::<_, bool>(0)
            && grants.get::<_, bool>(1)
            && !grants.get::<_, bool>(2)
            && !grants.get::<_, bool>(3),
        "history grants permit only append and read"
    );
    Ok(())
}

#[cfg(test)]
mod shape {
    use super::*;

    fn runtime() -> RuntimePhase {
        RuntimePhase {
            route_endpoint: "http://10.0.0.2:30999".to_owned(),
            inventory_id: "00000000-0000-0000-0000-000000000301".to_owned(),
            to_location_id: "00000000-0000-0000-0000-000000000202".to_owned(),
            to_packaging_id: "00000000-0000-0000-0000-000000000502".to_owned(),
        }
    }

    #[test]
    fn a_move_body_is_the_array_envelope_the_route_admits() {
        let body = move_body("r", "k", &runtime(), 1);
        let items = body.as_array().expect("array envelope");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["request_id"], "r");
        let value = &items[0]["value"];
        for key in [
            "idempotency_key",
            "inventory_id",
            "to_packaging_id",
            "to_location_id",
            "expected_row_version",
            "occurred_at",
        ] {
            assert!(value.get(key).is_some(), "value carries {key}");
        }
        assert_eq!(value["expected_row_version"], 1);
        assert_eq!(
            value.as_object().expect("object").len(),
            6,
            "exactly the declared fields, nothing extra"
        );
    }

    #[test]
    fn an_item_must_answer_the_request_it_was_asked_with() {
        let answer = json!([{"request_id": "other", "value": {}}]);
        assert!(item(&answer, "mine").is_err());
        let answer =
            json!([{"request_id": "mine", "value": {}}, {"request_id": "mine", "value": {}}]);
        assert!(
            item(&answer, "mine").is_err(),
            "two items for one request is refused"
        );
        let answer = json!([{"request_id": "mine", "value": {}}]);
        assert!(item(&answer, "mine").is_ok());
    }

    #[test]
    fn a_path_is_appended_to_the_endpoint_without_a_double_slash() {
        let route = Route {
            endpoint: "http://10.0.0.2:30999/".trim_end_matches('/').to_owned(),
            host: "wms.localhost".to_owned(),
            bearer: "t".to_owned(),
        };
        assert_eq!(
            format!("{}{}", route.endpoint, "/inventory/move"),
            "http://10.0.0.2:30999/inventory/move"
        );
    }
}

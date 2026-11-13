// `[WMS-RUNTIME-LIVE]` — the two assertions structure cannot make, over the
// local business route or released label route.
//
// ONE. Two moves of the same packaging, in flight together, yield EXACTLY ONE
// `concurrency_conflict`. Not at least one: two would mean neither moved.
// Not zero: that would mean the lock is not the packaging. And the count can
// coincide -- a serialized pair gives the same count as a working lock, and
// one request never arriving gives one success and zero conflicts -- so the
// two are fired behind one barrier and the SURVIVOR is asserted to have
// moved the stock from its own response: `location_id` is the target and
// `row_version` advanced once, while the loser observed that exact revision.
//
// TWO. The winner's exact body replayed returns the same result: the write
// log stored the result, and the replay answers it.
//
// These cases return structured results to the application test caller.

use anyhow::Context as _;
use serde_json::{Value, json};

use wamn_gate_harness::journey::RuntimePhase;

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

    /// Send the one request element of a read as a GET, as its route is published.
    async fn get(
        &self,
        client: &reqwest::Client,
        path: &str,
        request_element: &Value,
    ) -> anyhow::Result<Value> {
        let request_element = request_element
            .as_object()
            .context("a read sends one object request element")?;
        let query = wamn_client::encode_read_query(request_element);
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
            "packaging_id": runtime.packaging_id,
            "to_location_id": runtime.to_location_id,
            "expected_row_version": expected_revision,
            "occurred_at": OCCURRED_AT,
        }
    }])
}

/// The single request element of an array envelope, checked to carry the request id it
/// was asked with.
fn request_element<'a>(answer: &'a Value, request_id: &str) -> anyhow::Result<&'a Value> {
    let request_elements = answer
        .as_array()
        .context("the route answers with an array envelope")?;
    anyhow::ensure!(
        request_elements.len() == 1,
        "one request, one request element; got {}",
        request_elements.len()
    );
    anyhow::ensure!(
        request_elements[0]["request_id"] == request_id,
        "the request element answers request {request_id}: {}",
        request_elements[0]
    );
    Ok(&request_elements[0])
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
    let request_element_a = request_element(&answer_a, REQUEST_ID_A)?;
    let request_element_b = request_element(&answer_b, REQUEST_ID_B)?;

    let outcomes = [
        (REQUEST_ID_A, request_element_a, &a),
        (REQUEST_ID_B, request_element_b, &b),
    ];
    let winners: Vec<_> = outcomes
        .iter()
        .filter(|(_, request_element, _)| request_element.get("value").is_some())
        .collect();
    let conflicts: Vec<_> = outcomes
        .iter()
        .filter(|(_, request_element, _)| {
            request_element["error"]["code"] == "concurrency_conflict"
        })
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
        "the winner's packaging is at the target location: {value}"
    );
    anyhow::ensure!(
        value["row_version"].as_i64() == Some(next_revision),
        "the winner advanced the packaging's row_version from {initial_revision}: {value}"
    );
    anyhow::ensure!(
        value["packaging_id"] == runtime.packaging_id.as_str(),
        "the winner moved the fixture packaging: {value}"
    );
    // The packaging keeps the time the move names, not the time of the write.
    let answer = route
        .get(
            &client,
            "/packaging/get",
            &json!({"id": runtime.packaging_id}),
        )
        .await?;
    let moved = read_value(&answer)?;
    anyhow::ensure!(
        moved["located_at"] == OCCURRED_AT,
        "the moved packaging is located at the move's occurred_at: {moved}"
    );
    // And the loser lost to THAT version, not to something else.
    anyhow::ensure!(
        loser["error"]["detail"]["expected_row_version"].as_i64() == Some(expected_revision)
            && loser["error"]["detail"]["observed_row_version"].as_i64() == Some(next_revision),
        "the loser's conflict names expected {initial_revision} / observed {}: {loser}",
        initial_revision + 1
    );

    // TWO. The winner's exact body, again, with a fresh request id: the same
    // result, because the write log answers the stored result.
    let mut replay: Value = (**winning_body).clone();
    replay[0]["request_id"] = json!(REQUEST_ID_REPLAY);
    let answer = route.post(&client, "/inventory/move", &replay).await?;
    let replayed = request_element(&answer, REQUEST_ID_REPLAY)?;
    anyhow::ensure!(
        replayed["value"] == *value,
        "a replay returns the original result, not a second move: {replayed}"
    );

    Ok(json!({"row_version": next_revision}))
}

/// The `value` of the single request element answering `request_id`, or the refusal as
/// an error naming it.
/// The `value` of the single request element a read answers. A read carries no request
/// identity, so its one outcome answers its one request element by position.
fn read_value(answer: &Value) -> anyhow::Result<&Value> {
    let request_elements = answer
        .as_array()
        .context("the route answers with an array envelope")?;
    anyhow::ensure!(
        request_elements.len() == 1 && request_elements[0].get("request_id").is_none(),
        "one read, one outcome with no request identity: {answer}"
    );
    request_elements[0]
        .get("value")
        .with_context(|| format!("the read was refused: {}", request_elements[0]))
}

fn value<'a>(answer: &'a Value, request_id: &str) -> anyhow::Result<&'a Value> {
    let request_element = request_element(answer, request_id)?;
    request_element
        .get("value")
        .with_context(|| format!("{request_id} was refused: {request_element}"))
}

/// The `error` of the single request element answering `request_id`: the refusal that
/// was asked for, named by its code.
/// A code that is blank after a trim refuses in the codec, on its field, before
/// the CHECK that guards the same rule could answer (wamn-iowb.4).
pub(crate) async fn assert_blank_codes_refuse(route: &Route) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build the route client")?;
    let missing = "00000000-0000-0000-0000-00000000b1a2";
    for (path, request_element, field) in [
        (
            "/product/create",
            json!({"request_id": "blank", "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": "blank-product", "product_code": "  "}),
            "product_code",
        ),
        (
            "/location/create",
            json!({"request_id": "blank", "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": "blank-location", "location_code": "\t"}),
            "location_code",
        ),
        (
            "/packaging/create",
            json!({"request_id": "blank", "id": uuid::Uuid::now_v7().to_string(), "idempotency_key": "blank-packaging", "packaging_code": " ",
                "type": "tote", "location_id": missing, "status": "available"}),
            "packaging_code",
        ),
        (
            "/product/update",
            json!({"request_id": "blank", "id": missing, "expected_row_version": 1,
                "change": {"product_code": " "}}),
            "change.product_code",
        ),
        (
            "/location/update",
            json!({"request_id": "blank", "id": missing, "expected_row_version": 1,
                "change": {"location_code": ""}}),
            "change.location_code",
        ),
    ] {
        let answer = route.post(&client, path, &json!([request_element])).await?;
        let detail = refusal(&answer, "blank", "invalid_input")?;
        anyhow::ensure!(
            detail["field"] == field,
            "{path} refuses a blank code on {field}: {detail}"
        );
    }
    Ok(())
}

fn refusal<'a>(answer: &'a Value, request_id: &str, code: &str) -> anyhow::Result<&'a Value> {
    let request_element = request_element(answer, request_id)?;
    let error = request_element.get("error").with_context(|| {
        format!("{request_id} was expected to refuse with {code}: {request_element}")
    })?;
    anyhow::ensure!(
        error["code"] == code,
        "{request_id} refuses with {code}: {error}"
    );
    Ok(&error["detail"])
}

fn quantity(value: &Value) -> anyhow::Result<f64> {
    value
        .as_str()
        .and_then(|text| text.parse::<f64>().ok())
        .with_context(|| format!("a numeric crosses the wire as a decimal string: {value}"))
}

fn revision(value: &Value) -> anyhow::Result<i64> {
    value
        .as_i64()
        .with_context(|| format!("a revision crosses the wire as a JSON integer: {value}"))
}

/// The one transaction id of a result that wrote one transaction row.
fn one_transaction(value: &Value) -> anyhow::Result<String> {
    match value["transaction_ids"].as_array().map(Vec::as_slice) {
        Some([id]) => uuid(id),
        _ => anyhow::bail!("one transaction id: {value}"),
    }
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
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build the route client")?;
    let packaging_id = runtime.packaging_id.as_str();

    // WHERE THE FIXTURE STANDS, from the warehouse's own answers rather than
    // from an assumption about which test ran first: the packaging's revision and
    // location by get, and its product by the live-stock aggregate.
    let answer = route
        .get(&client, "/packaging/get", &json!({"id": packaging_id}))
        .await?;
    let packaging = read_value(&answer)?;
    anyhow::ensure!(
        packaging["status"] == "available",
        "the fixture packaging is live: {packaging}"
    );
    let version = revision(&packaging["row_version"])?;
    let location = uuid(&packaging["location_id"])?;

    let answer = route
        .get(&client, "/inventory/aggregate", &json!({}))
        .await?;
    let rows = read_value(&answer)?["rows"].clone();
    let rows = rows.as_array().context("aggregate answers rows")?;
    anyhow::ensure!(
        rows.len() == 1 && rows[0]["packaging_count"] == 1 && rows[0]["status"] == "available",
        "one product on one live packaging: {rows:?}"
    );
    anyhow::ensure!(
        rows[0]["location_id"] == location.as_str(),
        "at the packaging's location: {rows:?}"
    );
    let product_id = uuid(&rows[0]["product_id"])?;
    let held = quantity(&rows[0]["quantity"])?;

    // ADJUST: count the row to 7. The transaction records the stock that left,
    // and the packaging's revision moves.
    let answer = route
        .post(&client, "/inventory/adjust", &json!([{"request_id": "ops-adjust", "value": {
            "idempotency_key": "ops-adjust-key", "packaging_id": packaging_id, "product_id": product_id,
            "status": "available", "quantity": "7", "reason_code": "cycle-count",
            "expected_row_version": version, "occurred_at": OCCURRED_AT,
        }}]))
        .await?;
    let adjusted = value(&answer, "ops-adjust")?;
    let adjusted_revision = version + 1;
    anyhow::ensure!(
        (quantity(&adjusted["adjusted_quantity"])? - 7.0).abs() < f64::EPSILON
            && adjusted["row_version"].as_i64() == Some(adjusted_revision)
            && adjusted["packaging_status"] == "available",
        "the adjust counted 7 and advanced the revision from {version} (held {held}): {adjusted}"
    );
    one_transaction(adjusted)?;

    // Nothing to change refuses: a count equal to the balance, and a move to
    // the location the packaging is at.
    let answer = route
        .post(&client, "/inventory/adjust", &json!([{"request_id": "ops-adjust-same", "value": {
            "idempotency_key": "ops-adjust-same-key", "packaging_id": packaging_id, "product_id": product_id,
            "status": "available", "quantity": "7.0", "reason_code": "cycle-count",
            "expected_row_version": adjusted_revision, "occurred_at": OCCURRED_AT,
        }}]))
        .await?;
    let detail = refusal(&answer, "ops-adjust-same", "invalid_input")?;
    anyhow::ensure!(
        detail["field"] == "value.quantity",
        "an adjust to the balance names its field: {detail}"
    );
    let answer = route
        .post(
            &client,
            "/inventory/move",
            &json!([{"request_id": "ops-move-same", "value": {
                "idempotency_key": "ops-move-same-key", "packaging_id": packaging_id,
                "to_location_id": location, "expected_row_version": adjusted_revision,
                "occurred_at": OCCURRED_AT,
            }}]),
        )
        .await?;
    let detail = refusal(&answer, "ops-move-same", "invalid_input")?;
    anyhow::ensure!(
        detail["field"] == "value.to_location_id",
        "a move to the current location names its field: {detail}"
    );

    // SPLIT: 3 units onto a new packaging beside the source. The write log keeps
    // the result, so the exact body again yields the SAME id.
    let split = json!([{"request_id": "ops-split", "value": {
        "idempotency_key": "ops-split-key", "source_packaging_id": packaging_id, "product_id": product_id,
        "status": "available", "quantity": "3", "new_packaging_code": "PAL-302",
        "new_packaging_type": "tote", "to_location_id": location, "expected_row_version": version + 1, "occurred_at": OCCURRED_AT,
    }}]);
    let answer = route.post(&client, "/inventory/split", &split).await?;
    let first = value(&answer, "ops-split")?;
    let split_revision = version + 2;
    anyhow::ensure!(
        first["row_version"].as_i64() == Some(split_revision)
            && first["source_status"] == "available",
        "the split advanced the source: {first}"
    );
    let new_packaging_id = uuid(&first["new_packaging_id"])?;
    let split_transaction_id = one_transaction(first)?;
    let mut replay = split.clone();
    replay[0]["request_id"] = json!("ops-split-replay");
    let answer = route.post(&client, "/inventory/split", &replay).await?;
    let replayed = value(&answer, "ops-split-replay")?;
    anyhow::ensure!(
        replayed["new_packaging_id"] == new_packaging_id.as_str()
            && replayed["transaction_ids"] == json!([split_transaction_id])
            && replayed["row_version"].as_i64() == Some(split_revision),
        "a replayed split returns the same new packaging {new_packaging_id}, not a second one: {replayed}"
    );
    // And a split asking for more than the row holds is refused with what it
    // holds: 4, after 7 less the 3 that left.
    let answer = route
        .post(
            &client,
            "/inventory/split",
            &json!([{"request_id": "ops-split-too-much", "value": {
                "idempotency_key": "ops-split-too-much-key", "source_packaging_id": packaging_id,
                "product_id": product_id, "status": "available", "quantity": "100",
                "new_packaging_code": "PAL-303", "new_packaging_type": "tote",
                "to_location_id": location,
                "expected_row_version": version + 2, "occurred_at": OCCURRED_AT,
            }}]),
        )
        .await?;
    let detail = refusal(&answer, "ops-split-too-much", "insufficient_quantity")?;
    anyhow::ensure!(
        detail["field"] == "value.quantity" && detail["observed"] == "4",
        "the refusal names the field and what the row holds: {detail}"
    );

    // MERGE the tote back into the pallet: the two need not share a type. The
    // source is consumed, its balance rows are deleted, and the target's
    // revision moves.
    let answer = route
        .post(
            &client,
            "/inventory/merge",
            &json!([{"request_id": "ops-merge", "value": {
                "idempotency_key": "ops-merge-key", "source_packaging_id": new_packaging_id,
                "target_packaging_id": packaging_id, "expected_row_version": version + 2,
                "occurred_at": OCCURRED_AT,
            }}]),
        )
        .await?;
    let merged = value(&answer, "ops-merge")?;
    let merged_revision = version + 3;
    anyhow::ensure!(
        merged["row_version"].as_i64() == Some(merged_revision)
            && merged["target_status"] == "available"
            && merged["target_packaging_id"] == packaging_id
            && merged["source_packaging_id"] == new_packaging_id.as_str(),
        "the merge advanced the target: {merged}"
    );
    one_transaction(merged)?;
    let answer = route
        .get(&client, "/packaging/get", &json!({"id": new_packaging_id}))
        .await?;
    let consumed = read_value(&answer)?;
    anyhow::ensure!(
        consumed["status"] == "consumed" && consumed["located_at"] == OCCURRED_AT,
        "the merged packaging reads consumed, located at the split's occurred_at: {consumed}"
    );
    let answer = route
        .post(
            &client,
            "/inventory/merge",
            &json!([{"request_id": "ops-merge-self", "value": {
                "idempotency_key": "ops-merge-self-key", "source_packaging_id": packaging_id,
                "target_packaging_id": packaging_id, "expected_row_version": version + 3,
                "occurred_at": OCCURRED_AT,
            }}]),
        )
        .await?;
    let detail = refusal(&answer, "ops-merge-self", "invalid_input")?;
    anyhow::ensure!(
        detail["field"] == "value.target_packaging_id",
        "a self-merge names its field: {detail}"
    );

    // LIVE STOCK: 4 left plus the 3 merged back is 7, on one packaging. The
    // consumed one holds no balance row.
    let answer = route
        .get(&client, "/inventory/aggregate", &json!({}))
        .await?;
    let rows = read_value(&answer)?["rows"].clone();
    let rows = rows.as_array().context("aggregate answers rows")?;
    anyhow::ensure!(
        rows.len() == 1
            && (quantity(&rows[0]["quantity"])? - 7.0).abs() < f64::EPSILON
            && rows[0]["packaging_count"] == 1,
        "the aggregate counts 7 on one live packaging and not the consumed one: {rows:?}"
    );

    // QUERY: two packagings exist. By packaging code descending, one per page, the
    // new one comes first and the cursor continues to the fixture; the
    // consumed filter finds exactly the merged one.
    let sort = json!({"field": "packaging_code", "direction": "descending"});
    let answer = route
        .get(
            &client,
            "/packaging/query",
            &json!({"sort": sort, "limit": 1}),
        )
        .await?;
    let page = read_value(&answer)?;
    anyhow::ensure!(
        page["item"].as_array().is_some_and(|rows| rows.len() == 1)
            && page["item"][0]["id"] == new_packaging_id.as_str(),
        "the first page holds the new packaging: {page}"
    );
    let cursor = page["next_cursor"]
        .as_str()
        .with_context(|| format!("a second page exists, so the first carries a cursor: {page}"))?
        .to_owned();
    let answer = route
        .get(
            &client,
            "/packaging/query",
            &json!({"sort": sort, "limit": 1, "cursor": cursor}),
        )
        .await?;
    let page = read_value(&answer)?;
    anyhow::ensure!(
        page["item"][0]["id"] == packaging_id && page["next_cursor"].is_null(),
        "the second page holds the fixture packaging and ends: {page}"
    );
    let answer = route
        .get(
            &client,
            "/packaging/query",
            &json!({"filter": {"status": ["consumed"]}}),
        )
        .await?;
    let page = read_value(&answer)?;
    anyhow::ensure!(
        page["item"].as_array().is_some_and(|rows| rows.len() == 1)
            && page["item"][0]["id"] == new_packaging_id.as_str(),
        "the consumed filter finds exactly the merged packaging: {page}"
    );

    // ADJUST TO ZERO: the stock is gone, so its balance row is deleted.
    let answer = route
        .post(&client, "/inventory/adjust", &json!([{"request_id": "ops-adjust-zero", "value": {
            "idempotency_key": "ops-adjust-zero-key", "packaging_id": packaging_id, "product_id": product_id,
            "status": "available", "quantity": "0", "reason_code": "cycle-count",
            "expected_row_version": merged_revision, "occurred_at": OCCURRED_AT,
        }}]))
        .await?;
    let emptied = value(&answer, "ops-adjust-zero")?;
    one_transaction(emptied)?;
    let answer = route
        .get(&client, "/inventory/aggregate", &json!({}))
        .await?;
    let rows = read_value(&answer)?["rows"].clone();
    anyhow::ensure!(
        rows.as_array().is_some_and(Vec::is_empty),
        "no stock is left after the count of zero: {rows}"
    );

    Ok(json!({"split_packaging_id": new_packaging_id}))
}

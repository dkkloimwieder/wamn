//! `[WMS-RUNTIME-LIVE]` — the two assertions structure cannot make, over the
//! released composed route, from this machine.
//!
//! ONE. Two moves of the same pallet, in flight together, yield EXACTLY ONE
//! `concurrency_conflict`. Not at least one: two would mean neither moved.
//! Not zero: that would mean the lock is not the pallet. And the count can
//! coincide -- a serialized pair gives the same count as a working lock, and
//! one request never arriving gives one success and zero conflicts -- so the
//! two are fired behind one barrier and the SURVIVOR is asserted to have
//! moved the stock from its own response: `location_id` is the target and
//! `row_version` advanced to 2, while the loser's detail observed that 2.
//!
//! TWO. The winner's exact body replayed returns the same `movement_id` --
//! by construction (command-identity-from-claim), not by an early-return
//! path.
//!
//! The label object is the journey's assertion, with the store's own client:
//! this test prints `WMS_CONTENTION_PASS movement_id=<id>` and the journey
//! lists the prefix the composed wiring writes under.
//!
//! THREE (wamn-362o.52). The other five operations serve their released
//! routes: an operation counts as shipped when its route has been hit, and
//! admission, gate and release together prove only that it was declared. On
//! the same fixture, read where it stands, then adjust, split off a new
//! pallet (its id from the claim, so a replay returns the same one), merge it
//! back, and read the totals and the pages that follow; prints
//! `WMS_OPERATIONS_PASS split_pallet_id=<id>`.
//!
//! THIS TEST ASSERTS AND NEVER PROVISIONS. Everything it needs crosses as
//! fields of the journey document: where the route answers from here
//! (`runtime.route_endpoint`), the fixture ids the journey seeded, the route
//! host, and the route-caller PAT's file. No environment variable carries
//! data; `WAMN_JOURNEY_DOCUMENT` names the file.

use anyhow::Context as _;
use serde_json::{Value, json};

use crate::route_authentication_live::{JourneyDocument, RuntimePhase};

const REQUEST_ID_A: &str = "contention-a";
const REQUEST_ID_B: &str = "contention-b";
const REQUEST_ID_REPLAY: &str = "contention-replay";
const OCCURRED_AT: &str = "2026-09-05T12:00:00.000000Z";

struct Route {
    endpoint: String,
    host: String,
    bearer: String,
}

impl Route {
    fn from_document(document: &JourneyDocument, runtime: &RuntimePhase) -> anyhow::Result<Self> {
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

    async fn post(
        &self,
        client: &reqwest::Client,
        path: &str,
        body: &Value,
    ) -> anyhow::Result<Value> {
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
        anyhow::ensure!(
            status.is_success(),
            "POST {path} answered {status} with body {text}"
        );
        serde_json::from_str(&text).with_context(|| format!("the route's answer is JSON: {text}"))
    }
}

fn move_body(request_id: &str, idempotency_key: &str, runtime: &RuntimePhase) -> Value {
    json!([{
        "request_id": request_id,
        "value": {
            "idempotency_key": idempotency_key,
            "pallet_id": runtime.pallet_id,
            "to_location_id": runtime.to_location_id,
            "expected_row_version": 1,
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

#[tokio::test]
#[ignore = "requires the released WMS route on a disposable cluster, named by the journey document's runtime phase"]
async fn contention_and_replay_through_the_composed_route() -> anyhow::Result<()> {
    let document = JourneyDocument::required()?;
    let runtime = document.runtime.as_ref().context(
        "the journey document carries no runtime phase: the route must be reachable and the \
         fixture seeded before these assertions run",
    )?;
    let route = Route::from_document(&document, runtime)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build the route client")?;

    // ONE. Both requests are built before either is sent, then sent together.
    let a = move_body(REQUEST_ID_A, "contention-a-key", runtime);
    let b = move_body(REQUEST_ID_B, "contention-b-key", runtime);
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

    // The survivor MOVED THE STOCK, from its own response: the count above can
    // coincide with a request that never arrived; this cannot.
    let value = &winner["value"];
    anyhow::ensure!(
        value["location_id"] == runtime.to_location_id.as_str(),
        "the winner's pallet is at the target location: {value}"
    );
    anyhow::ensure!(
        value["row_version"] == 2,
        "the winner advanced the pallet's row_version to 2: {value}"
    );
    anyhow::ensure!(
        value["pallet_id"] == runtime.pallet_id.as_str(),
        "the winner moved the fixture pallet: {value}"
    );
    let movement_id = value["movement_id"]
        .as_str()
        .filter(|id| id.len() == 36)
        .context("the winner carries a movement_id")?
        .to_owned();
    // And the loser lost to THAT version, not to something else.
    anyhow::ensure!(
        loser["error"]["detail"]["expected_row_version"] == 1
            && loser["error"]["detail"]["observed_row_version"] == 2,
        "the loser's conflict names expected 1 / observed 2: {loser}"
    );

    // TWO. The winner's exact body, again, with a fresh request id: the same
    // movement id, because the claim was written once under a primary key.
    let mut replay: Value = (**winning_body).clone();
    replay[0]["request_id"] = json!(REQUEST_ID_REPLAY);
    let answer = route.post(&client, "/inventory/move", &replay).await?;
    let replayed = item(&answer, REQUEST_ID_REPLAY)?;
    anyhow::ensure!(
        replayed["value"]["movement_id"] == movement_id.as_str(),
        "a replay returns the same movement_id {movement_id}: {replayed}"
    );
    anyhow::ensure!(
        replayed["value"]["row_version"] == 2,
        "a replay returns the original result, not a second move: {replayed}"
    );

    // On its own line: under --nocapture cargo prints "test <name> ... " with
    // no newline before the test's stdout, and the journey reads this receipt.
    println!("\nWMS_CONTENTION_PASS movement_id={movement_id}");
    Ok(())
}

/// The `value` of the single item answering `request_id`, or the refusal as
/// an error naming it.
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

fn quantity(value: &Value) -> anyhow::Result<f64> {
    value
        .as_str()
        .and_then(|text| text.parse::<f64>().ok())
        .with_context(|| format!("a numeric crosses the wire as a decimal string: {value}"))
}

fn uuid(value: &Value) -> anyhow::Result<String> {
    value
        .as_str()
        .filter(|id| id.len() == 36)
        .map(str::to_owned)
        .with_context(|| format!("a uuid: {value}"))
}

#[tokio::test]
#[ignore = "requires the released WMS route on a disposable cluster, named by the journey document's runtime phase"]
async fn the_remaining_operations_serve_their_released_routes() -> anyhow::Result<()> {
    let document = JourneyDocument::required()?;
    let runtime = document.runtime.as_ref().context(
        "the journey document carries no runtime phase: the route must be reachable and the \
         fixture seeded before these assertions run",
    )?;
    let route = Route::from_document(&document, runtime)?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("build the route client")?;
    let pallet_id = runtime.pallet_id.as_str();

    // WHERE THE FIXTURE STANDS, from the warehouse's own answers rather than
    // from an assumption about which test ran first: the pallet's revision and
    // location by get, and its product by the live-stock aggregate.
    let answer = route
        .post(
            &client,
            "/pallet/get",
            &json!([{"request_id": "ops-get", "id": pallet_id}]),
        )
        .await?;
    let pallet = value(&answer, "ops-get")?;
    anyhow::ensure!(
        pallet["status"] == "available",
        "the fixture pallet is live: {pallet}"
    );
    let version = pallet["row_version"]
        .as_i64()
        .with_context(|| format!("row_version is an integer: {pallet}"))?;
    let location = uuid(&pallet["location_id"])?;

    let answer = route
        .post(
            &client,
            "/inventory/aggregate",
            &json!([{"request_id": "ops-aggregate"}]),
        )
        .await?;
    let rows = value(&answer, "ops-aggregate")?["rows"].clone();
    let rows = rows.as_array().context("aggregate answers rows")?;
    anyhow::ensure!(
        rows.len() == 1 && rows[0]["pallet_count"] == 1 && rows[0]["status"] == "available",
        "one product on one live pallet: {rows:?}"
    );
    anyhow::ensure!(
        rows[0]["location_id"] == location.as_str(),
        "at the pallet's location: {rows:?}"
    );
    let product_id = uuid(&rows[0]["product_id"])?;
    let held = quantity(&rows[0]["quantity"])?;

    // ADJUST: count the row to 7. The movement records what was counted and
    // the pallet's revision moves.
    let answer = route
        .post(&client, "/inventory/adjust", &json!([{"request_id": "ops-adjust", "value": {
            "idempotency_key": "ops-adjust-key", "pallet_id": pallet_id, "product_id": product_id,
            "status": "available", "quantity": "7", "reason_code": "cycle-count",
            "expected_row_version": version, "occurred_at": OCCURRED_AT,
        }}]))
        .await?;
    let adjusted = value(&answer, "ops-adjust")?;
    anyhow::ensure!(
        quantity(&adjusted["adjusted_quantity"])? == 7.0
            && adjusted["row_version"] == version + 1
            && adjusted["pallet_status"] == "available",
        "the adjust counted 7 and advanced the revision from {version} (held {held}): {adjusted}"
    );
    uuid(&adjusted["movement_id"])?;

    // SPLIT: 3 units onto a new pallet beside the source. The new pallet's id
    // comes from the claim, so the exact body again yields the SAME id.
    let split = json!([{"request_id": "ops-split", "value": {
        "idempotency_key": "ops-split-key", "source_pallet_id": pallet_id, "product_id": product_id,
        "status": "available", "quantity": "3", "new_pallet_code": "PAL-302",
        "to_location_id": location, "expected_row_version": version + 1, "occurred_at": OCCURRED_AT,
    }}]);
    let answer = route.post(&client, "/inventory/split", &split).await?;
    let first = value(&answer, "ops-split")?;
    anyhow::ensure!(
        first["row_version"] == version + 2 && first["source_status"] == "available",
        "the split advanced the source: {first}"
    );
    let new_pallet_id = uuid(&first["new_pallet_id"])?;
    let split_movement_id = uuid(&first["movement_id"])?;
    let mut replay = split.clone();
    replay[0]["request_id"] = json!("ops-split-replay");
    let answer = route.post(&client, "/inventory/split", &replay).await?;
    let replayed = value(&answer, "ops-split-replay")?;
    anyhow::ensure!(
        replayed["new_pallet_id"] == new_pallet_id.as_str()
            && replayed["movement_id"] == split_movement_id.as_str()
            && replayed["row_version"] == version + 2,
        "a replayed split returns the same new pallet {new_pallet_id}, not a second one: {replayed}"
    );
    // And a split asking for more than the row holds is refused with what it
    // holds: 4, after 7 less the 3 that left.
    let answer = route
        .post(
            &client,
            "/inventory/split",
            &json!([{"request_id": "ops-split-too-much", "value": {
                "idempotency_key": "ops-split-too-much-key", "source_pallet_id": pallet_id,
                "product_id": product_id, "status": "available", "quantity": "100",
                "new_pallet_code": "PAL-303", "to_location_id": location,
                "expected_row_version": version + 2, "occurred_at": OCCURRED_AT,
            }}]),
        )
        .await?;
    let detail = refusal(&answer, "ops-split-too-much", "insufficient_quantity")?;
    anyhow::ensure!(
        detail["field"] == "value.quantity" && quantity(&detail["observed"])? == 4.0,
        "the refusal names the field and what the row holds: {detail}"
    );

    // MERGE the new pallet back. The source is consumed -- a tombstone, the
    // platform admits no DELETE -- and the target's revision moves.
    let answer = route
        .post(
            &client,
            "/inventory/merge",
            &json!([{"request_id": "ops-merge", "value": {
                "idempotency_key": "ops-merge-key", "source_pallet_id": new_pallet_id,
                "target_pallet_id": pallet_id, "expected_row_version": version + 2,
                "occurred_at": OCCURRED_AT,
            }}]),
        )
        .await?;
    let merged = value(&answer, "ops-merge")?;
    anyhow::ensure!(
        merged["row_version"] == version + 3
            && merged["target_status"] == "available"
            && merged["target_pallet_id"] == pallet_id
            && merged["source_pallet_id"] == new_pallet_id.as_str(),
        "the merge advanced the target: {merged}"
    );
    let answer = route
        .post(
            &client,
            "/pallet/get",
            &json!([{"request_id": "ops-get-consumed", "id": new_pallet_id}]),
        )
        .await?;
    let consumed = value(&answer, "ops-get-consumed")?;
    anyhow::ensure!(
        consumed["status"] == "consumed",
        "the merged pallet reads consumed: {consumed}"
    );
    let answer = route
        .post(
            &client,
            "/inventory/merge",
            &json!([{"request_id": "ops-merge-self", "value": {
                "idempotency_key": "ops-merge-self-key", "source_pallet_id": pallet_id,
                "target_pallet_id": pallet_id, "expected_row_version": version + 3,
                "occurred_at": OCCURRED_AT,
            }}]),
        )
        .await?;
    let detail = refusal(&answer, "ops-merge-self", "invalid_input")?;
    anyhow::ensure!(
        detail["field"] == "value.target_pallet_id",
        "a self-merge names its field: {detail}"
    );

    // LIVE STOCK EXCLUDES THE CONSUMED PALLET: 4 left plus the 3 merged back
    // is 7, on one pallet, though the consumed one still holds its history.
    let answer = route
        .post(
            &client,
            "/inventory/aggregate",
            &json!([{"request_id": "ops-aggregate-after"}]),
        )
        .await?;
    let rows = value(&answer, "ops-aggregate-after")?["rows"].clone();
    let rows = rows.as_array().context("aggregate answers rows")?;
    anyhow::ensure!(
        rows.len() == 1 && quantity(&rows[0]["quantity"])? == 7.0 && rows[0]["pallet_count"] == 1,
        "the aggregate counts 7 on one live pallet and not the consumed one: {rows:?}"
    );

    // QUERY: two pallets exist. By pallet code descending, one per page, the
    // new one comes first and the cursor continues to the fixture; the
    // consumed filter finds exactly the merged one.
    let sort = json!({"field": "pallet_code", "direction": "descending"});
    let answer = route
        .post(
            &client,
            "/pallet/query",
            &json!([{"request_id": "ops-query-1", "sort": sort, "limit": 1}]),
        )
        .await?;
    let page = value(&answer, "ops-query-1")?;
    anyhow::ensure!(
        page["item"]
            .as_array()
            .is_some_and(|items| items.len() == 1)
            && page["item"][0]["id"] == new_pallet_id.as_str(),
        "the first page holds the new pallet: {page}"
    );
    let cursor = page["next_cursor"]
        .as_str()
        .with_context(|| format!("a second page exists, so the first carries a cursor: {page}"))?
        .to_owned();
    let answer = route
        .post(
            &client,
            "/pallet/query",
            &json!([{"request_id": "ops-query-2", "sort": sort, "limit": 1, "cursor": cursor}]),
        )
        .await?;
    let page = value(&answer, "ops-query-2")?;
    anyhow::ensure!(
        page["item"][0]["id"] == pallet_id && page["next_cursor"].is_null(),
        "the second page holds the fixture pallet and ends: {page}"
    );
    let answer = route
        .post(
            &client,
            "/pallet/query",
            &json!([{"request_id": "ops-query-consumed", "filter": {"status": ["consumed"]}}]),
        )
        .await?;
    let page = value(&answer, "ops-query-consumed")?;
    anyhow::ensure!(
        page["item"]
            .as_array()
            .is_some_and(|items| items.len() == 1)
            && page["item"][0]["id"] == new_pallet_id.as_str(),
        "the consumed filter finds exactly the merged pallet: {page}"
    );

    println!("\nWMS_OPERATIONS_PASS split_pallet_id={new_pallet_id}");
    Ok(())
}

#[cfg(test)]
mod shape {
    use super::*;

    fn runtime() -> RuntimePhase {
        RuntimePhase {
            route_endpoint: "http://10.0.0.2:30999".to_owned(),
            pallet_id: "00000000-0000-0000-0000-000000000301".to_owned(),
            to_location_id: "00000000-0000-0000-0000-000000000202".to_owned(),
        }
    }

    #[test]
    fn a_move_body_is_the_array_envelope_the_route_admits() {
        let body = move_body("r", "k", &runtime());
        let items = body.as_array().expect("array envelope");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["request_id"], "r");
        let value = &items[0]["value"];
        for key in [
            "idempotency_key",
            "pallet_id",
            "to_location_id",
            "expected_row_version",
            "occurred_at",
        ] {
            assert!(value.get(key).is_some(), "value carries {key}");
        }
        assert_eq!(value["expected_row_version"], 1);
        assert_eq!(
            value.as_object().expect("object").len(),
            5,
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

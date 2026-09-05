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

    async fn post(&self, client: &reqwest::Client, path: &str, body: &Value) -> anyhow::Result<Value> {
        let response = client
            .post(format!("{}{path}", self.endpoint))
            .header("Host", &self.host)
            .bearer_auth(&self.bearer)
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path} to the released route"))?;
        let status = response.status();
        let text = response.text().await.context("read the route's response body")?;
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
    let items = answer.as_array().context("the route answers with an array envelope")?;
    anyhow::ensure!(items.len() == 1, "one request, one item; got {}", items.len());
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
        for key in ["idempotency_key", "pallet_id", "to_location_id", "expected_row_version", "occurred_at"] {
            assert!(value.get(key).is_some(), "value carries {key}");
        }
        assert_eq!(value["expected_row_version"], 1);
        assert_eq!(value.as_object().expect("object").len(), 5, "exactly the declared fields, nothing extra");
    }

    #[test]
    fn an_item_must_answer_the_request_it_was_asked_with() {
        let answer = json!([{"request_id": "other", "value": {}}]);
        assert!(item(&answer, "mine").is_err());
        let answer = json!([{"request_id": "mine", "value": {}}, {"request_id": "mine", "value": {}}]);
        assert!(item(&answer, "mine").is_err(), "two items for one request is refused");
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
        assert_eq!(format!("{}{}", route.endpoint, "/inventory/move"), "http://10.0.0.2:30999/inventory/move");
    }
}

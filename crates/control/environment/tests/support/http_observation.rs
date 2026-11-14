//! Wire observations shared by product connection journeys.

use anyhow::ensure;
use serde_json::{Value, json};

pub fn assert_observed_request(status: u16, body: &str, log: &str) -> anyhow::Result<()> {
    ensure!(status == 200, "route returned {status}: {body}");
    let response: Value = serde_json::from_str(body)?;
    ensure!(response["status"] == 200, "guest response: {response}");
    ensure!(
        response["body"] == json!({"id": 1}),
        "guest body: {response}"
    );
    let records = route_requests(log)?;
    let requests: Vec<_> = records
        .iter()
        .map(|record| record["head"].as_str().unwrap_or_default())
        .collect();
    ensure!(
        requests.len() == 1,
        "fixture saw {} requests",
        requests.len()
    );
    ensure!(
        requests[0].lines().next() == Some("POST /reuse HTTP/1.1"),
        "fixture received the wrong target or protocol"
    );
    ensure!(
        records[0]["body"] == json!(br#"{"id":1}"#.as_slice()),
        "fixture received the wrong body"
    );
    let headers: Vec<_> = requests[0]
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .collect();
    let authorization: Vec<_> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.trim())
        .collect();
    ensure!(
        authorization == ["Basic Zml4dHVyZTpmaXh0dXJlLXBhc3N3b3Jk"],
        "wrong credential reached the wire"
    );
    ensure!(
        headers
            .iter()
            .any(|(name, value)| name.eq_ignore_ascii_case("idempotency-key")
                && value.trim().ends_with(":send:0")),
        "effect omitted its delivery idempotency key"
    );
    Ok(())
}

pub fn request_records(log: &str) -> anyhow::Result<Vec<Value>> {
    log.lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn route_requests(log: &str) -> anyhow::Result<Vec<Value>> {
    Ok(request_records(log)?
        .into_iter()
        .filter(|record| {
            // The schedule has its own target, so its traffic cannot count as a route request.
            record["head"].as_str().is_some_and(|head| {
                head.starts_with("POST /reuse HTTP/1.1\r\n")
                    || head.starts_with("POST /signed HTTP/1.1\r\n")
            })
        })
        .collect())
}

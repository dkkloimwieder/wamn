//! A root-provisioned operation returns its private file through a signed read.
//!
//! The operation accepts `value.document`, returns the same UUID as
//! `value.document`, and declares `result.signed_reads.document` with a
//! twelve-second lifetime. The operation session holds that operation alone.

use serde_json::json;
use std::time::Duration;

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name} for the signed file live test"))
}

#[tokio::test]
#[ignore = "requires a disposable host, a marked operation, private file, and scoped sessions"]
async fn operation_grant_signs_private_files_and_a_failure_returns_no_partial_url() {
    let route = required("WAMN_C4_SIGNED_OPERATION_URL");
    let session = required("WAMN_C4_SIGNED_OPERATION_SESSION");
    let reader = required("WAMN_C4_SIGNED_FILE_GET_SESSION");
    let ungranted = required("WAMN_C4_UNGRANTED_SESSION");
    let file_url = required("WAMN_C4_PRIVATE_FILE_URL");
    let file_id = required("WAMN_C4_PRIVATE_FILE_ID");
    let expected = required("WAMN_C4_PRIVATE_FILE_BYTES");
    let client = reqwest::Client::new();
    // Both an operation-only role and a role with file.get refuse the direct
    // private path. The operation's own grant controls the signed result.
    for credential in [&session, &reader] {
        assert_eq!(
            client
                .get(&file_url)
                .bearer_auth(credential)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16(),
            403
        );
    }
    let request = |id: &str| {
        let item = json!({"value": {"document": id}});
        format!(
            "{route}?{}",
            wamn_execution_contract::encode_read_query(item.as_object().unwrap())
        )
    };
    let request_url = request(&file_id);
    assert_eq!(
        client
            .get(&request_url)
            .bearer_auth(ungranted)
            .send()
            .await
            .unwrap()
            .status()
            .as_u16(),
        403
    );
    let response = client
        .get(&request_url)
        .bearer_auth(&session)
        .header("if-none-match", "*")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(!response.headers().contains_key("etag"));
    let result: serde_json::Value = response.json().await.unwrap();
    let signed = result[0]["value"]["document"].as_str().unwrap();
    let url = url::Url::parse(signed).unwrap();
    let expires = url
        .query_pairs()
        .find(|(key, _)| key == "X-Amz-Expires" || key == "X-Goog-Expires")
        .unwrap()
        .1
        .parse::<u64>()
        .unwrap();
    assert_eq!(expires, 12);
    let bytes = client
        .get(signed)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(bytes.as_ref(), expected.as_bytes());

    let refused = client
        .get(request(&uuid::Uuid::new_v4().to_string()))
        .bearer_auth(&session)
        .send()
        .await
        .unwrap();
    assert!(refused.status().is_server_error());
    let body = refused.text().await.unwrap();
    assert!(!body.contains("X-Amz-Signature"));
    assert!(!body.contains("X-Goog-Signature"));
    tokio::time::sleep(Duration::from_secs(13)).await;
    assert!(
        !client
            .get(signed)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
}

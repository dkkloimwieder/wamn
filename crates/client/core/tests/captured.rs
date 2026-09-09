//! Captured submission bytes stay fixed while credentials rotate between attempts.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::json;
use wamn_client::credentials::CredentialError;
use wamn_client::descriptor::{FieldDescriptor, FieldSchema};
use wamn_client::request::build_request;
use wamn_client::{
    ClientError, CredentialProvider, HttpRequest, HttpResponse, RouteMetadata, Transport,
    WamnClient,
};

#[path = "../../../../packages/receiving/generated/client/purchase_order.rs"]
pub mod purchase_order;

#[derive(Debug, Default)]
struct RotatingCredential {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl CredentialProvider for RotatingCredential {
    async fn bearer(&self) -> Result<String, CredentialError> {
        Ok(match self.calls.fetch_add(1, Ordering::Relaxed) {
            0 => "test-token-first",
            1 => "test-token-second",
            _ => panic!("the test expects exactly two credential requests"),
        }
        .to_owned())
    }
}

#[derive(Debug)]
struct RecordingTransport {
    requests: Mutex<Vec<HttpRequest>>,
    replies: Mutex<VecDeque<HttpResponse>>,
}

#[async_trait::async_trait]
impl Transport for RecordingTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, ClientError> {
        self.requests.lock().expect("record request").push(request);
        Ok(self
            .replies
            .lock()
            .expect("read reply")
            .pop_front()
            .expect("a reply for each attempt"))
    }
}

const fn scalar(path: &'static str, type_name: &'static str) -> FieldSchema {
    FieldSchema {
        field: FieldDescriptor {
            path,
            type_name,
            nullable: false,
            values: &[],
        },
        required: true,
        children: &[],
        minimum: None,
        maximum: None,
    }
}

const COMMAND: &[FieldSchema] = &[
    scalar("request_id", "string"),
    FieldSchema {
        children: &[
            scalar("value.idempotency_key", "text"),
            scalar("value.occurred_at", "timestamptz"),
            scalar("value.expected_row_version", "int64"),
            FieldSchema {
                children: &[
                    scalar("value.line[].id", "uuid"),
                    scalar("value.line[].quantity", "numeric"),
                ],
                minimum: Some(1),
                maximum: Some(2),
                ..scalar("value.line[]", "array")
            },
        ],
        ..scalar("value", "object")
    },
];

#[tokio::test]
async fn captured_retries_preserve_actual_command_bytes_and_raw_response_evidence() {
    let credentials = Arc::new(RotatingCredential::default());
    let first = HttpResponse {
        status: 403,
        body: r#"{"error":{"code":"permission-denied","operation":"wamn-test:inventory/adjust@1.0.0"},"evidence":{"unparsed":true}}"#.into(),
    };
    let second = HttpResponse {
        status: 503,
        body: "  opaque upstream failure\nnot JSON\n".into(),
    };
    let transport = Arc::new(RecordingTransport {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([first.clone(), second.clone()])),
    });
    let client = WamnClient::new(
        "http://127.0.0.1:12345/",
        Some("test.localhost".into()),
        credentials.clone(),
        transport.clone(),
    );
    let route = RouteMetadata {
        method: "POST".into(),
        template: "/inventory/adjust".into(),
    };
    let built = build_request(COMMAND, &json!({"request_id":"submission-7","value":{
        "idempotency_key":"intent-7", "occurred_at":"2026-09-08T15:30:00-04:00", "expected_row_version":7,
        "line":[{"id":"ABCDEF00000000000000000000000001","quantity":"+0005.000"}]
    }}), None).expect("capture a canonical nested command");

    assert_eq!(
        client
            .submit(&route, &BTreeMap::new(), &built)
            .await
            .expect("preserve raw 403"),
        first
    );
    assert_eq!(
        client
            .submit(&route, &BTreeMap::new(), &built)
            .await
            .expect("preserve arbitrary error body"),
        second
    );

    let requests = transport.requests.lock().expect("read sent requests");
    assert_eq!(requests.len(), 2);
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(request.body, br#"[{"request_id":"submission-7","value":{"expected_row_version":"7","idempotency_key":"intent-7","line":[{"id":"abcdef00-0000-0000-0000-000000000001","quantity":"5.000"}],"occurred_at":"2026-09-08T19:30:00.000000Z"}}]"#);
        assert_eq!(request.url, "http://127.0.0.1:12345/inventory/adjust");
        assert_eq!(request.method, "POST");
        assert_eq!(request.headers["host"], "test.localhost");
        assert_eq!(request.headers["content-type"], "application/json");
        assert_eq!(
            request.headers["authorization"],
            ["Bearer test-token-first", "Bearer test-token-second"][index]
        );
    }
    assert_eq!(credentials.calls.load(Ordering::Relaxed), 2);
}

#[tokio::test]
async fn generated_supplier_contract_preserves_absence_and_value_on_the_wire_and_blocks_null() {
    let credentials = Arc::new(RotatingCredential::default());
    let transport = Arc::new(RecordingTransport {
        requests: Mutex::new(Vec::new()),
        replies: Mutex::new(VecDeque::from([
            HttpResponse {
                status: 200,
                body: "[]".into(),
            },
            HttpResponse {
                status: 200,
                body: "[]".into(),
            },
        ])),
    });
    let client = WamnClient::new(
        "http://127.0.0.1:12345/",
        Some("test.localhost".into()),
        credentials.clone(),
        transport.clone(),
    );
    let attachments: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../packages/receiving/publication/attachments.json"
    ))
    .expect("read the Receiving serving schema");
    let schema = &attachments["purchase-order-update-http"]["definition"]["input-schema"];
    let fields = purchase_order::PURCHASE_ORDER_UPDATE_INPUT_SCHEMA;
    let route = purchase_order::update_route();
    let mut item = json!({"request_id":"supplier-absent", "id":"00000000-0000-0000-0000-000000000001",
        "expected_row_version":4,"change":{}});

    let absent = build_request(fields, &item, Some(schema)).expect("a supplier can stay absent");
    client
        .submit(&route, &BTreeMap::new(), &absent)
        .await
        .expect("send the absent supplier");

    item["request_id"] = json!("supplier-null");
    item["change"]["supplier_id"] = serde_json::Value::Null;
    let error = build_request(fields, &item, Some(schema))
        .expect_err("the generated supplier field refuses null");
    assert_eq!(
        error.kind(),
        wamn_client::request::RequestErrorKind::NullNotAllowed
    );
    assert_eq!(error.path(), "$.change.supplier_id");
    assert_eq!(transport.requests.lock().expect("read sent count").len(), 1);
    assert_eq!(credentials.calls.load(Ordering::Relaxed), 1);

    item["request_id"] = json!("supplier-value");
    item["change"]["supplier_id"] = json!("ABCDEF00000000000000000000000002");
    let value = build_request(fields, &item, Some(schema)).expect("the supplier UUID is valid");
    client
        .submit(&route, &BTreeMap::new(), &value)
        .await
        .expect("send the supplier UUID");

    let requests = transport
        .requests
        .lock()
        .expect("read actual outgoing requests");
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].body, br#"[{"change":{},"expected_row_version":"4","id":"00000000-0000-0000-0000-000000000001","request_id":"supplier-absent"}]"#);
    assert_eq!(requests[1].body, br#"[{"change":{"supplier_id":"abcdef00-0000-0000-0000-000000000002"},"expected_row_version":"4","id":"00000000-0000-0000-0000-000000000001","request_id":"supplier-value"}]"#);
    for request in requests.iter() {
        assert_eq!(request.url, "http://127.0.0.1:12345/purchase_order/update");
        assert_eq!(request.method, "POST");
    }
    assert_eq!(credentials.calls.load(Ordering::Relaxed), 2);
}

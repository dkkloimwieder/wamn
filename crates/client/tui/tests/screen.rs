//! Screen transitions use declared bindings and captured HTTP evidence.

use serde_json::{Value, json};
use wamn_client::descriptor::{FieldDescriptor, FieldSchema};
use wamn_client::{HttpResponse, RouteMetadata};
use wamn_client_tui::draft::FieldState;
use wamn_client_tui::screen::{
    Availability, ExitState, IntentValues, RecordLink, RevisionBinding, Screen, ScreenErrorKind,
    ScreenSpec, SuppliedField, SuppliedKind,
};
use wamn_client_tui::submission::{Replay, ResponseContract, SessionBinding, State};

const ID: &str = "00000000-0000-0000-0000-000000000001";
const REQUEST_ID: &str = "00000000-0000-0000-0000-000000000002";
const KEY: &str = "00000000-0000-0000-0000-000000000003";

const fn scalar(path: &'static str, type_name: &'static str, required: bool) -> FieldSchema {
    FieldSchema {
        field: FieldDescriptor {
            path,
            type_name,
            nullable: false,
            values: &[],
        },
        required,
        children: &[],
        minimum: None,
        maximum: None,
    }
}

const RESULT: &[FieldSchema] = &[
    scalar("id", "uuid", true),
    scalar("row_version", "int64", true),
];
const READ_INPUT: &[FieldSchema] = &[
    scalar("request_id", "uuid", true),
    scalar("id", "uuid", true),
    scalar("locale", "text", true),
];
const PAGE_INPUT: &[FieldSchema] = &[
    scalar("request_id", "uuid", true),
    scalar("cursor", "text", false),
    FieldSchema {
        children: &[scalar("filter.name", "text", false)],
        ..scalar("filter", "object", false)
    },
    scalar("sort", "text", false),
];
const COMMAND_INPUT: &[FieldSchema] = &[
    scalar("request_id", "uuid", true),
    scalar("id", "uuid", true),
    scalar("expected_row_version", "int64", true),
    scalar("idempotency_key", "uuid", true),
    scalar("occurred_at", "timestamptz", true),
    FieldSchema {
        children: &[scalar("line[].quantity", "numeric", true)],
        minimum: Some(1),
        maximum: Some(2),
        ..scalar("line[]", "array", true)
    },
];
const SUPPLIED: &[SuppliedField] = &[SuppliedField {
    path: "request_id",
    kind: SuppliedKind::RequestId,
}];
const RECORD: RecordLink = RecordLink {
    relation: "inventory",
    key_field: "id",
    key_input: Some("id"),
};
const READ: ScreenSpec = ScreenSpec {
    model: "inventory",
    name: "get",
    operation: "inventory.get",
    kind: "get",
    input: READ_INPUT,
    input_schema: None,
    response: ResponseContract {
        schema: None,
        partial_schema: None,
        fields: RESULT,
        result_class: Some("one"),
        errors: &[],
        kind: "get",
        transaction: Some("implicit"),
        direct: true,
        replay: Replay::Unknown,
    },
    route: Some(route),
    record: Some(RECORD),
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: SUPPLIED,
};
const PAGE: ScreenSpec = ScreenSpec {
    name: "query",
    operation: "inventory.query",
    kind: "query",
    input: PAGE_INPUT,
    response: ResponseContract {
        result_class: Some("page"),
        kind: "query",
        ..READ.response
    },
    record: Some(RecordLink {
        key_input: None,
        ..RECORD
    }),
    ..READ
};
const COMMAND: ScreenSpec = ScreenSpec {
    name: "adjust",
    operation: "inventory.adjust",
    kind: "command",
    input: COMMAND_INPUT,
    response: ResponseContract {
        kind: "command",
        replay: Replay::Claim,
        ..READ.response
    },
    revision: Some(RevisionBinding {
        read_operation: "inventory.get",
        read_key_input: "id",
        key_field: "id",
        revision_field: "row_version",
        command_key_input: "id",
        command_revision_input: "expected_row_version",
    }),
    revision_inputs: &["expected_row_version"],
    supplied: &[
        SuppliedField {
            path: "request_id",
            kind: SuppliedKind::RequestId,
        },
        SuppliedField {
            path: "idempotency_key",
            kind: SuppliedKind::IdempotencyKey,
        },
        SuppliedField {
            path: "occurred_at",
            kind: SuppliedKind::OccurredAt,
        },
    ],
    ..READ
};

fn route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".to_owned(),
        template: "/inventory".to_owned(),
    }
}
fn binding(target: &str) -> SessionBinding {
    SessionBinding {
        url: "http://localhost:8080".to_owned(),
        host: Some("inventory.local".to_owned()),
        target_instance: target.to_owned(),
    }
}
fn intent() -> IntentValues {
    IntentValues {
        request_id: REQUEST_ID.to_owned(),
        idempotency_key: KEY.to_owned(),
        occurred_at: "2026-09-08T12:00:00-04:00".to_owned(),
    }
}
fn result() -> Value {
    json!({"id":ID,"row_version":"7"})
}
fn response(screen: &Screen, value: &Value) -> HttpResponse {
    HttpResponse {
        status: 200,
        body: json!([{
            "request_id":screen.submission().captured().expect("capture").item()["request_id"],
            "value":value,
        }])
        .to_string(),
    }
}
fn read_record() -> Screen {
    let mut read = Screen::new(&READ, binding("a"));
    read.edit("/id", FieldState::Value(json!(ID))).expect("id");
    read.edit("/locale", FieldState::Value(json!("en")))
        .expect("locale");
    let attempt = read.begin(&intent(), false).expect("read starts");
    let response = response(&read, &result());
    assert!(read.resolve(attempt, Ok(response)));
    read
}
fn fill_command(command: &mut Screen) {
    command
        .bind_from_read(&read_record())
        .expect("declared read binds");
    command
        .insert_row("/line", 0, json!({"quantity":"+0002.50"}))
        .expect("line");
}
fn page_result(screen: &mut Screen, cursor: &str) {
    let attempt = screen.begin(&intent(), false).expect("page starts");
    let response = response(screen, &json!({"item":[result()],"next_cursor":cursor}));
    assert!(screen.resolve(attempt, Ok(response)));
}

#[test]
fn selected_record_supplies_only_the_declared_key_and_waits_for_extra_read_input() {
    const OTHER: ScreenSpec = ScreenSpec {
        record: Some(RecordLink {
            relation: "other",
            ..RECORD
        }),
        ..READ
    };
    let mut page = Screen::new(&PAGE, binding("a"));
    page_result(&mut page, "opaque");
    let mut read = Screen::new(&READ, binding("a"));
    read.populate_read_from_record(&page, 0)
        .expect("same declared relation/key");
    assert_eq!(read.draft().item(), &json!({"id":ID}));
    assert_eq!(
        read.begin(&intent(), false)
            .expect_err("locale still required")
            .kind(),
        ScreenErrorKind::Request
    );
    assert!(read.submission().captured().is_none());
    assert_eq!(
        read.draft().state("/locale").expect("locale"),
        FieldState::Absent
    );
    read.edit("/locale", FieldState::Value(json!("en")))
        .expect("remaining typed input");
    assert!(read.begin(&intent(), false).is_ok());

    let mut unrelated = Screen::new(&OTHER, binding("a"));
    assert!(unrelated.populate_read_from_record(&page, 0).is_err());
    assert_eq!(unrelated.draft().item(), &json!({}));
}

#[test]
fn revision_transfer_requires_the_exact_successful_read_and_reserves_envelope_paths() {
    let read = read_record();
    let mut command = Screen::new(&COMMAND, binding("a"));
    assert_eq!(command.availability(), Availability::NeedsRecord);
    for path in [
        "/id",
        "/expected_row_version",
        "/request_id",
        "/idempotency_key",
        "/occurred_at",
    ] {
        assert_eq!(
            command
                .edit(path, FieldState::Value(json!("typed")))
                .expect_err("reserved")
                .kind(),
            ScreenErrorKind::Draft
        );
    }
    let mut page = Screen::new(&PAGE, binding("a"));
    page_result(&mut page, "cursor");
    assert!(command.bind_from_read(&page).is_err());
    command.bind_from_read(&read).expect("declared get");
    assert_eq!(
        command.draft().item(),
        &json!({"id":ID,"expected_row_version":"7"})
    );
    assert_eq!(command.availability(), Availability::Ready);
    let mut other_target = Screen::new(&COMMAND, binding("b"));
    assert!(other_target.bind_from_read(&read).is_err());
}

#[test]
fn a_malformed_or_different_record_read_cannot_supply_a_revision() {
    for row in [
        json!({"id":ID,"row_version":7}),
        json!({"id":KEY,"row_version":"7"}),
    ] {
        let mut read = Screen::new(&READ, binding("a"));
        read.edit("/id", FieldState::Value(json!(ID))).expect("id");
        read.edit("/locale", FieldState::Value(json!("en")))
            .expect("locale");
        let attempt = read.begin(&intent(), false).expect("begin");
        let response = response(&read, &row);
        read.resolve(attempt, Ok(response));
        let mut command = Screen::new(&COMMAND, binding("a"));
        assert!(command.bind_from_read(&read).is_err());
        assert_eq!(command.availability(), Availability::NeedsRecord);
    }
}

#[test]
fn page_cursor_is_untouched_and_filter_or_sort_edits_reset_it_and_results() {
    let opaque = " {\"not\":\"a local token\"} +/%=\n";
    for (path, value) in [
        ("/filter/name", json!("new")),
        ("/sort", json!("name_desc")),
    ] {
        let mut page = Screen::new(&PAGE, binding("a"));
        page_result(&mut page, opaque);
        assert_eq!(page.cursor(), Some(opaque));
        assert_eq!(page.rows(), &[result()]);
        assert!(
            page.edit("/cursor", FieldState::Value(json!("invented")))
                .is_err()
        );
        page.next_page().expect("next page");
        let attempt = page.begin(&intent(), false).expect("next submission");
        assert_eq!(
            page.submission().captured().expect("capture").item()["cursor"],
            opaque
        );
        let response = response(&page, &json!({"item":[result()],"next_cursor":opaque}));
        page.resolve(attempt, Ok(response));
        page.edit(path, FieldState::Value(value))
            .expect("changed read input");
        assert_eq!(page.cursor(), None);
        assert!(page.rows().is_empty());
        assert_eq!(
            page.draft().state("/cursor").expect("cursor"),
            FieldState::Absent
        );
        page.begin(&intent(), false)
            .expect("new filter starts at first page");
        assert!(
            page.submission()
                .captured()
                .expect("capture")
                .item()
                .get("cursor")
                .is_none()
        );
    }
}

#[test]
fn captured_retry_keeps_new_intent_values_and_success_spends_the_mutation() {
    let mut command = Screen::new(&COMMAND, binding("a"));
    fill_command(&mut command);
    let first = command.begin(&intent(), false).expect("first intent");
    let capture = command.submission().captured().expect("capture").clone();
    assert_eq!(capture.item()["occurred_at"], "2026-09-08T16:00:00.000000Z");
    assert_eq!(capture.item()["line"][0]["quantity"], "2.50");
    assert!(command.begin(&intent(), false).is_err());
    command.resolve(
        first,
        Ok(HttpResponse {
            status: 502,
            body: "response lost".to_owned(),
        }),
    );
    assert_eq!(command.availability(), Availability::Uncertain);
    assert!(
        command
            .edit("/line/0/quantity", FieldState::Value(json!("3")))
            .is_err()
    );
    let retry = command.retry().expect("claim permits captured retry");
    assert_eq!(command.submission().captured(), Some(&capture));
    let response = response(&command, &result());
    command.resolve(retry, Ok(response));
    assert_eq!(command.availability(), Availability::Spent);
    assert!(command.begin(&intent(), false).is_err());
    command.new_command().expect("explicit new command");
    assert_eq!(command.availability(), Availability::NeedsRecord);
    assert_eq!(command.draft().item(), &json!({}));
    assert!(command.submission().captured().is_none());
}

#[test]
fn unavailable_unsupported_unexposed_and_composition_are_distinct() {
    const UNEXPOSED: ScreenSpec = ScreenSpec {
        route: None,
        ..READ
    };
    const UNSUPPORTED: ScreenSpec = ScreenSpec {
        input: &[scalar("opaque", "json", false)],
        supplied: &[],
        ..READ
    };
    const COMPOSED: ScreenSpec = ScreenSpec {
        revision: None,
        requires_composition: true,
        ..COMMAND
    };
    assert_eq!(
        Screen::new(&UNEXPOSED, binding("a")).availability(),
        Availability::NotExposed
    );
    assert_eq!(
        Screen::new(&UNSUPPORTED, binding("a")).availability(),
        Availability::Unsupported
    );
    let mut command = Screen::new(&COMPOSED, binding("a"));
    assert_eq!(command.availability(), Availability::RequiresComposition);
    assert!(command.mark_composed().is_err());
    assert!(
        command
            .edit("/expected_row_version", FieldState::Value(json!("8")))
            .is_err()
    );
    command
        .bind("/id", json!(ID))
        .expect("explicit exact key binding");
    command
        .bind("/expected_row_version", json!("8"))
        .expect("explicit exact revision binding");
    command.mark_composed().expect("composition completed");
    assert_eq!(command.availability(), Availability::Ready);
    command.invalidate();
    assert_eq!(command.availability(), Availability::Unavailable);
    command.activate(binding("b"));
    assert_eq!(command.availability(), Availability::RequiresComposition);
}

#[test]
fn unchanged_activation_preserves_state_but_same_schema_target_replacement_resets_everything() {
    let mut page = Screen::new(&PAGE, binding("a"));
    page.edit("/filter/name", FieldState::Value(json!("prior")))
        .expect("filter");
    page_result(&mut page, "old target cursor");
    let item = page.draft().item().clone();
    assert!(
        !page.activate(binding("a")),
        "failed replacement leaves prior activation usable"
    );
    assert_eq!(page.draft().item(), &item);
    assert_eq!(page.cursor(), Some("old target cursor"));
    assert_eq!(page.rows(), &[result()]);
    assert!(
        page.activate(binding("b")),
        "unchanged spec still gets new target state"
    );
    assert_eq!(page.draft().item(), &json!({}));
    assert!(page.rows().is_empty());
    assert_eq!(page.cursor(), None);
    assert!(!page.dirty());

    let mut command = Screen::new(&COMMAND, binding("a"));
    fill_command(&mut command);
    assert!(command.dirty());
    let old_attempt = command.begin(&intent(), false).expect("pending old target");
    let late = response(&command, &result());
    assert!(
        command.invalidate(),
        "work can still complete; invalidation does not cancel it"
    );
    assert!(command.begin(&intent(), false).is_err());
    assert_eq!(command.draft().item(), &json!({}));
    assert!(!command.dirty());
    assert!(command.submission().captured().is_none());
    command.activate(binding("b"));
    assert_eq!(command.availability(), Availability::NeedsRecord);
    assert!(!command.resolve(old_attempt, Ok(late)));
    assert_eq!(command.submission().state(), &State::Editable);
}

#[test]
fn delete_needs_confirmation_and_pending_prevents_exit_or_draft_discard() {
    const DELETE: ScreenSpec = ScreenSpec {
        kind: "delete",
        ..COMMAND
    };
    let mut command = Screen::new(&DELETE, binding("a"));
    assert_eq!(command.exit_state(), ExitState::Ready);
    fill_command(&mut command);
    assert_eq!(command.exit_state(), ExitState::ConfirmDiscard);
    assert_eq!(
        command
            .begin(&intent(), false)
            .expect_err("confirmation")
            .kind(),
        ScreenErrorKind::ConfirmationRequired
    );
    assert!(command.submission().captured().is_none());
    let attempt = command.begin(&intent(), true).expect("confirmed deletion");
    assert_eq!(command.exit_state(), ExitState::Pending);
    assert!(command.new_command().is_err());
    assert!(command.remove_row("/line", 0).is_err());
    command.resolve(
        attempt,
        Ok(HttpResponse {
            status: 401,
            body: String::new(),
        }),
    );
    assert!(matches!(command.submission().state(), State::Refused(_)));
    assert_eq!(command.exit_state(), ExitState::ConfirmDiscard);
    assert_eq!(
        command.draft().item()["line"],
        json!([{"quantity":"+0002.50"}])
    );
    assert_eq!(
        command
            .begin(&intent(), false)
            .expect_err("each delete needs confirmation")
            .kind(),
        ScreenErrorKind::ConfirmationRequired
    );
}

#[test]
fn unknown_output_remains_visible_and_cannot_supply_record_bindings() {
    const OPAQUE: ScreenSpec = ScreenSpec {
        response: ResponseContract {
            fields: &[],
            result_class: None,
            ..READ.response
        },
        ..READ
    };
    let mut read = Screen::new(&OPAQUE, binding("a"));
    read.edit("/id", FieldState::Value(json!(ID))).expect("id");
    read.edit("/locale", FieldState::Value(json!("en")))
        .expect("locale");
    let attempt = read.begin(&intent(), false).expect("begin");
    let raw = json!({"untyped":[true,9,{"detail":"retained"}]});
    let response = response(&read, &raw);
    read.resolve(attempt, Ok(response));
    assert!(matches!(
        read.submission().state(),
        State::Succeeded { opaque: true, .. }
    ));
    assert_eq!(read.rows(), &[raw]);
    let mut command = Screen::new(&COMMAND, binding("a"));
    assert!(command.bind_from_read(&read).is_err());
}

#[test]
fn record_refresh_changes_only_the_declared_key_and_keeps_other_inputs_explicit() {
    let mut command = Screen::new(&COMMAND, binding("a"));
    command.bind_from_read(&read_record()).unwrap();
    let mut read = Screen::new(&READ, binding("a"));
    read.edit("/id", FieldState::Value(json!(KEY))).unwrap();
    command.prepare_record_refresh(&mut read).unwrap();
    assert_eq!(read.draft().item()["id"], ID);
    assert!(read.draft().item().get("locale").is_none());
    assert!(read.begin(&intent(), false).is_err());
    read.edit("/locale", FieldState::Value(json!("fr")))
        .unwrap();
    command.prepare_record_refresh(&mut read).unwrap();
    assert_eq!(read.draft().item()["locale"], "fr");
    assert!(read.begin(&intent(), false).is_ok());
}

#[test]
fn record_refresh_refuses_other_targets_other_operations_and_unbound_commands() {
    const OTHER_READ: ScreenSpec = ScreenSpec {
        operation: "inventory.other_get",
        ..READ
    };
    let mut command = Screen::new(&COMMAND, binding("a"));
    let mut read = Screen::new(&READ, binding("a"));
    assert!(command.prepare_record_refresh(&mut read).is_err());
    command.bind_from_read(&read_record()).unwrap();
    let mut other_target = Screen::new(&READ, binding("b"));
    assert!(command.prepare_record_refresh(&mut other_target).is_err());
    let mut other_read = Screen::new(&OTHER_READ, binding("a"));
    assert!(command.prepare_record_refresh(&mut other_read).is_err());
    assert!(other_target.draft().item().get("id").is_none());
    assert!(other_read.draft().item().get("id").is_none());
}

mod cursor_transport {
    use std::collections::BTreeMap;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};

    use serde_json::{Value, json};
    use wamn_client::{ClientError, HttpRequest, HttpResponse, StaticPat, Transport, WamnClient};
    use wamn_client_tui::draft::FieldState;
    use wamn_client_tui::screen::Screen;
    use wamn_client_tui::submission::State;

    use super::{PAGE, REQUEST_ID, binding, intent, result, route};

    const CURSOR: &str = " {\"not\":\"a local token\"} +/%=\n";

    #[derive(Debug, Default)]
    struct RecordingTransport(Mutex<Vec<HttpRequest>>);

    impl Transport for RecordingTransport {
        // Match the trait's boxed future without adding a test macro dependency.
        fn send<'life0, 'async_trait>(
            &'life0 self,
            request: HttpRequest,
        ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, ClientError>> + Send + 'async_trait>>
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.0.lock().expect("record request").push(request);
                Ok(HttpResponse {
                    status: 200,
                    body: json!([{
                        "request_id":REQUEST_ID,
                        "value":{"item":[result()],"next_cursor":CURSOR}
                    }])
                    .to_string(),
                })
            })
        }
    }

    fn complete_immediately<F: Future>(future: F) -> F::Output {
        let mut future = Box::pin(future);
        // StaticPat and this transport perform no asynchronous I/O.
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("the recording fixture must complete in one poll"),
        }
    }

    fn send(screen: &mut Screen, client: &WamnClient) {
        let attempt = screen.begin(&intent(), false).expect("query starts");
        let response = complete_immediately(client.submit(
            &route(),
            &BTreeMap::new(),
            screen.submission().captured().expect("query body captured"),
        ));
        assert!(screen.resolve(attempt, response));
        assert!(matches!(
            screen.submission().state(),
            State::Succeeded { .. }
        ));
    }

    #[test]
    fn outgoing_cursor_bytes_stay_opaque_and_filter_or_sort_resets_remove_them() {
        let resets: [(&str, Value, &[u8]); 2] = [
            (
                "/filter/name",
                json!("new"),
                br#"[{"filter":{"name":"new"},"request_id":"00000000-0000-0000-0000-000000000002"}]"#,
            ),
            (
                "/sort",
                json!("name_desc"),
                br#"[{"request_id":"00000000-0000-0000-0000-000000000002","sort":"name_desc"}]"#,
            ),
        ];
        for (path, value, reset_body) in resets {
            let transport = Arc::new(RecordingTransport::default());
            let session = binding("a");
            let client = WamnClient::new(
                &session.url,
                session.host.clone(),
                Arc::new(StaticPat::new("test-cursor-token").expect("test credential")),
                transport.clone(),
            );
            let mut page = Screen::new(&PAGE, session);
            send(&mut page, &client);
            assert_eq!(page.cursor(), Some(CURSOR));
            page.next_page().expect("server cursor selects next page");
            send(&mut page, &client);
            page.edit(path, FieldState::Value(value))
                .expect("change query");
            assert_eq!(page.cursor(), None);
            send(&mut page, &client);

            let requests = transport.0.lock().expect("read sent requests");
            assert_eq!(requests.len(), 3);
            assert_eq!(
                requests[0].body,
                br#"[{"request_id":"00000000-0000-0000-0000-000000000002"}]"#
            );
            assert_eq!(requests[1].body, br#"[{"cursor":" {\"not\":\"a local token\"} +/%=\n","request_id":"00000000-0000-0000-0000-000000000002"}]"#);
            assert_eq!(requests[2].body, reset_body, "query edit {path}");
            for request in requests.iter() {
                assert_eq!(request.url, "http://localhost:8080/inventory");
                assert_eq!(request.method, "POST");
                assert_eq!(request.headers["host"], "inventory.local");
            }
        }
    }
}

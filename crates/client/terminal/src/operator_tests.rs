use super::*;
use wamn_client_tui::screen::{RecordLink, RevisionBinding, SuppliedField, SuppliedKind};
use wamn_client_tui::submission::{Replay, ResponseContract};

const fn field(path: &'static str, kind: &'static str) -> FieldSchema {
    FieldSchema {
        field: FieldDescriptor {
            path,
            type_name: kind,
            nullable: false,
            values: &[],
        },
        required: true,
        children: &[],
        minimum: None,
        maximum: None,
    }
}

const INPUT: &[FieldSchema] = &[
    field("request_id", "string"),
    FieldSchema {
        required: false,
        field: FieldDescriptor {
            nullable: true,
            ..field("note", "text").field
        },
        ..field("note", "text")
    },
    FieldSchema {
        required: false,
        field: FieldDescriptor {
            values: &["open", "closed"],
            ..field("status", "text").field
        },
        ..field("status", "text")
    },
];
const RESULT: &[FieldSchema] = &[field("id", "text")];
const SUPPLIED: &[SuppliedField] = &[SuppliedField {
    path: "request_id",
    kind: SuppliedKind::RequestId,
}];

fn route() -> RouteMetadata {
    RouteMetadata {
        method: "POST".into(),
        template: "/record/{location}".into(),
    }
}

const SPEC: ScreenSpec = ScreenSpec {
    model: "stock",
    name: "apply",
    operation: "example:stock/apply@1.0.0",
    kind: "command",
    input: INPUT,
    input_schema: None,
    response: ResponseContract {
        schema: None,
        fields: RESULT,
        result_class: Some("one"),
        errors: &[],
        kind: "command",
        transaction: Some("explicit_per_input"),
        direct: true,
        replay: Replay::Claim,
    },
    route: Some(route),
    record: None,
    revision: None,
    revision_inputs: &[],
    requires_composition: false,
    supplied: SUPPLIED,
};

fn binding() -> SessionBinding {
    SessionBinding {
        url: "https://example.invalid".into(),
        host: None,
        target_instance: "target-1".into(),
    }
}
fn app() -> GeneratedApplication {
    let mut app = GeneratedApplication::new("test", vec![Screen::new(&SPEC, binding())]);
    app.active = Some(0);
    app
}
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn send(retry: bool) -> Action {
    Action::Send {
        screen: 0,
        retry,
        delete_confirmed: false,
    }
}

#[test]
fn driver_edits_keep_optional_presence_null_and_closed_choice_separate() {
    let mut app = app();
    app.views[0].input = 2; // One route parameter, request_id, then optional note.
    app.key(key(KeyCode::Char('n')));
    assert_eq!(
        app.screens[0].draft().state("/note").unwrap(),
        FieldState::Null
    );
    app.key(key(KeyCode::Char('a')));
    assert_eq!(
        app.screens[0].draft().state("/note").unwrap(),
        FieldState::Absent
    );
    app.key(key(KeyCode::Enter));
    for character in "counted".chars() {
        app.key(key(KeyCode::Char(character)));
    }
    app.key(key(KeyCode::Enter));
    assert_eq!(
        app.screens[0].draft().state("/note").unwrap(),
        FieldState::Value(json!("counted"))
    );
    app.views[0].input = 3;
    app.key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Choice { .. }));
    app.key(key(KeyCode::Down));
    app.key(key(KeyCode::Enter));
    assert_eq!(
        app.screens[0].draft().state("/status").unwrap(),
        FieldState::Value(json!("closed"))
    );
    app.views[0].input = 1;
    app.key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Browse));
    assert!(app.message.contains("supplied"));
}

#[test]
fn nested_repeated_rows_expose_each_optional_field_and_removal_target() {
    const NESTED: &[FieldSchema] = &[FieldSchema {
        children: &[FieldSchema {
            minimum: Some(1),
            maximum: Some(2),
            children: &[
                field("value.line[].code", "text"),
                FieldSchema {
                    required: false,
                    ..field("value.line[].note", "text")
                },
            ],
            ..field("value.line[]", "array")
        }],
        ..field("value", "object")
    }];
    let rows = editor_rows(
        NESTED,
        &json!({"value": {"line": [{"code": "A"}, {"code": "B"}]}}),
    );
    assert!(
        rows.iter()
            .any(|row| row.pointer == "/value/line/0/note" && !row.schema.required)
    );
    assert!(
        rows.iter()
            .any(|row| row.pointer == "/value/line/1" && row.object_row)
    );
    let field = rows
        .iter()
        .find(|row| row.pointer == "/value/line/1/code")
        .unwrap();
    assert_eq!(field.parent_row, Some(("/value/line".into(), 1)));
}

#[test]
fn unsupported_input_never_enters_a_text_editor() {
    const UNKNOWN: ScreenSpec = ScreenSpec {
        input: &[field("opaque", "opaque")],
        supplied: &[],
        ..SPEC
    };
    let mut app = GeneratedApplication::new("test", vec![Screen::new(&UNKNOWN, binding())]);
    app.active = Some(0);
    app.views[0].input = 1;
    app.key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Browse));
    assert!(app.message.contains("Unsupported"));
    assert!(
        app.screens[0]
            .draft()
            .item()
            .as_object()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn escape_requires_dirty_confirmation_and_q_refuses_pending() {
    let mut app = app();
    app.screens[0]
        .edit("/note", FieldState::Value(json!("draft")))
        .unwrap();
    app.key(key(KeyCode::Esc));
    assert!(matches!(app.mode, Mode::Confirm(Confirmation::Leave)));
    app.key(key(KeyCode::Char('n')));
    assert_eq!(app.active, Some(0));
    app.views[0]
        .parameters
        .insert("location".into(), "dock-1".into());
    let _ = prepare_application(&mut app, send(false)).unwrap();
    assert!(matches!(app.key(key(KeyCode::Char('q'))), Action::None));
    assert!(app.message.contains("pending"));
    assert!(matches!(
        app.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Action::Exit
    ));
}

#[test]
fn captured_retry_keeps_route_parameters_and_request_bytes_unchanged() {
    let mut app = app();
    app.views[0]
        .parameters
        .insert("location".into(), "dock-1".into());
    app.screens[0]
        .edit("/note", FieldState::Value(json!("original")))
        .unwrap();
    let first = prepare_application(&mut app, send(false)).unwrap();
    app.screens[0].resolve(
        first.attempt,
        Err(ClientError::Transport {
            detail: "response lost".into(),
        }),
    );
    app.views[0]
        .parameters
        .insert("location".into(), "different".into());
    let retry = prepare_application(&mut app, send(true)).unwrap();
    assert_eq!(retry.body.body(), first.body.body());
    assert_eq!(retry.parameters, first.parameters);
    assert_eq!(retry.parameters["location"], "dock-1");
}

#[test]
fn route_parameters_are_required_before_any_request_becomes_pending() {
    let mut app = app();
    assert!(prepare_application(&mut app, send(false)).is_err());
    assert_eq!(app.screens[0].submission().state(), &State::Editable);
    assert!(app.screens[0].submission().captured().is_none());
}

#[test]
fn linked_navigation_requires_the_declared_relation_or_exact_read_operation() {
    const LIST: ScreenSpec = ScreenSpec {
        name: "query",
        kind: "query",
        operation: "example:stock/query@1.0.0",
        record: Some(RecordLink {
            relation: "inventory.stock",
            key_field: "id",
            key_input: None,
        }),
        ..SPEC
    };
    const READ: ScreenSpec = ScreenSpec {
        name: "get",
        kind: "get",
        operation: "example:stock/get@1.0.0",
        record: Some(RecordLink {
            relation: "inventory.stock",
            key_field: "id",
            key_input: Some("id"),
        }),
        ..SPEC
    };
    const OTHER: ScreenSpec = ScreenSpec {
        name: "other",
        kind: "get",
        record: Some(RecordLink {
            relation: "inventory.other",
            key_field: "id",
            key_input: Some("id"),
        }),
        ..SPEC
    };
    const MUTATE: ScreenSpec = ScreenSpec {
        revision: Some(RevisionBinding {
            read_operation: "example:stock/get@1.0.0",
            read_key_input: "id",
            key_field: "id",
            revision_field: "version",
            command_key_input: "id",
            command_revision_input: "expected",
        }),
        ..SPEC
    };
    let screens = [&LIST, &READ, &OTHER, &MUTATE]
        .into_iter()
        .map(|spec| Screen::new(spec, binding()))
        .collect::<Vec<_>>();
    assert_eq!(link_targets(&screens, 0), [1]);
    assert_eq!(link_targets(&screens, 1), [3]);
}

#[test]
fn status_preserves_conflict_versions_and_permission_details() {
    let conflict = error_text(
        &json!({"code": "concurrency_conflict", "detail": {"expected_row_version": "4", "observed_row_version": "7"}}),
    );
    for expected in [
        "concurrency_conflict",
        "expected_row_version",
        "observed_row_version",
        "4",
        "7",
    ] {
        assert!(conflict.contains(expected));
    }
    let permission =
        error_text(&json!({"code": "permission_denied", "detail": {"operation": "stock.update"}}));
    assert!(permission.contains("permission_denied"));
    assert!(permission.contains("stock.update"));
}

#[test]
fn cursor_display_stays_opaque_and_query_columns_come_from_its_schema() {
    const PAGE: ScreenSpec = ScreenSpec {
        kind: "query",
        input: &[
            field("request_id", "string"),
            FieldSchema {
                required: false,
                ..field("cursor", "text")
            },
        ],
        response: ResponseContract {
            result_class: Some("page"),
            kind: "query",
            ..SPEC.response
        },
        ..SPEC
    };
    let mut app = GeneratedApplication::new("test", vec![Screen::new(&PAGE, binding())]);
    app.active = Some(0);
    app.screens[0]
        .bind("/cursor", json!("private-server-cursor"))
        .unwrap();
    let area = Rect::new(0, 0, 120, 35);
    let mut buffer = Buffer::empty(area);
    AppWidget(&app).render(area, &mut buffer);
    let text = (0..area.height)
        .map(|row| wamn_client_tui::row_text(&buffer, row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!text.contains("private-server-cursor"));
    assert!(text.contains("managed by paging"));
    assert!(text.contains("id"));
    assert_eq!(
        result_fields(PAGE.response.fields)
            .iter()
            .map(|field| field.path)
            .collect::<Vec<_>>(),
        ["id"]
    );
}

#[test]
fn a_required_nullable_array_can_return_from_null_to_repeated_values() {
    const ARRAY: ScreenSpec = ScreenSpec {
        input: &[
            field("request_id", "string"),
            FieldSchema {
                field: FieldDescriptor {
                    nullable: true,
                    ..field("lines[]", "array").field
                },
                children: &[field("lines[].code", "text")],
                ..field("lines[]", "array")
            },
        ],
        ..SPEC
    };
    let mut app = GeneratedApplication::new("test", vec![Screen::new(&ARRAY, binding())]);
    app.active = Some(0);
    app.views[0].input = 2;
    app.key(key(KeyCode::Char('n')));
    assert_eq!(
        app.screens[0].draft().state("/lines").unwrap(),
        FieldState::Null
    );
    app.key(key(KeyCode::Enter));
    assert_eq!(
        app.screens[0].draft().state("/lines").unwrap(),
        FieldState::Value(json!([{}]))
    );
}

#[test]
fn nested_arrays_use_indexed_typed_editors_and_enforce_inner_bounds() {
    const MATRIX: ScreenSpec = ScreenSpec {
        input: &[
            field("request_id", "string"),
            FieldSchema {
                children: &[FieldSchema {
                    children: &[field("matrix[][]", "int64")],
                    maximum: Some(1),
                    ..field("matrix[][]", "array")
                }],
                ..field("matrix[]", "array")
            },
        ],
        ..SPEC
    };
    let mut app = GeneratedApplication::new("test", vec![Screen::new(&MATRIX, binding())]);
    app.open_screen(0);
    app.views[0]
        .parameters
        .insert("location".into(), "dock-1".into());
    app.views[0].input = 2;
    app.key(key(KeyCode::Enter));
    assert_eq!(app.screens[0].draft().item()["matrix"], json!([[]]));
    app.views[0].input = 3;
    app.key(key(KeyCode::Enter));
    assert_eq!(app.screens[0].draft().item()["matrix"], json!([[""]]));
    app.key(key(KeyCode::Enter));
    assert!(app.message.contains("bounds"));
    app.views[0].input = 4;
    app.key(key(KeyCode::Enter));
    assert!(
        matches!(&app.mode, Mode::Text { target: Target::Field(path), .. } if path == "/matrix/0/0")
    );
    app.key(key(KeyCode::Char('x')));
    app.key(key(KeyCode::Enter));
    assert!(prepare_application(&mut app, send(false)).is_err());
    assert_eq!(app.screens[0].submission().state(), &State::Editable);
    app.key(key(KeyCode::Enter));
    app.key(key(KeyCode::Backspace));
    for character in "+0007".chars() {
        app.key(key(KeyCode::Char(character)));
    }
    app.key(key(KeyCode::Enter));
    let prepared = prepare_application(&mut app, send(false)).unwrap();
    assert_eq!(prepared.body.item()["matrix"], json!([["7"]]));
    let rows = editor_rows(MATRIX.input, app.screens[0].draft().item());
    assert!(
        rows.iter()
            .all(|row| !row.pointer.contains("/matrix/0/matrix"))
    );
    assert_eq!(
        rows.last().unwrap().parent_row,
        Some(("/matrix/0".into(), 0))
    );
}

#[test]
fn an_unknown_nested_array_leaf_never_becomes_text_input() {
    const UNKNOWN: ScreenSpec = ScreenSpec {
        input: &[FieldSchema {
            children: &[FieldSchema {
                children: &[field("matrix[][]", "opaque")],
                ..field("matrix[][]", "array")
            }],
            ..field("matrix[]", "array")
        }],
        supplied: &[],
        ..SPEC
    };
    let mut app = GeneratedApplication::new("test", vec![Screen::new(&UNKNOWN, binding())]);
    app.open_screen(0);
    app.views[0].input = 1;
    app.key(key(KeyCode::Enter));
    app.views[0].input = 2;
    app.key(key(KeyCode::Enter));
    app.views[0].input = 3;
    app.key(key(KeyCode::Enter));
    assert!(matches!(app.mode, Mode::Browse));
    assert!(app.message.contains("Unsupported"));
    app.views[0]
        .parameters
        .insert("location".into(), "dock-1".into());
    assert!(prepare_application(&mut app, send(false)).is_err());
    assert_eq!(app.screens[0].submission().state(), &State::Editable);
}

#[test]
fn escape_warns_before_abandoning_an_uncertain_captured_intent() {
    let mut app = app();
    app.views[0]
        .parameters
        .insert("location".into(), "dock-1".into());
    app.screens[0]
        .edit("/note", FieldState::Value(json!("draft")))
        .unwrap();
    let prepared = prepare_application(&mut app, send(false)).unwrap();
    app.complete(
        0,
        prepared.attempt,
        Err(ClientError::Transport {
            detail: "response lost".into(),
        }),
    );
    app.key(key(KeyCode::Esc));
    assert!(matches!(
        app.mode,
        Mode::Confirm(Confirmation::LeaveUncertain)
    ));
    let area = Rect::new(0, 0, 140, 35);
    let mut buffer = Buffer::empty(area);
    AppWidget(&app).render(area, &mut buffer);
    let text = (0..area.height)
        .map(|row| wamn_client_tui::row_text(&buffer, row))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.contains("original request may still complete"));
    app.key(key(KeyCode::Char('n')));
    assert!(matches!(
        app.screens[0].submission().state(),
        State::Uncertain { .. }
    ));
    assert_eq!(
        app.screens[0].submission().captured().unwrap().body(),
        prepared.body.body()
    );
    app.key(key(KeyCode::Esc));
    app.key(key(KeyCode::Char('y')));
    assert!(app.active.is_none());
    assert!(app.screens[0].submission().captured().is_none());
}

#[test]
fn navigation_selects_a_model_before_its_own_operations() {
    const OTHER: ScreenSpec = ScreenSpec {
        model: "other",
        ..SPEC
    };
    const SECOND: ScreenSpec = ScreenSpec {
        name: "second",
        operation: "stock.second",
        ..SPEC
    };
    let mut app = GeneratedApplication::new(
        "test",
        vec![
            Screen::new(&SPEC, binding()),
            Screen::new(&OTHER, binding()),
            Screen::new(&SECOND, binding()),
        ],
    );
    assert_eq!(app.models, ["other", "stock"]);
    assert!(app.model.is_none());
    app.key(key(KeyCode::Down));
    app.key(key(KeyCode::Enter));
    assert_eq!(app.model, Some(1));
    assert!(app.active.is_none());
    assert_eq!(app.operations(), [0, 2]);
    app.key(key(KeyCode::Down));
    app.key(key(KeyCode::Enter));
    assert_eq!(app.active, Some(2));
    app.key(key(KeyCode::Esc));
    assert!(app.active.is_none());
    assert_eq!(app.model, Some(1));
    app.key(key(KeyCode::Esc));
    assert!(app.model.is_none());
    assert_eq!(app.selected, 1);
}

#[test]
fn f5_refreshes_command_a_even_when_the_shared_read_now_holds_b() {
    const READ: ScreenSpec = ScreenSpec {
        name: "get",
        operation: "stock.get",
        kind: "get",
        input: &[
            field("request_id", "string"),
            field("id", "text"),
            field("locale", "text"),
        ],
        response: ResponseContract {
            fields: &[field("id", "text"), field("version", "int64")],
            kind: "get",
            ..SPEC.response
        },
        ..SPEC
    };
    const COMMAND: ScreenSpec = ScreenSpec {
        input: &[
            field("request_id", "string"),
            field("id", "text"),
            field("expected", "int64"),
        ],
        revision: Some(RevisionBinding {
            read_operation: "stock.get",
            read_key_input: "id",
            key_field: "id",
            revision_field: "version",
            command_key_input: "id",
            command_revision_input: "expected",
        }),
        revision_inputs: &["expected"],
        ..SPEC
    };
    let mut read = Screen::new(&READ, binding());
    read.edit("/id", FieldState::Value(json!("A"))).unwrap();
    read.edit("/locale", FieldState::Value(json!("en")))
        .unwrap();
    let attempt = read
        .begin(
            &IntentValues {
                request_id: "request-a".into(),
                idempotency_key: "key".into(),
                occurred_at: "time".into(),
            },
            false,
        )
        .unwrap();
    assert!(
        read.resolve(
            attempt,
            Ok(HttpResponse {
                status: 200,
                body: json!([{"request_id": "request-a", "value": {"id": "A", "version": "7"}}])
                    .to_string()
            })
        )
    );
    let mut command = Screen::new(&COMMAND, binding());
    command.bind_from_read(&read).unwrap();
    read.edit("/id", FieldState::Value(json!("B"))).unwrap();
    let mut app = GeneratedApplication::new("test", vec![command, read]);
    app.open_screen(0);
    app.views[1]
        .parameters
        .insert("location".into(), "dock-1".into());
    let action = app.key(key(KeyCode::F(5)));
    assert_eq!(app.active, Some(1));
    let prepared = prepare_application(&mut app, action).unwrap();
    assert_eq!(prepared.body.item()["id"], "A");
    assert_eq!(prepared.body.item()["locale"], "en");
}

#[derive(Debug)]
struct Wrapper {
    generated: GeneratedApplication,
    intents: Vec<bool>,
    resolved: usize,
    keys: usize,
}
impl Application for Wrapper {
    fn render(&self, area: Rect, buffer: &mut Buffer) {
        self.generated.render(area, buffer);
    }
    fn key(&mut self, key: KeyEvent) -> Action {
        self.keys += 1;
        // A composition's exit request still passes the loop's pending guard.
        if key.code == KeyCode::Char('q') {
            Action::Exit
        } else {
            self.generated.key(key)
        }
    }
    fn prepare(
        &mut self,
        action: Action,
        intent: Option<&IntentValues>,
    ) -> Result<PreparedRequest, String> {
        self.intents.push(intent.is_some());
        let mut prepared = self.generated.prepare(action, intent)?;
        // The loop carries the composition's owner id without interpreting it.
        prepared.screen += 100;
        Ok(prepared)
    }
    fn resolve(
        &mut self,
        screen: usize,
        attempt: Attempt,
        response: Result<HttpResponse, ClientError>,
    ) {
        self.resolved += 1;
        self.generated.resolve(screen - 100, attempt, response);
    }
    fn set_message(&mut self, message: String) {
        self.generated.set_message(message);
    }
    fn pending(&self) -> bool {
        self.generated.pending()
    }
    fn unresolved(&self) -> bool {
        self.generated.unresolved()
    }
}
fn driver_ready() -> GeneratedApplication {
    let mut generated = app();
    generated.views[0]
        .parameters
        .insert("location".into(), "dock-1".into());
    generated
}
fn shared_round_trip(application: &mut impl Application) {
    let prepared = prepare_application(application, send(false)).unwrap();
    assert!(application.pending());
    assert!(application.unresolved());
    assert!(
        prepared.body.item()["request_id"]
            .as_str()
            .unwrap()
            .parse::<uuid::Uuid>()
            .is_ok()
    );
    assert_eq!(prepared.parameters["location"], "dock-1");
    assert_eq!(prepared.route.template, "/record/{location}");
    assert!(matches!(
        application_key(application, key(KeyCode::Char('q')), false),
        Action::None
    ));
    application.resolve(
        prepared.screen,
        prepared.attempt,
        Ok(HttpResponse {
            status: 200,
            body: json!([{"request_id": prepared.body.item()["request_id"], "value": {"id": "A"}}])
                .to_string(),
        }),
    );
    assert!(!application.pending());
    assert!(!application.unresolved());
}

#[test]
fn default_and_composed_adapters_share_preparation_resolution_and_pending_exit_protection() {
    let mut generated = driver_ready();
    shared_round_trip(&mut generated);
    let mut wrapped = Wrapper {
        generated: driver_ready(),
        intents: Vec::new(),
        resolved: 0,
        keys: 0,
    };
    shared_round_trip(&mut wrapped);
    assert_eq!(wrapped.intents, [true]);
    assert_eq!(wrapped.resolved, 1);
    assert_eq!(wrapped.keys, 1);
    let area = Rect::new(0, 0, 100, 30);
    let mut actual = Buffer::empty(area);
    let mut expected = Buffer::empty(area);
    ApplicationWidget(&wrapped).render(area, &mut actual);
    wrapped.generated.render(area, &mut expected);
    assert_eq!(actual, expected);
}

#[test]
fn a_wrapped_retry_gets_no_new_intent_and_ctrl_c_stays_owned_by_the_loop() {
    let mut wrapped = Wrapper {
        generated: driver_ready(),
        intents: Vec::new(),
        resolved: 0,
        keys: 0,
    };
    let first = prepare_application(&mut wrapped, send(false)).unwrap();
    let forced = application_key(
        &mut wrapped,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        true,
    );
    assert!(matches!(forced, Action::Exit));
    assert_eq!(
        wrapped.keys, 0,
        "Ctrl-C must not reach application key handling"
    );
    assert!(wrapped.unresolved());
    wrapped.resolve(
        first.screen,
        first.attempt,
        Err(ClientError::Transport {
            detail: "response lost".into(),
        }),
    );
    let retry = prepare_application(&mut wrapped, send(true)).unwrap();
    assert_eq!(wrapped.intents, [true, false]);
    assert_eq!(retry.body, first.body);
    assert_eq!(retry.parameters, first.parameters);
    assert_eq!(retry.route.template, first.route.template);
    assert_eq!(retry.screen, first.screen);
    assert!(wrapped.pending());
    assert!(matches!(
        application_key(&mut wrapped, key(KeyCode::Char('q')), false),
        Action::None
    ));
    assert!(wrapped.generated.message.contains("pending"));
}

#[test]
fn the_loop_also_refuses_exit_for_its_own_in_flight_transport() {
    let mut wrapped = Wrapper {
        generated: driver_ready(),
        intents: Vec::new(),
        resolved: 0,
        keys: 0,
    };
    assert!(!wrapped.pending());
    assert!(matches!(
        application_key(&mut wrapped, key(KeyCode::Char('q')), true),
        Action::None
    ));
    assert!(wrapped.generated.message.contains("pending"));
}

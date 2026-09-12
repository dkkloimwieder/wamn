//! Retained pilot grading cases over private recorded inputs.

use std::fs;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wamn_test_infrastructure::scratch::ScratchRoot;

use super::{
    GradeArgs, GradeContext, contracts, expand_run, grade_inner, grading::Grading, read_json,
    read_lines, write_json,
};

const NESTED_CREATE: &str = r#"{"fields":[{"path":"request_id","type":"text","nullable":false},
  {"path":"value.idempotency_key","type":"text","nullable":false},
  {"path":"value.name","type":"text","nullable":false}]}"#;

const NESTED_CREATE_RESULT: &str =
    r#"{"class":"one","fields":[{"path":"carrier_id","type":"uuid","nullable":false}]}"#;

const FLAT_CREATE: &str = r#"{"fields":[{"path":"request_id","type":"text","nullable":false},
  {"path":"idempotency_key","type":"text","nullable":false},
  {"path":"name","type":"text","nullable":false}]}"#;

const CRUD_CREATE: &str = r#"{"request_id":{"type":"string","required":true},
  "idempotency_key":{"type":"text","required":true},
  "canonical_command":{"over":"writable_fields","payload":"canonical_compact_json","changed":"idempotency_conflict"},
  "server_owned_fields":{"fields":["id","row_version"],"if_supplied":"invalid_input"},
  "writable_fields":[{"field":"name","type":"text","omitted":"postgres_default","explicit_null":"invalid_input"}]}"#;

const FLAT_QUERY: &str = r#"{"fields":[{"path":"request_id","type":"text","nullable":false},
  {"path":"dock_id","type":"uuid","nullable":false},
  {"path":"day","type":"text","nullable":false}]}"#;

const NESTED_QUERY_RESULT: &str = r#"{"class":"one","fields":[{"path":"dock_id","type":"uuid","nullable":false},
  {"path":"appointments[].appointment_id","type":"uuid","nullable":false},
  {"path":"appointments[].slot_start","type":"timestamptz","nullable":false}]}"#;

const CANONICAL_CREATE: &str = r#"{"canonicalization":{"payload":"canonical_compact_json",
  "timestamptz":"utc_rfc3339_six_fractional_digits","uuid":"lowercase_hyphenated"},
  "fields":[{"path":"request_id","type":"text","nullable":false},
  {"path":"value.idempotency_key","type":"text","nullable":false},
  {"path":"value.name","type":"text","nullable":false}]}"#;

const ONE_ROW: &str = r#"| `carrier.create` | `name` text | `carrier_id` uuid |"#;

fn directory() -> ScratchRoot {
    let path = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    ScratchRoot(path)
}

fn package(root: &Path) -> PathBuf {
    root.join("worktree/packages/dock")
}

fn placement_input(root: &Path) {
    fs::create_dir_all(root.join("grade")).unwrap();
    fs::create_dir_all(package(root).join("generated/contracts")).unwrap();
    write_json(&root.join("task.json"), &json!({"task":"placement","identity":{"route_host":"test.localhost"},"overlay_root":"packages/dock","grade":{"steps":"steps.json","checks":[],"fence_reports":[]}})).unwrap();
    write_json(
        &root.join("run.json"),
        &json!({"commit":"0000000000000000000000000000000000000000"}),
    )
    .unwrap();
    write_json(&package(root).join("wamn.json"), &json!({"name":"dock"})).unwrap();
}

fn contract(root: &Path, domain: &str, action: &str, input: &str, result: &str) {
    let directory = package(root).join("generated/contracts").join(domain);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(format!("{action}.input.json")), input).unwrap();
    fs::write(directory.join(format!("{action}.result.json")), result).unwrap();
}

fn create_step(root: &Path) {
    write_json(&root.join("steps.json"), &json!([{"id":"create-carrier","must":true,"invariant":"DOCK-0","route":{"operation":"carrier.create"},"body":{"name":"Northbound Freight"},"expect":{"status":200,"item":"value","present":["carrier_id"]}}])).unwrap();
}

fn recorded_input(root: &Path) {
    placement_input(root);
    write_json(&root.join("task.json"), &json!({"task":"test","identity":{"route_host":"test.localhost"},"overlay_root":"packages/dock","grade":{"steps":"steps.json","checks":["claim-replay"],"fence_reports":[]}})).unwrap();
    write_json(&root.join("run.json"), &json!({"commit":"0000000000000000000000000000000000000000","git":{"outside_allowed_paths":[]}})).unwrap();
    write_json(&root.join("steps.json"), &json!([
        {"id":"create","must":true,"route":{"operation":"dock.create"},"expect":{"status":200,"item":"value","present":["dock_id"]}},
        {"id":"book","must":true,"route":{"operation":"appointment.book"},"reuse":{"dock_id":"create.value.dock_id"},"expect":{"status":200,"item":"value","present":["appointment_id"]}},
        {"id":"book-replay","must":true,"proves":"claim-replay","route":{"operation":"appointment.book"},"expect":{"status":200,"item":"value","equals":{"appointment_id":"book.value.appointment_id"}}},
        {"id":"refused","must":true,"route":{"operation":"appointment.book"},"expect":{"status":200,"item":"error","error_code":"slot_unavailable"}},
        {"id":"exactly-one","must":true,"concurrent":["book","refused"],"expect":{"exactly_one":{"error_code":"slot_unavailable"}}},
        {"id":"count-rows","must":true,"sql":"select 1","expect":{"rows":"2"}}
    ])).unwrap();
    fs::write(root.join("grade/dev.out"), "run completed: Migrate,Introspect,Generate,Build,Virtualize,Apply,Acl,Admit,Gate,Publish,Release,Activate\nrun served: http://127.0.0.1:18080 host=test.localhost\nrun holding\n").unwrap();
    let mut records = Vec::new();
    for (id, path, item) in [
        (
            "create",
            "/dock/create",
            json!({"value":{"dock_id":"11111111-1111-1111-1111-111111111111"}}),
        ),
        (
            "book",
            "/appointment/book",
            json!({"value":{"appointment_id":"22222222-2222-2222-2222-222222222222"}}),
        ),
        (
            "book-replay",
            "/appointment/book",
            json!({"value":{"appointment_id":"22222222-2222-2222-2222-222222222222"}}),
        ),
        (
            "refused",
            "/appointment/book",
            json!({"error":{"code":"slot_unavailable"}}),
        ),
    ] {
        let mut response = item;
        response["request_id"] = json!("x");
        records.push(json!({"id":id,"path":path,"request":{},"status":"200","response":serde_json::to_string(&json!([response])).unwrap()}));
    }
    records.push(json!({"id":"count-rows","sql":"select 1","rows":"2"}));
    save_records(root, &records);
    fs::write(
        root.join("grade/exactly-one-book.out"),
        "[{\"value\":{\"appointment_id\":\"2222\"}}]\n",
    )
    .unwrap();
    fs::write(
        root.join("grade/exactly-one-refused.out"),
        "[{\"error\":{\"code\":\"slot_unavailable\"}}]\n",
    )
    .unwrap();
    fs::write(root.join("grade/results.json"), "[]\n").unwrap();
    fs::write(
        root.join("checklist.json"),
        "{\"teardown\":{\"verification_database_removed\":true},\"outcome\":\"PASS\"}\n",
    )
    .unwrap();
}

fn save_records(root: &Path, records: &[Value]) {
    let lines = records
        .iter()
        .map(|row| serde_json::to_string(row).unwrap() + "\n")
        .collect::<String>();
    fs::write(root.join("grade/http.jsonl"), lines).unwrap();
}

async fn replay(root: &Path) -> (u8, Value) {
    let code = grade_inner(GradeArgs {
        run: None,
        replay: Some(root.to_owned()),
        placement: None,
        contract: None,
    })
    .await
    .unwrap();
    (
        code,
        read_json(&root.join("checklist-replay.json")).unwrap(),
    )
}

fn step<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["id"] == id)
        .unwrap()
}

fn placed(root: &Path) -> Vec<Value> {
    let mut context = GradeContext::load(&GradeArgs {
        run: None,
        replay: None,
        placement: Some(root.to_owned()),
        contract: None,
    })
    .unwrap();
    let records = read_lines(&root.join("grade/http.jsonl")).unwrap_or_default();
    context.grading.placement(&records).unwrap()
}

fn row<'a>(rows: &'a [Value], id: &str) -> &'a Value {
    rows.iter().find(|row| row["id"] == id).unwrap()
}

#[tokio::test]
async fn recorded_run_grades_without_live_services() {
    let root = directory();
    recorded_input(root.path());
    let (code, report) = replay(root.path()).await;
    assert_eq!(code, 0);
    assert_eq!(report["outcome"], "PASS");
    assert_eq!(
        (
            report["loop"]["pass"].clone(),
            report["loop"]["stages"].clone()
        ),
        (json!(true), json!(12))
    );
    assert_eq!(report["checks"]["claim-replay"], "pass");
}

#[tokio::test]
async fn replay_preserves_all_three_original_records() {
    let root = directory();
    recorded_input(root.path());
    let files = ["grade/http.jsonl", "grade/results.json", "checklist.json"];
    let before = files.map(|file| fs::read(root.path().join(file)).unwrap());
    replay(root.path()).await;
    for (file, before) in files.into_iter().zip(before) {
        assert_eq!(fs::read(root.path().join(file)).unwrap(), before, "{file}");
    }
    assert!(
        fs::metadata(root.path().join("checklist-replay.json"))
            .unwrap()
            .len()
            > 0
    );
}

#[tokio::test]
async fn changed_recorded_response_changes_the_step_and_named_check() {
    let root = directory();
    recorded_input(root.path());
    let mut records = read_lines(&root.path().join("grade/http.jsonl")).unwrap();
    records
        .iter_mut()
        .find(|row| row["id"] == "book-replay")
        .unwrap()["response"] = json!(
        r#"[{"request_id":"x","value":{"appointment_id":"33333333-3333-3333-3333-333333333333"}}]"#
    );
    save_records(root.path(), &records);
    let (_, report) = replay(root.path()).await;
    assert_eq!(step(&report, "book-replay")["pass"], false);
    assert_eq!(report["checks"]["claim-replay"], "fail");
}

#[tokio::test]
async fn missing_recorded_response_reports_the_absent_step() {
    let root = directory();
    recorded_input(root.path());
    let records = read_lines(&root.path().join("grade/http.jsonl"))
        .unwrap()
        .into_iter()
        .filter(|row| row["id"] != "book-replay")
        .collect::<Vec<_>>();
    save_records(root.path(), &records);
    let (_, report) = replay(root.path()).await;
    assert_eq!(step(&report, "book-replay")["pass"], false);
    assert!(
        step(&report, "book-replay")["evidence"]
            .as_str()
            .unwrap()
            .starts_with("not replayable")
    );
}

#[tokio::test]
async fn recorded_concurrent_arms_require_one_refusal() {
    let root = directory();
    recorded_input(root.path());
    fs::write(
        root.path().join("grade/exactly-one-refused.out"),
        r#"[{"value":{"appointment_id":"4444"}}]"#,
    )
    .unwrap();
    let (_, report) = replay(root.path()).await;
    assert_eq!(step(&report, "exactly-one")["pass"], false);
}

#[tokio::test]
async fn recorded_sql_count_decides_its_verdict() {
    let root = directory();
    recorded_input(root.path());
    let mut records = read_lines(&root.path().join("grade/http.jsonl")).unwrap();
    records
        .iter_mut()
        .find(|row| row["id"] == "count-rows")
        .unwrap()["rows"] = json!("5");
    save_records(root.path(), &records);
    let (_, report) = replay(root.path()).await;
    assert_eq!(step(&report, "count-rows")["pass"], false);
}

#[tokio::test]
async fn a_run_without_a_served_release_does_not_grade_saved_responses() {
    let root = directory();
    recorded_input(root.path());
    let log = root.path().join("grade/dev.out");
    let output = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter(|line| !line.starts_with("run served:"))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(log, output).unwrap();
    let (code, report) = replay(root.path()).await;
    assert_eq!(code, 30);
    assert_eq!(
        step(&report, "create")["evidence"],
        "not run: the loop served no release"
    );
}

#[tokio::test]
async fn eleven_completed_stages_still_fail_with_a_served_release() {
    let root = directory();
    recorded_input(root.path());
    let log = root.path().join("grade/dev.out");
    fs::write(
        &log,
        fs::read_to_string(&log).unwrap().replace(",Activate", ""),
    )
    .unwrap();
    let (code, report) = replay(root.path()).await;
    assert_eq!(
        (
            report["loop"]["pass"].clone(),
            report["loop"]["stages"].clone()
        ),
        (json!(false), json!(11))
    );
    assert!(code == 30 && report["outcome"].as_str().unwrap().starts_with("FAIL"));
}

#[test]
fn nested_contract_places_each_field_at_its_published_path() {
    let root = directory();
    placement_input(root.path());
    create_step(root.path());
    contract(
        root.path(),
        "carrier",
        "create",
        NESTED_CREATE,
        NESTED_CREATE_RESULT,
    );
    let rows = placed(root.path());
    let body = &row(&rows, "create-carrier")["body"];
    assert_eq!(body["value"]["name"], "Northbound Freight");
    assert_eq!(body["request_id"], "create-carrier");
    assert!(!body["value"]["idempotency_key"].is_null());
    assert_eq!(
        body.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["request_id", "value"]
    );
}

#[test]
fn flat_contract_keeps_the_same_fields_at_the_top_level() {
    let root = directory();
    placement_input(root.path());
    create_step(root.path());
    contract(
        root.path(),
        "carrier",
        "create",
        FLAT_CREATE,
        NESTED_CREATE_RESULT,
    );
    let rows = placed(root.path());
    let body = &row(&rows, "create-carrier")["body"];
    assert_eq!(body["name"], "Northbound Freight");
    assert!(!body["idempotency_key"].is_null());
}

#[test]
fn writable_fields_supply_only_declared_client_fields() {
    let root = directory();
    placement_input(root.path());
    create_step(root.path());
    contract(
        root.path(),
        "carrier",
        "create",
        CRUD_CREATE,
        NESTED_CREATE_RESULT,
    );
    let rows = placed(root.path());
    let body = &row(&rows, "create-carrier")["body"];
    assert_eq!(body["name"], "Northbound Freight");
    assert!(!body["idempotency_key"].is_null());
    assert_eq!(
        body.as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["idempotency_key", "name", "request_id"]
    );
}

#[test]
fn projection_without_a_declared_key_receives_none() {
    let root = directory();
    placement_input(root.path());
    write_json(&root.path().join("steps.json"),&json!([{"id":"list","must":true,"invariant":"DOCK-6","route":{"operation":"appointment.query"},"body":{"day":"2026-10-01"},"reuse":{"dock_id":"create-dock.value.dock_id"},"expect":{"status":200,"item":"value","sorted_by":{"path":"appointments","field":"slot_start"}}}])).unwrap();
    contract(
        root.path(),
        "appointment",
        "query",
        FLAT_QUERY,
        NESTED_QUERY_RESULT,
    );
    let rows = placed(root.path());
    assert!(row(&rows, "list")["body"].get("idempotency_key").is_none());
}

#[test]
fn an_explicit_replay_key_keeps_its_exact_value() {
    let root = directory();
    placement_input(root.path());
    write_json(&root.path().join("steps.json"),&json!([{"id":"book-replay","must":true,"invariant":"DOCK-2","route":{"operation":"carrier.create"},"body":{"name":"Northbound Freight","idempotency_key":"dock-gate-book-1"},"expect":{"status":200,"item":"value","present":["carrier_id"]}}])).unwrap();
    contract(
        root.path(),
        "carrier",
        "create",
        NESTED_CREATE,
        NESTED_CREATE_RESULT,
    );
    let rows = placed(root.path());
    assert_eq!(
        row(&rows, "book-replay")["body"]["value"]["idempotency_key"],
        "dock-gate-book-1"
    );
}

#[test]
fn undeclared_input_reports_the_field_operation_and_all_declared_paths() {
    let root = directory();
    placement_input(root.path());
    create_step(root.path());
    contract(
        root.path(),
        "carrier",
        "create",
        r#"{"fields":[{"path":"request_id","type":"text","nullable":false},{"path":"carrier_name","type":"text","nullable":false}]}"#,
        NESTED_CREATE_RESULT,
    );
    let rows = placed(root.path());
    let result = row(&rows, "create-carrier");
    assert!(result.get("body").is_none());
    assert_eq!(
        result["evidence"],
        "fixture field name matches no declared path for carrier.create. the contract declares carrier_name, request_id"
    );
}

#[test]
fn missing_input_contract_is_refused_without_guessing() {
    let root = directory();
    placement_input(root.path());
    create_step(root.path());
    let rows = placed(root.path());
    assert!(
        row(&rows, "create-carrier")["evidence"]
            .as_str()
            .unwrap()
            .starts_with("no published input contract for carrier.create")
    );
}

#[test]
fn each_operation_uses_its_own_published_shape() {
    let root = directory();
    placement_input(root.path());
    write_json(&root.path().join("steps.json"),&json!([
        {"id":"create-carrier","must":true,"invariant":"DOCK-0","route":{"operation":"carrier.create"},"body":{"name":"Northbound Freight"},"expect":{"status":200,"item":"value","present":["carrier_id"]}},
        {"id":"list","must":true,"invariant":"DOCK-6","route":{"operation":"appointment.query"},"body":{"day":"2026-10-01","dock_id":"11111111-1111-1111-1111-111111111111"},"expect":{"status":200,"item":"value","sorted_by":{"path":"appointments","field":"slot_start"}}}
    ])).unwrap();
    contract(
        root.path(),
        "carrier",
        "create",
        NESTED_CREATE,
        NESTED_CREATE_RESULT,
    );
    contract(
        root.path(),
        "appointment",
        "query",
        FLAT_QUERY,
        NESTED_QUERY_RESULT,
    );
    let rows = placed(root.path());
    assert!(
        row(&rows, "create-carrier")["body"]["value"]["name"] == "Northbound Freight"
            && row(&rows, "list")["body"]["day"] == "2026-10-01"
    );
}

#[test]
fn a_reused_value_follows_the_second_operation_contract() {
    let root = directory();
    placement_input(root.path());
    write_json(&root.path().join("steps.json"),&json!([
        {"id":"create-carrier","must":true,"invariant":"DOCK-0","route":{"operation":"carrier.create"},"body":{"name":"Northbound Freight"},"expect":{"status":200,"item":"value","present":["carrier_id"]}},
        {"id":"book","must":true,"invariant":"DOCK-2","route":{"operation":"appointment.book"},"body":{"slot_start":"2026-10-01T09:00:00Z"},"reuse":{"carrier_id":"create-carrier.value.carrier_id"},"expect":{"status":200,"item":"value","present":["appointment_id"]}}
    ])).unwrap();
    contract(
        root.path(),
        "carrier",
        "create",
        NESTED_CREATE,
        NESTED_CREATE_RESULT,
    );
    contract(
        root.path(),
        "appointment",
        "book",
        r#"{"fields":[{"path":"request_id","type":"text","nullable":false},{"path":"value.idempotency_key","type":"text","nullable":false},{"path":"value.carrier_id","type":"uuid","nullable":false},{"path":"value.slot_start","type":"timestamptz","nullable":false}]}"#,
        r#"{"class":"one","fields":[{"path":"appointment_id","type":"uuid","nullable":false}]}"#,
    );
    save_records(
        root.path(),
        &[
            json!({"id":"create-carrier","response":r#"[{"request_id":"x","value":{"carrier_id":"aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"}}]"#}),
        ],
    );
    let rows = placed(root.path());
    assert_eq!(
        row(&rows, "book")["body"]["value"]["carrier_id"],
        "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa"
    );
}

#[test]
fn run_markers_change_only_marked_values_and_keep_explicit_keys() {
    let root = directory();
    placement_input(root.path());
    contract(
        root.path(),
        "carrier",
        "create",
        NESTED_CREATE,
        NESTED_CREATE_RESULT,
    );
    let source = json!([{"id":"book-replay","must":true,"invariant":"DOCK-2","route":{"operation":"carrier.create"},"body":{"name":"Door 7 {{run}}","idempotency_key":"dock-gate-book-1"},"expect":{"status":200,"item":"value","present":["carrier_id"]}}]);
    let mut bodies = Vec::new();
    for key in ["place-alpha", "place-beta"] {
        let mut steps = source.clone();
        expand_run(&mut steps, key);
        let mut grading = Grading {
            directory: root.path().to_owned(),
            root: package(root.path()),
            overlay: "packages/dock".to_owned(),
            steps: steps.as_array().unwrap().clone(),
            results: Vec::new(),
        };
        let rows = grading.placement(&[]).unwrap();
        bodies.push(row(&rows, "book-replay")["body"].clone());
    }
    assert_eq!(bodies[0]["value"]["name"], "Door 7 place-alpha");
    assert!(
        bodies[0]["value"]["name"] != bodies[1]["value"]["name"]
            && bodies[1]["value"]["name"] == "Door 7 place-beta"
    );
    assert!(
        bodies[0]["value"]["idempotency_key"] == "dock-gate-book-1"
            && bodies[0]["request_id"] == "book-replay"
    );
}

fn brief(root: &Path, row: &str, second_table: &str) {
    fs::write(root.join("SCENARIO.md"),format!("# Test scenario\n\n## The data contract\n\n| operation | input | result |\n|---|---|---|\n{row}\n{second_table}\n\n## What the words mean here\n\nA dock is a physical door.\n")).unwrap();
}

async fn contract_code(root: &Path) -> u8 {
    grade_inner(GradeArgs {
        run: None,
        replay: None,
        placement: None,
        contract: Some(root.to_owned()),
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn nested_flat_and_crud_contracts_satisfy_the_same_brief() {
    for input in [NESTED_CREATE, FLAT_CREATE, CRUD_CREATE] {
        let root = directory();
        placement_input(root.path());
        brief(root.path(), ONE_ROW, "");
        contract(
            root.path(),
            "carrier",
            "create",
            input,
            NESTED_CREATE_RESULT,
        );
        assert_eq!(contract_code(root.path()).await, 0, "{input}");
    }
}

#[tokio::test]
async fn missing_brief_field_reports_every_published_path() {
    let root = directory();
    placement_input(root.path());
    brief(
        root.path(),
        "| `carrier.create` | `carrier_name` text | `carrier_id` uuid |",
        "",
    );
    contract(
        root.path(),
        "carrier",
        "create",
        NESTED_CREATE,
        NESTED_CREATE_RESULT,
    );
    assert_eq!(contract_code(root.path()).await, 30);
    let rows = contracts::grade(&package(root.path()), &root.path().join("SCENARIO.md")).unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row["field"] == "carrier_name")
            .unwrap()["evidence"],
        "the package declares no input field named carrier_name. it declares request_id, value.idempotency_key, value.name"
    );
}

#[tokio::test]
async fn a_brief_field_with_the_wrong_type_is_refused() {
    let root = directory();
    placement_input(root.path());
    brief(
        root.path(),
        "| `carrier.create` | `name` uuid | `carrier_id` uuid |",
        "",
    );
    contract(
        root.path(),
        "carrier",
        "create",
        NESTED_CREATE,
        NESTED_CREATE_RESULT,
    );
    assert_eq!(contract_code(root.path()).await, 30);
}

#[tokio::test]
async fn result_lists_must_exist_in_the_published_contract() {
    let root = directory();
    placement_input(root.path());
    brief(
        root.path(),
        "| `appointment.query` | `dock_id` uuid | `appointments` list, `slot_start` timestamp |",
        "",
    );
    contract(
        root.path(),
        "appointment",
        "query",
        FLAT_QUERY,
        NESTED_QUERY_RESULT,
    );
    assert_eq!(contract_code(root.path()).await, 0);
    contract(
        root.path(),
        "appointment",
        "query",
        FLAT_QUERY,
        NESTED_CREATE_RESULT,
    );
    assert_eq!(contract_code(root.path()).await, 30);
}

#[tokio::test]
async fn canonical_forms_must_match_an_actual_published_input() {
    let root = directory();
    placement_input(root.path());
    brief(
        root.path(),
        ONE_ROW,
        "| scalar | canonical form |\n|---|---|\n| `timestamp` | `utc_rfc3339_six_fractional_digits` |",
    );
    contract(
        root.path(),
        "carrier",
        "create",
        CANONICAL_CREATE,
        NESTED_CREATE_RESULT,
    );
    assert_eq!(contract_code(root.path()).await, 0);
    brief(
        root.path(),
        ONE_ROW,
        "| scalar | canonical form |\n|---|---|\n| `timestamp` | `utc_rfc3339_three_fractional_digits` |",
    );
    assert_eq!(contract_code(root.path()).await, 30);
    brief(
        root.path(),
        ONE_ROW,
        "| scalar | canonical form |\n|---|---|\n| `timestamp` | `utc_rfc3339_six_fractional_digits` |",
    );
    contract(
        root.path(),
        "carrier",
        "create",
        NESTED_CREATE,
        NESTED_CREATE_RESULT,
    );
    assert_eq!(contract_code(root.path()).await, 30);
}

#[tokio::test]
async fn unreadable_brief_cells_are_reported_without_guessing() {
    let root = directory();
    placement_input(root.path());
    brief(
        root.path(),
        "| `carrier.create` | a name in text | `carrier_id` uuid |",
        "",
    );
    contract(
        root.path(),
        "carrier",
        "create",
        NESTED_CREATE,
        NESTED_CREATE_RESULT,
    );
    assert_eq!(contract_code(root.path()).await, 30);
    let rows = contracts::grade(&package(root.path()), &root.path().join("SCENARIO.md")).unwrap();
    assert_eq!(
        rows.iter().find(|row| row["ok"] == false).unwrap()["evidence"],
        "the entry a name in text is not a `name` type pair"
    );
}

#[tokio::test]
async fn contract_deviations_are_reported_while_steps_decide_the_result() {
    let root = directory();
    recorded_input(root.path());
    contract(
        root.path(),
        "carrier",
        "create",
        r#"{"fields":[{"path":"request_id"},{"path":"value.name","type":"text"}]}"#,
        r#"{"fields":[{"path":"value.carrier_id","type":"uuid"}]}"#,
    );
    brief(
        root.path(),
        "| `carrier.create` | `arrival_time` text | `carrier_id` uuid |",
        "",
    );
    let (_, report) = replay(root.path()).await;
    assert_eq!(report["contract"]["state"], "deviates");
    assert!(report["contract"]["deviations"].as_u64().unwrap() >= 1);
    assert_eq!(report["outcome"], "PASS");
    brief(root.path(), ONE_ROW, "");
    let (_, report) = replay(root.path()).await;
    assert_eq!(report["contract"]["state"], "holds");
}

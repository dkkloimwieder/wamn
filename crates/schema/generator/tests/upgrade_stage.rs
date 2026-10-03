use serde_json::{Value, json};
use wamn_schema_generator::{PackageManifest, UpgradeStagePhase, compile_manifest};

fn manifest() -> Value {
    json!({
        "package": {"id":"fixture", "version":"2.0.0", "predecessor_version":"1.0.0"},
        "required_platform_policy_contract":{"id":"fixture_data_access", "state":"satisfied"},
        "models":{}, "components":{}, "connections":["main"],
        "upgrade_stage": {
            "phase":"backfill",
            "preconditions":{"ready":"SELECT true"},
            "postconditions":{"finished":"SELECT true"},
            "backfill":{"sql":"SELECT $1::jsonb AS next_cursor, true AS complete WHERE $2::int4 > 0", "batch_size":100, "initial_cursor":null}
        }
    })
}

fn parse(value: &Value) -> Result<PackageManifest, wamn_schema_generator::GenerateError> {
    PackageManifest::from_slice(&serde_json::to_vec(value).unwrap())
}

#[test]
fn stage_bytes_and_explicit_null_cursor_round_trip() {
    let input = manifest();
    let parsed = parse(&input).unwrap();
    assert_eq!(
        serde_json::to_value(parsed).unwrap()["upgrade_stage"],
        input["upgrade_stage"]
    );
    let mut missing_cursor = input;
    missing_cursor["upgrade_stage"]["backfill"]
        .as_object_mut()
        .unwrap()
        .remove("initial_cursor");
    assert!(parse(&missing_cursor).is_err());
}

#[test]
fn strict_stage_refuses_unknown_fields_and_invalid_combinations() {
    for (path, value) in [
        ("/upgrade_stage/phase", json!("drain")),
        ("/upgrade_stage/preconditions", json!({})),
        ("/upgrade_stage/postconditions", json!({" ":"SELECT true"})),
        ("/upgrade_stage/preconditions", json!({"ready":" "})),
        ("/upgrade_stage/backfill/sql", json!(" ")),
        ("/upgrade_stage/backfill/batch_size", json!(0)),
        (
            "/upgrade_stage/backfill/batch_size",
            json!(2_147_483_648_u64),
        ),
        ("/upgrade_stage/backfill/batch_size", json!(-1)),
        ("/upgrade_stage/backfill", Value::Null),
        ("/upgrade_stage/phase", json!("expand")),
        ("/package/predecessor_version", Value::Null),
        ("/connections", json!([])),
    ] {
        let mut input = manifest();
        *input.pointer_mut(path).unwrap() = value;
        assert!(parse(&input).is_err(), "must refuse {path}: {input}");
    }
    for path in ["/upgrade_stage", "/upgrade_stage/backfill"] {
        let mut input = manifest();
        input
            .pointer_mut(path)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), json!(true));
        assert!(parse(&input).is_err());
    }
    for exception in ["whole_row_grants", "skip_proof"] {
        let mut input = manifest();
        input["upgrade_stage"]["exceptions"] = json!([exception]);
        assert!(parse(&input).is_err());
    }
}

#[test]
fn expand_exception_and_contract_need_no_backfill() {
    for phase in ["expand", "contract"] {
        let mut input = manifest();
        input["upgrade_stage"]["phase"] = json!(phase);
        input["upgrade_stage"]
            .as_object_mut()
            .unwrap()
            .remove("backfill");
        if phase == "expand" {
            input["upgrade_stage"]["exceptions"] = json!(["whole_row_grants"]);
        }
        parse(&input).unwrap();
    }
}

#[test]
fn absent_stage_preserves_existing_serialization() {
    let mut input = manifest();
    input.as_object_mut().unwrap().remove("upgrade_stage");
    // These existing defaults were serialized before the stage vocabulary existed.
    input["base_dependencies"] = json!({});
    input["internal_relations"] = json!({});
    input["custom_operations"] = json!({});
    assert_eq!(serde_json::to_value(parse(&input).unwrap()).unwrap(), input);
}

#[test]
fn kcl_authored_stage_round_trips_exact_sql_and_cursor() {
    let root = std::env::temp_dir().join(format!("wamn-upgrade-stage-kcl-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(
        root.join("wamn.k"),
        r#"import manifest
manifest.Package {
    package = {id = "fixture", version = "2.0.0", predecessor_version = "1.0.0"}
    required_platform_policy_contract = {id = "fixture_data_access", state = "satisfied"}
    models = {}
    components = {}
    connections = ["main"]
    upgrade_stage = {
        phase = "backfill"
        preconditions = {ready = "SELECT true"}
        postconditions = {finished = "SELECT true"}
        backfill = {
            sql = "SELECT $1::jsonb AS next_cursor, true AS complete WHERE $2::int4 > 0"
            batch_size = 100
            initial_cursor = {last_id = 0}
        }
    }
}
"#,
    )
    .unwrap();
    let result = compile_manifest(&root);
    std::fs::remove_dir_all(&root).unwrap();
    let bytes = result.unwrap();
    let stage = PackageManifest::from_slice(&bytes)
        .unwrap()
        .upgrade_stage
        .unwrap();
    assert_eq!(stage.phase, UpgradeStagePhase::Backfill);
    let batch = stage.backfill.unwrap();
    assert_eq!(batch.initial_cursor, json!({"last_id":0}));
    assert_eq!(
        batch.sql,
        manifest()["upgrade_stage"]["backfill"]["sql"]
            .as_str()
            .unwrap()
    );
    assert_eq!(batch.batch_size, 100);
}

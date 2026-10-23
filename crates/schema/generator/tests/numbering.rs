//! Numbering and caller-filled columns (`docs/plan/platform-config.md`,
//! "Numbering"): one refusal test per build rule, the two-statement generated
//! insert, and the generated `sequences::<name>::next` binding.

use serde_json::{Value, json};
use wamn_schema_generator::{DATA_ACCESS_OVERLAY_PATH, GenerateErrorType, GeneratedPackage};
use wamn_schema_introspection::ir::{CatalogIr, Column, ColumnType, Constraint};

#[path = "support/generation.rs"]
#[expect(
    dead_code,
    reason = "the shared support module serves generation.rs too"
)]
mod support;
use support::{
    QUERY_SOURCES, artifact_json, catalog, manifest, object_named, rebuilt_table, replacing_table,
    run, table,
};

/// The gadget fixture with a numbered `part_code`, keyed per stock, and a
/// create that writes the stock. `row_version` is the caller column.
fn numbered() -> Value {
    let mut manifest = manifest();
    manifest["sequences"] = json!({
        "gadget_number": {"pattern": "{field:stock_id}-G{seq:n,reset=stock_id,width=4}"}
    });
    manifest["models"]["gadget"]["fields"] = json!({
        "part_code": {"number": "gadget_number"}
    });
    manifest["models"]["gadget"]["operations"]["create"] = json!({
        "permission": "gadget.create",
        "writable_fields": ["stock_id"],
        "result": "one"
    });
    manifest
}

/// The gadget catalog with one more uuid column, `checked_by`, for the actor.
fn actor_catalog() -> CatalogIr {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let mut columns = gadget.columns().to_vec();
    columns.push(Column::new(
        "checked_by",
        ColumnType::Uuid,
        false,
        None,
        None,
    ));
    replacing_table(
        &base,
        rebuilt_table(gadget, columns, gadget.constraints().to_vec()),
    )
}

/// The gadget catalog with an integer key that has no default.
fn integer_key_catalog() -> CatalogIr {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let columns = gadget
        .columns()
        .iter()
        .map(|column| {
            if column.name() == "id" {
                Column::new("id", ColumnType::Int32, false, None, None)
            } else {
                column.clone()
            }
        })
        .collect();
    let constraints = gadget
        .constraints()
        .iter()
        .filter(|constraint| constraint.name() != "gadget_id_pkey")
        .cloned()
        .chain([Constraint::primary_key("gadget_id_pkey", ["id"]).unwrap()])
        .collect();
    replacing_table(&base, rebuilt_table(gadget, columns, constraints))
}

fn refusal(catalog: &CatalogIr, manifest: &Value) -> (GenerateErrorType, String) {
    let error = run(catalog, manifest, &QUERY_SOURCES).expect_err("the manifest was accepted");
    (error.error_type(), error.to_string())
}

/// The source without whitespace, so an assertion does not depend on rustfmt.
fn squashed(source: &str) -> String {
    source
        .split_whitespace()
        .collect::<String>()
        .replace(",)", ")")
}

fn text(package: &GeneratedPackage, path: &str) -> String {
    String::from_utf8(
        package
            .file(path)
            .unwrap_or_else(|| panic!("{path} was emitted"))
            .bytes()
            .to_vec(),
    )
    .unwrap()
}

#[test]
fn the_generated_insert_numbers_from_the_host_and_reads_the_actor_claim() {
    let mut manifest = numbered();
    manifest["models"]["gadget"]["fields"]["checked_by"] = json!({"role": "actor"});
    let package = run(&actor_catalog(), &manifest, &QUERY_SOURCES).unwrap();
    assert_eq!(
        text(&package, "generated/sql/gadget/create.sql"),
        "INSERT INTO gadget (stock_id, part_code, checked_by)\n\
         VALUES ($1::uuid, $2::text, current_setting('app.user_id')::uuid)\n\
         RETURNING\n    \
         checked_by,\n    created_at,\n    id,\n    part_code,\n    row_version,\n    status,\n    stock_id;\n",
    );
    // The first statement: next-number with exactly the fields the pattern
    // names, then the insert with the text it returned.
    let data = squashed(&text(&package, "generated/data/gadget.rs"));
    let next = data
        .find(&squashed(r#"let part_code = wamn_postgres_statements::next_number("gadget_number", &[("stock_id", Some(stock_id.0.clone()))])"#))
        .unwrap_or_else(|| panic!("no next-number call in\n{data}"));
    let insert = data
        .find("sql::create(transaction,stock_id,part_code)")
        .unwrap();
    assert!(next < insert);
    // The create takes neither filled field from the caller.
    let signature = &data[data.find("pubasyncfncreate(").unwrap()..];
    let signature = &signature[..signature.find('{').unwrap()];
    assert!(
        !signature.contains("part_code") && !signature.contains("checked_by"),
        "{signature}"
    );
    let wit = text(
        &package,
        "generated/wit/deps/platform-gadget-gadget/package.wit",
    );
    assert!(!wit.contains("part-code: option") && !wit.contains("checked-by: option"));

    let overlay = artifact_json(&package, DATA_ACCESS_OVERLAY_PATH);
    let model = object_named(overlay["relations"].as_array().unwrap(), "table", "gadget");
    assert_eq!(
        model["insert_fields"],
        json!(["checked_by", "part_code", "stock_id"])
    );
    // A package that declares no callable sequence gets no binding.
    assert!(package.file("generated/data/sequences.rs").is_none());
}

#[test]
fn a_callable_sequence_gets_next_with_a_key_of_its_pattern_fields() {
    let mut manifest = manifest();
    manifest["sequences"] = json!({
        "pick_label": {
            "pattern": "B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}",
            "callable": true
        }
    });
    let package = run(&catalog(false), &manifest, &QUERY_SOURCES).unwrap();
    let sequences = squashed(&text(&package, "generated/data/sequences.rs"));
    for expected in [
        "pub mod pick_label {",
        "pub struct PickLabelKey {",
        "pub batch_id: String,",
        "pub async fn next(key: PickLabelKey) -> Result<String, wamn_postgres_statements::StatementError>",
        r#"wamn_postgres_statements::next_number("pick_label", &[("batch_id", Some(batch_id))])"#,
    ] {
        assert!(
            sequences.contains(&squashed(expected)),
            "{expected} in\n{sequences}"
        );
    }
    assert!(text(&package, "generated/data/mod.rs").contains("pub mod sequences {"));
}

#[test]
fn a_create_that_supplies_a_number_field_is_refused() {
    let mut manifest = numbered();
    manifest["models"]["gadget"]["server_owned_fields"] =
        json!(["id", "status", "row_version", "created_at"]);
    manifest["models"]["gadget"]["operations"]["create"]["writable_fields"] =
        json!(["stock_id", "part_code"]);
    let (kind, message) = refusal(&catalog(false), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidOperation, "{message}");
    assert!(
        message.contains("gadget.create writes gadget.part_code"),
        "{message}"
    );
}

#[test]
fn a_command_that_writes_an_actor_field_is_refused() {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["fields"] = json!({"checked_by": {"role": "actor"}});
    let mut command = support::projection_operation();
    command["type"] = json!("command");
    command["permission"] = json!("quality.check_gadget");
    command["idempotent_by"] = json!("claim");
    command["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path": "idempotency_key", "type": "text", "nullable": false}));
    command["transaction"] = json!("explicit_per_input");
    command["automatic_retry"] = json!(false);
    command["relations"][0]["update_fields"] = json!(["checked_by"]);
    manifest["custom_operations"] = json!({"quality.check_gadget": command});
    let (kind, message) = refusal(&actor_catalog(), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidOperation, "{message}");
    assert!(
        message.contains("quality.check_gadget writes gadget.checked_by"),
        "{message}"
    );
}

#[test]
fn a_sequence_with_no_field_and_no_caller_is_refused() {
    let mut manifest = manifest();
    manifest["sequences"] = json!({"unused": {"pattern": "U{seq:n}"}});
    let (kind, message) = refusal(&catalog(false), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidManifest);
    assert!(
        message.contains("sequence unused numbers no field"),
        "{message}"
    );
}

#[test]
fn per_on_the_key_field_is_refused() {
    let mut manifest = numbered();
    manifest["sequences"]["gadget_number"]["pattern"] = json!("G{seq:n,per=id}");
    manifest["models"]["gadget"]["operations"]["create"]["writable_fields"] = json!(["stock_id"]);
    let (kind, message) = refusal(&catalog(false), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidModel);
    assert!(
        message.contains("is per= the key field gadget.id"),
        "{message}"
    );
}

#[test]
fn reset_or_per_in_a_sequence_bound_to_the_key_is_refused() {
    for pattern in ["{seq:n,reset=stock_id}", "{seq:n,per=stock_id}"] {
        let mut manifest = numbered();
        manifest["sequences"]["gadget_number"]["pattern"] = json!(pattern);
        manifest["models"]["gadget"]["fields"] = json!({"id": {"number": "gadget_number"}});
        let (kind, message) = refusal(&catalog(false), &manifest);
        assert_eq!(kind, GenerateErrorType::InvalidModel, "{pattern}");
        assert!(message.contains("gadget.id is the key"), "{message}");
    }
}

#[test]
fn an_integer_key_takes_only_a_bare_counter() {
    let mut manifest = numbered();
    manifest["sequences"]["gadget_number"]["pattern"] = json!("G{seq:n}");
    manifest["models"]["gadget"]["fields"] = json!({"id": {"number": "gadget_number"}});
    // The generated reads key a row by a uuid, so the model only creates.
    let create = manifest["models"]["gadget"]["operations"]["create"].clone();
    manifest["models"]["gadget"]["operations"] = json!({"create": create});
    let (kind, message) = refusal(&integer_key_catalog(), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidModel, "{message}");
    assert!(
        message.contains("takes a bare {seq:<counter>} pattern only"),
        "{message}"
    );

    manifest["sequences"]["gadget_number"]["pattern"] = json!("{seq:n}");
    let package = run(&integer_key_catalog(), &manifest, &[]).unwrap();
    assert!(
        text(&package, "generated/sql/gadget/create.sql")
            .contains("VALUES ($1::uuid, $2::text::int4)")
    );
}

#[test]
fn a_key_that_is_no_field_counter_or_date_is_refused() {
    let mut manifest = numbered();
    manifest["sequences"]["gadget_number"]["pattern"] = json!("{seq:n,reset=facility}");
    let (kind, message) = refusal(&catalog(false), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidModel);
    assert!(
        message.contains("names facility, which is not a field of gadget"),
        "{message}"
    );
    // Another counter and a date are keys without a field.
    for pattern in ["{seq:a}-{seq:n,reset=a}", "{date:YY}{seq:n,reset=date:YY}"] {
        manifest["sequences"]["gadget_number"]["pattern"] = json!(pattern);
        run(&catalog(false), &manifest, &QUERY_SOURCES)
            .unwrap_or_else(|error| panic!("{pattern}: {error}"));
    }
}

#[test]
fn an_unknown_date_format_is_refused() {
    let mut manifest = numbered();
    manifest["sequences"]["gadget_number"]["pattern"] = json!("{date:YYMM}-{seq:n}");
    let (kind, message) = refusal(&catalog(false), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidManifest);
    assert!(message.contains("date format \"YYMM\""), "{message}");
}

#[test]
fn a_model_on_a_local_target_is_refused() {
    let mut manifest = manifest();
    manifest["targets"] = json!(["cloud", "local"]);
    let (kind, message) = refusal(&catalog(false), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidManifest);
    assert!(message.contains("runs on cloud only"), "{message}");

    manifest["targets"] = json!(["cloud"]);
    run(&catalog(false), &manifest, &QUERY_SOURCES).unwrap();
}

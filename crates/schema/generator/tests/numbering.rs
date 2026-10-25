//! Numbering and caller-filled columns (`docs/plan/platform-config.md`,
//! "Numbering"): one refusal test per build rule, the two-statement generated
//! insert, the generated update of `updated_by`, and the generated
//! `sequences::<name>::next` and `peek` bindings.

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
    run, run_with_bases, table,
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

/// The gadget catalog with one more column, such as `created_by` for the
/// actor.
fn actor_catalog(name: &str, ty: ColumnType) -> CatalogIr {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let mut columns = gadget.columns().to_vec();
    columns.push(Column::new(name, ty, false, None, None));
    replacing_table(
        &base,
        rebuilt_table(gadget, columns, gadget.constraints().to_vec()),
    )
}

/// The gadget catalog with the two stamp actors, `created_by` and
/// `updated_by`, and `updated_at`, which the audit log pairs with them.
fn stamped_catalog() -> CatalogIr {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let mut columns = gadget.columns().to_vec();
    for name in ["created_by", "updated_by"] {
        columns.push(Column::new(name, ColumnType::Uuid, false, None, None));
    }
    columns.push(Column::new(
        "updated_at",
        ColumnType::Timestamptz,
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

fn refusal_with_base(
    catalog: &CatalogIr,
    manifest: &Value,
    base: &Value,
) -> (GenerateErrorType, String) {
    let error = run_with_bases(
        catalog,
        manifest,
        &QUERY_SOURCES,
        std::slice::from_ref(base),
    )
    .expect_err("the manifest was accepted");
    (error.error_type(), error.to_string())
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
    manifest["models"]["gadget"]["fields"]["created_by"] = json!({"role": "actor"});
    manifest["models"]["gadget"]["audit_log"]["columns"] = json!(["created_at", "created_by"]);
    let package = run(
        &actor_catalog("created_by", ColumnType::Uuid),
        &manifest,
        &QUERY_SOURCES,
    )
    .unwrap();
    assert_eq!(
        text(&package, "generated/sql/gadget/create.sql"),
        "INSERT INTO gadget (stock_id, part_code, created_by)\n\
         VALUES ($1::uuid, $2::text, current_setting('app.user_id')::uuid)\n\
         RETURNING\n    \
         created_at,\n    created_by,\n    id,\n    part_code,\n    row_version,\n    status,\n    stock_id;\n",
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
        !signature.contains("part_code") && !signature.contains("created_by"),
        "{signature}"
    );
    let wit = text(
        &package,
        "generated/wit/deps/platform-gadget-gadget/package.wit",
    );
    assert!(!wit.contains("part-code: option") && !wit.contains("created-by: option"));

    let overlay = artifact_json(&package, DATA_ACCESS_OVERLAY_PATH);
    let model = object_named(overlay["relations"].as_array().unwrap(), "table", "gadget");
    assert_eq!(
        model["insert_fields"],
        json!(["created_by", "part_code", "stock_id"])
    );
    // The numbered field is one caller: commands can call the sequence too.
    assert!(
        squashed(&text(&package, "generated/data/sequences.rs")).contains("pubmodgadget_number{")
    );
}

#[test]
fn every_sequence_gets_next_with_a_key_of_the_values_its_pattern_names() {
    let mut manifest = manifest();
    manifest["sequences"] = json!({
        "pick_label": {
            "pattern": "B{seq:batch,per=batch_id,width=4}-{seq:line,reset=batch,width=3}"
        },
        "label": {"pattern": "L-{seq:n}"}
    });
    let package = run(&catalog(false), &manifest, &QUERY_SOURCES).unwrap();
    let sequences = squashed(&text(&package, "generated/data/sequences.rs"));
    for expected in [
        "pub mod pick_label {",
        "pub struct PickLabelKey {",
        "pub batch_id: String,",
        "pub async fn next(key: PickLabelKey) -> Result<String, wamn_postgres_statements::StatementError>",
        r#"wamn_postgres_statements::next_number("pick_label", &[("batch_id", Some(batch_id))])"#,
        "pub async fn peek(key: PickLabelKey) -> Result<String, wamn_postgres_statements::StatementError>",
        r#"wamn_postgres_statements::peek_number("pick_label", &[("batch_id", Some(batch_id))])"#,
    ] {
        assert!(
            sequences.contains(&squashed(expected)),
            "{expected} in\n{sequences}"
        );
    }
    // A bare pattern takes no key.
    for expected in [
        "pub mod label {",
        "pub async fn next() -> Result<String, wamn_postgres_statements::StatementError>",
        r#"wamn_postgres_statements::next_number("label", &[])"#,
        "pub async fn peek() -> Result<String, wamn_postgres_statements::StatementError>",
    ] {
        assert!(
            sequences.contains(&squashed(expected)),
            "{expected} in\n{sequences}"
        );
    }
    assert!(!sequences.contains("structLabelKey"), "{sequences}");
    assert!(text(&package, "generated/data/mod.rs").contains("pub mod sequences {"));
}

/// An insert fills every actor field; an update fills `updated_by` on a
/// change and never touches `created_by` (owner ruling 2026-10-23).
#[test]
fn an_update_fills_updated_by_and_never_created_by() {
    let mut manifest = numbered();
    manifest["models"]["gadget"]["fields"]["created_by"] = json!({"role": "actor"});
    manifest["models"]["gadget"]["fields"]["updated_by"] = json!({"role": "actor"});
    manifest["models"]["gadget"]["audit_log"]["columns"] =
        json!(["created_at", "created_by", "updated_at", "updated_by"]);
    let package = run(&stamped_catalog(), &manifest, &QUERY_SOURCES).unwrap();
    let create = text(&package, "generated/sql/gadget/create.sql");
    assert!(
        create.starts_with(
            "INSERT INTO gadget (stock_id, part_code, created_by, updated_by)\n\
             VALUES ($1::uuid, $2::text, current_setting('app.user_id')::uuid, \
             current_setting('app.user_id')::uuid)\n"
        ),
        "{create}"
    );
    let update = text(&package, "generated/sql/gadget/update.sql");
    assert!(
        update.contains(
            "        updated_by = CASE\n            \
             WHEN ($3::boolean AND $4::uuid::text IS DISTINCT FROM model.stock_id::text)\n            \
             THEN current_setting('app.user_id')::uuid\n            \
             ELSE model.updated_by\n        END,\n"
        ),
        "{update}"
    );
    assert!(!update.contains("created_by ="), "{update}");

    let overlay = artifact_json(&package, DATA_ACCESS_OVERLAY_PATH);
    let model = object_named(overlay["relations"].as_array().unwrap(), "table", "gadget");
    assert_eq!(
        model["insert_fields"],
        json!(["created_by", "part_code", "stock_id", "updated_by"])
    );
    assert_eq!(
        model["update_fields"],
        json!(["row_version", "stock_id", "updated_by"])
    );
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
    manifest["models"]["gadget"]["fields"] = json!({"updated_by": {"role": "actor"}});
    manifest["models"]["gadget"]["audit_log"]["columns"] =
        json!(["created_at", "created_by", "updated_at", "updated_by"]);
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
    command["relations"][0]["update_fields"] = json!(["updated_by"]);
    manifest["custom_operations"] = json!({"quality.check_gadget": command});
    let (kind, message) = refusal(&stamped_catalog(), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidOperation, "{message}");
    assert!(
        message.contains("quality.check_gadget writes gadget.updated_by"),
        "{message}"
    );
}

/// A sequence is independent of the records (owner ruling 2026-10-24): one
/// that numbers no field builds, and so does one whose pattern names a key
/// that no model has. Commands pass the keys.
#[test]
fn a_sequence_that_numbers_no_field_builds() {
    let mut manifest = manifest();
    manifest["sequences"] = json!({
        "unused": {"pattern": "U{seq:n}"},
        "dock_label": {"pattern": "{field:dock}-{seq:n,reset=dock}"}
    });
    let package = run(&catalog(false), &manifest, &QUERY_SOURCES).unwrap();
    let sequences = squashed(&text(&package, "generated/data/sequences.rs"));
    for expected in [
        "pub mod unused {",
        "pub mod dock_label {",
        "pub dock: String,",
    ] {
        assert!(
            sequences.contains(&squashed(expected)),
            "{expected} in\n{sequences}"
        );
    }
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

/// A model's numbered field needs a field of the model for each key its
/// pattern names, because the generated insert passes the model's fields as
/// the keys.
#[test]
fn a_numbered_field_whose_model_lacks_a_key_of_the_pattern_is_refused() {
    let mut manifest = numbered();
    manifest["sequences"]["gadget_number"]["pattern"] = json!("{seq:n,reset=facility}");
    let (kind, message) = refusal(&catalog(false), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidModel);
    assert!(
        message.contains("whose pattern names key facility, but gadget has no field facility"),
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

/// A numbered field that is not text takes only a bare `{seq}`, key or not.
#[test]
fn a_numbered_field_that_is_not_text_takes_only_a_bare_counter() {
    let mut manifest = numbered();
    manifest["sequences"]["gadget_number"]["pattern"] = json!("{seq:n,width=4}");
    manifest["models"]["gadget"]["fields"] = json!({"line_no": {"number": "gadget_number"}});
    manifest["models"]["gadget"]["server_owned_fields"]
        .as_array_mut()
        .unwrap()
        .push(json!("line_no"));
    for ty in [ColumnType::Int32, ColumnType::Int64] {
        let catalog = actor_catalog("line_no", ty);
        for pattern in ["{seq:n,width=4}", "L{seq:n}", "{seq:n,reset=stock_id}"] {
            manifest["sequences"]["gadget_number"]["pattern"] = json!(pattern);
            let (kind, message) = refusal(&catalog, &manifest);
            assert_eq!(
                kind,
                GenerateErrorType::InvalidModel,
                "{pattern}: {message}"
            );
            assert!(
            message.contains(&format!(
                "gadget.line_no is {}, not text, so sequence gadget_number takes a bare {{seq:<counter>}} pattern only", ty.as_str()
            )),
            "{pattern}: {message}"
        );
        }

        manifest["sequences"]["gadget_number"]["pattern"] = json!("{seq:n}");
        run(&catalog, &manifest, &QUERY_SOURCES).unwrap();
    }
}

/// An actor field is a uuid field named `created_by` or `updated_by`.
#[test]
fn an_actor_field_is_a_uuid_created_by_or_updated_by() {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["fields"] = json!({"checked_by": {"role": "actor"}});
    let (kind, message) = refusal(&actor_catalog("checked_by", ColumnType::Uuid), &manifest);
    assert_eq!(kind, GenerateErrorType::InvalidModel, "{message}");
    assert!(
        message.contains(
            "gadget.checked_by is an actor field, which must be a uuid field named created_by or updated_by"
        ),
        "{message}"
    );
    // The audit log refuses a text created_by that it selects. An overlay
    // selects no base column, so the actor rule refuses it.
    let mut text_actor = overlay(manifest.clone());
    text_actor["models"]["gadget"]["fields"] = json!({"created_by": {"role": "actor"}});
    let mut base = manifest.clone();
    base["models"]["gadget"]["fields"] = text_actor["models"]["gadget"]["fields"].clone();
    let (kind, message) = refusal_with_base(
        &actor_catalog("created_by", ColumnType::Text),
        &text_actor,
        &base,
    );
    assert_eq!(kind, GenerateErrorType::InvalidModel, "{message}");
    assert!(
        message.contains(
            "gadget.created_by is an actor field, which must be a uuid field named created_by or updated_by"
        ),
        "{message}"
    );

    manifest["models"]["gadget"]["fields"] =
        json!({"created_by": {"role": "actor"}, "updated_by": {"role": "actor"}});
    manifest["models"]["gadget"]["audit_log"]["columns"] =
        json!(["created_at", "created_by", "updated_at", "updated_by"]);
    run(&stamped_catalog(), &manifest, &QUERY_SOURCES).unwrap();
}

/// The manifest as an overlay: `acme_inventory` declares the model of the
/// `platform_gadget` relation.
fn overlay(mut manifest: Value) -> Value {
    manifest["package"]["id"] = json!("acme_inventory");
    manifest["base_dependencies"] = json!({"base_inventory": {
        "package": "platform_gadget",
        "version": "1.0.0",
        "digest": format!("sha256:{}", "a".repeat(64)),
        "operations": ["gadget.get"]
    }});
    manifest["models"]["gadget"]
        .as_object_mut()
        .unwrap()
        .remove("audit_log");
    manifest
}

/// An overlay numbers and keys only the relation owner's fields: the owner's
/// inserts never fill a column the overlay added. An actor field is always a
/// reserved column, which an overlay cannot add without an audit log.
#[test]
fn an_overlay_is_checked_against_the_relation_owners_fields() {
    let note = actor_catalog("note", ColumnType::Text);
    let mut base = numbered();
    let mut numbered = overlay(base.clone());
    numbered["models"]["gadget"]["field_owners"] = json!({"note": "acme_inventory"});
    // The overlay numbers the owner's field by the owner's key.
    run_with_bases(
        &note,
        &numbered,
        &QUERY_SOURCES,
        std::slice::from_ref(&base),
    )
    .unwrap();

    let mut own_number = numbered.clone();
    own_number["models"]["gadget"]["fields"] = json!({"note": {"number": "gadget_number"}});
    let (kind, message) = refusal_with_base(&note, &own_number, &base);
    assert_eq!(kind, GenerateErrorType::InvalidModel, "{message}");
    assert!(
        message.contains(
            "gadget.note belongs to acme_inventory, not to the relation owner platform_gadget"
        ),
        "{message}"
    );

    let mut own_key = numbered;
    own_key["sequences"]["gadget_number"]["pattern"] = json!("{field:note}-{seq:n,reset=note}");
    own_key["models"]["gadget"]["operations"]["create"]["writable_fields"] =
        json!(["stock_id", "note"]);
    base["sequences"] = own_key["sequences"].clone();
    let (kind, message) = refusal_with_base(&note, &own_key, &base);
    assert_eq!(kind, GenerateErrorType::InvalidModel, "{message}");
    assert!(
        message.contains(
            "whose pattern names key note, but gadget.note belongs to acme_inventory, not to the relation owner platform_gadget"
        ),
        "{message}"
    );

    // The owner's actor field is the overlay's to stamp.
    let mut base_actor = overlay(manifest());
    base_actor["models"]["gadget"]["fields"] = json!({"updated_by": {"role": "actor"}});
    base["models"]["gadget"]["fields"] = base_actor["models"]["gadget"]["fields"].clone();
    run_with_bases(
        &actor_catalog("updated_by", ColumnType::Uuid),
        &base_actor,
        &QUERY_SOURCES,
        &[base],
    )
    .unwrap();
}

#[test]
fn numbered_fields_accept_only_text_int32_and_int64() {
    for ty in [
        ColumnType::Boolean,
        ColumnType::Float64,
        ColumnType::Bytes,
        ColumnType::Numeric,
        ColumnType::Timestamptz,
        ColumnType::Json,
        ColumnType::Uuid,
        ColumnType::Text,
        ColumnType::Int32,
        ColumnType::Int64,
    ] {
        let mut manifest = numbered();
        manifest["models"]["gadget"]["fields"] = json!({"serial": {"number": "gadget_number"}});
        manifest["models"]["gadget"]["server_owned_fields"]
            .as_array_mut()
            .unwrap()
            .push(json!("serial"));
        manifest["sequences"]["gadget_number"]["pattern"] = json!("{seq:n}");
        let catalog = actor_catalog("serial", ty);
        if matches!(ty, ColumnType::Text | ColumnType::Int32 | ColumnType::Int64) {
            run(&catalog, &manifest, &QUERY_SOURCES).unwrap();
        } else {
            let (kind, message) = refusal(&catalog, &manifest);
            assert_eq!(kind, GenerateErrorType::InvalidModel, "{message}");
            assert!(
                message.contains("a numbered field must be text, int32 or int64"),
                "{ty:?}: {message}"
            );
        }
    }
}

fn stamped_numbered() -> Value {
    let mut base = numbered();
    base["models"]["gadget"]["fields"]["created_by"] = json!({"role": "actor"});
    base["models"]["gadget"]["fields"]["updated_by"] = json!({"role": "actor"});
    base["models"]["gadget"]["audit_log"]["columns"] =
        json!(["created_at", "created_by", "updated_at", "updated_by"]);
    base
}

fn undeclared_overlay(base: &Value) -> Value {
    let mut overlay = overlay(base.clone());
    overlay["models"]["gadget"]
        .as_object_mut()
        .unwrap()
        .remove("fields");
    overlay.as_object_mut().unwrap().remove("sequences");
    overlay
}

#[test]
fn an_overlay_inherits_numbers_and_actor_stamps_without_field_declarations() {
    let base = stamped_numbered();
    for overlay in [undeclared_overlay(&base), overlay(base.clone())] {
        let package = run_with_bases(
            &stamped_catalog(),
            &overlay,
            &QUERY_SOURCES,
            std::slice::from_ref(&base),
        )
        .unwrap();
        let create = text(&package, "generated/sql/gadget/create.sql");
        assert!(
            create.contains("current_setting('app.user_id')::uuid"),
            "{create}"
        );
        let update = text(&package, "generated/sql/gadget/update.sql");
        assert!(
            update.contains("updated_by = CASE")
                && update.contains("THEN current_setting('app.user_id')::uuid"),
            "{update}"
        );
        let rust = squashed(&text(&package, "generated/data/gadget.rs"));
        assert!(
            rust.contains(&squashed(r#"let part_code = wamn_postgres_statements::next_number("gadget_number", &[("stock_id", Some(stock_id.0.clone()))])"#)),
            "{rust}"
        );
        let access = artifact_json(&package, DATA_ACCESS_OVERLAY_PATH);
        let model = object_named(access["relations"].as_array().unwrap(), "table", "gadget");
        assert_eq!(
            model["insert_fields"],
            json!(["created_by", "part_code", "stock_id", "updated_by"])
        );
    }
}

#[test]
fn an_overlay_cannot_supply_inherited_number_or_actor_fields() {
    let base = stamped_numbered();
    for action in ["create", "update"] {
        for field in ["part_code", "created_by", "updated_by"] {
            let mut overlay = undeclared_overlay(&base);
            overlay["models"]["gadget"]["operations"][action]["writable_fields"]
                .as_array_mut()
                .unwrap()
                .push(json!(field));
            let (kind, message) = refusal_with_base(&stamped_catalog(), &overlay, &base);
            assert_eq!(kind, GenerateErrorType::InvalidOperation, "{message}");
            assert!(
                message.contains(&format!(
                    "gadget.{action} exposes server-owned field {field}"
                )) || message.contains(&format!("gadget.{action} writes gadget.{field}")),
                "{message}"
            );
        }
    }
}

#[test]
fn an_overlay_cannot_change_or_add_the_owners_number_or_actor_declaration() {
    let base = stamped_numbered();
    for (field, declaration) in [
        ("part_code", json!({"number": "other_sequence"})),
        ("created_by", json!({"number": "gadget_number"})),
        ("part_code", json!({"role": "actor"})),
        ("stock_id", json!({"number": "gadget_number"})),
        ("stock_id", json!({"role": "actor"})),
    ] {
        let mut overlay = undeclared_overlay(&base);
        overlay["models"]["gadget"]["fields"] = json!({(field): declaration});
        let (kind, message) = refusal_with_base(&stamped_catalog(), &overlay, &base);
        assert_eq!(kind, GenerateErrorType::InvalidModel, "{message}");
        assert!(
            message.contains("disagrees with the number or actor declaration"),
            "{message}"
        );
    }
    let mut overlay = overlay(base.clone());
    overlay["sequences"]["gadget_number"]["pattern"] = json!("OTHER-{seq:n}");
    let (_, message) = refusal_with_base(&stamped_catalog(), &overlay, &base);
    assert!(
        message.contains("sequence gadget_number disagrees"),
        "{message}"
    );
}

#[test]
fn an_overlay_requires_its_declared_owner_and_relation() {
    let base = stamped_numbered();
    let overlay = undeclared_overlay(&base);
    let error = run_with_bases(&stamped_catalog(), &overlay, &QUERY_SOURCES, &[]).unwrap_err();
    assert!(
        error.to_string().contains("needs the compiled manifest"),
        "{error}"
    );
    let mut wrong_version = base.clone();
    wrong_version["package"]["version"] = json!("9.0.0");
    let (_, message) = refusal_with_base(&stamped_catalog(), &overlay, &wrong_version);
    assert!(message.contains("needs the compiled manifest"), "{message}");
    let mut wrong_relation = base;
    wrong_relation["models"]["gadget"]["table"] = json!("other_table");
    let (_, message) = refusal_with_base(&stamped_catalog(), &overlay, &wrong_relation);
    assert!(
        message.contains("has no relation inventory.gadget"),
        "{message}"
    );
}

#[test]
fn an_overlay_command_cannot_write_inherited_number_or_actor_fields() {
    let base = stamped_numbered();
    for field in ["part_code", "created_by", "updated_by"] {
        let mut overlay = undeclared_overlay(&base);
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
        command["relations"][0]["update_fields"] = json!([field]);
        overlay["custom_operations"] = json!({"quality.check_gadget": command});
        let (kind, message) = refusal_with_base(&stamped_catalog(), &overlay, &base);
        assert_eq!(kind, GenerateErrorType::InvalidOperation, "{message}");
        assert!(
            message.contains(&format!("quality.check_gadget writes gadget.{field}")),
            "{message}"
        );
    }
}

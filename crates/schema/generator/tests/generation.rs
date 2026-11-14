use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use wamn_compiler::ManifestErrorType as GenerateErrorType;
use wamn_compiler::manifest::{
    CrudAction, PackageManifest, resolve_operation_reference, validate_operation_vocabulary,
};
use wamn_compiler::{AuthoredSql, GeneratedPackage};
use wamn_scalar::SqlValueType;
use wamn_schema_introspection::ir::{CatalogIr, Column, ColumnDefault, Constraint, Table};

#[path = "support/generation.rs"]
mod support;
use support::{
    QUERY_SOURCES, catalog, description_json, manifest, parsed_manifest, projection_operation,
    published_input, rebuilt_table, replacing_table, run, table,
};

#[test]
fn strict_manifest_and_ir_references_fail_loudly() {
    let ir = catalog(false);
    let mut unknown = manifest();
    unknown["future"] = json!(true);
    assert_eq!(
        run(&ir, &unknown, &QUERY_SOURCES).unwrap_err().error_type(),
        GenerateErrorType::InvalidManifest
    );

    let mut mismatch = manifest();
    mismatch["models"]["gadget"]["table"] = json!("missing");
    let error = run(&ir, &mismatch, &QUERY_SOURCES).unwrap_err();
    assert_eq!(error.error_type(), GenerateErrorType::UnknownRelation);
    assert_eq!(error.object(), Some("inventory.missing"));

    let mut unknown_action = manifest();
    unknown_action["models"]["gadget"]["operations"]["merge"] =
        json!({"permission": "gadget.merge", "result": "one"});
    assert_eq!(
        run(&ir, &unknown_action, &QUERY_SOURCES)
            .unwrap_err()
            .error_type(),
        GenerateErrorType::InvalidManifest
    );
}

#[test]
fn custom_sql_generated_rust_symbols_are_unique_before_emission() {
    let mut row_collision = manifest();
    let mut operation = projection_operation();
    let statement = operation["statements"]["load_gadget_part"].clone();
    operation["statements"] = json!({
        "foo1": statement,
        "foo_1": {
            "path": "query/other_gadget_part.sql",
            "fetch": "optional_one",
            "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
            "row": [{"name": "id", "type": "uuid", "nullable": false}]
        }
    });
    row_collision["custom_operations"]["quality.load_gadget_part"] = operation;
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&row_collision))
            .expect_err("two statements collapsed to one Rust row symbol")
            .error_type(),
        GenerateErrorType::InvalidOperation
    );
}

/// A custom projection or command cannot declare the page result class. No
/// paging contract exists for it, so page belongs to the generated query.
#[test]
fn a_custom_operation_refuses_the_page_result_class() {
    let projection = projection_operation();
    let mut command = projection_operation();
    command["type"] = json!("command");
    command["transaction"] = json!("explicit_per_input");
    command["automatic_retry"] = json!(false);
    command["idempotent_by"] = json!({"state": {"guards": {"gadget": "gadget_id"}}});
    for (type_, mut operation) in [("projection", projection), ("command", command)] {
        let mut admitted = manifest();
        admitted["custom_operations"]["quality.load_gadget_part"] = operation.clone();
        validate_operation_vocabulary(&parsed_manifest(&admitted))
            .unwrap_or_else(|error| panic!("the {type_} fixture is valid with class one: {error}"));

        operation["result"]["class"] = json!("page");
        let mut paged = manifest();
        paged["custom_operations"]["quality.load_gadget_part"] = operation;
        let refusal = run(&catalog(false), &paged, &QUERY_SOURCES)
            .expect_err("a custom operation declaring page generated");
        assert_eq!(
            refusal.error_type(),
            GenerateErrorType::InvalidOperation,
            "{type_}"
        );
        assert_eq!(
            refusal.context(),
            format!(
                "{type_} quality.load_gadget_part must not declare result class page; \
                 page belongs to the generated query"
            ),
        );
    }
}

#[test]
fn a_component_with_no_operation_refuses_over_base_sql() {
    let mut adapter = manifest();
    adapter["models"] = json!({});
    adapter["base_dependencies"] = json!({"integration": {
        "package": "wamn_integration", "version": "0.1.0",
        "digest": format!("sha256:{}", "a".repeat(64)),
        "operations": ["setting.read", "setting.write"]
    }});
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&adapter))
            .unwrap_err()
            .error_type(),
        GenerateErrorType::InvalidComponent
    );
    adapter["base_dependencies"] = json!({});
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&adapter))
            .unwrap_err()
            .error_type(),
        GenerateErrorType::InvalidComponent
    );
    adapter["base_dependencies"] = json!({"integration": {
        "package": "wamn_integration", "version": "0.1.0",
        "digest": format!("sha256:{}", "a".repeat(64)), "operations": ["setting.read"]
    }});
    adapter["connections"] =
        json!({"reporting": {"interface":"wamn:connection/http@0.1.0", "mode": "read"}});
    adapter["components"]["reporting"] = json!({});
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&adapter))
            .unwrap_err()
            .error_type(),
        GenerateErrorType::InvalidComponent
    );
}

#[test]
fn component_grouping_defaults_one_group_and_refuses_invalid_splits() {
    let single = parsed_manifest(&manifest());
    assert_eq!(
        validate_operation_vocabulary(&single).unwrap(),
        BTreeSet::from([
            "gadget.get".to_owned(),
            "gadget.query".to_owned(),
            "gadget.update".to_owned(),
        ])
    );
    assert!(
        single.models["gadget"].operations[&CrudAction::Get]
            .component
            .is_none()
    );

    let mut empty = manifest();
    empty["models"]["gadget"]["operations"]["get"]["component"] = json!("");
    let error = validate_operation_vocabulary(&parsed_manifest(&empty))
        .expect_err("an empty component name was accepted");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidComponent);

    let mut unknown = manifest();
    unknown["models"]["gadget"]["operations"]["get"]["component"] = json!("missing");
    let error = validate_operation_vocabulary(&parsed_manifest(&unknown))
        .expect_err("an unknown component was accepted");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidComponent);

    let mut identical = manifest();
    identical["components"]["duplicate"] = json!({});
    for operation in ["get", "query", "update"] {
        identical["models"]["gadget"]["operations"][operation]["component"] =
            json!(if operation == "query" {
                "duplicate"
            } else {
                "inventory"
            });
    }
    validate_operation_vocabulary(&parsed_manifest(&identical)).unwrap();

    let mut distinct = manifest();
    distinct["connections"] =
        json!({"reporting": {"interface":"wamn:connection/http@0.1.0", "mode": "read"}});
    distinct["components"]["reporting"] = json!({});
    distinct["models"]["gadget"]["operations"]["query"]["component"] = json!("reporting");
    let error = validate_operation_vocabulary(&parsed_manifest(&distinct))
        .expect_err("a multi-component manifest omitted explicit operation grouping");
    assert_eq!(error.error_type(), GenerateErrorType::InvalidComponent);

    for operation in ["get", "update"] {
        distinct["models"]["gadget"]["operations"][operation]["component"] = json!("inventory");
    }
    validate_operation_vocabulary(&parsed_manifest(&distinct))
        .expect("distinct requirement groups with explicit operation membership");

    let mut old_grouping = manifest();
    old_grouping["components"]["inventory"]["operations"] = json!(["gadget.get"]);
    assert!(PackageManifest::from_slice(&serde_json::to_vec(&old_grouping).unwrap()).is_err());
}

#[test]
fn shared_operation_vocabulary_refuses_permission_identity_drift() {
    let mut mismatch = manifest();
    mismatch["models"]["gadget"]["operations"]["get"]["permission"] = json!("gadget.query");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&mismatch))
            .expect_err("permission identity drift was accepted")
            .error_type(),
        GenerateErrorType::InvalidOperation
    );
    assert_eq!(
        run(&catalog(false), &mismatch, &QUERY_SOURCES)
            .expect_err("generation accepted permission identity drift")
            .error_type(),
        GenerateErrorType::InvalidOperation
    );
}

#[test]
fn shared_operation_vocabulary_refuses_noncanonical_coordinates() {
    let mut package = manifest();
    package["package"]["id"] = json!("platform-Gadget");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&package))
            .expect_err("noncanonical package id was accepted")
            .error_type(),
        GenerateErrorType::InvalidIdentity
    );

    for ambiguous in ["1.0.0:shadow", "1.0.0/shadow", "1.0.0@shadow"] {
        let mut version = manifest();
        version["package"]["version"] = json!(ambiguous);
        assert_eq!(
            validate_operation_vocabulary(&parsed_manifest(&version))
                .expect_err("ambiguous package version was accepted")
                .error_type(),
            GenerateErrorType::InvalidIdentity
        );
    }
}

#[test]
fn server_owned_fields_are_not_writable() {
    let ir = catalog(false);
    let mut server_owned = manifest();
    server_owned["models"]["gadget"]["operations"]["update"]["writable_fields"] = json!(["status"]);
    assert_eq!(
        run(&ir, &server_owned, &QUERY_SOURCES)
            .unwrap_err()
            .error_type(),
        GenerateErrorType::InvalidOperation
    );
}

/// An application integer is int32 by default, and int64 is opt-in, so a
/// revision carries the width of its own column. The request member, the JSON
/// typed export and the refusal detail all follow that column.
#[test]
fn a_revision_carries_the_width_its_column_declares() {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let columns = gadget
        .columns()
        .iter()
        .map(|column| {
            if column.name() == "row_version" {
                Column::new(
                    "row_version",
                    SqlValueType::Int32,
                    false,
                    Some(ColumnDefault::int32(1)),
                    None,
                )
            } else {
                column.clone()
            }
        })
        .collect::<Vec<_>>();
    let narrow = replacing_table(
        &base,
        rebuilt_table(gadget, columns, gadget.constraints().to_vec()),
    );
    let package = run(&narrow, &manifest(), &QUERY_SOURCES).expect("an int32 revision generates");

    let input = description_json(&package, "gadget.update", "input");
    assert_eq!(input["expected_row_version"]["type"], "int32");

    // A revision of any other type is still refused, and the message names the
    // two widths an application can declare.
    let columns = gadget
        .columns()
        .iter()
        .map(|column| {
            if column.name() == "row_version" {
                Column::new("row_version", SqlValueType::Text, false, None, None)
            } else {
                column.clone()
            }
        })
        .collect::<Vec<_>>();
    let wrong = replacing_table(
        &base,
        rebuilt_table(gadget, columns, gadget.constraints().to_vec()),
    );
    let refusal = run(&wrong, &manifest(), &QUERY_SOURCES).expect_err("a text revision generated");
    assert_eq!(refusal.error_type(), GenerateErrorType::InvalidOperation);
    assert!(
        refusal
            .context()
            .contains("revision field must be a non-null int32 or int64"),
        "{}",
        refusal.context()
    );
}

/// A filter on a text field can match by contains, and exact stays the
/// default, which the contract leaves unstated (wamn-yxm6). The contract
/// states the mode and the generated SQL matches a part of the column.
#[test]
fn a_contains_filter_matches_a_part_of_a_text_field() {
    let ir = catalog(false);
    let mut contains = manifest();
    let query = &mut contains["models"]["gadget"]["operations"]["query"];
    query.as_object_mut().unwrap().remove("authored_sql");
    query["filters"] = json!([
        {"field": "stock_id"},
        {"field": "part_code", "match": "contains"}
    ]);
    let package = run(&ir, &contains, &[]).unwrap();
    let input = description_json(&package, "gadget.query", "input");
    assert_eq!(
        input["filters"],
        json!([
            {"field": "stock_id", "binding": "json_array", "type": "uuid"},
            {"field": "part_code", "binding": "json_array", "type": "text", "match": "contains"}
        ])
    );
    let sql = std::str::from_utf8(
        package
            .file("generated/sql/gadget/query_created_at_ascending.sql")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    assert!(
        sql.contains(concat!(
            "    ($1::jsonb IS NULL OR model.stock_id IN (\n",
            "        SELECT filter.value::uuid\n",
        )),
        "exact stays the default: {sql}"
    );
    assert!(
        sql.contains(concat!(
            "    ($2::jsonb IS NULL OR EXISTS (\n",
            "        SELECT 1\n",
            "        FROM jsonb_array_elements_text($2::jsonb) AS filter(value)\n",
            "        WHERE strpos(model.part_code, filter.value) > 0\n",
            "    ))",
        )),
        "{sql}"
    );

    // A uuid holds no part, and a declared value domain is matched whole.
    for field in ["stock_id", "status"] {
        let mut refused = contains.clone();
        refused["models"]["gadget"]["operations"]["query"]["filters"] =
            json!([{"field": field, "match": "contains"}]);
        let error = run(&ir, &refused, &[]).unwrap_err();
        assert_eq!(
            error.error_type(),
            GenerateErrorType::InvalidOperation,
            "{field}"
        );
        assert!(
            error.to_string().contains(&format!(
                "gadget.query filter {field} matches by contains, which only a text field with no declared values takes it"
            )),
            "{error}"
        );
    }
}

/// A filter can match by prefix, by a range of two inclusive bounds, or by
/// whether its field is empty (wamn-3nsf.1). The contract states each mode and
/// its binding, the route admits each value's shape, and the generated SQL
/// matches it. Each mode refuses a field that cannot hold it.
#[test]
fn prefix_range_and_is_null_filters_match_their_fields() {
    let ir = catalog_with_columns(&[
        ("note", SqlValueType::Text, true),
        ("weight", SqlValueType::Numeric, false),
    ]);
    let mut modes = manifest();
    let query = &mut modes["models"]["gadget"]["operations"]["query"];
    query.as_object_mut().unwrap().remove("authored_sql");
    query["filters"] = json!([
        {"field": "part_code", "match": "prefix"},
        {"field": "created_at", "match": "range"},
        {"field": "weight", "match": "range"},
        {"field": "note", "match": "is_null"}
    ]);
    let package = run(&ir, &modes, &[]).unwrap();
    let input = description_json(&package, "gadget.query", "input");
    assert_eq!(
        input["filters"],
        json!([
            {"field": "part_code", "binding": "json_array", "type": "text", "match": "prefix"},
            {"field": "created_at", "binding": "json_range", "type": "timestamptz", "match": "range"},
            {"field": "weight", "binding": "json_range", "type": "numeric", "match": "range"},
            {"field": "note", "binding": "json_boolean", "type": "text", "match": "is_null"}
        ])
    );
    let route = published_input(&package, "gadget.query");
    let filter = &route["items"]["properties"]["filter"]["properties"];
    assert_eq!(
        filter["part_code"],
        json!({"type": "array", "items": {"type": "string"}})
    );
    assert_eq!(
        filter["created_at"],
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "min": {"type": "string", "format": "date-time"},
                "max": {"type": "string", "format": "date-time"},
            },
        })
    );
    assert_eq!(filter["note"], json!({"type": "boolean"}));
    // A uuid holds no prefix, text has no order a range reads, and a column
    // that is never empty takes no is-null filter.
    for (field, mode) in [
        ("stock_id", "prefix"),
        ("status", "prefix"),
        ("part_code", "range"),
        ("row_version", "range"),
        ("part_code", "is_null"),
    ] {
        let mut refused = modes.clone();
        refused["models"]["gadget"]["operations"]["query"]["filters"] =
            json!([{"field": field, "match": mode}]);
        let error = run(&ir, &refused, &[]).unwrap_err();
        assert_eq!(
            error.error_type(),
            GenerateErrorType::InvalidOperation,
            "{field}"
        );
    }
}

/// A query can search its declared text fields on the server (wamn-3nsf.2).
/// The search binds after the filters and before the cursor, and it matches
/// a part of any field in any case.
#[test]
fn a_query_searches_its_declared_text_fields() {
    let ir = catalog_with_columns(&[("note", SqlValueType::Text, true)]);
    let mut search = manifest();
    let query = &mut search["models"]["gadget"]["operations"]["query"];
    query.as_object_mut().unwrap().remove("authored_sql");
    query["filters"] = json!([{"field": "status"}]);
    query["search"] = json!({"fields": ["part_code", "note"]});
    let package = run(&ir, &search, &[]).unwrap();
    let input = description_json(&package, "gadget.query", "input");
    assert_eq!(
        input["search"],
        json!({"fields": ["part_code", "note"], "binding": "text"})
    );
    let route = published_input(&package, "gadget.query");
    assert_eq!(
        route["items"]["properties"]["search"],
        json!({"type": "string", "minLength": 1})
    );
    let wit = std::str::from_utf8(
        package
            .file("generated/wit/deps/platform-gadget-gadget/package.wit")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    assert!(
        wit.contains("    status: option<list<string>>,\n    search: option<string>,\n"),
        "{wit}"
    );
    let sql = std::str::from_utf8(
        package
            .file("generated/sql/gadget/query_created_at_ascending.sql")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    for predicate in [
        concat!(
            "    ($2::text IS NULL OR strpos(lower(model.part_code), lower($2::text)) > 0\n",
            "        OR strpos(lower(model.note), lower($2::text)) > 0)",
        ),
        "    ($3::timestamptz IS NULL OR ",
        "LIMIT $5::int8;",
    ] {
        assert!(sql.contains(predicate), "{predicate}\n{sql}");
    }
    // A query without a search keeps its contract bytes.
    let plain = run(&ir, &manifest(), &QUERY_SOURCES).unwrap();
    assert!(
        description_json(&plain, "gadget.query", "input")
            .get("search")
            .is_none()
    );

    // A search reads text, names each field once, and names at least one.
    for fields in [json!(["stock_id"]), json!(["note", "note"]), json!([])] {
        let mut refused = search.clone();
        refused["models"]["gadget"]["operations"]["query"]["search"] = json!({"fields": fields});
        let error = run(&ir, &refused, &[]).unwrap_err();
        assert_eq!(
            error.error_type(),
            GenerateErrorType::InvalidOperation,
            "{fields}"
        );
        assert!(error.to_string().contains("gadget.query search"), "{error}");
    }
    // Only a query takes a search.
    let mut get = search.clone();
    get["models"]["gadget"]["operations"]["get"]["search"] = json!({"fields": ["part_code"]});
    assert_eq!(
        run(&ir, &get, &[]).unwrap_err().error_type(),
        GenerateErrorType::InvalidOperation
    );
}

/// A range on a time field can be a required band with a default
/// (wamn-3nsf.3). A read without the band reads the default one, as of the
/// statement. A read that sends the band states its minimum, and a band with
/// no default is always sent.
#[test]
fn a_required_band_reads_its_default_and_states_its_minimum() {
    let ir = catalog(false);
    let mut band = manifest();
    let query = &mut band["models"]["gadget"]["operations"]["query"];
    query.as_object_mut().unwrap().remove("authored_sql");
    query["filters"] = json!([{
        "field": "created_at",
        "match": "range",
        "required": true,
        "default": {"last_days": 30}
    }]);
    let package = run(&ir, &band, &[]).unwrap();
    let input = description_json(&package, "gadget.query", "input");
    assert_eq!(
        input["filters"],
        json!([{
            "field": "created_at",
            "binding": "json_range",
            "type": "timestamptz",
            "match": "range",
            "required": true,
            "default": {"last_days": 30}
        }])
    );
    let route = published_input(&package, "gadget.query");
    let item = &route["items"];
    assert_eq!(
        item["properties"]["filter"]["properties"]["created_at"]["required"],
        json!(["min"])
    );
    assert!(
        item.get("required").is_none(),
        "a band with a default is not sent: {item}"
    );
    let sql = std::str::from_utf8(
        package
            .file("generated/sql/gadget/query_created_at_ascending.sql")
            .unwrap()
            .bytes(),
    )
    .unwrap();
    assert!(
        sql.contains(concat!(
            "    (CASE WHEN $1::jsonb IS NULL\n",
            "        THEN model.created_at >= now() - make_interval(days => 30)\n",
            "        ELSE (\n",
            "        ($1::jsonb->>'min' IS NULL OR model.created_at >= ($1::jsonb->>'min')::timestamptz)\n",
        )),
        "{sql}"
    );

    // A band with no default is always sent.
    let mut always = band.clone();
    always["models"]["gadget"]["operations"]["query"]["filters"][0]
        .as_object_mut()
        .unwrap()
        .remove("default");
    let package = run(&ir, &always, &[]).unwrap();
    let route = published_input(&package, "gadget.query");
    assert_eq!(route["items"]["required"], json!(["filter"]));
    assert_eq!(
        route["items"]["properties"]["filter"]["required"],
        json!(["created_at"])
    );

    // Only a required range over time is a band, and only a band has a
    // default of at least one day.
    for filter in [
        json!({"field": "part_code", "required": true}),
        json!({"field": "created_at", "required": true}),
        json!({"field": "created_at", "match": "range", "default": {"last_days": 30}}),
        json!({"field": "created_at", "match": "range", "required": true, "default": {"last_days": 0}}),
    ] {
        let mut refused = band.clone();
        refused["models"]["gadget"]["operations"]["query"]["filters"] = json!([filter]);
        let error = run(&ir, &refused, &[]).unwrap_err();
        assert_eq!(
            error.error_type(),
            GenerateErrorType::InvalidOperation,
            "{filter}"
        );
        assert!(
            error.to_string().contains("is required or has a default"),
            "{error}"
        );
    }
}

#[test]
fn duplicate_filters_and_schema_qualified_authored_sql_refuse() {
    let ir = catalog(false);
    let mut duplicate = manifest();
    duplicate["models"]["gadget"]["operations"]["query"]["filters"] = json!([
        {"field": "status"},
        {"field": "status"}
    ]);
    assert_eq!(
        run(&ir, &duplicate, &QUERY_SOURCES)
            .unwrap_err()
            .error_type(),
        GenerateErrorType::InvalidOperation
    );

    let qualified = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_gadget.sql" {
            AuthoredSql::new(source.path(), b"SELECT * FROM inventory.gadget;\n")
        } else {
            source
        }
    });
    let error = run(&ir, &manifest(), &qualified).unwrap_err();
    assert_eq!(error.error_type(), GenerateErrorType::SchemaQualifiedSql);
    assert_eq!(error.path(), Some("query/open_gadget.sql"));

    let quoted = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_gadget.sql" {
            AuthoredSql::new(source.path(), b"SELECT * FROM \"inventory\".\"gadget\";\n")
        } else {
            source
        }
    });
    assert_eq!(
        run(&ir, &manifest(), &quoted).unwrap_err().error_type(),
        GenerateErrorType::SchemaQualifiedSql
    );

    let inert = QUERY_SOURCES.map(|source| {
        if source.path() == "query/open_gadget.sql" {
            AuthoredSql::new(
                source.path(),
                b"-- inventory.gadget\nSELECT 'inventory.gadget', $$\"inventory\".\"gadget\"$$, $3, $4, $5\nWHERE ($1::jsonb IS NULL OR stock_id::text IN (SELECT jsonb_array_elements_text($1::jsonb)))\n    AND ($2::jsonb IS NULL OR status IN (SELECT jsonb_array_elements_text($2::jsonb)))\n/* inventory.gadget */;\n",
            )
        } else {
            source
        }
    });
    run(&ir, &manifest(), &inert).unwrap();
}

/// An authored query implements the match mode that each filter declares
/// (wamn-efxf). The refusal names the file and the line of the predicate.
#[test]
fn authored_query_sql_that_does_not_implement_a_filter_mode_refuses() {
    let ir = catalog(false);
    let mut contains = manifest();
    contains["models"]["gadget"]["operations"]["query"]["filters"][1] =
        json!({"field": "part_code", "match": "contains"});
    let error = run(&ir, &contains, &QUERY_SOURCES).unwrap_err();
    assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
    assert_eq!(
        error.context(),
        "query/open_gadget_by_part_code_ascending.sql:3: gadget.query filter `part_code` declares match contains, \
         but its predicate on $2 matches exact"
    );
    assert_eq!(
        error.path(),
        Some("query/open_gadget_by_part_code_ascending.sql")
    );

    let substring = QUERY_SOURCES.map(|source| {
        AuthoredSql::new(
            source.path(),
            b"SELECT $3, $4, $5\nWHERE ($1::jsonb IS NULL OR stock_id::text IN (SELECT jsonb_array_elements_text($1::jsonb)))\n    AND ($2::jsonb IS NULL OR EXISTS (\n        SELECT 1 FROM jsonb_array_elements_text($2::jsonb) AS filter(value)\n        WHERE strpos(part_code, filter.value) > 0\n    ));\n",
        )
    });
    run(&ir, &contains, &substring).unwrap();
    let error = run(&ir, &manifest(), &substring).unwrap_err();
    assert_eq!(
        error.context(),
        "query/open_gadget_by_part_code_ascending.sql:3: gadget.query filter `status` declares match exact, \
         but its predicate on $2 matches contains"
    );

    let unread = QUERY_SOURCES.map(|source| {
        AuthoredSql::new(source.path(), b"SELECT $1, $3, $4, $5 WHERE $2 IS NULL;\n")
    });
    assert_eq!(
        run(&ir, &manifest(), &unread).unwrap_err().context(),
        "query/open_gadget_by_part_code_ascending.sql:1: gadget.query filter `stock_id` declares match exact, \
         but its predicate on $1 matches no filter mode"
    );
}

// ---------------------------------------------------------------------------
// Generated create: the write log
// (docs/architecture/data-access.md#the-write-log).
//
// A create claims its key in `platform.write_log`, inserts its row and
// stores its result in one transaction that its typed export holds. The binding
// supplies the id, and `RETURNING` hands the row back.
// ---------------------------------------------------------------------------

fn create_manifest() -> Value {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["operations"]["create"] = json!({
        "permission": "gadget.create",
        "writable_fields": ["stock_id"],
        "result": "one"
    });
    manifest
}

#[test]
fn id_is_client_owned_and_never_a_writable_field() {
    let mut declared = create_manifest();
    declared["models"]["gadget"]["server_owned_fields"] = json!(["id"]);
    let error = run(&catalog(false), &declared, &QUERY_SOURCES).unwrap_err();
    assert_eq!(error.error_type(), GenerateErrorType::InvalidModel);
    assert!(
        error
            .to_string()
            .contains("gadget lists id in server_owned_fields")
    );
    let mut declared = create_manifest();
    declared["models"]["gadget"]["fields"] = json!({"id": {"role": "actor"}});
    let error = run(&catalog(false), &declared, &QUERY_SOURCES).unwrap_err();
    assert!(error.to_string().contains("gadget.id is the key"));
    for action in ["create", "update"] {
        let mut declared = create_manifest();
        declared["models"]["gadget"]["operations"][action]["writable_fields"] = json!(["id"]);
        let error = run(&catalog(false), &declared, &QUERY_SOURCES).unwrap_err();
        assert_eq!(error.error_type(), GenerateErrorType::InvalidOperation);
        assert!(
            error
                .to_string()
                .contains(&format!("gadget.{action} lists id in writable_fields"))
        );
    }
}

/// A second model with create fields in each admitted presence state
/// and a revision name that is not `row_version`.
fn inventory_item_fixture() -> (CatalogIr, Value) {
    let model = Table::new(
        "inventory",
        "inventory_item",
        vec![
            Column::new(
                "id",
                SqlValueType::Uuid,
                false,
                Some(ColumnDefault::GenRandomUuid),
                None,
            ),
            Column::new("sku", SqlValueType::Text, false, None, None),
            Column::new("note", SqlValueType::Text, true, None, None),
            Column::new(
                "priority",
                SqlValueType::Int64,
                false,
                Some(ColumnDefault::int64(0)),
                None,
            ),
            Column::new(
                "sequence_number",
                SqlValueType::Int64,
                false,
                Some(ColumnDefault::int64(1)),
                None,
            ),
            Column::new(
                "created_at",
                SqlValueType::Timestamptz,
                false,
                Some(ColumnDefault::CurrentTimestamp),
                None,
            ),
        ],
        vec![Constraint::primary_key("inventory_item_id_pkey", ["id"]).unwrap()],
        Vec::new(),
    );
    let manifest = json!({
        "package": {"id": "wamn_inventory", "version": "1.0.0"},
        "required_platform_policy_contract": {
            "id": "inventory_data_access",
            "state": "unsatisfied"
        },
        "models": {
            "inventory_item": {
                "schema": "inventory",
                "table": "inventory_item",
                "owner": "wamn_inventory",
                "server_owned_fields": ["sequence_number", "created_at"],
                "audit_log": {"columns": ["created_at"], "retention": "none"},
                "delete_mode": "hard",
                "operations": {
                    "create": {
                        "permission": "inventory_item.create",
                        "writable_fields": ["sku", "note", "priority"],
                        "result": "one"
                    },
                    "update": {
                        "permission": "inventory_item.update",
                        "writable_fields": ["priority"],
                        "revision_field": "sequence_number",
                        "result": "one"
                    },
                    "delete": {
                        "permission": "inventory_item.delete",
                        "revision_field": "sequence_number",
                        "result": "one"
                    }
                }
            }
        },
        "connections": {},
        "components": {"inventory": {}}
    });
    (CatalogIr::new(vec![model]), manifest)
}

/// Compile the generated CRUD exports against their typed contracts.
fn compile_inventory_item_component(package: &GeneratedPackage, export_tests: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .expect("the generator sits three levels below the repository root");
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let scratch = target.join("generator-fixtures/typed-crud-contracts");
    let fixture_target = target.join("generator-fixture-build");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.join("src")).expect("create fixture source directory");
    std::fs::create_dir_all(scratch.join("wit")).expect("create fixture WIT directory");

    for (source, target) in [
        (
            "generated/wit/deps/wamn-inventory-inventory-item/package.wit",
            "wit/package.wit",
        ),
        (
            "generated/wit/inventory_item_create_operation.rs",
            "src/create_operation.rs",
        ),
        (
            "generated/wit/inventory_item_update_operation.rs",
            "src/update_operation.rs",
        ),
        (
            "generated/wit/inventory_item_delete_operation.rs",
            "src/delete_operation.rs",
        ),
    ] {
        std::fs::write(
            scratch.join(target),
            package
                .file(source)
                .unwrap_or_else(|| panic!("missing {source}"))
                .bytes(),
        )
        .unwrap_or_else(|error| panic!("write {target}: {error}"));
    }

    let platform = root.join("apps/platform");
    std::fs::write(
        scratch.join("Cargo.toml"),
        format!(
            "[package]\nname = \"typed-crud-fixture\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
             \n[dependencies]\nserde = {{ version = \"1\", features = [\"derive\"] }}\nserde_json = \"1\"\n\
             uuid = \"1\"\nwit-bindgen = {{ version = \"0.61\", default-features = false, features = [\"async\", \"macros\", \"realloc\"] }}\n\
             wamn-scalar = {{ path = \"{}\" }}\n\
             wamn-postgres-statements = {{ path = \"{}\" }}\n\
             wamn-data-access = {{ path = \"{}\" }}\n\
             \n[lib]\ncrate-type = [\"cdylib\"]\n\n[workspace]\n",
            root.join("crates/platform/scalar").display(),
            platform.join("data/postgres-statements").display(),
            platform.join("data/access").display(),
        ),
    )
    .expect("write fixture manifest");
    let node = root.join("wit/wamn-node");
    let lib = format!(
        r##"wit_bindgen::generate!({{
    world: "wamn:inventory-fixture/component@1.0.0",
    inline: r#"
        package wamn:inventory-fixture@1.0.0;
        world component {{
          export wamn-inventory:inventory-item/create@1.0.0;
          export wamn-inventory:inventory-item/update@1.0.0;
          export wamn-inventory:inventory-item/delete@1.0.0;
        }}
    "#,
    path: ["{}", "{}"],
    generate_all,
    async: true,
}});

struct Component;

macro_rules! operation {{
    ($module:ident, $contract:ident, $handler:ident, $request:ident, $result:ident, $error:ident) => {{
        mod $module {{
            use crate::exports::wamn_inventory::inventory_item::$contract as contract;
            include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/", stringify!($module), ".rs"));
        }}
        async fn $handler(
            _transaction: &mut wamn_postgres_statements::Transaction,
            _request: exports::wamn_inventory::inventory_item::$contract::$request,
        ) -> Result<
            exports::wamn_inventory::inventory_item::$contract::$result,
            exports::wamn_inventory::inventory_item::$contract::$error,
        > {{
            unreachable!()
        }}
    }};
}}
"##,
        node.display(),
        scratch.join("wit").display()
    );
    let lib = format!(
        "{lib}\noperation!(create_operation, create, create_handler, CreateRequest, CreateResult, CreateError);\n\
         operation!(update_operation, update, update_handler, UpdateRequest, UpdateResult, UpdateError);\n\
         operation!(delete_operation, delete, delete_handler, DeleteRequest, DeleteResult, DeleteError);\n\n\
         create_operation::export_operation!(Component, crate::exports::wamn_inventory::inventory_item::create, crate::wamn::node::types, create_handler, create_operation);\n\
         update_operation::export_operation!(Component, crate::exports::wamn_inventory::inventory_item::update, crate::wamn::node::types, update_handler, update_operation);\n\
         delete_operation::export_operation!(Component, crate::exports::wamn_inventory::inventory_item::delete, crate::wamn::node::types, delete_handler, delete_operation);\n\n\
         export!(Component);\n\n{export_tests}"
    );
    std::fs::write(scratch.join("src/lib.rs"), lib).expect("write fixture component");

    let output = Command::new(env!("CARGO"))
        .args(["test", "--offline", "--quiet"])
        .env("CARGO_TARGET_DIR", fixture_target)
        .current_dir(&scratch)
        .output()
        .expect("cargo test runs for the typed CRUD fixture");
    assert!(
        output.status.success(),
        "the generated typed CRUD component does not compile or its typed export check fails:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn typed_crud_contracts_follow_a_second_model_declaration() {
    let (catalog, manifest) = inventory_item_fixture();
    let package = run(&catalog, &manifest, &[]).expect("the inventory CRUD fixture generates");
    compile_inventory_item_component(&package, "");
    for ty in [SqlValueType::Int32, SqlValueType::Int64] {
        let model = table(&catalog, "inventory_item");
        let columns = model
            .columns()
            .iter()
            .map(|column| {
                if column.name() == "id" {
                    Column::new(
                        "id",
                        ty,
                        false,
                        None,
                        Some(wamn_schema_introspection::ir::ColumnGeneration::Identity {
                            mode: wamn_schema_introspection::ir::IdentityMode::Always,
                        }),
                    )
                } else {
                    column.clone()
                }
            })
            .collect();
        let catalog = replacing_table(
            &catalog,
            rebuilt_table(model, columns, model.constraints().to_vec()),
        );
        let package = run(&catalog, &manifest, &[]).unwrap();
        compile_inventory_item_component(&package, "");
    }
}

/// Validation refuses the `claim` object of a create:
/// `idempotent_by: claim` is the whole declaration.
#[test]
fn a_create_that_declares_a_claim_refuses() {
    run(&catalog(false), &create_manifest(), &QUERY_SOURCES)
        .expect("a create without one generates");
    let mut manifest = create_manifest();
    manifest["models"]["gadget"]["operations"]["create"]["claim"] =
        json!({"table": "gadget_command", "identities": {"id": "gadget_id"}});
    let refusal =
        run(&catalog(false), &manifest, &QUERY_SOURCES).expect_err("a claim object refuses");
    assert_eq!(refusal.error_type(), GenerateErrorType::InvalidManifest);
}

// ---------------------------------------------------------------------------
// Record history, level 1: the audit_log declaration (spec section 2, test 6).
// ---------------------------------------------------------------------------

/// The support catalog, with each named column added to `gadget`.
fn catalog_with_columns(columns: &[(&str, SqlValueType, bool)]) -> CatalogIr {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let stamped = rebuilt_table(
        gadget,
        gadget
            .columns()
            .iter()
            .cloned()
            .chain(
                columns
                    .iter()
                    .map(|(name, ty, nullable)| Column::new(*name, *ty, *nullable, None, None)),
            )
            .collect(),
        gadget.constraints().to_vec(),
    );
    replacing_table(&base, stamped)
}

fn all_stamps_catalog() -> CatalogIr {
    catalog_with_columns(&[
        ("created_by", SqlValueType::Uuid, false),
        ("updated_at", SqlValueType::Timestamptz, false),
        ("updated_by", SqlValueType::Uuid, false),
    ])
}

fn with_audit_log(columns: &Value, retention: &str) -> Value {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["audit_log"] = json!({"columns": columns, "retention": retention});
    manifest
}

fn all_stamps_manifest() -> Value {
    with_audit_log(
        &json!(["created_at", "created_by", "updated_at", "updated_by"]),
        "none",
    )
}

/// An authored reference takes the version of the package that owns it, from
/// `wamn.json`, and a reference that carries a version refuses (wamn-uqs5.1).
#[test]
fn an_operation_reference_takes_its_version_from_the_manifest() {
    let mut overlay = overlay_manifest();
    overlay["package"]["version"] = json!("2.0.0");
    let manifest = parsed_manifest(&overlay).operation_owners();
    assert_eq!(
        resolve_operation_reference(&manifest, "acme-inventory:gadget/assemble").unwrap(),
        "acme-inventory:gadget/assemble@2.0.0"
    );
    assert_eq!(
        resolve_operation_reference(&manifest, "platform-gadget:gadget/get").unwrap(),
        "platform-gadget:gadget/get@1.0.0"
    );
    for (reference, reason) in [
        (
            "acme-inventory:gadget/assemble@2.0.0",
            "must not carry a version",
        ),
        ("other-package:gadget/get", "names neither this package"),
        (
            "acme-inventory:gadget",
            "must be <package>:<interface>/<operation>",
        ),
        (
            "acme-inventory:gadget/as_semble",
            "must be <package>:<interface>/<operation>",
        ),
    ] {
        let error = resolve_operation_reference(&manifest, reference).unwrap_err();
        assert_eq!(error.error_type(), GenerateErrorType::InvalidIdentity);
        assert!(error.to_string().contains(reason), "{reference}: {error}");
    }
}

/// A package that overlays the `platform_gadget` purchase order.
fn overlay_manifest() -> Value {
    let mut manifest = manifest();
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

/// One declared delete, in the shape the closed error-detail set requires.
///
/// The fixture relation carries no inbound foreign key, so no constraint code
/// belongs in the set.
fn delete_operation() -> Value {
    json!({
        "permission": "gadget.delete",

        "revision_field": "row_version",
        "result": "one"
    })
}

fn deleting_manifest(mode: &str) -> Value {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["delete_mode"] = json!(mode);
    manifest["models"]["gadget"]["operations"]["delete"] = delete_operation();
    manifest
}

fn tombstone_catalog() -> CatalogIr {
    catalog_with_columns(&[
        ("deleted_at", SqlValueType::Timestamptz, true),
        ("deleted_by", SqlValueType::Uuid, true),
    ])
}

/// Authored commands declare their delete and every column that the SQL reads.
#[test]
fn authored_sql_deletes_only_from_a_hard_delete_model() {
    let authored = AuthoredSql::new(
        "query/quality_gadget_part.sql",
        b"WITH removed AS (DELETE FROM gadget WHERE id = $1 AND row_version = $2 RETURNING id) SELECT removed.id FROM removed;",
    );
    let mut operation = projection_operation();
    operation["type"] = json!("command");
    operation["transaction"] = json!("explicit_per_input");
    operation["automatic_retry"] = json!(false);
    operation["idempotent_by"] = json!({"state": {"guards": {"gadget": "expected_row_version"}}});
    let revision = json!({"path": "expected_row_version", "type": "int64", "nullable": false});
    operation["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(revision);
    operation["statements"]["load_gadget_part"]["parameters"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "expected_row_version", "type": "int64", "nullable": false}));
    let mut sources = QUERY_SOURCES.to_vec();
    sources.push(authored);

    let generate = |manifest: &Value, catalog: &CatalogIr, operation: &Value| {
        let mut with_operation = manifest.clone();
        with_operation["custom_operations"]["quality.load_gadget_part"] = operation.clone();
        run(catalog, &with_operation, &sources)
    };
    generate(&deleting_manifest("hard"), &catalog(false), &operation)
        .expect("an authored hard delete generates with its declared access");

    let marked = generate(
        &deleting_manifest("tombstone"),
        &tombstone_catalog(),
        &operation,
    )
    .unwrap_err()
    .to_string();
    assert!(marked.contains("delete_mode: tombstone"), "{marked}");
    let undeclared = generate(&manifest(), &catalog(false), &operation)
        .unwrap_err()
        .to_string();
    assert!(
        undeclared.contains("declares no delete_mode"),
        "{undeclared}"
    );

    let projection = projection_operation();
    let refusal = generate(&deleting_manifest("hard"), &catalog(false), &projection)
        .unwrap_err()
        .to_string();
    assert!(refusal.contains("must be read-only"), "{refusal}");

    // A delete with no column reads still declares relation access.
    let mut delete_only = deleting_manifest("hard");
    delete_only["custom_operations"]["quality.load_gadget_part"] = operation;
    sources.pop();
    sources.push(AuthoredSql::new(
        "query/quality_gadget_part.sql",
        b"WITH removed AS (DELETE FROM gadget) SELECT $1::uuid AS id WHERE $2::int8 IS NOT NULL;",
    ));
    run(&catalog(false), &delete_only, &sources).expect("delete alone is nonempty relation access");
}

#[test]
fn a_delete_mode_travels_with_its_owner_and_its_marker_columns() {
    let mut overlay = overlay_manifest();
    overlay["models"]["gadget"]["delete_mode"] = json!("tombstone");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&overlay))
            .expect_err("an overlay that declares a delete_mode")
            .error_type(),
        GenerateErrorType::InvalidModel,
    );

    for marker in ["deleted_at", "deleted_by"] {
        let ty = if marker == "deleted_at" {
            SqlValueType::Timestamptz
        } else {
            SqlValueType::Uuid
        };
        assert_eq!(
            run(
                &catalog_with_columns(&[(marker, ty, true)]),
                &manifest(),
                &QUERY_SOURCES,
            )
            .expect_err(marker)
            .to_string(),
            format!(
                "InvalidModel: gadget carries reserved column {marker} without a tombstone delete"
            ),
            "{marker}"
        );
    }
}

/// A catalog-free defect refuses in the shared vocabulary as well as in generation.
#[test]
fn audit_log_shape_refuses_without_a_catalog() {
    let mut missing = manifest();
    missing["models"]["gadget"]
        .as_object_mut()
        .unwrap()
        .remove("audit_log");
    let mut on_overlay = overlay_manifest();
    on_overlay["models"]["gadget"]["audit_log"] = json!({"columns": [], "retention": "none"});
    let cases = [
        ("a relation-owning model without the key", missing),
        ("an overlay model with the key", on_overlay),
        (
            "a repeated name",
            with_audit_log(
                &json!([
                    "created_at",
                    "created_at",
                    "created_by",
                    "updated_at",
                    "updated_by"
                ]),
                "none",
            ),
        ),
        (
            "created_by without created_at",
            with_audit_log(&json!(["created_by", "updated_at", "updated_by"]), "none"),
        ),
        (
            "updated_by without updated_at",
            with_audit_log(&json!(["created_at", "created_by", "updated_by"]), "none"),
        ),
    ];
    for (label, manifest) in cases {
        assert_eq!(
            validate_operation_vocabulary(&parsed_manifest(&manifest))
                .expect_err(label)
                .error_type(),
            GenerateErrorType::InvalidModel,
            "{label}"
        );
        assert_eq!(
            run(&all_stamps_catalog(), &manifest, &QUERY_SOURCES)
                .expect_err(label)
                .error_type(),
            GenerateErrorType::InvalidModel,
            "{label}"
        );
    }

    let outside = with_audit_log(&json!(["created_at", "deleted_at"]), "none");
    assert_eq!(
        run(&all_stamps_catalog(), &outside, &QUERY_SOURCES)
            .expect_err("a name outside the four refuses")
            .error_type(),
        GenerateErrorType::InvalidManifest
    );
}

/// A catalog defect names the column that the trigger cannot stamp.
#[test]
fn audit_log_columns_refuse_against_the_catalog() {
    let timestamps_only = with_audit_log(&json!(["created_at", "updated_at"]), "none");
    let mut overlay_added = overlay_manifest();
    overlay_added["models"]["gadget"]["field_owners"] = json!({"updated_by": "acme_inventory"});
    let cases = [
        (
            "an absent selected column",
            catalog(false),
            timestamps_only.clone(),
            "updated_at",
        ),
        (
            "a wrongly typed time",
            catalog_with_columns(&[("updated_at", SqlValueType::Text, false)]),
            timestamps_only.clone(),
            "updated_at",
        ),
        (
            "a wrongly typed actor",
            catalog_with_columns(&[("created_by", SqlValueType::Text, false)]),
            with_audit_log(&json!(["created_at", "created_by"]), "none"),
            "created_by",
        ),
        (
            "a nullable selected column",
            catalog_with_columns(&[("updated_at", SqlValueType::Timestamptz, true)]),
            timestamps_only.clone(),
            "updated_at",
        ),
        (
            "an unselected reserved-name column",
            all_stamps_catalog(),
            timestamps_only,
            "created_by",
        ),
        (
            "an off declaration over a reserved-name column",
            catalog(false),
            with_audit_log(&json!([]), "none"),
            "created_at",
        ),
        (
            "an overlay-added reserved-name column",
            catalog_with_columns(&[("updated_by", SqlValueType::Uuid, false)]),
            overlay_added,
            "updated_by",
        ),
    ];
    for (label, catalog, manifest, column) in cases {
        validate_operation_vocabulary(&parsed_manifest(&manifest))
            .unwrap_or_else(|error| panic!("{label} is a catalog defect: {error:?}"));
        let refusal = run(&catalog, &manifest, &QUERY_SOURCES).expect_err(label);
        assert_eq!(
            refusal.error_type(),
            GenerateErrorType::InvalidModel,
            "{label}"
        );
        let object = format!("inventory.gadget.{column}");
        assert_eq!(refusal.object(), Some(object.as_str()), "{label}");
    }

    let mut writable = all_stamps_manifest();
    writable["models"]["gadget"]["operations"]["update"]["writable_fields"] =
        json!(["stock_id", "updated_by"]);
    assert_eq!(
        run(&all_stamps_catalog(), &writable, &QUERY_SOURCES)
            .expect_err("a selected column declared writable refuses")
            .error_type(),
        GenerateErrorType::InvalidOperation
    );
}

// ---------------------------------------------------------------------------
// Record history, level 2: retention, history tables, and the log trigger.
// ---------------------------------------------------------------------------

const ALL_STAMP_COLUMNS: [&str; 4] = ["created_at", "created_by", "updated_at", "updated_by"];

/// Spec test 12: a retention is none, unlimited, or whole days, with or without stamp columns.
#[test]
fn audit_log_retention_is_none_unlimited_or_whole_days() {
    for retention in ["none", "unlimited", "P1D", "P30D", "P365D"] {
        for columns in [json!([]), json!(ALL_STAMP_COLUMNS)] {
            validate_operation_vocabulary(&parsed_manifest(&with_audit_log(&columns, retention)))
                .unwrap_or_else(|error| panic!("{retention} with {columns} refused: {error:?}"));
        }
        run(
            &all_stamps_catalog(),
            &with_audit_log(&json!(ALL_STAMP_COLUMNS), retention),
            &QUERY_SOURCES,
        )
        .unwrap_or_else(|error| panic!("{retention} did not generate: {error:?}"));
    }
    for retention in [
        "",
        "forever",
        "P0D",
        "P01D",
        "P-1D",
        "P+1D",
        "P1.5D",
        "P1W",
        "P1M",
        "P1Y",
        "PT24H",
        "P1DT1H",
        "p30d",
        "UNLIMITED",
    ] {
        let manifest = with_audit_log(&json!(ALL_STAMP_COLUMNS), retention);
        assert_eq!(
            validate_operation_vocabulary(&parsed_manifest(&manifest))
                .expect_err(retention)
                .error_type(),
            GenerateErrorType::InvalidModel,
            "{retention}"
        );
        assert_eq!(
            run(&all_stamps_catalog(), &manifest, &QUERY_SOURCES)
                .expect_err(retention)
                .error_type(),
            GenerateErrorType::InvalidModel,
            "{retention}"
        );
    }
}

/// A custom operation reads the declared history table.
#[test]
fn a_custom_operation_reads_a_history_table() {
    let mut manifest = with_audit_log(&json!(ALL_STAMP_COLUMNS), "unlimited");
    let mut operation = projection_operation();
    operation["relations"][0]["table"] = json!("gadget_history");
    operation["relations"][0]["select_fields"] = json!(["changed_by", "type"]);
    operation["statements"]["load_gadget_part"]["row"] =
        json!([{"name": "changed_by", "type": "uuid", "nullable": false}]);
    operation["result"]["fields"] =
        json!([{"path": "changed_by", "type": "uuid", "nullable": false}]);
    manifest["custom_operations"]["quality.load_gadget_part"] = operation;
    let mut sources = QUERY_SOURCES.to_vec();
    sources.push(AuthoredSql::new(
        "query/quality_gadget_part.sql",
        b"SELECT changed_by FROM gadget_history WHERE type = $1;\n",
    ));

    run(&all_stamps_catalog(), &manifest, &sources)
        .expect("a declared read of a history table generates");
    let unlogged = with_audit_log(&json!(ALL_STAMP_COLUMNS), "none");
    let mut reads_unlogged = unlogged;
    reads_unlogged["custom_operations"] = manifest["custom_operations"].clone();
    assert_eq!(
        run(&all_stamps_catalog(), &reads_unlogged, &sources)
            .expect_err("a relation that keeps no log has no history table to read")
            .error_type(),
        GenerateErrorType::UnknownRelation
    );
}

/// Spec test 17: generation refuses an authored history name and an overlong relation.
#[test]
fn history_names_are_reserved_and_fit_in_a_postgres_name() {
    let mut model_table = all_stamps_manifest();
    model_table["models"]["gadget"]["table"] = json!("gadget_history");
    let mut internal_table = manifest();
    internal_table["internal_relations"] =
        json!({"command": {"schema": "inventory", "table": "command_history", "cdc": "excluded"}});
    let mut internal_id = manifest();
    internal_id["internal_relations"] =
        json!({"command_history": {"schema": "inventory", "table": "command", "cdc": "excluded"}});
    let mut cases = vec![
        (
            "a model table",
            model_table,
            GenerateErrorType::InvalidManifest,
            "inventory.gadget_history",
        ),
        (
            "an internal relation table",
            internal_table,
            GenerateErrorType::InvalidManifest,
            "inventory.command_history",
        ),
        (
            "an internal relation id",
            internal_id,
            GenerateErrorType::InvalidManifest,
            "inventory.command",
        ),
    ];
    for (label, insert, update, lock, delete) in [
        (
            "a declared insert",
            json!(["type"]),
            json!([]),
            false,
            false,
        ),
        (
            "a declared update",
            json!([]),
            json!(["type"]),
            false,
            false,
        ),
        ("a declared row lock", json!([]), json!([]), true, false),
        ("a declared delete", json!([]), json!([]), false, true),
    ] {
        let mut writes = with_audit_log(&json!(ALL_STAMP_COLUMNS), "unlimited");
        // An event handler can write, so the refusal is the history reservation.
        writes["custom_operations"]["gadget.record_history"] = json!({
            "type": "event_handler",
            "visibility": "private",
            "connection": "postgres",
            "input": {"fields": [{"path": "new.id", "type": "uuid", "nullable": false}]},
            "errors": ["invalid_input", "retry", "timeout", "internal_error"],

            "relations": [{
                "schema": "inventory",
                "table": "gadget_history",
                "select_fields": [],
                "insert_fields": insert,
                "update_fields": update,
                "lock": lock,
                "delete": delete,
                "constraints": []
            }],
            "statements": {
                "write_history": {
                    "path": "command/record_history/write_history.sql",
                    "fetch": "optional_one",
                    "parameters": [{"name": "id", "type": "uuid", "nullable": false}],
                    "row": [{"name": "type", "type": "text", "nullable": false}]
                }
            },
            "registration": {
                "source_package": "platform_gadget",
                "entity": "gadget",
                "ops": ["insert"]
            }
        });
        cases.push((
            label,
            writes,
            GenerateErrorType::InvalidOperation,
            "inventory.gadget_history",
        ));
    }
    let long_relation = "r".repeat(32);
    let mut overlong = with_audit_log(&json!([]), "P30D");
    overlong["models"]["gadget"]["table"] = json!(long_relation);
    let long_object = format!("inventory.{long_relation}");
    cases.push((
        "a logged relation of 32 bytes",
        overlong,
        GenerateErrorType::InvalidModel,
        &long_object,
    ));
    for (label, manifest, type_, object) in cases {
        let refusal = validate_operation_vocabulary(&parsed_manifest(&manifest)).expect_err(label);
        assert_eq!(refusal.error_type(), type_, "{label}");
        assert_eq!(refusal.object(), Some(object), "{label}");
        assert_eq!(
            run(&all_stamps_catalog(), &manifest, &QUERY_SOURCES)
                .expect_err(label)
                .error_type(),
            type_,
            "{label}"
        );
    }

    let mut fits = with_audit_log(&json!([]), "P30D");
    fits["models"]["gadget"]["table"] = json!("r".repeat(31));
    validate_operation_vocabulary(&parsed_manifest(&fits))
        .expect("a logged relation of 31 bytes fits its history names");
    let mut unlogged_long = with_audit_log(&json!([]), "none");
    unlogged_long["models"]["gadget"]["table"] = json!("r".repeat(32));
    validate_operation_vocabulary(&parsed_manifest(&unlogged_long))
        .expect("a relation that keeps no log derives no history name");
}

/// A logged relation needs a primary key.
#[test]
fn a_logged_relation_needs_a_primary_key() {
    let logged = with_audit_log(&json!(ALL_STAMP_COLUMNS), "P30D");
    let stamped = all_stamps_catalog();
    let gadget = table(&stamped, "gadget");
    let keyless = replacing_table(
        &stamped,
        rebuilt_table(
            gadget,
            gadget.columns().to_vec(),
            gadget
                .constraints()
                .iter()
                .filter(|constraint| constraint.name() != "gadget_id_pkey")
                .cloned()
                .collect(),
        ),
    );
    let refusal = run(&keyless, &logged, &QUERY_SOURCES).expect_err("a keyless logged relation");
    assert_eq!(refusal.error_type(), GenerateErrorType::InvalidModel);
    assert_eq!(refusal.object(), Some("inventory.gadget"));
}

#[test]
fn integer_keys_use_typed_data_and_server_generated_create_ids() {
    use wamn_schema_introspection::ir::{ColumnGeneration, IdentityMode};

    let base = catalog(false);
    let gadget = table(&base, "gadget");
    for (ty, rust, default) in [
        (SqlValueType::Int32, "i32", ColumnDefault::int32(42)),
        (
            SqlValueType::Int64,
            "i64",
            ColumnDefault::int64(9_007_199_254_740_993),
        ),
    ] {
        for generation in [
            None,
            Some(ColumnGeneration::Identity {
                mode: IdentityMode::Always,
            }),
            Some(ColumnGeneration::Identity {
                mode: IdentityMode::ByDefault,
            }),
        ] {
            let columns = gadget
                .columns()
                .iter()
                .map(|column| {
                    if column.name() == "id" {
                        Column::new(
                            "id",
                            ty,
                            false,
                            generation.is_none().then(|| default.clone()),
                            generation.clone(),
                        )
                    } else {
                        column.clone()
                    }
                })
                .collect();
            let catalog = replacing_table(
                &base,
                rebuilt_table(gadget, columns, gadget.constraints().to_vec()),
            );
            let mut manifest = manifest();
            let model = &mut manifest["models"]["gadget"];
            model["server_owned_fields"]
                .as_array_mut()
                .unwrap()
                .retain(|field| field != "id");
            model["delete_mode"] = json!("hard");
            model["operations"]["query"]
                .as_object_mut()
                .unwrap()
                .remove("authored_sql");
            model["operations"]["create"] = json!({"permission": "gadget.create", "writable_fields": ["stock_id"], "result": "one"});
            model["operations"]["delete"] = json!({"permission": "gadget.delete", "revision_field": "row_version", "result": "one"});
            let package = run(&catalog, &manifest, &[]).unwrap();
            let data =
                std::str::from_utf8(package.file("generated/data/gadget.rs").unwrap().bytes())
                    .unwrap();
            assert_eq!(data.matches(&format!("id: {rust}")).count(), 3, "{data}");
            assert!(!data.contains("scalar::uuid(id)"));
            assert!(data.contains("cursor::id::encode_with_id("));
            assert!(data.contains(&format!("cursor::id::decode_with_id::<String, {rust}>")));
            let create = std::str::from_utf8(
                package
                    .file("generated/sql/gadget/create.sql")
                    .unwrap()
                    .bytes(),
            )
            .unwrap();
            let (insert, returning) = create.split_once("RETURNING").unwrap();
            assert_eq!(insert, "INSERT INTO gadget (stock_id)\nVALUES ($1::uuid)\n");
            assert!(returning.contains("\n    id,"), "{create}");
            let input = description_json(&package, "gadget.create", "input");
            assert!(input.get("id").is_none());
            assert_eq!(input["request"]["over"], "writable_fields");
            for declared in [false, true] {
                manifest["models"]["gadget"]["server_owned_fields"] =
                    if declared { json!(["id"]) } else { json!([]) };
                let package = run(&catalog, &manifest, &[])
                    .expect("the database owns a generated integer key");
                for action in ["create", "get", "update", "delete", "query"] {
                    let input = description_json(&package, &format!("gadget.{action}"), "input");
                    assert_eq!(
                        input["server_owned_fields"]["fields"]
                            .as_array()
                            .unwrap()
                            .contains(&json!("id")),
                        action == "create",
                        "{action}: {input}"
                    );
                    if matches!(action, "get" | "update" | "delete") {
                        assert_eq!(input["id"]["required"], true, "{action}: {input}");
                    }
                }
            }
            manifest["models"]["gadget"]["operations"]["create"]["writable_fields"] =
                json!(["id", "stock_id"]);
            let error = run(&catalog, &manifest, &[]).unwrap_err();
            assert!(error.context().contains("server-owned field id"), "{error}");
        }
    }
}

#[test]
fn generated_operations_require_a_supported_single_column_key() {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let manifest = manifest();
    for ty in [
        SqlValueType::Uuid,
        SqlValueType::Int32,
        SqlValueType::Int64,
        SqlValueType::Text,
        SqlValueType::Boolean,
        SqlValueType::Float64,
        SqlValueType::Bytes,
        SqlValueType::Numeric,
        SqlValueType::Timestamptz,
        SqlValueType::Json,
    ] {
        let columns = gadget
            .columns()
            .iter()
            .map(|column| {
                if column.name() == "id" {
                    Column::new("id", ty, false, None, None)
                } else {
                    column.clone()
                }
            })
            .collect();
        let catalog = replacing_table(
            &base,
            rebuilt_table(gadget, columns, gadget.constraints().to_vec()),
        );
        let result = run(&catalog, &manifest, &QUERY_SOURCES);
        if ty == SqlValueType::Uuid {
            result.unwrap();
        } else if matches!(ty, SqlValueType::Int32 | SqlValueType::Int64) {
            let error = result.unwrap_err();
            assert_eq!(error.error_type(), GenerateErrorType::InvalidModel);
            assert_eq!(error.object(), Some("inventory.gadget.id"));
            assert_eq!(
                error.context(),
                "inventory.gadget.id integer key must be GENERATED AS IDENTITY or have a sequence DEFAULT"
            );
        } else {
            let error = result.unwrap_err();
            assert_eq!(error.error_type(), GenerateErrorType::InvalidModel);
            assert_eq!(error.object(), Some("inventory.gadget.id"));
            assert!(error.context().contains("unsupported primary key type"));
            assert!(error.context().contains(ty.as_str()));
        }
    }
    for keys in [vec![], vec!["id", "stock_id"], vec!["stock_id"]] {
        let mut constraints = gadget
            .constraints()
            .iter()
            .filter(|constraint| constraint.name() != "gadget_id_pkey")
            .cloned()
            .collect::<Vec<_>>();
        if !keys.is_empty() {
            constraints.push(Constraint::primary_key("gadget_id_pkey", keys).unwrap());
        }
        let catalog = replacing_table(
            &base,
            rebuilt_table(gadget, gadget.columns().to_vec(), constraints),
        );
        let error = run(&catalog, &manifest, &QUERY_SOURCES).unwrap_err();
        assert_eq!(error.error_type(), GenerateErrorType::InvalidModel);
        assert_eq!(error.object(), Some("inventory.gadget"));
        assert!(
            error
                .context()
                .contains("single-column primary key named id")
        );
    }
}

#[test]
fn package_id_wamn_is_reserved_for_the_platform() {
    let mut reserved = manifest();
    reserved["package"]["id"] = json!("wamn");
    assert_eq!(
        validate_operation_vocabulary(&parsed_manifest(&reserved))
            .expect_err("package id wamn was accepted")
            .error_type(),
        GenerateErrorType::InvalidIdentity
    );
    assert_eq!(
        run(&catalog(false), &reserved, &QUERY_SOURCES)
            .expect_err("generation accepted package id wamn")
            .error_type(),
        GenerateErrorType::InvalidIdentity
    );
}

fn feed_manifest() -> Value {
    let mut value = manifest();
    let model = &mut value["models"]["gadget"];
    model["operations"]["query"]
        .as_object_mut()
        .unwrap()
        .remove("feed");
    model["audit_log"]["retention"] = json!("unlimited");
    model["operations"]["query"]
        .as_object_mut()
        .unwrap()
        .remove("authored_sql");
    value
}

#[test]
fn a_feed_accepts_an_int64_write_revision() {
    run(&catalog(false), &feed_manifest(), &[]).unwrap();
}

#[test]
fn a_feed_refuses_missing_history() {
    let mut value = feed_manifest();
    value["models"]["gadget"]["audit_log"]["retention"] = json!("none");
    let refusal = run(&catalog(false), &value, &[]).unwrap_err();
    assert_eq!(refusal.error_type(), GenerateErrorType::InvalidModel);
    assert_eq!(
        refusal.context(),
        "gadget feed needs a history table: declare audit_log.retention other than none"
    );
    value["models"]["gadget"]
        .as_object_mut()
        .unwrap()
        .remove("audit_log");
    let refusal = run(&catalog(false), &value, &[]).unwrap_err();
    assert!(refusal.context().contains("gadget"));
    assert!(refusal.context().contains("audit_log"));
}

#[test]
fn an_append_only_operation_refuses_a_revision_guard() {
    let mut value = manifest();
    value["models"]["gadget"]["operations"]["update"]["append_only"] = json!(true);
    let refusal = run(&catalog(false), &value, &QUERY_SOURCES).unwrap_err();
    assert_eq!(refusal.error_type(), GenerateErrorType::InvalidOperation);
    assert_eq!(
        refusal.context(),
        "gadget.update declares append_only = true with revision guard revision_field = row_version"
    );

    for revision in [json!(true), json!("gadget_id"), json!(false)] {
        let mut value = manifest();
        let mut operation = projection_operation();
        operation["type"] = json!("command");
        operation["transaction"] = json!("explicit_per_input");
        operation["automatic_retry"] = json!(false);
        operation["append_only"] = json!(true);
        operation["input"]["fields"][0]["references"] = json!({"model": "gadget"});
        operation["idempotent_by"] =
            json!({"state": {"guards": {"gadget": "expected_row_version"}}});
        operation["input"]["fields"].as_array_mut().unwrap().push(json!({
            "path": "expected_row_version", "type": "int64", "nullable": false, "revision": revision
        }));
        value["custom_operations"]["quality.load_gadget_part"] = operation;
        let mut sources = QUERY_SOURCES.to_vec();
        sources.push(AuthoredSql::new(
            "query/quality_gadget_part.sql",
            b"SELECT id FROM gadget WHERE id = $1;",
        ));
        let refusal = run(&catalog(false), &value, &sources).unwrap_err();
        assert_eq!(refusal.error_type(), GenerateErrorType::InvalidOperation);
        assert_eq!(
            refusal.context(),
            "quality.load_gadget_part declares append_only = true with revision guard expected_row_version"
        );
    }
}

#[test]
fn a_feed_refuses_authored_list_sql() {
    for explicit in [false, true] {
        let mut value = manifest();
        if explicit {
            value["models"]["gadget"]["operations"]["query"]["feed"] = json!(true);
        } else {
            value["models"]["gadget"]["operations"]["query"]
                .as_object_mut()
                .unwrap()
                .remove("feed");
        }
        let refusal = run(&catalog(false), &value, &QUERY_SOURCES).unwrap_err();
        assert_eq!(refusal.error_type(), GenerateErrorType::InvalidModel);
        assert_eq!(
            refusal.context(),
            "gadget authored list SQL cannot carry a feed"
        );
    }
}

#[test]
fn an_authored_list_defaults_to_no_feed_and_refuses_a_table() {
    let mut value = manifest();
    let mut operation = projection_operation();
    operation["result"]["class"] = json!("bounded_list");
    operation["lists"] = json!({"model": "gadget", "key_field": "id"});
    operation["statements"]["load_gadget_part"]["fetch"] = json!("bounded_list");
    value["custom_operations"]["quality.load_gadget_part"] = operation;
    let mut sources = QUERY_SOURCES.to_vec();
    sources.push(AuthoredSql::new(
        "query/quality_gadget_part.sql",
        b"SELECT id FROM gadget WHERE id = $1;",
    ));
    let default = run(&catalog(false), &value, &sources).unwrap();
    value["custom_operations"]["quality.load_gadget_part"]["feed"] = json!(false);
    assert_eq!(default, run(&catalog(false), &value, &sources).unwrap());
    value["custom_operations"]["quality.load_gadget_part"]["feed"] = json!(true);
    let refusal = run(&catalog(false), &value, &sources).unwrap_err();
    assert_eq!(refusal.error_type(), GenerateErrorType::InvalidOperation);
    assert_eq!(
        refusal.context(),
        "quality.load_gadget_part feed: relation gadget is not an authored view"
    );
}

#[test]
fn a_model_without_a_list_read_has_no_feed() {
    let mut value = feed_manifest();
    value["models"]["gadget"]["operations"]
        .as_object_mut()
        .unwrap()
        .remove("query");
    run(&catalog(false), &value, &[]).expect("no list read means no default feed");
    assert!(!parsed_manifest(&value).models["gadget"].feed_enabled());
}

#[test]
fn the_model_level_feed_is_refused() {
    let mut value = feed_manifest();
    value["models"]["gadget"]["feed"] = json!(false);
    let refusal = run(&catalog(false), &value, &[]).unwrap_err();
    assert_eq!(refusal.error_type(), GenerateErrorType::InvalidManifest);
}

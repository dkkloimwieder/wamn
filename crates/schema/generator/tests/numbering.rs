//! Numbering and caller-filled columns
//! (docs/architecture/data-access.md#declaration): build refusals and generated
//! SQL executed in PostgreSQL.

use serde_json::{Value, json};
use wamn_compiler::GeneratedPackage;
use wamn_compiler::ManifestErrorType as GenerateErrorType;
use wamn_scalar::SqlValueType;
use wamn_schema_introspection::ir::{CatalogIr, Column, Constraint};

#[path = "support/generation.rs"]
#[expect(
    dead_code,
    reason = "the shared support module serves generation.rs too"
)]
mod support;
use support::{
    QUERY_SOURCES, catalog, manifest, object_named, rebuilt_table, replacing_table, run,
    run_with_bases, table,
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
fn actor_catalog(name: &str, ty: SqlValueType) -> CatalogIr {
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
        columns.push(Column::new(name, SqlValueType::Uuid, false, None, None));
    }
    columns.push(Column::new(
        "updated_at",
        SqlValueType::Timestamptz,
        false,
        None,
        None,
    ));
    replacing_table(
        &base,
        rebuilt_table(gadget, columns, gadget.constraints().to_vec()),
    )
}

/// The gadget catalog with an integer identity key.
fn integer_key_catalog() -> CatalogIr {
    let base = catalog(false);
    let gadget = table(&base, "gadget");
    let columns = gadget
        .columns()
        .iter()
        .map(|column| {
            if column.name() == "id" {
                Column::new(
                    "id",
                    SqlValueType::Int32,
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
fn a_create_that_supplies_a_number_field_is_refused() {
    let mut manifest = numbered();
    manifest["models"]["gadget"]["server_owned_fields"] =
        json!(["status", "row_version", "created_at"]);
    manifest["models"]["gadget"]["operations"]["create"]["writable_fields"] =
        json!(["stock_id", "part_code"]);
    let (type_, message) = refusal(&catalog(false), &manifest);
    assert_eq!(type_, GenerateErrorType::InvalidOperation, "{message}");
}

#[test]
fn a_command_that_writes_an_actor_field_is_refused() {
    let mut manifest = manifest();
    manifest["models"]["gadget"]["fields"] = json!({"updated_by": {"role": "actor"}});
    manifest["models"]["gadget"]["audit_log"]["columns"] =
        json!(["created_at", "created_by", "updated_at", "updated_by"]);
    let mut command = support::projection_operation();
    command["type"] = json!("command");
    command["permission"] = json!("quality.inspect_gadget");
    command["idempotent_by"] = json!("claim");
    command["input"]["fields"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path": "idempotency_key", "type": "text", "nullable": false}));
    command["transaction"] = json!("explicit_per_input");
    command["automatic_retry"] = json!(false);
    command["relations"][0]["update_fields"] = json!(["updated_by"]);
    manifest["custom_operations"] = json!({"quality.inspect_gadget": command});
    let (type_, message) = refusal(&stamped_catalog(), &manifest);
    assert_eq!(type_, GenerateErrorType::InvalidOperation, "{message}");
}

#[test]
fn per_on_the_key_field_is_refused() {
    let mut manifest = numbered();
    manifest["sequences"]["gadget_number"]["pattern"] = json!("G{seq:n,per=id}");
    manifest["models"]["gadget"]["operations"]["create"]["writable_fields"] = json!(["stock_id"]);
    let (type_, _) = refusal(&catalog(false), &manifest);
    assert_eq!(type_, GenerateErrorType::InvalidModel);
}

#[test]
fn reset_or_per_in_a_sequence_bound_to_the_key_is_refused() {
    for pattern in ["{seq:n,reset=stock_id}", "{seq:n,per=stock_id}"] {
        let mut manifest = numbered();
        manifest["sequences"]["gadget_number"]["pattern"] = json!(pattern);
        manifest["models"]["gadget"]["fields"] = json!({"id": {"number": "gadget_number"}});
        let (type_, _) = refusal(&catalog(false), &manifest);
        assert_eq!(type_, GenerateErrorType::InvalidModel, "{pattern}");
    }
}

#[test]
fn an_integer_key_cannot_be_numbered_even_with_a_bare_counter() {
    for pattern in ["G{seq:n}", "{seq:n}"] {
        let mut manifest = numbered();
        manifest["sequences"]["gadget_number"]["pattern"] = json!(pattern);
        manifest["models"]["gadget"]["fields"] = json!({"id": {"number": "gadget_number"}});
        let create = manifest["models"]["gadget"]["operations"]["create"].clone();
        manifest["models"]["gadget"]["operations"] = json!({"create": create});
        let (error_type, message) = refusal(&integer_key_catalog(), &manifest);
        assert_eq!(error_type, GenerateErrorType::InvalidModel, "{message}");
    }
}

/// A model's numbered field needs a field of the model for each key its
/// pattern names, because the generated insert passes the model's fields as
/// the keys.
#[test]
fn a_numbered_field_whose_model_lacks_a_key_of_the_pattern_is_refused() {
    let mut manifest = numbered();
    manifest["sequences"]["gadget_number"]["pattern"] = json!("{seq:n,reset=facility}");
    let (type_, _) = refusal(&catalog(false), &manifest);
    assert_eq!(type_, GenerateErrorType::InvalidModel);
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
    let (type_, _) = refusal(&catalog(false), &manifest);
    assert_eq!(type_, GenerateErrorType::InvalidManifest);
}

#[test]
fn a_model_on_a_local_target_is_refused() {
    let mut manifest = manifest();
    manifest["targets"] = json!(["cloud", "local"]);
    let (type_, _) = refusal(&catalog(false), &manifest);
    assert_eq!(type_, GenerateErrorType::InvalidManifest);

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
    for ty in [SqlValueType::Int32, SqlValueType::Int64] {
        let catalog = actor_catalog("line_no", ty);
        for pattern in ["{seq:n,width=4}", "L{seq:n}", "{seq:n,reset=stock_id}"] {
            manifest["sequences"]["gadget_number"]["pattern"] = json!(pattern);
            let (type_, message) = refusal(&catalog, &manifest);
            assert_eq!(
                type_,
                GenerateErrorType::InvalidModel,
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
    manifest["models"]["gadget"]["fields"] = json!({"validated_by": {"role": "actor"}});
    let (type_, message) = refusal(
        &actor_catalog("validated_by", SqlValueType::Uuid),
        &manifest,
    );
    assert_eq!(type_, GenerateErrorType::InvalidModel, "{message}");
    // The audit log refuses a text created_by that it selects. An overlay
    // selects no base column, so the actor rule refuses it.
    let mut text_actor = overlay(manifest.clone());
    text_actor["models"]["gadget"]["fields"] = json!({"created_by": {"role": "actor"}});
    let mut base = manifest.clone();
    base["models"]["gadget"]["fields"] = text_actor["models"]["gadget"]["fields"].clone();
    let (type_, message) = refusal_with_base(
        &actor_catalog("created_by", SqlValueType::Text),
        &text_actor,
        &base,
    );
    assert_eq!(type_, GenerateErrorType::InvalidModel, "{message}");

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
fn an_overlay_is_validated_against_the_relation_owners_fields() {
    let note = actor_catalog("note", SqlValueType::Text);
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
    let (type_, message) = refusal_with_base(&note, &own_number, &base);
    assert_eq!(type_, GenerateErrorType::InvalidModel, "{message}");

    let mut own_key = numbered;
    own_key["sequences"]["gadget_number"]["pattern"] = json!("{field:note}-{seq:n,reset=note}");
    own_key["models"]["gadget"]["operations"]["create"]["writable_fields"] =
        json!(["stock_id", "note"]);
    base["sequences"] = own_key["sequences"].clone();
    let (type_, message) = refusal_with_base(&note, &own_key, &base);
    assert_eq!(type_, GenerateErrorType::InvalidModel, "{message}");

    // The owner's actor field is the overlay's to stamp.
    let mut base_actor = overlay(manifest());
    base_actor["models"]["gadget"]["fields"] = json!({"updated_by": {"role": "actor"}});
    base["models"]["gadget"]["fields"] = base_actor["models"]["gadget"]["fields"].clone();
    run_with_bases(
        &actor_catalog("updated_by", SqlValueType::Uuid),
        &base_actor,
        &QUERY_SOURCES,
        &[base],
    )
    .unwrap();
}

#[test]
fn numbered_fields_accept_only_text_int32_and_int64() {
    for ty in [
        SqlValueType::Boolean,
        SqlValueType::Float64,
        SqlValueType::Bytes,
        SqlValueType::Numeric,
        SqlValueType::Timestamptz,
        SqlValueType::Json,
        SqlValueType::Uuid,
        SqlValueType::Text,
        SqlValueType::Int32,
        SqlValueType::Int64,
    ] {
        let mut manifest = numbered();
        manifest["models"]["gadget"]["fields"] = json!({"serial": {"number": "gadget_number"}});
        manifest["models"]["gadget"]["server_owned_fields"]
            .as_array_mut()
            .unwrap()
            .push(json!("serial"));
        manifest["sequences"]["gadget_number"]["pattern"] = json!("{seq:n}");
        let catalog = actor_catalog("serial", ty);
        if matches!(
            ty,
            SqlValueType::Text | SqlValueType::Int32 | SqlValueType::Int64
        ) {
            run(&catalog, &manifest, &QUERY_SOURCES).unwrap();
        } else {
            let (type_, message) = refusal(&catalog, &manifest);
            assert_eq!(type_, GenerateErrorType::InvalidModel, "{message}");
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
fn an_overlay_cannot_supply_inherited_number_or_actor_fields() {
    let base = stamped_numbered();
    for action in ["create", "update"] {
        for field in ["part_code", "created_by", "updated_by"] {
            let mut overlay = undeclared_overlay(&base);
            overlay["models"]["gadget"]["operations"][action]["writable_fields"]
                .as_array_mut()
                .unwrap()
                .push(json!(field));
            let (type_, message) = refusal_with_base(&stamped_catalog(), &overlay, &base);
            assert_eq!(type_, GenerateErrorType::InvalidOperation, "{message}");
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
        let (type_, message) = refusal_with_base(&stamped_catalog(), &overlay, &base);
        assert_eq!(type_, GenerateErrorType::InvalidModel, "{message}");
    }
    let mut overlay = overlay(base.clone());
    overlay["sequences"]["gadget_number"]["pattern"] = json!("OTHER-{seq:n}");
    refusal_with_base(&stamped_catalog(), &overlay, &base);
}

#[test]
fn an_overlay_requires_its_declared_owner_and_relation() {
    let base = stamped_numbered();
    let overlay = undeclared_overlay(&base);
    run_with_bases(&stamped_catalog(), &overlay, &QUERY_SOURCES, &[]).unwrap_err();
    let mut wrong_version = base.clone();
    wrong_version["package"]["version"] = json!("9.0.0");
    refusal_with_base(&stamped_catalog(), &overlay, &wrong_version);
    let mut wrong_relation = base;
    wrong_relation["models"]["gadget"]["table"] = json!("other_table");
    refusal_with_base(&stamped_catalog(), &overlay, &wrong_relation);
}

#[test]
fn an_overlay_command_cannot_write_inherited_number_or_actor_fields() {
    let base = stamped_numbered();
    for field in ["part_code", "created_by", "updated_by"] {
        let mut overlay = undeclared_overlay(&base);
        let mut command = support::projection_operation();
        command["type"] = json!("command");
        command["permission"] = json!("quality.inspect_gadget");
        command["idempotent_by"] = json!("claim");
        command["input"]["fields"]
            .as_array_mut()
            .unwrap()
            .push(json!({"path": "idempotency_key", "type": "text", "nullable": false}));
        command["transaction"] = json!("explicit_per_input");
        command["automatic_retry"] = json!(false);
        command["relations"][0]["update_fields"] = json!([field]);
        overlay["custom_operations"] = json!({"quality.inspect_gadget": command});
        let (type_, message) = refusal_with_base(&stamped_catalog(), &overlay, &base);
        assert_eq!(type_, GenerateErrorType::InvalidOperation, "{message}");
    }
}

/// Run the emitted statements under their emitted column grants, including
/// an overlay that omits the owner's field declarations.
#[tokio::test]
async fn live_generated_inserts_and_updates_stamp_only_changed_rows() {
    let _lock = wamn_test_postgres::lock();
    let base = stamped_numbered();
    for manifest in [
        base.clone(),
        undeclared_overlay(&base),
        overlay(base.clone()),
    ] {
        let package = run_with_bases(
            &stamped_catalog(),
            &manifest,
            &QUERY_SOURCES,
            std::slice::from_ref(&base),
        )
        .unwrap();
        let mut database_server_database = wamn_test_postgres::start(&[]).unwrap();
        let database = url::Url::parse(
            database_server_database
                .create_database("fixture")
                .await
                .unwrap()
                .url(),
        )
        .unwrap();
        wamn_catalog::test_database::project(database.as_str()).await;
        sqlx::raw_sql("CREATE SCHEMA inventory; CREATE TABLE inventory.gadget (
            id uuid PRIMARY KEY DEFAULT gen_random_uuid(), part_code text UNIQUE NOT NULL,
            stock_id uuid NOT NULL, status text NOT NULL DEFAULT 'open',
            row_version bigint NOT NULL DEFAULT 1, created_at timestamptz NOT NULL DEFAULT now(),
            created_by uuid NOT NULL, updated_by uuid NOT NULL, updated_at timestamptz NOT NULL DEFAULT now());
            GRANT USAGE ON SCHEMA inventory TO wamn_app;
            GRANT SELECT ON inventory.gadget TO wamn_app;").execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap()).await.unwrap();
        sqlx::raw_sql("GRANT INSERT, UPDATE ON inventory.gadget TO wamn_app")
            .execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
            .await
            .unwrap();
        let first = "00000000-0000-4000-8000-000000000001";
        let second = "00000000-0000-4000-8000-000000000002";
        let stock = "00000000-0000-4000-8000-000000000003";
        let id = "00000000-0000-4000-8000-000000000004";
        let create = text(&package, "generated/sql/gadget/create.sql");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SET ROLE wamn_app; SET search_path TO inventory, pg_catalog;
            SET app.user_id = '{first}'; PREPARE create_gadget AS {create}
            EXECUTE create_gadget('{id}', '{stock}', 'G-1');"
        )))
        .execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
        .await
        .unwrap();
        let mut observer = wamn_postgres::connect(database.as_str()).await.unwrap();
        let row: uuid::Uuid = sqlx::query_scalar("SELECT id FROM inventory.gadget")
            .fetch_one(&mut observer)
            .await
            .unwrap();
        assert_eq!(row, uuid::Uuid::parse_str(id).unwrap());
        let update = text(&package, "generated/sql/gadget/update.sql");
        let actors = async || -> (uuid::Uuid, uuid::Uuid) {
            sqlx::query_as("SELECT created_by, updated_by FROM inventory.gadget")
                .fetch_one(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
                .await
                .unwrap()
        };
        let first_actor = uuid::Uuid::parse_str(first).unwrap();
        assert_eq!(actors().await, (first_actor, first_actor));
        // Both an omitted field and the same supplied value leave the actor alone.
        for (present, value) in [(false, stock), (true, stock), (true, second)] {
            let revision: i64 = sqlx::query_scalar("SELECT row_version FROM inventory.gadget")
                .fetch_one(&mut observer)
                .await
                .unwrap();
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "SET ROLE wamn_app; SET search_path TO inventory, pg_catalog;
                SET app.user_id = '{second}'; PREPARE update_gadget AS {update}
                EXECUTE update_gadget('{id}', {}, {present}, '{value}');",
                revision
            )))
            .execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
            .await
            .unwrap();
            assert_eq!(
                actors().await,
                (
                    first_actor,
                    uuid::Uuid::parse_str(if value == stock { first } else { second }).unwrap()
                )
            );
        }
    }
}

/// PostgreSQL accepts the generated cast of a rendered number into an integer field.
#[sqlx::test(migrations = false)]
async fn live_a_numbered_integer_field_inserts_the_rendered_number(pool: sqlx::PgPool) {
    let _lock = wamn_test_postgres::lock();
    let database = wamn_test_postgres::url(&pool);
    wamn_catalog::test_database::project(database.as_str()).await;
    let mut manifest = numbered();
    manifest["sequences"]["gadget_number"]["pattern"] = json!("{seq:n}");
    manifest["models"]["gadget"]["fields"] = json!({"line_no": {"number": "gadget_number"}});
    manifest["models"]["gadget"]["server_owned_fields"]
        .as_array_mut()
        .unwrap()
        .push(json!("line_no"));
    let create = manifest["models"]["gadget"]["operations"]["create"].clone();
    manifest["models"]["gadget"]["operations"] = json!({"create": create});
    let package = run(
        &actor_catalog("line_no", SqlValueType::Int32),
        &manifest,
        &[],
    )
    .unwrap();
    sqlx::raw_sql("CREATE TABLE public.gadget (id uuid PRIMARY KEY, line_no int NOT NULL, part_code text DEFAULT 'fixture',
        stock_id uuid NOT NULL, status text DEFAULT 'open', row_version bigint DEFAULT 1,
        created_at timestamptz DEFAULT now()); GRANT SELECT, INSERT ON public.gadget TO wamn_app;").execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap()).await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "SET ROLE wamn_app; PREPARE create_gadget AS {}
        EXECUTE create_gadget('00000000-0000-4000-8000-000000000004', '00000000-0000-4000-8000-000000000003', '42');",
            text(&package, "generated/sql/gadget/create.sql")
        ))).execute(&mut wamn_postgres::connect(database.as_str()).await.unwrap()).await
        .unwrap();
    let (id, number): (uuid::Uuid, i32) = sqlx::query_as("SELECT id, line_no FROM public.gadget")
        .fetch_one(&mut wamn_postgres::connect(database.as_str()).await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        id,
        uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000004").unwrap()
    );
    assert_eq!(number, 42);
}

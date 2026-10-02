//! The compile step of `wamn.k` (docs/plan/manifest-authoring.md §4.3).
//!
//! Each test writes a package with a `wamn.k` to its own directory and runs the
//! pinned `kcl` CLI that `tools/install-kcl` installs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use wamn_schema_generator::{
    COMPILED_MANIFEST, check_compiled_manifest, compile_manifest, package_manifest_path,
};

struct Package(PathBuf);

impl Package {
    fn new(source: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "wamn-manifest-authoring-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).expect("create the package");
        std::fs::write(root.join("wamn.k"), source).expect("write wamn.k");
        Self(root)
    }

    fn root(&self) -> &Path {
        &self.0
    }

    fn compile(&self) -> Value {
        let bytes = compile_manifest(self.root()).expect("compile wamn.k");
        serde_json::from_slice(&bytes).expect("compiled JSON")
    }

    fn refusal(&self) -> String {
        format!(
            "{:#}",
            compile_manifest(self.root()).expect_err("wamn.k must not compile")
        )
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A package with one model and one command. `{model}` and `{command}` stand
/// for extra lines of each.
fn source(model: &str, command: &str) -> String {
    format!(
        r#"import manifest

manifest.Package {{
    package = {{id = "fixture", version = "1.0.0"}}
    required_platform_policy_contract = {{id = "fixture_data_access", state = "satisfied"}}
    models = {{
        widget = manifest.Model("widget", "fixture") {{
            table = "widget"
            owner = "fixture"
            audit_log = {{columns = ["created_at"]}}
            operations: {{
                get: {{result = "one", revision_field = "row_version"}}
                query: {{result = "page"}}
            }}
{model}
        }}
    }}
    custom_operations = {{
        "widget.archive" = manifest.Command("widget.archive") {{
            input.fields = [manifest.request_id, {{path = "id", type = "uuid"}}]
            result = {{class = "one", fields = [{{path = "id", type = "uuid"}}]}}
            errors = manifest.platform_errors
            idempotent_by = "claim"
            relations = [{{
                $schema = "fixture"
                table = "widget"
                select_fields = ["id"]
                insert_fields = []
                update_fields = []
                lock = True
            }}]
            statements = {{
                archive = manifest.Statement("command/widget_archive", "archive") {{
                    fetch = "one"
                    parameters = [manifest.uuid("id")]
                    row = [manifest.nullable(manifest.uuid("id"))]
                }}
            }}
{command}
        }}
    }}
    connections = ["postgres"]
    components = {{fixture = {{connections = ["postgres"]}}}}
}}
"#
    )
}

#[test]
fn each_authoring_default_fills() {
    let package = Package::new(&source("", ""));
    let manifest = package.compile();
    let widget = &manifest["models"]["widget"];
    assert_eq!(widget["schema"], "fixture");
    assert_eq!(widget["operations"]["get"]["permission"], "widget.get");
    assert_eq!(widget["operations"]["query"]["permission"], "widget.query");
    assert_eq!(
        widget["operations"]["query"]["pagination"],
        json!({"default_sort": {"field": "created_at", "direction": "ascending"}, "tie_breaker": {"field": "id"}})
    );
    assert_eq!(
        widget["operations"]["query"]["limit"],
        json!({"default": 100, "minimum": 1, "maximum": 100})
    );
    assert_eq!(widget["audit_log"]["retention"], "none");

    let archive = &manifest["custom_operations"]["widget.archive"];
    assert_eq!(archive["type"], "command");
    assert_eq!(archive["permission"], "widget.archive");
    assert_eq!(archive["visibility"], "public");
    assert_eq!(archive["connection"], "postgres");
    assert_eq!(archive["transaction"], "explicit_per_input");
    assert_eq!(archive["automatic_retry"], false);
    assert_eq!(
        archive["input"]["fields"][0],
        json!({"path": "request_id", "type": "text", "nullable": false})
    );
    assert_eq!(archive["input"]["fields"][1]["nullable"], false);
    assert_eq!(
        archive["errors"],
        json!([
            "invalid_input",
            "concurrency_conflict",
            "idempotency_conflict",
            "retry",
            "timeout",
            "permission_denied",
            "internal_error"
        ])
    );
    let statement = &archive["statements"]["archive"];
    assert_eq!(statement["path"], "command/widget_archive/archive.sql");
    assert_eq!(
        statement["parameters"],
        json!([{"name": "id", "type": "uuid", "nullable": false}])
    );
    assert_eq!(
        statement["row"],
        json!([{"name": "id", "type": "uuid", "nullable": true}])
    );
}

#[test]
fn an_authored_value_replaces_its_default() {
    let package = Package::new(&source(
        r#"            operations: {get: {permission = "widget.read"}}"#,
        r#"            visibility = "private"
            automatic_retry = True"#,
    ));
    let manifest = package.compile();
    assert_eq!(
        manifest["models"]["widget"]["operations"]["get"]["permission"],
        "widget.read"
    );
    let archive = &manifest["custom_operations"]["widget.archive"];
    assert_eq!(archive["visibility"], "private");
    assert_eq!(archive["automatic_retry"], true);
}

#[test]
fn a_key_the_reader_defaults_stays_absent() {
    let package = Package::new(&source(
        "",
        r"            connection = None
            transaction = None",
    ));
    let manifest = package.compile();
    let archive = manifest["custom_operations"]["widget.archive"]
        .as_object()
        .expect("an operation object");
    // Optional keys the author did not write, and defaults the author set to
    // None, are absent from the compiled JSON.
    for key in [
        "connection",
        "transaction",
        "pre_commit",
        "participant",
        "label",
        "lists",
    ] {
        assert!(!archive.contains_key(key), "{key} must stay absent");
    }
    let widget = manifest["models"]["widget"].as_object().expect("a model");
    for key in ["client_field_extensible", "delete_mode", "field_text"] {
        assert!(!widget.contains_key(key), "{key} must stay absent");
    }
}

#[test]
fn a_wrong_enum_value_fails_at_compile_with_its_path() {
    for (model, command, wrong) in [
        (
            "",
            r#"            statements.archive.fetch = "several""#,
            "several",
        ),
        ("", r#"            transaction = "always""#, "always"),
        (
            r#"            operations: {query: {filters = [{field = "code", match = "fuzzy"}]}}"#,
            "",
            "fuzzy",
        ),
    ] {
        let package = Package::new(&source(model, command));
        let refusal = package.refusal();
        assert!(
            refusal.contains("wamn.k:") && refusal.contains(wrong),
            "the refusal names the file, line and value: {refusal}"
        );
    }
}

#[test]
fn an_operation_assigned_with_equals_is_refused() {
    let package = Package::new(&source("", "").replace(
        "            operations: {\n                get: {result = \"one\", revision_field = \"row_version\"}\n                query: {result = \"page\"}\n            }",
        "            operations = {get = {result = \"one\"}}",
    ));
    let refusal = package.refusal();
    assert!(
        refusal.contains("so that the model fills each permission"),
        "{refusal}"
    );
}

#[test]
fn a_hand_written_wamn_json_beside_wamn_k_is_refused() {
    let package = Package::new(&source("", ""));
    std::fs::write(package.root().join("wamn.json"), "{}").expect("write wamn.json");
    assert!(
        package
            .refusal()
            .contains("the manifest is authored in wamn.k; wamn.json is generated")
    );
}

#[test]
fn a_hand_edited_compiled_manifest_fails_the_check() {
    let package = Package::new(&source("", ""));
    let compiled = compile_manifest(package.root()).expect("compile wamn.k");
    assert_eq!(
        package_manifest_path(package.root()),
        package.root().join(COMPILED_MANIFEST)
    );
    let committed = package.root().join(COMPILED_MANIFEST);
    std::fs::create_dir_all(committed.parent().expect("generated/")).expect("create generated/");
    std::fs::write(&committed, &compiled).expect("write generated/wamn.json");
    check_compiled_manifest(package.root(), &compiled).expect("the committed bytes match");

    let edited = String::from_utf8(compiled.clone())
        .expect("UTF-8")
        .replace("\"widget.get\"", "\"widget.read\"");
    std::fs::write(&committed, edited).expect("edit generated/wamn.json");
    let refusal = format!(
        "{:#}",
        check_compiled_manifest(package.root(), &compiled).expect_err("an edit must fail")
    );
    assert!(
        refusal.ends_with("differs from wamn.k at .models.widget.operations.get.permission"),
        "{refusal}"
    );

    let reformatted =
        serde_json::to_vec(&serde_json::from_slice::<Value>(&compiled).expect("JSON"))
            .expect("serialize");
    std::fs::write(&committed, reformatted).expect("reformat generated/wamn.json");
    let refusal = format!(
        "{:#}",
        check_compiled_manifest(package.root(), &compiled).expect_err("a reformat must fail")
    );
    assert!(refusal.ends_with("in its formatting only"), "{refusal}");
}

#[test]
fn the_compiled_bytes_are_two_space_json_in_schema_order() {
    let package = Package::new(&source("", ""));
    let compiled =
        String::from_utf8(compile_manifest(package.root()).expect("compile")).expect("UTF-8");
    assert!(compiled.starts_with("{\n  \"package\": {\n    \"id\": \"fixture\",\n"));
    assert!(compiled.ends_with("}\n"));
    // The schema lists type, visibility and permission first.
    let archive = compiled.find("\"widget.archive\": {").expect("the command");
    let after = &compiled[archive..];
    let position = |key: &str| after.find(&format!("\"{key}\":")).expect(key);
    assert!(position("type") < position("visibility"));
    assert!(position("visibility") < position("permission"));
    assert!(position("permission") < position("input"));
}

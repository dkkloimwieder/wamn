//! The compile step of `wamn.k`
//! (docs/architecture/data-access.md#application-declarations).
//!
//! Each test writes a package with a `wamn.k` to its own directory and runs the
//! pinned `kcl` CLI that `cargo xtask install-kcl` installs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use wamn_compiler::compile_manifest;

struct Package(PathBuf);

impl Package {
    fn new(source: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        // The package lies in its own scratch directory, beside the
        // `target/wamn` directory that holds its build output.
        let root = std::env::temp_dir()
            .join(format!(
                "wamn-manifest-authoring-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .join("package");
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

    fn refusal(&self) {
        compile_manifest(self.root()).expect_err("wamn.k must not compile");
    }
}

impl Drop for Package {
    fn drop(&mut self) {
        if let Some(scratch) = self.0.parent() {
            let _ = std::fs::remove_dir_all(scratch);
        }
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
            input.fields = [manifest.request_id, manifest.idempotency_key, {{path = "id", type = "uuid"}}]
            result = {{class = "one", fields = [{{path = "id", type = "uuid"}}]}}
            errors = manifest.platform_errors
            idempotent_by = "claim"
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
    connections = {{}}
    components = {{fixture = {{connections = ["postgres"]}}}}
}}
"#
    )
}

#[test]
fn feed_and_append_only_declarations_round_trip() {
    let defaults = Package::new(&source("", "")).compile();
    let parsed = wamn_compiler::manifest::PackageManifest::from_slice(
        &serde_json::to_vec(&defaults).unwrap(),
    )
    .unwrap();
    assert!(parsed.models["widget"].feed_enabled());
    assert!(
        !parsed.models["widget"].operations[&wamn_compiler::manifest::CrudAction::Get].append_only
    );
    assert!(!parsed.custom_operations["widget.archive"].append_only);

    let explicit = Package::new(&source(
        "            operations: {query: {feed = False}, get: {append_only = True}}",
        "            append_only = True",
    ))
    .compile();
    let parsed = wamn_compiler::manifest::PackageManifest::from_slice(
        &serde_json::to_vec(&explicit).unwrap(),
    )
    .unwrap();
    assert!(!parsed.models["widget"].feed_enabled());
    assert!(
        parsed.models["widget"].operations[&wamn_compiler::manifest::CrudAction::Get].append_only
    );
    assert!(parsed.custom_operations["widget.archive"].append_only);
}

#[test]
fn authored_list_feed_declarations_round_trip() {
    for feed in ["", "feed = False", "feed = True"] {
        let read = format!(
            r#"    custom_operations = {{
        "widget.list" = manifest.Projection("widget.list") {{
            {feed}
            input.fields = []
            result = {{class = "bounded_list", fields = [{{path = "id", type = "uuid"}}]}}
            lists = {{model = "widget", key_field = "id"}}
            errors = manifest.platform_errors
            statements = {{
                list = manifest.Statement("query", "widget_list") {{
                    fetch = "bounded_list"
                    row = [manifest.uuid("id")]
                }}
            }}
        }}"#
        );
        let compiled =
            Package::new(&source("", "").replace("    custom_operations = {", &read)).compile();
        let parsed = wamn_compiler::manifest::PackageManifest::from_slice(
            &serde_json::to_vec(&compiled).unwrap(),
        )
        .unwrap();
        assert_eq!(
            parsed.custom_operations["widget.list"].feed,
            match feed {
                "" => None,
                "feed = False" => Some(false),
                _ => Some(true),
            }
        );
    }
}

#[test]
fn a_model_feed_declaration_is_refused() {
    Package::new(&source("            feed = False", "")).refusal();
}

#[test]
fn an_input_assigned_with_equals_is_refused() {
    let package = Package::new(&source("", "").replace(
        "            input.fields = [manifest.request_id, manifest.idempotency_key, {path = \"id\", type = \"uuid\"}]",
        "            input = {fields = [manifest.request_id, manifest.idempotency_key, {path = \"id\", type = \"uuid\"}]}",
    ));
    package.refusal();
}

#[test]
fn a_stated_stamp_column_is_refused() {
    let package = Package::new(&source(
        r#"            server_owned_fields = ["updated_by"]"#,
        "",
    ));
    package.refusal();
}

#[test]
fn a_derived_member_is_refused() {
    let relations = Package::new(&source(
        "",
        r#"            relations = [{$schema = "fixture", table = "widget"}]"#,
    ));
    relations.refusal();

    let conflict = Package::new(&source(
        "",
        r#"            error_details = {idempotency_conflict = {required = ["field"]}}"#,
    ));
    conflict.refusal();
}

/// The schema module lets a statement value leave out its type, which only
/// `wamn build` derives
/// (docs/architecture/data-access.md#application-declarations). KCL accepts it;
/// a reader that has no build to derive it refuses the compiled manifest.
#[test]
fn a_statement_value_type_is_optional_and_only_the_build_derives_it() {
    let package = Package::new(&source("", "").replace(
        "parameters = [manifest.uuid(\"id\")]",
        "parameters = [manifest.Value {name = \"id\"}]",
    ));
    package.refusal();
}

#[test]
fn a_wrong_enum_value_fails_at_compile_with_its_path() {
    for (model, command, _) in [
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
        package.refusal();
    }
}

#[test]
fn an_operation_assigned_with_equals_is_refused() {
    let package = Package::new(&source("", "").replace(
        "            operations: {\n                get: {result = \"one\", revision_field = \"row_version\"}\n                query: {result = \"page\"}\n            }",
        "            operations = {get = {result = \"one\"}}",
    ));
    package.refusal();
}

#[test]
fn a_hand_written_wamn_json_beside_wamn_k_is_refused() {
    let package = Package::new(&source("", ""));
    std::fs::write(package.root().join("wamn.json"), "{}").expect("write wamn.json");
    package.refusal();
}

#[test]
fn field_visibility_is_optional_and_boolean_for_models_and_custom_results() {
    let original = source("", "");
    let authored = source(
        r#"            field_text = {id = {visible = False}, code = {label = "Code", visible = True}}"#,
        "",
    ).replace(
        r#"result = {class = "one", fields = [{path = "id", type = "uuid"}]}"#,
        r#"result = {class = "one", fields = [{path = "id", type = "uuid", visible = True}]}"#,
    );
    let compiled = Package::new(&authored).compile();
    assert_eq!(
        compiled["models"]["widget"]["field_text"]["code"]["visible"],
        true
    );
    assert_eq!(
        compiled["models"]["widget"]["field_text"]["id"]["visible"],
        false
    );
    assert_eq!(
        compiled["custom_operations"]["widget.archive"]["result"]["fields"][0]["visible"],
        true
    );
    let typed = wamn_compiler::manifest::PackageManifest::from_slice(
        &serde_json::to_vec(&compiled).unwrap(),
    )
    .unwrap();
    assert_eq!(
        typed.models["widget"].field_text["code"].visible,
        Some(true)
    );
    assert_eq!(
        typed.custom_operations["widget.archive"]
            .result
            .as_ref()
            .unwrap()
            .fields[0]
            .visible,
        Some(true)
    );
    let omitted = Package::new(&original).compile();
    assert!(
        omitted["custom_operations"]["widget.archive"]["result"]["fields"][0]
            .get("visible")
            .is_none()
    );
    let invalid = authored.replace("visible = True", r#"visible = "yes""#);
    Package::new(&invalid).refusal();
}

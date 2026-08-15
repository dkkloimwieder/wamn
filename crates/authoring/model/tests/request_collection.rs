use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_authoring_model::{
    AuthoringDocument, AuthoringRequestEnvelope, SCHEMA_VERSION, decode_document,
};

const COMMANDS: [&str; 5] = [
    "save-flow-draft",
    "validate",
    "draft-run",
    "test-set-run",
    "publish",
];
const QUERIES: [&str; 3] = ["read-draft", "get-run", "get-report"];
const ENDPOINT_LINE: &str = "POST {{$processEnv WAMN_AUTHORING_ENDPOINT}}";
const AUTHORIZATION_LINE: &str =
    "Authorization: Bearer {{$processEnv WAMN_AUTHORING_BEARER_TOKEN}}";
const PRIVILEGED_FIELDS: [&str; 20] = [
    "access-token",
    "admin",
    "authorization",
    "bearer-token",
    "bundle",
    "credential",
    "credentials",
    "database",
    "database-url",
    "database_url",
    "dsn",
    "endpoint",
    "execution-bundle",
    "frontend-state",
    "operator",
    "principal",
    "shell-host",
    "superuser",
    "token",
    "trusted-context",
];

#[derive(Debug)]
struct HttpExample {
    name: String,
    document: Value,
}

fn contract_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../docs/archive/contracts")
        .join(name)
}

fn reject_privileged_fields(value: &Value, path: String) -> Result<(), String> {
    match value {
        Value::Object(fields) => {
            for (field, child) in fields {
                if PRIVILEGED_FIELDS.contains(&field.as_str()) {
                    return Err(format!(
                        "client example contains forbidden field {field:?} at {path}"
                    ));
                }
                reject_privileged_fields(child, format!("{path}.{field}"))?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                reject_privileged_fields(child, format!("{path}[{index}]"))?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

fn checked_document(value: &Value) -> Result<AuthoringDocument, String> {
    reject_privileged_fields(value, "$".to_owned())?;
    decode_document(&serde_json::to_string(value).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
}

fn http_examples(collection: &str) -> Result<Vec<HttpExample>, String> {
    let mut examples = Vec::new();
    for section in collection.split("\n### ").skip(1) {
        let (name, request) = section
            .split_once('\n')
            .ok_or_else(|| "request section has no header lines".to_owned())?;
        let (headers, body) = request
            .split_once("\n\n")
            .ok_or_else(|| format!("request {name:?} has no JSON body"))?;
        if headers.lines().next() != Some(ENDPOINT_LINE) {
            return Err(format!(
                "request {name:?} must use the caller-supplied endpoint"
            ));
        }
        let authorization = headers
            .lines()
            .filter(|line| {
                line.split_once(':')
                    .is_some_and(|(header, _)| header.eq_ignore_ascii_case("authorization"))
            })
            .collect::<Vec<_>>();
        if authorization != [AUTHORIZATION_LINE] {
            return Err(format!(
                "request {name:?} must require exactly one caller-supplied bearer token"
            ));
        }
        for required in ["Accept: application/json", "Content-Type: application/json"] {
            if !headers.lines().any(|line| line == required) {
                return Err(format!("request {name:?} is missing {required:?}"));
            }
        }
        let document: Value = serde_json::from_str(body.trim())
            .map_err(|error| format!("request {name:?} has invalid JSON: {error}"))?;
        checked_document(&document)?;
        examples.push(HttpExample {
            name: name.to_owned(),
            document,
        });
    }
    Ok(examples)
}

fn schema_variants(schema: &Value, definition: &str, discriminator: &str) -> BTreeSet<String> {
    schema["definitions"][definition]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("{definition} has no oneOf"))
        .iter()
        .map(|variant| {
            variant["properties"][discriminator]["enum"][0]
                .as_str()
                .unwrap_or_else(|| panic!("{definition} variant has no {discriminator}"))
                .to_owned()
        })
        .collect()
}

fn operation(document: &AuthoringDocument) -> &str {
    let AuthoringDocument::Request(request) = document else {
        panic!("collection contains a response")
    };
    match request.as_ref() {
        AuthoringRequestEnvelope::Command(request) => match &request.command {
            wamn_authoring_model::AuthoringCommand::SaveFlowDraft(_) => "save-flow-draft",
            wamn_authoring_model::AuthoringCommand::Validate(_) => "validate",
            wamn_authoring_model::AuthoringCommand::DraftRun(_) => "draft-run",
            wamn_authoring_model::AuthoringCommand::TestSetRun(_) => "test-set-run",
            wamn_authoring_model::AuthoringCommand::Publish(_) => "publish",
        },
        AuthoringRequestEnvelope::Query(request) => match &request.query {
            wamn_authoring_model::AuthoringQuery::ReadDraft(_) => "read-draft",
            wamn_authoring_model::AuthoringQuery::GetRun(_) => "get-run",
            wamn_authoring_model::AuthoringQuery::GetReport(_) => "get-report",
        },
    }
}

#[test]
fn collection_and_examples_cover_the_exact_public_surface() {
    let schema_text = std::fs::read_to_string(contract_path("authoring-surface.schema.json"))
        .expect("read public schema");
    let schema: Value = serde_json::from_str(&schema_text).expect("parse public schema");
    assert_eq!(schema, wamn_authoring_model::json_schema());

    let commands = COMMANDS
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let queries = QUERIES
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        schema_variants(&schema, "AuthoringCommand", "kind"),
        commands
    );
    assert_eq!(schema_variants(&schema, "AuthoringQuery", "kind"), queries);

    let collection = std::fs::read_to_string(contract_path("authoring-surface.v0.1.http"))
        .expect("read request collection");
    let examples = http_examples(&collection).expect("validate request collection");
    let expected = commands.union(&queries).cloned().collect::<BTreeSet<_>>();
    let observed = examples
        .iter()
        .map(|example| {
            let decoded = checked_document(&example.document).expect("decode example");
            let operation = operation(&decoded);
            assert_eq!(example.name, operation);
            operation.to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(observed, expected);

    let corpus: Value = serde_json::from_str(
        &std::fs::read_to_string(contract_path("authoring-surface.v0.1.examples.json"))
            .expect("read response examples"),
    )
    .expect("parse response examples");
    assert_eq!(corpus["schema-version"], SCHEMA_VERSION);
    let response_operations = corpus["examples"]
        .as_array()
        .expect("examples array")
        .iter()
        .map(|example| {
            let name = example["operation"].as_str().expect("operation name");
            checked_document(&example["success"]).expect("success decodes");
            checked_document(&example["refusal"]).expect("refusal decodes");
            name.to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(response_operations, expected);
}

#[test]
fn collection_rejects_auth_version_and_privilege_drift() {
    let collection = std::fs::read_to_string(contract_path("authoring-surface.v0.1.http"))
        .expect("read request collection");
    assert!(http_examples(&collection.replacen(AUTHORIZATION_LINE, "", 1)).is_err());

    let mut request = http_examples(&collection)
        .expect("valid collection")
        .remove(0)
        .document;
    request["body"]["schema-version"] = serde_json::json!("0.2");
    assert!(checked_document(&request).is_err());
    request["body"]["schema-version"] = serde_json::json!(SCHEMA_VERSION);
    request["body"]["command"]["input"]["database-url"] =
        serde_json::json!("postgresql://operator.invalid/platform");
    assert!(
        checked_document(&request)
            .expect_err("privileged field must fail")
            .contains("forbidden field")
    );
}

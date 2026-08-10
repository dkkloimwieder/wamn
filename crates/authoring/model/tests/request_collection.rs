use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::Value;
use wamn_authoring_model::{
    AuthoringCommand, AuthoringCommandKind, AuthoringDocument, AuthoringOutcome, AuthoringSuccess,
    SCHEMA_VERSION, decode_document,
};

const COMMANDS: [&str; 6] = [
    "save-flow-draft",
    "validate",
    "draft-run",
    "suite-run",
    "publish",
    "suite-projection",
];
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

fn checked_document(value: &Value) -> Result<AuthoringDocument, String> {
    reject_privileged_fields(value, "$".to_string())?;
    decode_document(&serde_json::to_string(value).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
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

fn http_examples(collection: &str) -> Result<Vec<HttpExample>, String> {
    let mut examples = Vec::new();
    for section in collection.split("\n### ").skip(1) {
        let (name, request) = section
            .split_once('\n')
            .ok_or_else(|| "request section has no header lines".to_string())?;
        let (headers, body) = request
            .split_once("\n\n")
            .ok_or_else(|| format!("request {name:?} has no JSON body"))?;
        if headers.lines().next() != Some(ENDPOINT_LINE) {
            return Err(format!(
                "request {name:?} must use the caller-supplied endpoint"
            ));
        }
        let authorization_lines: Vec<_> = headers
            .lines()
            .filter(|line| {
                line.split_once(':')
                    .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
            })
            .collect();
        if authorization_lines != [AUTHORIZATION_LINE] {
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
            name: name.to_string(),
            document,
        });
    }
    if examples.is_empty() {
        return Err("request collection has no examples".to_string());
    }
    Ok(examples)
}

fn schema_variants(schema: &Value, definition: &str, discriminator: &str) -> BTreeSet<String> {
    schema["definitions"][definition]["oneOf"]
        .as_array()
        .unwrap_or_else(|| panic!("schema definition {definition:?} has no oneOf variants"))
        .iter()
        .map(|variant| {
            variant["properties"][discriminator]["enum"][0]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "schema definition {definition:?} has no {discriminator:?} discriminator"
                    )
                })
                .to_string()
        })
        .collect()
}

fn request_kind(document: &AuthoringDocument) -> &'static str {
    let AuthoringDocument::Request(request) = document else {
        panic!("request collection decoded a response document")
    };
    match &request.command {
        AuthoringCommand::SaveFlowDraft(_) => "save-flow-draft",
        AuthoringCommand::Validate(_) => "validate",
        AuthoringCommand::DraftRun(_) => "draft-run",
        AuthoringCommand::SuiteRun(_) => "suite-run",
        AuthoringCommand::Publish(_) => "publish",
        AuthoringCommand::SuiteProjection(_) => "suite-projection",
    }
}

fn success_kind(success: &AuthoringSuccess) -> &'static str {
    match success {
        AuthoringSuccess::SaveFlowDraft(_) => "save-flow-draft",
        AuthoringSuccess::Validate(_) => "validate",
        AuthoringSuccess::DraftRun(_) => "draft-run",
        AuthoringSuccess::SuiteRun(_) => "suite-run",
        AuthoringSuccess::Publish(_) => "publish",
        AuthoringSuccess::SuiteProjection(_) => "suite-projection",
    }
}

fn refusal_kind(kind: AuthoringCommandKind) -> &'static str {
    match kind {
        AuthoringCommandKind::SaveFlowDraft => "save-flow-draft",
        AuthoringCommandKind::Validate => "validate",
        AuthoringCommandKind::DraftRun => "draft-run",
        AuthoringCommandKind::SuiteRun => "suite-run",
        AuthoringCommandKind::Publish => "publish",
        AuthoringCommandKind::SuiteProjection => "suite-projection",
    }
}

#[test]
fn collection_and_examples_cover_the_exact_public_schema() {
    let committed_schema_text =
        std::fs::read_to_string(contract_path("authoring-surface.schema.json"))
            .expect("read public authoring schema");
    let committed_schema: Value =
        serde_json::from_str(&committed_schema_text).expect("parse public authoring schema");
    assert_eq!(
        committed_schema,
        wamn_authoring_model::json_schema(),
        "request collection must be reviewed after public schema drift"
    );
    assert_eq!(
        committed_schema["definitions"]["AuthoringRequest"]["properties"]["schema-version"]["enum"],
        serde_json::json!([SCHEMA_VERSION])
    );

    let expected: BTreeSet<String> = COMMANDS.into_iter().map(str::to_string).collect();
    assert_eq!(
        schema_variants(&committed_schema, "AuthoringCommand", "kind"),
        expected,
        "public request command inventory changed"
    );
    assert_eq!(
        schema_variants(&committed_schema, "AuthoringSuccess", "command"),
        expected,
        "public success command inventory changed"
    );

    let collection = std::fs::read_to_string(contract_path("authoring-surface.v0.1.http"))
        .expect("read authoring request collection");
    let requests = http_examples(&collection).expect("validate authoring request collection");
    let mut request_commands = BTreeSet::new();
    for example in requests {
        let document = checked_document(&example.document).expect("decode typed request example");
        let kind = request_kind(&document);
        assert_eq!(example.name, kind, "request section label drifted");
        assert!(
            request_commands.insert(kind.to_string()),
            "duplicate request example for {kind}"
        );
    }
    assert_eq!(request_commands, expected);

    let corpus: Value = serde_json::from_str(
        &std::fs::read_to_string(contract_path("authoring-surface.v0.1.examples.json"))
            .expect("read typed authoring examples"),
    )
    .expect("parse typed authoring examples");
    assert_eq!(corpus["schema"], "authoring-surface.schema.json");
    assert_eq!(corpus["schema-version"], SCHEMA_VERSION);
    reject_privileged_fields(&corpus, "$".to_string()).expect("examples are client-safe");

    let mut response_commands = BTreeSet::new();
    for example in corpus["examples"]
        .as_array()
        .expect("typed examples must be an array")
    {
        let command = example["command"]
            .as_str()
            .expect("typed example command must be a string");
        assert!(
            response_commands.insert(command.to_string()),
            "duplicate typed response examples for {command}"
        );

        let success = checked_document(&example["success"]).expect("decode typed success example");
        let AuthoringDocument::Response(success) = success else {
            panic!("success example for {command} is not a response")
        };
        let AuthoringOutcome::Completed(success) = &success.outcome else {
            panic!("success example for {command} is not completed")
        };
        assert_eq!(success_kind(success), command);

        let refusal = checked_document(&example["refusal"]).expect("decode typed refusal example");
        let AuthoringDocument::Response(refusal) = refusal else {
            panic!("refusal example for {command} is not a response")
        };
        let AuthoringOutcome::Refused(refusal) = refusal.outcome else {
            panic!("refusal example for {command} is not refused")
        };
        assert_eq!(refusal_kind(refusal.command), command);
    }
    assert_eq!(response_commands, expected);
}

#[test]
fn collection_gate_rejects_no_auth_schema_and_privilege_mutants() {
    let collection = std::fs::read_to_string(contract_path("authoring-surface.v0.1.http"))
        .expect("read authoring request collection");

    let no_auth = collection.replacen(AUTHORIZATION_LINE, "", 1);
    assert!(
        http_examples(&no_auth)
            .expect_err("a request without authentication must fail")
            .contains("bearer token")
    );

    let shared_token = collection.replacen(
        AUTHORIZATION_LINE,
        "Authorization: Bearer shared-development-token",
        1,
    );
    assert!(
        http_examples(&shared_token)
            .expect_err("a checked-in shared token must fail")
            .contains("bearer token")
    );

    let duplicate_shared_token = collection.replacen(
        AUTHORIZATION_LINE,
        "Authorization: Bearer {{$processEnv WAMN_AUTHORING_BEARER_TOKEN}}\nauthorization: Bearer shared-development-token",
        1,
    );
    assert!(
        http_examples(&duplicate_shared_token)
            .expect_err("an additional case-insensitive authorization header must fail")
            .contains("bearer token")
    );

    let mut request = http_examples(&collection)
        .expect("valid request collection")
        .remove(0)
        .document;
    request["body"]["schema-version"] = serde_json::json!("0.2");
    assert!(
        checked_document(&request)
            .expect_err("schema-version drift must fail")
            .contains("unsupported authoring contract version")
    );

    request["body"]["schema-version"] = serde_json::json!(SCHEMA_VERSION);
    request["body"]["command"]["input"]["database-url"] =
        serde_json::json!("postgresql://operator.invalid/platform");
    assert!(
        checked_document(&request)
            .expect_err("privileged database authority must fail")
            .contains("forbidden field \"database-url\"")
    );
}

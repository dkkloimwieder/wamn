//! Project-scoped integration administration and private credential writes.

use serde_json::{Value, json};
use tokio_postgres::Transaction;
use wamn_credential_store::{Store, StoreVersion};

use super::{ConnectionError, refuse};
use crate::auth_changes::AuthDrops;
use crate::connection_changes::ConnectionDrops;

fn store_error(source: wamn_credential_store::Error) -> ConnectionError {
    ConnectionError {
        code: "store_error",
        field: "credential",
        source: Some(Box::new(source)),
    }
}

/// Execute a route with a store selected from the target environment.
pub async fn execute(
    tx: &Transaction<'_>,
    org: &str,
    operation: &str,
    payload: &str,
    drops: &mut AuthDrops,
    connections: &mut ConnectionDrops,
) -> Result<Value, ConnectionError> {
    execute_in(
        tx,
        org,
        operation,
        payload,
        (drops, connections),
        None,
        None,
    )
    .await
}

/// The store injection point for administration tests.
pub async fn execute_with_store(
    tx: &Transaction<'_>,
    org: &str,
    operation: &str,
    payload: &str,
    drops: &mut AuthDrops,
    connections: &mut ConnectionDrops,
    store: Option<&dyn Store>,
) -> Result<Value, ConnectionError> {
    execute_in(
        tx,
        org,
        operation,
        payload,
        (drops, connections),
        store,
        None,
    )
    .await
}

/// Execute a connection write against every version retained by live releases.
pub async fn execute_in_environment(
    tx: &Transaction<'_>,
    environment: &Transaction<'_>,
    org: &str,
    operation: &str,
    payload: &str,
    drops: &mut AuthDrops,
    connections: &mut ConnectionDrops,
) -> Result<Value, ConnectionError> {
    execute_in(
        tx,
        org,
        operation,
        payload,
        (drops, connections),
        None,
        Some(environment),
    )
    .await
}

async fn execute_in(
    tx: &Transaction<'_>,
    org: &str,
    operation: &str,
    payload: &str,
    (drops, connections): (&mut AuthDrops, &mut ConnectionDrops),
    store: Option<&dyn Store>,
    environment: Option<&Transaction<'_>>,
) -> Result<Value, ConnectionError> {
    let mut request: Value =
        serde_json::from_str(payload).map_err(|_| refuse("invalid_input", "value"))?;
    validate_request(operation, &request)?;
    // Remove the value before any database call, including reads and lock queries.
    let credential = request.as_object_mut().unwrap().remove("credential");
    let mut result = if matches!(
        operation,
        "connection.write" | "connection.copy" | "connection.set_credential"
    ) {
        generation(
            tx,
            org,
            operation,
            (request.clone(), credential),
            drops,
            store,
            environment,
        )
        .await?
    } else {
        configure(tx, org, operation, &request).await?
    };
    record_connection_drops(org, operation, &request, &mut result, connections);
    Ok(result)
}

/// Record the memory key of each connection a committed write changes
/// (docs/architecture/execution.md#connection-memory). The integration
/// writers return the connections of the integration in every environment,
/// which the route does not show.
fn record_connection_drops(
    org: &str,
    operation: &str,
    request: &Value,
    result: &mut Value,
    connections: &mut ConnectionDrops,
) {
    let project = request["project"].as_str().unwrap_or_default();
    let mut record = |env: &Value, id: &Value| {
        if let (Some(env), Some(id)) = (
            env.as_str(),
            id.as_str().and_then(|id| id.parse::<uuid::Uuid>().ok()),
        ) {
            connections.connection(&format!("{org}--{project}--{env}"), id);
        }
    };
    match operation {
        "connection.write"
        | "connection.copy"
        | "connection.set_credential"
        | "connection.enable"
        | "connection.disable"
        | "connection.delete" => record(&result["env"], &result["id"]),
        "integration.enable" | "integration.disable" => {
            if let Some(Value::Array(rows)) = result
                .as_object_mut()
                .and_then(|result| result.remove("connections"))
            {
                for row in &rows {
                    record(&row["env"], &row["id"]);
                }
            }
        }
        _ => {}
    }
}

fn validate_request(operation: &str, request: &Value) -> Result<(), ConnectionError> {
    let object = request
        .as_object()
        .ok_or_else(|| refuse("invalid_input", "value"))?;
    let allowed: &[&str] = match operation {
        "integration_type.list" | "external_system.list" | "integration.list" => &["project"],
        "external_system.write"
        | "external_system.delete"
        | "integration.get"
        | "integration.enable"
        | "integration.disable"
        | "integration.delete" => &["project", "name"],
        "integration.write" => &[
            "project",
            "name",
            "integration_type",
            "auth_type",
            "external_system",
        ],
        "connection.list" => &["project", "env"],
        "connection.write" => &["project", "env", "name", "parameters", "credential"],
        "connection.copy" => &["project", "env", "name", "source_env"],
        "connection.set_credential" => &["project", "env", "name", "credential"],
        "connection.get" | "connection.enable" | "connection.disable" | "connection.delete" => {
            &["project", "env", "name"]
        }
        _ => return Err(refuse("invalid_input", "operation")),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(refuse("invalid_input", "value"));
    }
    for field in [
        "project",
        "name",
        "env",
        "integration_type",
        "auth_type",
        "source_env",
    ] {
        if allowed.contains(&field) && request[field].as_str().is_none_or(str::is_empty) {
            return Err(refuse("invalid_input", field));
        }
    }
    if let Some(name) = request["name"].as_str()
        && (name.len() > 63
            || !name.as_bytes()[0].is_ascii_lowercase()
            || !name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            }))
    {
        return Err(refuse("invalid_input", "name"));
    }
    if operation == "connection.write" && !request["parameters"].is_object() {
        return Err(refuse("invalid_input", "parameters"));
    }
    if operation == "connection.set_credential" && !request["credential"].is_string() {
        return Err(refuse("invalid_input", "credential"));
    }
    Ok(())
}

/// Call only the security-definer family that owns the requested group.
pub async fn configure(
    tx: &Transaction<'_>,
    org: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, ConnectionError> {
    if request.get("credential").is_some() {
        return Err(refuse("invalid_input", "credential"));
    }
    let sql = match operation.split_once('.').map(|(group, _)| group) {
        Some("external_system") => {
            "SELECT registry.configure_external_system($1,$2,$3::text::jsonb)::text"
        }
        Some("integration") => "SELECT registry.configure_integration($1,$2,$3::text::jsonb)::text",
        Some("integration_type") => {
            "SELECT registry.configure_integration_type($1,$2,$3::text::jsonb)::text"
        }
        Some("connection") => "SELECT registry.configure_connection($1,$2,$3::text::jsonb)::text",
        _ => return Err(refuse("invalid_input", "operation")),
    };
    let text: String = tx
        .query_one(sql, &[&org, &operation, &request.to_string()])
        .await?
        .get(0);
    let result: Value = serde_json::from_str(&text).expect("PostgreSQL JSON");
    if let Some(code) = result["error"].as_str() {
        let code = match code {
            "invalid_input" => "invalid_input",
            "not_found" => "not_found",
            "immutable_field" => "immutable_field",
            "unsupported_auth_type" => "unsupported_auth_type",
            "resource_in_use" => "resource_in_use",
            "referenced" => "referenced",
            _ => "database_error",
        };
        let field = match result["field"].as_str() {
            Some("name") => "name",
            Some("project") => "project",
            Some("integration") => "integration",
            Some("connection") => "connection",
            Some("external_system") => "external_system",
            Some("auth_type") => "auth_type",
            Some("target_id") => "target_id",
            Some("resource_key") => "resource_key",
            Some("credential") => "credential",
            _ => "value",
        };
        return Err(refuse(code, field));
    }
    Ok(result)
}

async fn generation(
    tx: &Transaction<'_>,
    org: &str,
    operation: &str,
    (mut request, credential): (Value, Option<Value>),
    drops: &mut AuthDrops,
    store: Option<&dyn Store>,
    environment: Option<&Transaction<'_>>,
) -> Result<Value, ConnectionError> {
    request["action"] = operation.into();
    let context = configure(tx, org, "connection.prepare", &request).await?;
    let schemas = context["schemas"]
        .as_array()
        .filter(|schemas| !schemas.is_empty())
        .ok_or_else(|| refuse("invalid_input", "integration_type"))?;
    if operation == "connection.copy" {
        let mut source = request.clone();
        source["env"] = source["source_env"].take();
        request["parameters"] = configure(tx, org, "connection.source_parameters", &source).await?;
    } else if operation == "connection.set_credential" {
        request["parameters"] = context["parameters"].clone();
    }
    validate_connection_parameters(
        &request["parameters"],
        context.get("parameters"),
        schemas,
        &context["auth_schema"],
    )?;
    let required = if let Some(environment) = environment {
        required_connection_versions(
            environment,
            context["id"].as_str().expect("connection id"),
            &context["versions"],
        )
        .await?
    } else {
        Vec::new()
    };
    for schema in required {
        validate_required_fields(&request["parameters"], &schema)?;
        if let Some(fields) = schema["properties"].as_object() {
            for (field, definition) in fields {
                if let Some(value) = request["parameters"].get(field)
                    && !validates(value, definition)
                {
                    return Err(refuse("invalid_input", "parameters"));
                }
            }
        }
    }
    let auth = context["auth_type"].as_str().expect("database auth type");
    let stored = if let Some(credential) = credential {
        if auth == "none" {
            return Err(refuse("invalid_input", "credential"));
        }
        let value = credential
            .as_str()
            .ok_or_else(|| refuse("invalid_input", "credential"))?;
        let parsed: Value =
            serde_json::from_str(value).map_err(|_| refuse("invalid_input", "credential"))?;
        if !validates(&parsed, &context["credential_schema"]) {
            return Err(refuse("invalid_input", "credential"));
        }
        let owned_store;
        let store = if let Some(store) = store {
            store
        } else {
            owned_store = open_environment_store(
                context["store"].as_str().expect("database store"),
                org,
                request["project"].as_str().unwrap(),
                request["env"].as_str().unwrap(),
            )?;
            owned_store.as_ref()
        };
        Some(store_credential(store, value).await?)
    } else {
        if auth != "none" && context["credential_id"].is_null() {
            return Err(refuse("invalid_input", "credential"));
        }
        None
    };
    if let Some(stored) = &stored {
        request["stored_credential"] = stored.clone();
    }
    let result = configure(tx, org, "connection.finish", &request).await;
    if result.is_err()
        && let Some(stored) = stored
    {
        if let Some(store) = store {
            let _ = store
                .destroy(
                    stored["ref"].as_str().unwrap(),
                    stored["version"].as_str().unwrap(),
                )
                .await;
            return result;
        }
        let backend = open_environment_store(
            stored["store"].as_str().unwrap(),
            org,
            request["project"].as_str().unwrap(),
            request["env"].as_str().unwrap(),
        );
        if let Ok(backend) = backend {
            let _ = backend
                .destroy(
                    stored["ref"].as_str().unwrap(),
                    stored["version"].as_str().unwrap(),
                )
                .await;
        }
    }
    let result = result?;
    drops.identity(
        context["principal_id"]
            .as_str()
            .expect("database principal"),
    );
    Ok(result)
}

/// Open Kubernetes writes with the complete target environment identity.
pub fn open_environment_store(
    backend: &str,
    org: &str,
    project: &str,
    env: &str,
) -> Result<Box<dyn Store>, ConnectionError> {
    if backend == "k8s" {
        Ok(Box::new(
            wamn_credential_store::kubernetes::KubernetesStore::in_cluster(org, project, env)
                .map_err(store_error)?,
        ))
    } else {
        wamn_credential_store::open(backend).map_err(store_error)
    }
}

async fn store_credential(store: &dyn Store, value: &str) -> Result<Value, ConnectionError> {
    let id = uuid::Uuid::new_v4();
    let StoreVersion {
        store,
        reference,
        version,
    } = store.put(id, value.as_bytes()).await.map_err(store_error)?;
    let hint: String = value
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(json!({"id":id.to_string(),"store":store,"ref":reference,"version":version,"hint":hint}))
}

/// Read versions retained until apply contracts releases outside the live set.
pub async fn required_connection_versions(
    environment: &Transaction<'_>,
    connection: &str,
    published: &Value,
) -> Result<Vec<Value>, ConnectionError> {
    let mut schemas = Vec::new();
    for row in environment.query(
        "SELECT DISTINCT version FROM catalog.release_connection WHERE connection_id=$1::text::uuid",
        &[&connection],
    ).await? {
        let version: i32 = row.get(0);
        let schema = published.get(version.to_string()).ok_or_else(|| refuse("invalid_input", "version"))?;
        schemas.push(schema.clone());
    }
    Ok(schemas)
}

fn validate_required_fields(parameters: &Value, schema: &Value) -> Result<(), ConnectionError> {
    if schema["required"].as_array().is_some_and(|fields| {
        fields.iter().any(|field| {
            field
                .as_str()
                .is_none_or(|field| parameters.get(field).is_none())
        })
    }) {
        return Err(refuse("invalid_input", "parameters"));
    }
    Ok(())
}

/// Validate staged fields against the published union and preserve immutable values.
pub fn validate_connection_parameters(
    parameters: &Value,
    previous: Option<&Value>,
    schemas: &[Value],
    auth_schema: &Value,
) -> Result<(), ConnectionError> {
    let values = parameters
        .as_object()
        .ok_or_else(|| refuse("invalid_input", "parameters"))?;
    if !schemas
        .iter()
        .any(|schema| validate_required_fields(parameters, schema).is_ok())
    {
        return Err(refuse("invalid_input", "parameters"));
    }
    for (field, value) in values {
        if field == "auth" {
            continue;
        }
        if !schemas
            .iter()
            .filter_map(|schema| schema["properties"].get(field))
            .any(|schema| validates(value, schema))
        {
            return Err(refuse("invalid_input", "parameters"));
        }
    }
    if !validates(parameters.get("auth").unwrap_or(&json!({})), auth_schema) {
        return Err(refuse("invalid_input", "parameters.auth"));
    }
    if let Some(previous) = previous.filter(|value| value.is_object()) {
        for schema in schemas {
            if let Some(fields) = schema["properties"].as_object() {
                for (field, definition) in fields {
                    if definition["immutable"] == true
                        && previous.get(field) != parameters.get(field)
                    {
                        return Err(refuse("immutable_field", "parameters"));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Validate the JSON schema vocabulary used by integration and auth parameters.
pub fn validates(value: &Value, schema: &Value) -> bool {
    if let Some(expected) = schema["type"].as_str() {
        let matches = match expected {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "number" => value.is_number(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "null" => value.is_null(),
            _ => false,
        };
        if !matches {
            return false;
        }
    }
    if schema["enum"]
        .as_array()
        .is_some_and(|values| !values.contains(value))
    {
        return false;
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if schema["minLength"].as_u64().is_some_and(|min| length < min)
            || schema["maxLength"].as_u64().is_some_and(|max| length > max)
        {
            return false;
        }
    }
    if let Some(number) = value.as_f64()
        && (schema["minimum"].as_f64().is_some_and(|min| number < min)
            || schema["maximum"].as_f64().is_some_and(|max| number > max))
    {
        return false;
    }
    if let Some(values) = value.as_array()
        && let Some(items) = schema.get("items")
        && !values.iter().all(|value| validates(value, items))
    {
        return false;
    }
    if let Some(values) = value.as_object() {
        if validate_required_fields(value, schema).is_err() {
            return false;
        }
        for (field, value) in values {
            if let Some(definition) = schema["properties"].get(field) {
                if !validates(value, definition) {
                    return false;
                }
            } else if schema["additionalProperties"] == false {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default)]
    struct RecordingStore(std::sync::Mutex<Vec<Vec<u8>>>);

    #[async_trait::async_trait]
    impl Store for RecordingStore {
        async fn put(
            &self,
            id: uuid::Uuid,
            value: &[u8],
        ) -> Result<StoreVersion, wamn_credential_store::Error> {
            self.0.lock().unwrap().push(value.to_vec());
            Ok(StoreVersion {
                store: "file",
                reference: format!("reference-{id}"),
                version: "1".into(),
            })
        }
        async fn get(&self, _: &str, _: &str) -> Result<Vec<u8>, wamn_credential_store::Error> {
            Ok(Vec::new())
        }
        async fn destroy(&self, _: &str, _: &str) -> Result<(), wamn_credential_store::Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn credential_value_reaches_only_the_store_and_metadata_carries_the_hint() {
        let store = RecordingStore::default();
        let value = r#"{"key":"private-1234"}"#;
        let metadata = store_credential(&store, value).await.unwrap();
        assert_eq!(*store.0.lock().unwrap(), vec![value.as_bytes()]);
        assert_eq!(metadata["hint"], "34\"}");
        assert!(metadata.get("credential").is_none());
        assert!(!metadata.to_string().contains("private-1234"));
    }

    #[test]
    fn staged_parameters_use_the_union_and_immutable_fields_stay_fixed() {
        let schemas = vec![
            json!({"properties":{"bucket":{"type":"string","immutable":true}}}),
            json!({"properties":{"bucket":{"type":"string","immutable":true},"region":{"type":"string"}}}),
        ];
        let before = json!({"bucket":"one"});
        let auth = json!({"type":"object","properties":{},"additionalProperties":false});
        assert!(
            validate_connection_parameters(
                &json!({"bucket":"one","region":"east"}),
                Some(&before),
                &schemas,
                &auth
            )
            .is_ok()
        );
        assert_eq!(
            validate_connection_parameters(
                &json!({"bucket":"two"}),
                Some(&before),
                &schemas,
                &auth
            )
            .unwrap_err()
            .code,
            "immutable_field"
        );
        assert!(
            validate_connection_parameters(
                &json!({"bucket":"one","unknown":1}),
                Some(&before),
                &schemas,
                &auth
            )
            .is_err()
        );
        assert!(
            validate_connection_parameters(
                &json!({"bucket":"one","auth":{"key":"secret"}}),
                Some(&before),
                &schemas,
                &auth
            )
            .is_err()
        );
    }
}

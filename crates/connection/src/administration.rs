//! Connection administration in one project database, with system-owned type metadata.

use std::fmt;

use serde_json::{Value, json};
use uuid::Uuid;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::OrgId;

use crate::connection_credentials::{StoreVersion, WriteCredentials};
use crate::references;
use wamn_kubernetes::kubernetes::CredentialSecret;

use crate::project_connections::{Administration, Lookup, NewConnection, TokenChange};

/// A refused configuration write, or its database failure.
#[derive(Debug)]
pub struct ConnectionError {
    pub code: &'static str,
    pub field: &'static str,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl fmt::Display for ConnectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.field)?;
        if let Some(reason) = self.reason() {
            write!(f, ": {reason}")?;
        }
        Ok(())
    }
}

impl ConnectionError {
    /// Explain a connection refusal that the caller can resolve.
    pub fn reason(&self) -> Option<&'static str> {
        match self.code {
            "ipv6_authority_not_supported" => Some(
                "the operator CRD's allowedHosts pattern has no bracketed IPv6 form; give a hostname that resolves to the device",
            ),
            "exclusive_connection_requires_local_target" => {
                Some("exclusive connections belong to local targets")
            }
            _ => None,
        }
    }
}
impl std::error::Error for ConnectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|source| source as _)
    }
}
impl ConnectionError {
    /// Attach an infrastructure failure to the existing administration error.
    pub fn with_source(code: &'static str, field: &'static str, source: anyhow::Error) -> Self {
        Self {
            code,
            field,
            source: Some(source.into_boxed_dyn_error()),
        }
    }
}

/// Build a refusal using the field named in the control response.
pub fn refuse(code: &'static str, field: &'static str) -> ConnectionError {
    ConnectionError {
        code,
        field,
        source: None,
    }
}

pub(crate) fn store_error(source: anyhow::Error) -> ConnectionError {
    ConnectionError::with_source("store_error", "credential", source)
}

/// Validate the non-secret request shape before any database call.
pub fn validate_request(operation: &str, request: &Value) -> Result<(), ConnectionError> {
    let object = request
        .as_object()
        .ok_or_else(|| refuse("invalid_input", "value"))?;
    let allowed: &[&str] = match operation {
        "integration_type.list" => &["project"],
        "external_system.list" | "connection.list" => &["project", "env"],
        "external_system.write"
        | "external_system.delete"
        | "connection.authorize"
        | "connection.get"
        | "connection.enable"
        | "connection.disable"
        | "connection.delete" => &["project", "env", "name"],
        "connection.write" => &[
            "project",
            "env",
            "name",
            "integration_type",
            "auth_type",
            "external_system",
            "authority",
            "parameters",
            "credential",
        ],
        "connection.test" => &[
            "project",
            "env",
            "name",
            "parameters",
            "credential",
            "version",
        ],
        "connection.copy" => &["project", "env", "name", "source_env"],
        "connection.set_credential" => &["project", "env", "name", "credential"],
        _ => return Err(refuse("invalid_input", "operation")),
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err(refuse("invalid_input", "value"));
    }
    for field in [
        "project",
        "env",
        "name",
        "integration_type",
        "auth_type",
        "source_env",
    ] {
        if allowed.contains(&field) && request[field].as_str().is_none_or(str::is_empty) {
            return Err(refuse("invalid_input", field));
        }
    }
    if let Some(name) = request["name"].as_str()
        && !wamn_registry::configuration::valid_connection_name(name)
    {
        return Err(refuse("invalid_input", "name"));
    }
    if matches!(operation, "connection.write" | "connection.test")
        && !request["parameters"].is_object()
    {
        return Err(refuse("invalid_input", "parameters"));
    }
    if operation == "connection.set_credential" && !request["credential"].is_string() {
        return Err(refuse("invalid_input", "credential"));
    }
    if operation == "connection.test"
        && request
            .get("version")
            .filter(|v| !v.is_null())
            .is_some_and(|v| v.as_i64().is_none_or(|v| v <= 0 || v > i64::from(i32::MAX)))
    {
        return Err(refuse("invalid_input", "version"));
    }
    Ok(())
}

/// Authority and payload for one administration operation.
pub struct Request<'a> {
    pub caller: PrincipalId,
    pub org: &'a OrgId,
    pub operation: &'a str,
    pub payload: &'a str,
}

impl fmt::Debug for Request<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Request")
            .field("org", self.org)
            .field("operation", &self.operation)
            .finish_non_exhaustive()
    }
}

/// Execute the named administration steps against the selected project.
pub async fn execute(
    database: &mut Administration<'_, '_>,
    credentials: &dyn WriteCredentials,
    authority_port: fn(&str) -> Result<u16, ConnectionError>,
    request: Request<'_>,
    apps: &[wamn_app::app_config::AppConfig],
) -> Result<Value, ConnectionError> {
    let Request {
        caller,
        org,
        operation,
        payload,
    } = request;
    let mut request: Value =
        serde_json::from_str(payload).map_err(|_| refuse("invalid_input", "value"))?;
    validate_request(operation, &request)?;
    if operation == "connection.delete" {
        let id = database
            .connection_id(request["name"].as_str(), Lookup::Read)
            .await?;
        if let Some(id) = id {
            references::refuse_app_references(apps, &id.to_string())?;
        }
    }
    let credential = request
        .as_object_mut()
        .expect("validated object")
        .remove("credential");
    let requested_authority = request
        .as_object_mut()
        .expect("validated object")
        .remove("authority");
    if matches!(
        operation,
        "connection.write" | "connection.copy" | "connection.set_credential"
    ) {
        let existing = database.lock_connection(request["name"].as_str()).await?;
        let context = match existing {
            Some(row) => row,
            None if operation == "connection.write" => {
                json!({"id":Uuid::new_v4(),"integration_type":request["integration_type"],"auth_type":request["auth_type"]})
            }
            None => return Err(refuse("not_found", "connection")),
        };
        let id: Uuid = serde_json::from_value(context["id"].clone())
            .expect("UUID from PostgreSQL or generator");
        let new = context["active_generation"].is_null();
        let metadata = database
            .integration_metadata(
                context["integration_type"].as_str(),
                context["auth_type"].as_str(),
            )
            .await?;
        let metadata = metadata.ok_or_else(|| refuse("invalid_input", "auth_type"))?;
        let target = if new {
            Some(
                database
                    .target(
                        caller,
                        operation,
                        org,
                        request["env"].as_str(),
                        metadata["exclusive"] == true,
                    )
                    .await?,
            )
        } else {
            None
        };
        let authority = if operation == "connection.write" {
            match requested_authority.as_ref() {
                Some(Value::String(value)) => Some(value.as_str()),
                None | Some(Value::Null) => None,
                _ => return Err(refuse("invalid_input", "authority")),
            }
        } else {
            context["authority"].as_str()
        };
        if metadata["runtime_type"] == "wasm" {
            authority_port(authority.ok_or_else(|| refuse("invalid_input", "authority"))?)?;
        } else if authority.is_some() {
            return Err(refuse("invalid_input", "authority"));
        }
        if !new && context["authority"].as_str() != authority {
            return Err(refuse("immutable_field", "authority"));
        }
        let versions = database
            .versions(context["integration_type"].as_str())
            .await?;
        let schemas: Vec<Value> = versions
            .iter()
            .map(|v| v["parameter_schema"].clone())
            .collect();
        if schemas.is_empty() {
            return Err(refuse("invalid_input", "integration_type"));
        }
        let previous = database
            .previous(id, context["active_generation"].as_i64())
            .await?;
        if operation == "connection.copy" {
            let copied = database.copy_source(request["name"].as_str()).await?;
            let (integration_type, auth, parameters, source_authority) =
                copied.ok_or_else(|| refuse("not_found", "connection"))?;
            if context["integration_type"] != integration_type || context["auth_type"] != auth {
                return Err(refuse("immutable_field", "integration_type"));
            }
            if source_authority.as_deref() != authority {
                return Err(refuse("immutable_field", "authority"));
            }
            request["parameters"] = parameters;
        } else if operation == "connection.set_credential" {
            request["parameters"] = previous.as_ref().expect("existing generation").0.clone();
        }
        if metadata["runtime_type"] == "wasm" && request["parameters"].get("authority").is_some() {
            return Err(refuse("invalid_input", "parameters"));
        }
        validate_connection_parameters(
            &request["parameters"],
            previous.as_ref().map(|p| &p.0),
            &schemas,
            &metadata["auth_schema"],
        )?;
        let version = database
            .required_version(
                caller,
                operation,
                org,
                request["env"].as_str().expect("validated env"),
                request["name"].as_str().expect("validated name"),
            )
            .await?;
        if let Some(version) = version {
            let published = versions
                .iter()
                .find(|entry| entry["version"] == version)
                .ok_or_else(|| refuse("invalid_input", "version"))?;
            validate_version_parameters(&request["parameters"], &published["parameter_schema"])?;
        }
        let external: Option<Uuid> = if let Some(name) = request["external_system"].as_str() {
            Some(
                database
                    .external_system_id(Some(name), Lookup::Read)
                    .await?
                    .ok_or_else(|| refuse("not_found", "external_system"))?,
            )
        } else {
            None
        };
        if operation == "connection.write"
            && !new
            && (context["integration_type"] != request["integration_type"]
                || context["auth_type"] != request["auth_type"]
                || context["external_system_id"] != json!(external))
        {
            return Err(refuse("immutable_field", "connection"));
        }
        let auth = context["auth_type"].as_str().expect("stored auth type");
        let principal = if new {
            Some(
                database
                    .write_service(
                        &format!("connection-{id}"),
                        request["name"].as_str().expect("validated connection name"),
                    )
                    .await?,
            )
        } else {
            None
        };
        let stored = if let Some(credential) = credential {
            if auth == "none" {
                return Err(refuse("invalid_input", "credential"));
            }
            let value = credential
                .as_str()
                .ok_or_else(|| refuse("invalid_input", "credential"))?;
            let parsed: Value =
                serde_json::from_str(value).map_err(|_| refuse("invalid_input", "credential"))?;
            if !validates(&parsed, &metadata["credential_schema"]) {
                return Err(refuse("invalid_input", "credential"));
            }
            let target = wamn_registry::Triple::new(
                org.to_owned(),
                request["env"]
                    .as_str()
                    .expect("validated env")
                    .parse()
                    .map_err(|_| refuse("invalid_input", "env"))?,
                request["project"]
                    .as_str()
                    .expect("validated project")
                    .parse()
                    .map_err(|_| refuse("invalid_input", "project"))?,
            );
            Some(store_credential(credentials, &target, value).await?)
        } else {
            None
        };
        let credential_id = stored
            .as_ref()
            .map(|v| serde_json::from_value::<Uuid>(v["id"].clone()).expect("generated UUID"))
            .or_else(|| previous.as_ref().and_then(|p| p.1));
        if (auth == "none") != credential_id.is_none() {
            return Err(refuse("invalid_input", "credential"));
        }
        if let Some(principal) = principal {
            let target = target.expect("new connection has a target");
            let mut resource = metadata["resource_key_template"]
                .as_str()
                .map(str::to_owned);
            if let Some(resource) = &mut resource {
                let template = resource.clone();
                for part in template.split('{').skip(1) {
                    let field = part
                        .split_once('}')
                        .ok_or_else(|| refuse("invalid_input", "resource_key"))?
                        .0;
                    if field == "authority"
                        && let Some(authority) = authority
                    {
                        *resource = resource.replace("{authority}", authority);
                        continue;
                    }
                    if !schemas
                        .iter()
                        .any(|s| s["properties"][field]["immutable"] == true)
                    {
                        return Err(refuse("invalid_input", "resource_key"));
                    }
                    let value = database
                        .resource_value(&request["parameters"], field)
                        .await?;
                    let value = value.ok_or_else(|| refuse("invalid_input", "resource_key"))?;
                    *resource = resource.replace(&format!("{{{field}}}"), &value);
                }
            }
            database
                .insert_connection(NewConnection {
                    id,
                    name: request["name"].as_str(),
                    integration: context["integration_type"].as_str(),
                    auth,
                    external,
                    target,
                    resource,
                    principal,
                    authority,
                })
                .await?;
        }
        let token_change = if previous.as_ref().map(|p| &p.0["auth"])
            != Some(&request["parameters"]["auth"])
            || (stored.is_some()
                && matches!(
                    auth,
                    "oauth2_client_credentials" | "oauth2_client_credentials_jwt"
                )) {
            TokenChange::Delete
        } else if stored.is_some() && auth == "oauth2_authorization_code" {
            TokenChange::Expire
        } else {
            TokenChange::Keep
        };
        database
            .write_generation(
                id,
                credential_id,
                stored.as_ref(),
                token_change,
                &request["parameters"],
            )
            .await?;
        return summary(database, org, &request, id).await;
    }
    configure(database, org, operation, &request).await
}

/// Read system type metadata and project rows without a system connection function.
pub async fn configure(
    database: &mut Administration<'_, '_>,
    org: &OrgId,
    operation: &str,
    request: &Value,
) -> Result<Value, ConnectionError> {
    if request.get("credential").is_some() {
        return Err(refuse("invalid_input", "credential"));
    }
    if operation == "integration_type.list" {
        return database
            .configure_integration_type(org, operation, request)
            .await;
    }
    let name = request["name"].as_str();
    if operation.starts_with("external_system.") {
        return match operation {
            "external_system.list" => database.list_external_systems().await,
            "external_system.write" => {
                database.insert_external_system(name).await?;
                Ok(json!({"name":name}))
            }
            "external_system.delete" => {
                let id = database
                    .external_system_id(name, Lookup::Lock)
                    .await?
                    .ok_or_else(|| refuse("not_found", "name"))?;
                database
                    .refuse_env_references("external_system", &id.to_string())
                    .await?;
                database.delete_external_system(id).await?;
                Ok(json!({"name":name}))
            }
            _ => Err(refuse("invalid_input", "operation")),
        };
    }
    if operation == "connection.list" {
        let ids = database.connection_ids().await?;
        let mut rows = Vec::new();
        for id in ids {
            rows.push(summary(database, org, request, id).await?);
        }
        return Ok(json!({"connections":rows}));
    }
    let lookup = if operation == "connection.test" {
        Lookup::Read
    } else {
        Lookup::Lock
    };
    let id = database
        .connection_id(name, lookup)
        .await?
        .ok_or_else(|| refuse("not_found", "connection"))?;
    let mut row = summary(database, org, request, id).await?;
    match operation {
        "connection.get" => Ok(row),
        "connection.authorize" => {
            if row["auth_type"] != "oauth2_authorization_code" {
                return Err(refuse("unsupported_auth_type", "auth_type"));
            }
            Ok(row)
        }
        "connection.test" => {
            if row["auth_type"] == "oauth2_authorization_code" {
                return Err(refuse("unsupported_auth_type", "auth_type"));
            }
            let metadata = database
                .test_metadata(row["integration_type"].as_str(), row["auth_type"].as_str())
                .await?;
            for (key, value) in metadata.as_object().expect("object") {
                row[key] = value.clone();
            }
            row["credential"] = database.credential_reference(id).await?;
            Ok(row)
        }
        "connection.enable" | "connection.disable" => {
            database
                .set_connection_active(id, operation == "connection.enable")
                .await?;
            Ok(
                json!({"id":id,"name":name,"env":request["env"],"principal_id":row["service_principal_id"]}),
            )
        }
        "connection.delete" => {
            database
                .refuse_env_references("connection", &id.to_string())
                .await?;
            let credentials = database.delete_connection(id).await?;
            Ok(
                json!({"id":id,"name":name,"env":request["env"],"principal_id":row["service_principal_id"],"credentials":credentials}),
            )
        }
        _ => Err(refuse("invalid_input", "operation")),
    }
}

async fn summary(
    database: &mut Administration<'_, '_>,
    org: &OrgId,
    request: &Value,
    id: Uuid,
) -> Result<Value, ConnectionError> {
    let mut row = database.connection_summary(id).await?;
    let metadata = database
        .version_summary(row["integration_type"].as_str())
        .await?;
    row["versions"] = metadata["versions"].clone();
    row["deprecated_fields"] = metadata["deprecated_fields"].clone();
    row["org"] = json!(org);
    row["project"] = request["project"].clone();
    row["env"] = request["env"].clone();
    Ok(row)
}

async fn store_credential(
    store: &dyn WriteCredentials,
    target: &wamn_registry::Triple,
    value: &str,
) -> Result<Value, ConnectionError> {
    let id = uuid::Uuid::new_v4();
    let secret = CredentialSecret {
        name: wamn_registry::credential_secret_name(&target.org, &target.project, &target.env),
        org: target.org.as_str(),
        env: target.env.as_str(),
        project: target.project.as_str(),
    };
    let StoreVersion {
        store,
        reference,
        version,
    } = store
        .put(secret, id, value.as_bytes())
        .await
        .map_err(store_error)?;
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

/// Validate the fields required by one selected or live version.
pub fn validate_version_parameters(
    parameters: &Value,
    schema: &Value,
) -> Result<(), ConnectionError> {
    validate_required_fields(parameters, schema)?;
    if let Some(fields) = schema["properties"].as_object() {
        for (field, definition) in fields {
            if let Some(value) = parameters.get(field)
                && !validates(value, definition)
            {
                return Err(refuse("invalid_input", "parameters"));
            }
        }
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

    #[test]
    fn candidate_versions_are_optional_positive_integers() {
        let mut request = json!({"project":"one","env":"dev","name":"api","parameters":{}});
        validate_request("connection.test", &request).unwrap();
        for version in [Value::Null, json!(1), json!(i32::MAX)] {
            request["version"] = version;
            validate_request("connection.test", &request).unwrap();
        }
        for version in [json!(0), json!(-1), json!(i64::MAX), json!("1")] {
            request["version"] = version;
            assert_eq!(
                validate_request("connection.test", &request)
                    .unwrap_err()
                    .field,
                "version"
            );
        }
    }

    #[derive(Debug, Default)]
    struct RecordingStore(std::sync::Mutex<Vec<Vec<u8>>>);

    #[async_trait::async_trait]
    impl WriteCredentials for RecordingStore {
        async fn put(
            &self,
            _target: CredentialSecret<'_>,
            id: uuid::Uuid,
            value: &[u8],
        ) -> anyhow::Result<StoreVersion> {
            self.0.lock().unwrap().push(value.to_vec());
            Ok(StoreVersion {
                store: "k8s",
                reference: format!("reference-{id}"),
                version: "1".into(),
            })
        }
        async fn destroy(&self, _: &str, _: &str) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn credential_value_reaches_only_the_store_and_metadata_carries_the_hint() {
        let store = RecordingStore::default();
        let value = r#"{"key":"private-1234"}"#;
        let metadata = store_credential(
            &store,
            &wamn_registry::Triple::new(
                "acme".parse().unwrap(),
                "dev".parse().unwrap(),
                "app".parse().unwrap(),
            ),
            value,
        )
        .await
        .unwrap();
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

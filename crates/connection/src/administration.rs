//! Connection administration in one project database, with system-owned type metadata.

use std::fmt;

use serde_json::{Value, json};
use uuid::Uuid;
use wamn_identity_protocol::PrincipalId;
use wamn_registry::{EnvName, OrgId, ProjectId};

use crate::connection_credentials::{StoreVersion, WriteCredentials};
use wamn_kubernetes::kubernetes::CredentialSecret;

use crate::project_connections::{
    Administration, ConnectionRow, ConnectionState, CopySource, EventRow, EventType, Lookup,
    NewConnection, TokenChange,
};

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
        | "connection.deactivate" => &["project", "env", "name"],
        "connection.events" => &["project", "env", "name", "before"],
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
        "connection.activate" | "connection.set_credential" => {
            &["project", "env", "name", "credential"]
        }
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

/// Authority and payload for a connection write.
pub struct Request<'a> {
    pub caller: PrincipalId,
    pub org: &'a OrgId,
    pub operation: &'a str,
    pub payload: Value,
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

/// Project row and registry version data for a connection response.
#[derive(Debug)]
pub struct ConnectionDetails {
    pub row: ConnectionRow,
    pub versions: Vec<i32>,
    pub deprecated_fields: Vec<String>,
}

/// Results passed to the control route for serialization.
#[derive(Debug)]
pub enum Outcome {
    Events {
        events: Vec<EventRow>,
        next_before: Option<String>,
    },
    Connection(ConnectionDetails),
    List(Vec<ConnectionDetails>),
    Authorize(ConnectionRow),
    Other(Value),
}

/// A validated write that can stage its Secret after the read transaction ends.
pub struct ConnectionWrite {
    expected: Option<ConnectionState>,
    id: Uuid,
    name: String,
    new_connection: Option<NewConnection>,
    activation_principal: Option<Uuid>,
    credential: Option<String>,
    credential_id: Option<Uuid>,
    token_change: TokenChange,
    parameters: Value,
}

impl fmt::Debug for ConnectionWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionWrite")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Validate a write against the current project and registry state.
pub async fn prepare_write(
    database: &mut Administration<'_, '_>,
    authority_port: fn(&str) -> Result<u16, ConnectionError>,
    request: Request<'_>,
    source: Option<CopySource>,
) -> Result<ConnectionWrite, ConnectionError> {
    let Request {
        caller,
        org,
        operation,
        payload: mut request,
    } = request;
    validate_request(operation, &request)?;
    let credential = request
        .as_object_mut()
        .expect("validated object")
        .remove("credential")
        .filter(|value| operation != "connection.activate" || !value.is_null());
    let requested_parameters = request
        .as_object_mut()
        .expect("validated object")
        .remove("parameters");
    let name = request["name"].as_str().expect("validated name").to_owned();
    let activating = operation == "connection.activate";
    let existing = database.lock_connection(&name).await?;
    if existing.is_none() && operation != "connection.write" {
        return Err(refuse("not_found", "connection"));
    }
    let new = existing.is_none();
    let id = existing
        .as_ref()
        .map_or_else(Uuid::new_v4, |row| row.state.id);
    let (integration, auth) = match &existing {
        Some(row) => (row.integration_type.as_str(), row.auth_type.as_str()),
        None => (
            request["integration_type"]
                .as_str()
                .expect("validated integration type"),
            request["auth_type"].as_str().expect("validated auth type"),
        ),
    };
    if activating && (auth == "none") == credential.is_some() {
        return Err(refuse("invalid_input", "credential"));
    }
    let metadata = database
        .integration_metadata(Some(integration), Some(auth))
        .await?
        .ok_or_else(|| refuse("invalid_input", "auth_type"))?;
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
    let stored_authority = existing.as_ref().and_then(|row| row.authority.as_deref());
    let authority = if operation == "connection.write" {
        match &request["authority"] {
            Value::String(value) => Some(value.as_str()),
            Value::Null => None,
            _ => return Err(refuse("invalid_input", "authority")),
        }
    } else {
        stored_authority
    };
    if metadata["runtime_type"] == "wasm" {
        authority_port(authority.ok_or_else(|| refuse("invalid_input", "authority"))?)?;
    } else if authority.is_some() {
        return Err(refuse("invalid_input", "authority"));
    }
    if !new && stored_authority != authority {
        return Err(refuse("immutable_field", "authority"));
    }
    let versions = database.versions(Some(integration)).await?;
    let schemas: Vec<Value> = versions
        .iter()
        .map(|v| v["parameter_schema"].clone())
        .collect();
    if schemas.is_empty() {
        return Err(refuse("invalid_input", "integration_type"));
    }
    let previous = database
        .previous(id, existing.as_ref().map(|row| row.state.generation))
        .await?;
    let changed_parameters = if operation == "connection.copy" {
        let source = source.ok_or_else(|| refuse("not_found", "connection"))?;
        if integration != source.integration_type || auth != source.auth_type {
            return Err(refuse("immutable_field", "integration_type"));
        }
        if source.authority.as_deref() != authority {
            return Err(refuse("immutable_field", "authority"));
        }
        Some(source.parameters)
    } else if activating || operation == "connection.set_credential" {
        // Both operations reuse the destination generation's parameters.
        None
    } else {
        requested_parameters
    };
    let parameters = changed_parameters
        .as_ref()
        .unwrap_or_else(|| &previous.as_ref().expect("existing generation").0);
    if metadata["runtime_type"] == "wasm" && parameters.get("authority").is_some() {
        return Err(refuse("invalid_input", "parameters"));
    }
    validate_connection_parameters(
        parameters,
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
        validate_version_parameters(parameters, &published["parameter_schema"])?;
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
        && let Some(row) = &existing
        && (Some(row.integration_type.as_str()) != request["integration_type"].as_str()
            || Some(row.auth_type.as_str()) != request["auth_type"].as_str()
            || row.external_system_id != external)
    {
        return Err(refuse("immutable_field", "connection"));
    }
    let credential = match credential {
        Some(Value::String(value)) if auth != "none" => {
            let parsed: Value =
                serde_json::from_str(&value).map_err(|_| refuse("invalid_input", "credential"))?;
            if !validates(&parsed, &metadata["credential_schema"]) {
                return Err(refuse("invalid_input", "credential"));
            }
            Some(value)
        }
        Some(_) => return Err(refuse("invalid_input", "credential")),
        None => None,
    };
    let credential_id = if activating {
        None
    } else {
        previous.as_ref().and_then(|p| p.1)
    };
    if (auth == "none") != (credential.is_none() && credential_id.is_none()) {
        return Err(refuse("invalid_input", "credential"));
    }

    let new_connection = if let Some(target) = target {
        let template = metadata["resource_key_template"].as_str();
        let mut resource = template.map(str::to_owned);
        if let (Some(template), Some(resource)) = (template, &mut resource) {
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
                let value = database.resource_value(parameters, field).await?;
                let value = value.ok_or_else(|| refuse("invalid_input", "resource_key"))?;
                *resource = resource.replace(&format!("{{{field}}}"), &value);
            }
        }
        Some(NewConnection {
            id,
            integration: integration.to_owned(),
            auth: auth.to_owned(),
            external,
            target,
            resource,
            authority: authority.map(str::to_owned),
        })
    } else {
        None
    };
    let token_change = if activating
        || previous.as_ref().map(|p| &p.0["auth"]) != Some(&parameters["auth"])
        || (credential.is_some()
            && matches!(
                auth,
                "oauth2_client_credentials" | "oauth2_client_credentials_jwt"
            )) {
        TokenChange::Delete
    } else if credential.is_some() && auth == "oauth2_authorization_code" {
        TokenChange::Expire
    } else {
        TokenChange::Keep
    };
    let activation_principal = activating.then(|| {
        existing
            .as_ref()
            .expect("existing connection")
            .service_principal_id
    });
    Ok(ConnectionWrite {
        expected: existing.map(|row| row.state),
        id,
        name,
        new_connection,
        activation_principal,
        credential,
        credential_id,
        token_change,
        parameters: changed_parameters.unwrap_or_else(|| previous.expect("existing generation").0),
    })
}

impl ConnectionWrite {
    /// Stage a fresh credential version with no transaction open.
    pub async fn stage(
        mut self,
        store: &dyn WriteCredentials,
        org: &OrgId,
        project: &ProjectId,
        env: &EnvName,
    ) -> Result<StagedWrite, ConnectionError> {
        let stored = match self.credential.take() {
            Some(value) => Some(store_credential(store, org, project, env, &value).await?),
            None => None,
        };
        Ok(StagedWrite {
            write: self,
            stored,
        })
    }
}

/// A validated write whose new credential, if any, is available in the store.
#[derive(Debug)]
pub struct StagedWrite {
    write: ConnectionWrite,
    stored: Option<StoredCredential>,
}

impl StagedWrite {
    /// Publish the validated generation. The caller commits before using the result.
    pub async fn publish(
        &self,
        database: &mut Administration<'_, '_>,
        env: &EnvName,
    ) -> Result<Outcome, ConnectionError> {
        let write = &self.write;
        let current = database.lock_connection(&write.name).await?;
        if current.map(|row| row.state) != write.expected {
            return Err(refuse("invalid_input", "connection"));
        }
        if let Some(connection) = &write.new_connection {
            let principal = database
                .write_service(&format!("connection-{}", write.id), &write.name)
                .await?;
            database
                .insert_connection(connection, &write.name, principal)
                .await?;
        }
        let generation = database
            .write_generation(
                write.id,
                self.stored
                    .as_ref()
                    .map(|credential| credential.id)
                    .or(write.credential_id),
                self.stored.as_ref(),
                write.token_change,
                &write.parameters,
            )
            .await?;
        if let Some(principal) = write.activation_principal {
            database.activate_connection(write.id).await?;
            database
                .record_success(write.id, generation, EventType::Activate)
                .await?;
            return Ok(Outcome::Other(json!({
                "id": write.id,
                "name": write.name,
                "env": env,
                "principal_id": principal,
            })));
        }
        if self.stored.is_some() {
            database
                .record_success(write.id, generation, EventType::CredentialChange)
                .await?;
        }
        Ok(Outcome::Connection(details(database, write.id).await?))
    }

    /// Discard a staged version after a confirmed rollback.
    pub async fn discard(self, store: &dyn WriteCredentials) -> Result<(), ConnectionError> {
        if let Some(stored) = self.stored {
            store
                .destroy(&stored.version.reference, &stored.version.version)
                .await
                .map_err(store_error)?;
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct StoredCredential {
    pub(crate) id: Uuid,
    pub(crate) version: StoreVersion,
}

/// Read connection configuration or change local state in the caller's transaction.
pub async fn configure(
    database: &mut Administration<'_, '_>,
    org: &OrgId,
    operation: &str,
    request: &Value,
) -> Result<Outcome, ConnectionError> {
    if request.get("credential").is_some() {
        return Err(refuse("invalid_input", "credential"));
    }
    if operation == "integration_type.list" {
        return database
            .configure_integration_type(org, operation, request)
            .await
            .map(Outcome::Other);
    }
    let name = request["name"].as_str();
    if operation.starts_with("external_system.") {
        let value = match operation {
            "external_system.list" => database.list_external_systems().await?,
            "external_system.write" => {
                database.insert_external_system(name).await?;
                json!({"name":name})
            }
            "external_system.delete" => {
                let id = database
                    .external_system_id(name, Lookup::Lock)
                    .await?
                    .ok_or_else(|| refuse("not_found", "name"))?;
                database.refuse_external_system_references(id).await?;
                database.delete_external_system(id).await?;
                json!({"name":name})
            }
            _ => return Err(refuse("invalid_input", "operation")),
        };
        return Ok(Outcome::Other(value));
    }
    if operation == "connection.list" {
        let ids = database.connection_ids().await?;
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            rows.push(details(database, id).await?);
        }
        return Ok(Outcome::List(rows));
    }
    let lookup = if matches!(operation, "connection.test" | "connection.events") {
        Lookup::Read
    } else {
        Lookup::Lock
    };
    let id = database
        .connection_id(name, lookup)
        .await?
        .ok_or_else(|| refuse("not_found", "connection"))?;
    if operation == "connection.deactivate" {
        let row = database.deactivate_connection(id).await?;
        return Ok(Outcome::Other(json!({
            "id": id,
            "name": name,
            "env": request["env"],
            "principal_id": row["service_principal_id"],
            "credentials": row["credentials"],
        })));
    }
    if operation == "connection.get" {
        return Ok(Outcome::Connection(details(database, id).await?));
    }
    if operation == "connection.events" {
        const PAGE_SIZE: u16 = 100;
        let before = request
            .get("before")
            .map(|before| {
                before
                    .as_str()
                    .and_then(|value| value.parse::<i64>().ok())
                    .filter(|id| *id > 0)
                    .ok_or_else(|| refuse("invalid_input", "before"))
            })
            .transpose()?;
        let mut events = database
            .connection_events(id, before, i64::from(PAGE_SIZE) + 1)
            .await?;
        let next_before = if events.len() > usize::from(PAGE_SIZE) {
            events.pop();
            events.last().map(|event| event.id.clone())
        } else {
            None
        };
        return Ok(Outcome::Events {
            events,
            next_before,
        });
    }
    let row = database.connection_summary(id).await?;
    if !row.active {
        return Err(refuse("connection_inactive", "connection"));
    }
    match operation {
        "connection.authorize" => {
            if row.auth_type != "oauth2_authorization_code" {
                return Err(refuse("unsupported_auth_type", "auth_type"));
            }
            Ok(Outcome::Authorize(row))
        }
        "connection.test" => {
            if row.auth_type == "oauth2_authorization_code" {
                return Err(refuse("unsupported_auth_type", "auth_type"));
            }
            let mut metadata = database
                .test_metadata(Some(&row.integration_type), Some(&row.auth_type))
                .await?;
            metadata["integration_type"] = row.integration_type.into();
            metadata["auth_type"] = row.auth_type.into();
            metadata["parameters"] = row.parameters;
            metadata["credential"] = database.credential_reference(id).await?;
            Ok(Outcome::Other(metadata))
        }
        _ => Err(refuse("invalid_input", "operation")),
    }
}

async fn details(
    database: &mut Administration<'_, '_>,
    id: Uuid,
) -> Result<ConnectionDetails, ConnectionError> {
    let row = database.connection_summary(id).await?;
    let (versions, deprecated_fields) = database.version_summary(&row.integration_type).await?;
    Ok(ConnectionDetails {
        row,
        versions,
        deprecated_fields,
    })
}

async fn store_credential(
    store: &dyn WriteCredentials,
    org: &OrgId,
    project: &ProjectId,
    env: &EnvName,
    value: &str,
) -> Result<StoredCredential, ConnectionError> {
    let id = Uuid::new_v4();
    let secret = CredentialSecret {
        name: wamn_registry::credential_secret_name(org, project, env),
        org: org.as_str(),
        env: env.as_str(),
        project: project.as_str(),
    };
    let version = store
        .put(secret, id, value.as_bytes())
        .await
        .map_err(store_error)?;
    Ok(StoredCredential { id, version })
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
    async fn credential_value_reaches_the_store_with_a_new_reference() {
        let store = RecordingStore::default();
        let value = r#"{"key":"private-1234"}"#;
        let metadata = store_credential(
            &store,
            &"acme".parse().unwrap(),
            &"app".parse().unwrap(),
            &"dev".parse().unwrap(),
            value,
        )
        .await
        .unwrap();
        assert_eq!(*store.0.lock().unwrap(), vec![value.as_bytes()]);
        assert_eq!(
            metadata.version.reference,
            format!("reference-{}", metadata.id)
        );
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

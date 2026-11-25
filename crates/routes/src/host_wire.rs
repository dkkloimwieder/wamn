//! Serialized values of the native host routes.

use schemars::JsonSchema;
use schemars::r#gen::{SchemaGenerator, SchemaSettings};
use schemars::schema::Schema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_registry::{EnvName, ProjectId};

mod refusals;
mod types;

pub use refusals::DeclaredRefusal;
pub use types::*;

pub(super) fn schemas<I: JsonSchema, O: JsonSchema>() -> (Value, Value) {
    (schema::<I>(), schema::<O>())
}

pub(super) fn schema<T: JsonSchema>() -> Value {
    let mut settings = SchemaSettings::draft07();
    settings.inline_subschemas = true;
    let mut schema = serde_json::to_value(T::json_schema(&mut settings.into_generator()))
        .expect("native JSON Schema serializes");
    openapi_schema(&mut schema);
    schema
}

fn nullable<T: JsonSchema>(generator: &mut SchemaGenerator) -> Schema {
    Option::<T>::json_schema(generator)
}

fn openapi_schema(schema: &mut Value) {
    // Represent unrestricted JSON as an empty schema for OpenAPI clients.
    if *schema == Value::Bool(true) {
        *schema = json!({});
    }
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    if object.get("type").is_some_and(|kind| {
        kind == "integer"
            || kind
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "integer"))
    }) {
        object.remove("format");
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for child in properties.values_mut() {
            openapi_schema(child);
        }
    }
    if let Some(items) = object.get_mut("items") {
        openapi_schema(items);
    }
    for keyword in ["oneOf", "anyOf", "allOf"] {
        if let Some(variants) = object.get_mut(keyword).and_then(Value::as_array_mut) {
            for variant in variants {
                openapi_schema(variant);
            }
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectCreateRequest {
    pub project: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectRequest {
    #[schemars(with = "String")]
    pub project: ProjectId,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentRequest {
    #[schemars(with = "String")]
    pub project: ProjectId,
    #[schemars(with = "String")]
    pub env: EnvName,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectCreated {
    #[schemars(with = "String")]
    pub project: ProjectId,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectStatus {
    #[schemars(with = "String")]
    pub project: ProjectId,
    pub status: &'static str,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentStatus {
    #[schemars(with = "String")]
    pub project: ProjectId,
    #[schemars(with = "String")]
    pub env: EnvName,
    pub status: &'static str,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionTest {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub error: Option<String>,
}

/// The project's connection summary after the control handler adds route context.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSummary {
    pub id: String,
    pub name: String,
    pub org: String,
    pub project: String,
    pub env: String,
    pub integration_type: String,
    pub auth_type: String,
    #[schemars(schema_with = "nullable::<String>")]
    pub external_system_id: Option<String>,
    pub target_id: String,
    #[schemars(schema_with = "nullable::<String>")]
    pub resource_key: Option<String>,
    #[schemars(schema_with = "nullable::<String>")]
    pub authority: Option<String>,
    pub active_generation: i64,
    pub service_principal_id: String,
    pub status: String,
    pub parameters: Value,
    #[schemars(schema_with = "nullable::<String>")]
    pub hint: Option<String>,
    #[schemars(schema_with = "nullable::<String>")]
    pub credential_created_at: Option<String>,
    #[schemars(schema_with = "nullable::<String>")]
    pub credential_created_by: Option<String>,
    #[schemars(schema_with = "nullable::<String>")]
    pub refresh_status: Option<String>,
    #[schemars(schema_with = "nullable::<String>")]
    pub expires_at: Option<String>,
    pub versions: Vec<i32>,
    pub deprecated_fields: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectionGetResult {
    #[serde(flatten)]
    pub connection: ConnectionSummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub redirect_uri: Option<String>,
}

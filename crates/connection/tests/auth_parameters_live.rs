//! Connection parameter validation with the system bootstrap's auth schemas.

use anyhow::Context as _;
use serde_json::{Value, json};
use wamn_connection::administration::validate_connection_parameters;
use wamn_test_infrastructure::{containers::start_postgres, identity};

#[tokio::test]
#[ignore = "requires: docker"]
async fn seeded_auth_schemas_refuse_undeclared_parameters() -> anyhow::Result<()> {
    let postgres = start_postgres().await?;
    let url = identity::system(&postgres).await;
    let mut system = wamn_postgres::connect(url.as_str()).await?;
    let auth_types: Vec<(String, Value)> =
        sqlx::query_as("SELECT code, parameter_schema FROM registry.auth_type ORDER BY code")
            .fetch_all(&mut system)
            .await?;
    assert!(!auth_types.is_empty(), "bootstrap must seed auth types");

    let integration_schemas = [json!({})];
    for (auth_type, schema) in auth_types {
        let properties = schema["properties"]
            .as_object()
            .with_context(|| format!("{auth_type} must declare its auth properties"))?;
        let mut parameters = json!({"auth": {}});
        for (key, property) in properties {
            parameters["auth"][key] = if let Some(example) = property["enum"].get(0) {
                example.clone()
            } else {
                match property["type"].as_str() {
                    Some("string") => json!("example"),
                    Some("array") => json!(["example"]),
                    _ => anyhow::bail!("no sample for {auth_type}.{key}"),
                }
            };
        }
        validate_connection_parameters(&parameters, None, &integration_schemas, &schema)
            .with_context(|| format!("{auth_type} must accept its declared auth parameters"))?;

        let unknown_key = "undeclared";
        assert!(!properties.contains_key(unknown_key), "{auth_type}");
        parameters["auth"][unknown_key] = json!("example");
        let error =
            validate_connection_parameters(&parameters, None, &integration_schemas, &schema)
                .expect_err(&format!(
                    "{auth_type} accepted an undeclared auth parameter"
                ));
        assert_eq!(
            (error.code, error.field),
            ("invalid_input", "parameters.auth"),
            "{auth_type}"
        );
    }
    Ok(())
}

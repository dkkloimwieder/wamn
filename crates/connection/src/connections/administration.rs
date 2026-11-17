//! Connection administration in one project database, with system-owned type metadata.

use serde_json::{Value, json};
use sqlx::Connection as _;
use uuid::Uuid;
use wamn_identity_protocol::PrincipalId;
use wamn_kubernetes::{Store, StoreVersion};

use super::{ConnectionError, refuse};

fn store_error(source: wamn_kubernetes::Error) -> ConnectionError {
    ConnectionError {
        code: "store_error",
        field: "credential",
        source: Some(Box::new(source)),
    }
}

/// Require one hostname or IPv4 address and port that the operator can admit.
pub fn validate_wasm_authority(value: &str) -> Result<(), ConnectionError> {
    use wash_runtime::host::allowed_hosts::AllowedHost;

    let Ok(AllowedHost::Authority(authority)) = value.parse::<AllowedHost>() else {
        return Err(refuse("invalid_input", "authority"));
    };
    if authority.host().contains(':') {
        return Err(refuse("ipv6_authority_not_supported", "authority"));
    }
    if authority.port_u16().is_none() || value.contains('@') || value.trim() != value {
        return Err(refuse("invalid_input", "authority"));
    }
    Ok(())
}

/// Validate the non-secret request shape before any database call.
pub fn validate_request(operation: &str, request: &Value) -> Result<(), ConnectionError> {
    let object = request
        .as_object()
        .ok_or_else(|| refuse("invalid_input", "value"))?;
    let allowed: &[&str] = match operation {
        "integration_type.list" => &["project"],
        "external_system.list" | "integration.list" | "connection.list" => &["project", "env"],
        "external_system.write"
        | "external_system.delete"
        | "integration.get"
        | "integration.enable"
        | "integration.disable"
        | "integration.delete"
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
        && (name.len() > 63
            || !name.as_bytes()[0].is_ascii_lowercase()
            || !name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
            }))
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

/// Execute against the selected project; source is the separately opened copy source.
pub async fn execute(
    project: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    system: &mut sqlx::PgConnection,
    caller: PrincipalId,
    org: &wamn_registry::OrgId,
    operation: &str,
    payload: &str,
    source: Option<&mut sqlx::PgConnection>,
    applications: &[wamn_catalog::application_config::ApplicationConfig],
    write_service: impl AsyncFnOnce(
        &mut sqlx::PgConnection,
        &mut sqlx::Transaction<'_, sqlx::Postgres>,
        &str,
        &str,
    ) -> anyhow::Result<PrincipalId>,
) -> Result<Value, ConnectionError> {
    let mut request: Value =
        serde_json::from_str(payload).map_err(|_| refuse("invalid_input", "value"))?;
    validate_request(operation, &request)?;
    if matches!(operation, "connection.delete" | "integration.delete") {
        let id: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM platform.connection WHERE name=$1")
                .bind(request["name"].as_str())
                .fetch_optional(&mut **project)
                .await?;
        if let Some(id) = id {
            super::references::refuse_application_references(applications, &id.to_string())?;
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
        let existing: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(c) FROM platform.connection c WHERE name=$1 FOR UPDATE",
        )
        .bind(request["name"].as_str())
        .fetch_optional(&mut **project)
        .await?;
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
        let metadata: Option<Value> = sqlx::query_scalar("SELECT to_jsonb(t)||jsonb_build_object('auth_schema',a.parameter_schema,'credential_schema',a.credential_schema) FROM registry.integration_type t JOIN registry.integration_type_auth allowed ON allowed.integration_type=t.code JOIN registry.auth_type a ON a.code=allowed.auth_type WHERE t.code=$1 AND a.code=$2")
            .bind(context["integration_type"].as_str()).bind(context["auth_type"].as_str()).fetch_optional(&mut *system).await?;
        let metadata = metadata.ok_or_else(|| refuse("invalid_input", "auth_type"))?;
        let target = if new {
            let mut tx = system.begin().await?;
            bind_actor(&mut tx, caller, operation).await?;
            let (target, target_type): (Uuid, String) = sqlx::query_as(
                "SELECT destination.id, destination.type FROM registry.environments e \
                 JOIN registry.target destination ON destination.id=e.target_id \
                 WHERE e.org=$1 AND e.env=$2",
            )
            .bind(org)
            .bind(request["env"].as_str())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| refuse("invalid_input", "target_id"))?;
            if metadata["exclusive"] == true && target_type == "cloud" {
                return Err(refuse(
                    "exclusive_connection_requires_local_target",
                    "integration_type",
                ));
            }
            tx.commit().await?;
            Some(target)
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
            validate_wasm_authority(
                authority.ok_or_else(|| refuse("invalid_input", "authority"))?,
            )?;
        } else if authority.is_some() {
            return Err(refuse("invalid_input", "authority"));
        }
        if !new && context["authority"].as_str() != authority {
            return Err(refuse("immutable_field", "authority"));
        }
        let versions: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(v) FROM registry.integration_type_version v WHERE integration_type=$1 ORDER BY version")
            .bind(context["integration_type"].as_str()).fetch_all(&mut *system).await?;
        let schemas: Vec<Value> = versions
            .iter()
            .map(|v| v["parameter_schema"].clone())
            .collect();
        if schemas.is_empty() {
            return Err(refuse("invalid_input", "integration_type"));
        }
        let previous: Option<(Value,Option<Uuid>)> = sqlx::query_as("SELECT parameters,credential_id FROM platform.connection_generation WHERE connection_id=$1 AND generation=$2")
            .bind(id).bind(context["active_generation"].as_i64()).fetch_optional(&mut **project).await?;
        if operation == "connection.copy" {
            let source = source.ok_or_else(|| refuse("invalid_input", "source_env"))?;
            let copied: Option<(String,String,Value,Option<String>)> = sqlx::query_as("SELECT c.integration_type,c.auth_type,g.parameters,c.authority FROM platform.connection c JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) WHERE c.name=$1")
                .bind(request["name"].as_str()).fetch_optional(source).await?;
            let (kind, auth, parameters, source_authority) =
                copied.ok_or_else(|| refuse("not_found", "connection"))?;
            if context["integration_type"] != kind || context["auth_type"] != auth {
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
        let published: Value = versions
            .iter()
            .map(|v| (v["version"].to_string(), v["parameter_schema"].clone()))
            .collect();
        for schema in required_connection_versions(
            system,
            org,
            &request["project"]
                .as_str()
                .expect("validated project")
                .parse()
                .map_err(|_| refuse("invalid_input", "project"))?,
            &request["env"]
                .as_str()
                .expect("validated env")
                .parse()
                .map_err(|_| refuse("invalid_input", "env"))?,
            request["name"].as_str().expect("validated name"),
            &published,
        )
        .await?
        {
            validate_version_parameters(&request["parameters"], &schema)?;
        }
        let external: Option<Uuid> = if let Some(name) = request["external_system"].as_str() {
            Some(
                sqlx::query_scalar("SELECT id FROM platform.external_system WHERE name=$1")
                    .bind(name)
                    .fetch_optional(&mut **project)
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
            Some(write_service(
                system,
                project,
                &format!("connection-{id}"),
                request["name"].as_str().expect("validated connection name"),
            )
            .await
            .map_err(|source| ConnectionError {
                code: "database_error",
                field: "principal_id",
                source: Some(source.into_boxed_dyn_error()),
            })?)
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
            let store =
                wamn_kubernetes::kubernetes::KubernetesStore::in_cluster().map_err(store_error)?;
            Some(store_credential(&store, &target, value).await?)
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
                    let value: Option<String> = sqlx::query_scalar("SELECT $1::jsonb ->> $2::text")
                        .bind(&request["parameters"])
                        .bind(field)
                        .fetch_one(&mut **project)
                        .await?;
                    let value = value.ok_or_else(|| refuse("invalid_input", "resource_key"))?;
                    *resource = resource.replace(&format!("{{{field}}}"), &value);
                }
            }
            sqlx::query("INSERT INTO platform.connection(id,name,integration_type,auth_type,external_system_id,target_id,resource_key,active_generation,service_principal_id,authority) VALUES($1,$2,$3,$4,$5,$6,$7,1,$8,$9)")
                .bind(id).bind(request["name"].as_str()).bind(context["integration_type"].as_str()).bind(auth).bind(external).bind(target).bind(resource).bind(principal).bind(authority).execute(&mut **project).await.map_err(connection_write_error)?;
        }
        let next: i64=sqlx::query_scalar("SELECT coalesce(max(generation),0)+1 FROM platform.connection_generation WHERE connection_id=$1").bind(id).fetch_one(&mut **project).await?;
        if let Some(stored) = &stored {
            sqlx::query("INSERT INTO platform.credential(id,connection_id,store,ref,version,hint) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(credential_id).bind(id).bind(stored["store"].as_str()).bind(stored["ref"].as_str()).bind(stored["version"].as_str()).bind(stored["hint"].as_str()).execute(&mut **project).await?;
        }
        if previous.as_ref().map(|p| &p.0["auth"]) != Some(&request["parameters"]["auth"])
            || (stored.is_some()
                && matches!(
                    auth,
                    "oauth2_client_credentials" | "oauth2_client_credentials_jwt"
                ))
        {
            sqlx::query("DELETE FROM platform.oauth_token WHERE connection_id=$1")
                .bind(id)
                .execute(&mut **project)
                .await?;
        } else if stored.is_some() && auth == "oauth2_authorization_code" {
            sqlx::query(
                "UPDATE platform.oauth_token SET expires_at='-infinity' WHERE connection_id=$1",
            )
            .bind(id)
            .execute(&mut **project)
            .await?;
        }
        sqlx::query("DELETE FROM platform.oauth_authorization WHERE connection_id=$1")
            .bind(id)
            .execute(&mut **project)
            .await?;
        sqlx::query("INSERT INTO platform.connection_generation(connection_id,generation,parameters,credential_id) VALUES($1,$2,$3,$4)")
            .bind(id).bind(next).bind(&request["parameters"]).bind(credential_id).execute(&mut **project).await?;
        sqlx::query("UPDATE platform.connection SET active_generation=$2 WHERE id=$1")
            .bind(id)
            .bind(next)
            .execute(&mut **project)
            .await?;
        return summary(project, system, org, &request, id).await;
    }
    configure(project, system, org, operation, &request).await
}

fn connection_write_error(error: sqlx::Error) -> ConnectionError {
    if error
        .as_database_error()
        .is_some_and(sqlx::error::DatabaseError::is_unique_violation)
    {
        refuse("resource_in_use", "resource_key")
    } else {
        error.into()
    }
}

async fn bind_actor(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    caller: PrincipalId,
    operation: &str,
) -> Result<(), ConnectionError> {
    sqlx::query("INSERT INTO platform.tx_identity(principal_id,role,application,operation) VALUES($1,'','',$2)").bind(caller).bind(operation).execute(&mut **tx).await?;
    Ok(())
}

/// Read system type metadata and project rows without a system connection function.
pub async fn configure(
    project: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    system: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    operation: &str,
    request: &Value,
) -> Result<Value, ConnectionError> {
    if request.get("credential").is_some() {
        return Err(refuse("invalid_input", "credential"));
    }
    if operation == "integration_type.list" {
        return Ok(
            sqlx::query_scalar("SELECT registry.configure_integration_type($1,$2,$3)")
                .bind(org)
                .bind(operation)
                .bind(request)
                .fetch_one(system)
                .await?,
        );
    }
    let name = request["name"].as_str();
    if operation.starts_with("external_system.") {
        return match operation {
            "external_system.list" => Ok(sqlx::query_scalar("SELECT jsonb_build_object('external_systems',coalesce(jsonb_agg(to_jsonb(s) ORDER BY name),'[]'::jsonb)) FROM platform.external_system s").fetch_one(&mut **project).await?),
            "external_system.write" => { sqlx::query("INSERT INTO platform.external_system(name) VALUES($1) ON CONFLICT(name) DO NOTHING").bind(name).execute(&mut **project).await?; Ok(json!({"name":name})) },
            "external_system.delete" => {
                let id: Uuid=sqlx::query_scalar("SELECT id FROM platform.external_system WHERE name=$1 FOR UPDATE").bind(name).fetch_optional(&mut **project).await?.ok_or_else(||refuse("not_found","name"))?;
                super::references::refuse_environment_references(project,"external_system",&id.to_string()).await?;
                sqlx::query("DELETE FROM platform.external_system WHERE id=$1").bind(id).execute(&mut **project).await?;
                Ok(json!({"name":name}))
            },
            _ => Err(refuse("invalid_input","operation")),
        };
    }
    if matches!(operation, "connection.list" | "integration.list") {
        let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM platform.connection ORDER BY name")
            .fetch_all(&mut **project)
            .await?;
        let mut rows = Vec::new();
        for id in ids {
            rows.push(summary(project, system, org, request, id).await?);
        }
        return Ok(if operation == "connection.list" {
            json!({"connections":rows})
        } else {
            json!({"integrations":rows})
        });
    }
    let lookup = if operation == "connection.test" {
        "SELECT id FROM platform.connection WHERE name=$1"
    } else {
        "SELECT id FROM platform.connection WHERE name=$1 FOR NO KEY UPDATE"
    };
    let id: Uuid = sqlx::query_scalar(lookup)
        .bind(name)
        .fetch_optional(&mut **project)
        .await?
        .ok_or_else(|| refuse("not_found", "connection"))?;
    let mut row = summary(project, system, org, request, id).await?;
    match operation {
        "connection.get" | "integration.get" => Ok(row),
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
            let metadata: Value=sqlx::query_scalar("SELECT jsonb_build_object('runtime_type',t.runtime_type,'auth_schema',a.parameter_schema,'credential_schema',a.credential_schema,'versions',(SELECT jsonb_object_agg(v.version,to_jsonb(v)) FROM registry.integration_type_version v WHERE v.integration_type=t.code),'schemas',(SELECT jsonb_agg(parameter_schema ORDER BY version) FROM registry.integration_type_version WHERE integration_type=t.code)) FROM registry.integration_type t CROSS JOIN registry.auth_type a WHERE t.code=$1 AND a.code=$2")
                .bind(row["integration_type"].as_str()).bind(row["auth_type"].as_str()).fetch_one(system).await?;
            for (key, value) in metadata.as_object().expect("object") {
                row[key] = value.clone();
            }
            row["credential"] = credential_reference(project, id).await?;
            Ok(row)
        }
        "connection.enable"
        | "integration.enable"
        | "connection.disable"
        | "integration.disable" => {
            sqlx::query("UPDATE platform.connection SET status=$2 WHERE id=$1")
                .bind(id)
                .bind(if operation.ends_with(".enable") {
                    "active"
                } else {
                    "disabled"
                })
                .execute(&mut **project)
                .await
                .map_err(connection_write_error)?;
            Ok(
                json!({"id":id,"name":name,"env":request["env"],"principal_id":row["service_principal_id"]}),
            )
        }
        "connection.delete" | "integration.delete" => {
            super::references::refuse_environment_references(
                project,
                "connection",
                &id.to_string(),
            )
            .await?;
            let credentials: Value=sqlx::query_scalar("SELECT coalesce(jsonb_agg(jsonb_build_object('store',store,'ref',ref,'version',version)),'[]'::jsonb) FROM platform.credential WHERE connection_id=$1").bind(id).fetch_one(&mut **project).await?;
            sqlx::query("DELETE FROM platform.connection WHERE id=$1")
                .bind(id)
                .execute(&mut **project)
                .await?;
            Ok(
                json!({"id":id,"name":name,"env":request["env"],"principal_id":row["service_principal_id"],"credentials":credentials}),
            )
        }
        _ => Err(refuse("invalid_input", "operation")),
    }
}

async fn credential_reference(
    project: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
) -> Result<Value, ConnectionError> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('store',k.store,'ref',k.ref,'version',k.version) FROM platform.connection c JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) JOIN platform.credential k ON k.id=g.credential_id WHERE c.id=$1").bind(id).fetch_optional(&mut **project).await?.unwrap_or(Value::Null))
}

async fn summary(
    project: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    system: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    request: &Value,
    id: Uuid,
) -> Result<Value, ConnectionError> {
    let mut row: Value=sqlx::query_scalar("SELECT to_jsonb(c)||jsonb_build_object('parameters',g.parameters,'hint',k.hint,'credential_created_at',k.created_at,'credential_created_by',k.created_by,'refresh_status',o.refresh_status,'expires_at',o.expires_at) FROM platform.connection c JOIN platform.connection_generation g ON (g.connection_id,g.generation)=(c.id,c.active_generation) LEFT JOIN platform.credential k ON k.id=g.credential_id LEFT JOIN platform.oauth_token o ON o.connection_id=c.id WHERE c.id=$1")
        .bind(id).fetch_one(&mut **project).await?;
    let metadata: Value=sqlx::query_scalar("SELECT jsonb_build_object('versions',coalesce(jsonb_agg(version ORDER BY version),'[]'::jsonb),'deprecated_fields',(SELECT coalesce(jsonb_agg(DISTINCT f.key),'[]'::jsonb) FROM registry.integration_type_version v,jsonb_each(v.parameter_schema->'properties') f WHERE v.integration_type=$1 AND f.value->>'deprecated'='true')) FROM registry.integration_type_version WHERE integration_type=$1")
        .bind(row["integration_type"].as_str()).fetch_one(system).await?;
    row["versions"] = metadata["versions"].clone();
    row["deprecated_fields"] = metadata["deprecated_fields"].clone();
    row["org"] = json!(org);
    row["project"] = request["project"].clone();
    row["env"] = request["env"].clone();
    Ok(row)
}

async fn store_credential(
    store: &dyn Store,
    target: &wamn_registry::Triple,
    value: &str,
) -> Result<Value, ConnectionError> {
    let id = uuid::Uuid::new_v4();
    let StoreVersion {
        store,
        reference,
        version,
    } = store
        .put(target, id, value.as_bytes())
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

/// Validate edits against the registry's desired connection selections.
pub async fn required_connection_versions(
    system: &mut sqlx::PgConnection,
    org: &wamn_registry::OrgId,
    project: &wamn_registry::ProjectId,
    env: &wamn_registry::EnvName,
    name: &str,
    published: &Value,
) -> Result<Vec<Value>, ConnectionError> {
    let versions: Vec<i32> = sqlx::query_scalar("SELECT DISTINCT version FROM registry.environment_integrations WHERE org=$1 AND project=$2 AND env=$3 AND name=$4 UNION SELECT DISTINCT connection_version FROM registry.environment_application_connections WHERE org=$1 AND project=$2 AND env=$3 AND connection_name=$4 AND connection_version IS NOT NULL")
        .bind(org).bind(project).bind(env).bind(name).fetch_all(system).await?;
    versions
        .into_iter()
        .map(|version| {
            published
                .get(version.to_string())
                .cloned()
                .ok_or_else(|| refuse("invalid_input", "version"))
        })
        .collect()
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
    impl Store for RecordingStore {
        async fn put(
            &self,
            _target: &wamn_registry::Triple,
            id: uuid::Uuid,
            value: &[u8],
        ) -> Result<StoreVersion, wamn_kubernetes::Error> {
            self.0.lock().unwrap().push(value.to_vec());
            Ok(StoreVersion {
                store: "k8s",
                reference: format!("reference-{id}"),
                version: "1".into(),
            })
        }
        async fn get(&self, _: &str, _: &str) -> Result<Vec<u8>, wamn_kubernetes::Error> {
            Ok(Vec::new())
        }
        async fn destroy(&self, _: &str, _: &str) -> Result<(), wamn_kubernetes::Error> {
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

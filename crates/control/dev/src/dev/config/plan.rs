use super::{
    ADMINISTRATION_DATABASE_URL, CATALOG, CDC_READER, CONDITION_BEARER_TOKEN, CdcReader,
    DOCUMENT_KEY, DUP_WINDOW_SECS, DatabaseIdentity, DevActivationIdentity, DevConfig,
    DevConfigDocument, DevConfigError, DevConfigErrorType, ENVIRONMENT,
    EVENT_MATERIALIZER_DATABASE_URL, EXECUTOR_PLATFORM_DATABASE_URL, GUEST_DATABASE_URL,
    HOST_BINARY, HOST_CONFIG_FILE, HOST_GROUP, HOST_NAME, HTTP_ADMITTER_DATABASE_URL,
    IDENTITY_DATABASE_URL, LOCAL_ARTIFACTS, LocalArtifacts, NATS_TLS_CA, NATS_TLS_CERT,
    NATS_TLS_KEY, ORG, OTEL_EXPORTER_OTLP_ENDPOINT, PACKAGE_SOURCES, PLATFORM_DOMAIN,
    POSTGRES_SYSTEM_DATABASES, PROJECT, PackageSource, PathBuf, ROUTE_HOST, RUNNER,
    ReachabilityProbe, SCHEDULER_NATS_URL, STREAM_REPLICAS, SYSTEM_DATABASE_URL, SessionIdentity,
    TARGET_DATABASE_ACL_FILE, TARGET_DATABASE_URL, TARGET_PRIVILEGES_FILE,
    TARGET_TEMPLATE_DATABASE, Url, Value, WASMTIME_CACHE_DIR,
};

/// Language-neutral JSON Schema generated from the strict `dev.json` input type.
pub fn dev_config_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(DevConfigDocument)).expect("schema serializes")
}

/// Byte-stable pretty JSON Schema generated from the strict `dev.json` input type.
pub fn dev_config_schema_bytes() -> Vec<u8> {
    let mut bytes =
        serde_json::to_vec_pretty(&dev_config_schema()).expect("dev config schema serializes");
    bytes.push(b'\n');
    bytes
}

pub(super) fn package_source_matches(
    source: &PackageSource,
    dependency: &wamn_compiler::manifest::BaseDependencyRequirement,
) -> bool {
    source.manifest.package.id == dependency.package
        && source.manifest.package.version == dependency.version
}

pub(super) fn validate_config_document_shape(
    object: &serde_json::Map<String, Value>,
) -> Result<(), DevConfigError> {
    let schema = dev_config_schema();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .expect("derived dev config schema has object properties");
    for key in object.keys() {
        if !properties.contains_key(key) {
            return Err(DevConfigError::new(
                DevConfigErrorType::UnknownKey,
                key.as_str(),
                "remove the unknown key",
            ));
        }
    }
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .expect("derived dev config schema names required properties");
    for key in required {
        let key = key
            .as_str()
            .expect("derived dev config required properties are strings");
        if !object.contains_key(key) {
            return Err(DevConfigError::new(
                DevConfigErrorType::MissingKey,
                key,
                "supply the required deployment value",
            ));
        }
    }
    for (key, value) in object {
        if key == "session_identity" {
            serde_json::from_value::<Option<SessionIdentity>>(value.clone()).map_err(|_| {
                DevConfigError::new(
                    DevConfigErrorType::InvalidValue,
                    key.as_str(),
                    "session identity requires issuer, ca, and instance_suffix",
                )
            })?;
            continue;
        }
        if key == CDC_READER {
            serde_json::from_value::<CdcReader>(value.clone()).map_err(|_| {
                DevConfigError::new(
                    DevConfigErrorType::InvalidValue,
                    key.as_str(),
                    "the CDC reader requires exactly its declared fields",
                )
            })?;
            continue;
        }
        if key == LOCAL_ARTIFACTS {
            serde_json::from_value::<LocalArtifacts>(value.clone()).map_err(|_| {
                DevConfigError::new(
                    DevConfigErrorType::InvalidValue,
                    key.as_str(),
                    "local artifacts require only directory, flow_http_component and materializer_component paths",
                )
            })?;
            continue;
        }
        if !json_value_matches_schema(
            properties
                .get(key)
                .expect("unknown properties were refused above"),
            value,
        ) {
            return Err(DevConfigError::new(
                DevConfigErrorType::InvalidValue,
                key.as_str(),
                "value does not match the generated dev config schema",
            ));
        }
    }
    Ok(())
}

pub(super) fn json_value_matches_schema(schema: &Value, value: &Value) -> bool {
    match schema.get("type").and_then(Value::as_str) {
        Some("string") => value.is_string(),
        Some("boolean") => value.is_boolean(),
        Some("integer") => value.as_u64().is_some_and(|number| {
            if schema.get("format").and_then(Value::as_str) == Some("uint32")
                && u32::try_from(number).is_err()
            {
                return false;
            }
            let number = value
                .as_f64()
                .expect("unsigned JSON integer has a numeric value");
            schema
                .get("minimum")
                .and_then(Value::as_f64)
                .is_none_or(|minimum| number >= minimum)
                && schema
                    .get("maximum")
                    .and_then(Value::as_f64)
                    .is_none_or(|maximum| number <= maximum)
        }),
        Some("array") => value.as_array().is_some_and(|values| {
            schema.get("items").is_some_and(|item| {
                values
                    .iter()
                    .all(|value| json_value_matches_schema(item, value))
            })
        }),
        _ => false,
    }
}

pub(super) fn nonempty_string(
    value: String,
    key: &'static str,
) -> Result<Box<str>, DevConfigError> {
    validate_nonempty_string(&value, key)?;
    Ok(value.into_boxed_str())
}

pub(super) fn validate_nonempty_string(
    value: &str,
    key: &'static str,
) -> Result<(), DevConfigError> {
    if value.is_empty() {
        return Err(DevConfigError::new(
            DevConfigErrorType::InvalidValue,
            key,
            "expected a non-empty string",
        ));
    }
    Ok(())
}

pub(super) fn nonempty_path(value: PathBuf, key: &'static str) -> Result<PathBuf, DevConfigError> {
    if value.as_os_str().is_empty() {
        return Err(DevConfigError::new(
            DevConfigErrorType::InvalidValue,
            key,
            "expected a non-empty path",
        ));
    }
    Ok(value)
}

pub(super) fn validate_route_host(route_host: &str) -> Result<(), DevConfigError> {
    if route_host != "*"
        && (route_host.contains('/') || route_host.chars().any(char::is_whitespace))
    {
        return Err(DevConfigError::new(
            DevConfigErrorType::InvalidValue,
            ROUTE_HOST,
            "expected a hostname without a path or whitespace",
        ));
    }
    Ok(())
}

/// Parse one strict deployment-owned JSON document.
pub(super) fn parse_config(bytes: &[u8]) -> Result<DevConfig, DevConfigError> {
    let document: Value = serde_json::from_slice(bytes).map_err(|source| {
        DevConfigError::new(
            DevConfigErrorType::MalformedDocument,
            DOCUMENT_KEY,
            "expected one JSON object",
        )
        .with_source(source)
    })?;
    let object = document.as_object().ok_or_else(|| {
        DevConfigError::new(
            DevConfigErrorType::MalformedDocument,
            DOCUMENT_KEY,
            "expected one JSON object",
        )
    })?;
    validate_config_document_shape(object)?;
    let input: DevConfigDocument = serde_json::from_value(document).map_err(|source| {
        DevConfigError::new(
            DevConfigErrorType::MalformedDocument,
            DOCUMENT_KEY,
            "document disagrees with its generated schema",
        )
        .with_source(source)
    })?;
    let DevConfigDocument {
        target_database_url,
        target_privileges_file,
        target_template_database,
        target_database_acl_file,
        system_database_url,
        identity_database_url,
        connection_database_url,
        session_identity,
        guest_database_url,
        executor_platform_database_url,
        http_admitter_database_url,
        event_materializer_database_url,
        administration_database_url,
        scheduler_nats_url,
        nats_tls_ca,
        nats_tls_cert,
        nats_tls_key,
        stream_replicas,
        dup_window_secs,
        otel_exporter_otlp_endpoint,
        local_artifacts,
        condition_bearer_token,
        route_host,
        platform_domain,
        package_sources,
        catalog,
        environment,
        org,
        project,
        host_group,
        host_name,
        runner,
        host_binary,
        wasmtime_cache_dir,
        host_config_file,
        cdc_reader,
    } = input;

    let target_database_url = nonempty_string(target_database_url, TARGET_DATABASE_URL)?;
    let system_database_url = nonempty_string(system_database_url, SYSTEM_DATABASE_URL)?;
    let identity_database_url = nonempty_string(identity_database_url, IDENTITY_DATABASE_URL)?;
    let guest_database_url = nonempty_string(guest_database_url, GUEST_DATABASE_URL)?;
    let executor_platform_database_url = nonempty_string(
        executor_platform_database_url,
        EXECUTOR_PLATFORM_DATABASE_URL,
    )?;
    let http_admitter_database_url =
        nonempty_string(http_admitter_database_url, HTTP_ADMITTER_DATABASE_URL)?;
    let event_materializer_database_url = nonempty_string(
        event_materializer_database_url,
        EVENT_MATERIALIZER_DATABASE_URL,
    )?;
    let administration_database_url =
        nonempty_string(administration_database_url, ADMINISTRATION_DATABASE_URL)?;
    let scheduler_nats_url = nonempty_string(scheduler_nats_url, SCHEDULER_NATS_URL)?;
    let certificate_path = |path, key| {
        let path = nonempty_path(path, key)?;
        if path.is_absolute() {
            Ok(path)
        } else {
            Err(DevConfigError::new(
                DevConfigErrorType::InvalidValue,
                key,
                "the NATS certificate paths must be absolute",
            ))
        }
    };
    let nats_tls_ca = certificate_path(nats_tls_ca, NATS_TLS_CA)?;
    let nats_tls_cert = certificate_path(nats_tls_cert, NATS_TLS_CERT)?;
    let nats_tls_key = certificate_path(nats_tls_key, NATS_TLS_KEY)?;
    if !(1..=5).contains(&stream_replicas) {
        return Err(DevConfigError::new(
            DevConfigErrorType::InvalidValue,
            STREAM_REPLICAS,
            "NATS stream copies must be between one and five",
        ));
    }
    if dup_window_secs == 0 {
        return Err(DevConfigError::new(
            DevConfigErrorType::InvalidValue,
            DUP_WINDOW_SECS,
            "source-stream duplicate window must be positive",
        ));
    }
    let otel_exporter_otlp_endpoint =
        nonempty_string(otel_exporter_otlp_endpoint, OTEL_EXPORTER_OTLP_ENDPOINT)?;
    for path in [
        &local_artifacts.directory,
        &local_artifacts.flow_http_component,
        &local_artifacts.materializer_component,
    ] {
        if !path.is_absolute() {
            return Err(DevConfigError::new(
                DevConfigErrorType::InvalidValue,
                LOCAL_ARTIFACTS,
                "local artifact paths must be absolute",
            ));
        }
    }
    let target_privileges_file = nonempty_path(target_privileges_file, TARGET_PRIVILEGES_FILE)?;
    let target_template_database =
        nonempty_string(target_template_database, TARGET_TEMPLATE_DATABASE)?;
    let target_database_acl_file =
        nonempty_path(target_database_acl_file, TARGET_DATABASE_ACL_FILE)?;
    let condition_bearer_token = nonempty_string(condition_bearer_token, CONDITION_BEARER_TOKEN)?;
    let route_host = nonempty_string(route_host, ROUTE_HOST)?;
    wamn_control_provision::validate_platform_domain(&platform_domain).map_err(|source| {
        DevConfigError::new(
            DevConfigErrorType::InvalidValue,
            PLATFORM_DOMAIN,
            "expected a domain name",
        )
        .with_source(source)
    })?;
    let package_sources = package_sources
        .into_iter()
        .map(|root| nonempty_path(root, PACKAGE_SOURCES))
        .collect::<Result<Vec<_>, _>>()?
        .into_boxed_slice();
    for (key, value) in [
        (CATALOG, catalog.as_str()),
        (ENVIRONMENT, environment.as_str()),
        (ORG, org.as_str()),
        (PROJECT, project.as_str()),
        (HOST_GROUP, host_group.as_str()),
        (HOST_NAME, host_name.as_str()),
        (RUNNER, runner.as_str()),
    ] {
        validate_nonempty_string(value, key)?;
    }
    // The project is the environment coordinate's (owner ruling of 2026-10-09
    // on `wamn-snz0`): the host derives the same value from its own target.
    let activation_identity = DevActivationIdentity {
        catalog,
        environment,
        org,
        project,
        host_group,
        host_name,
        runner,
    };
    let host_binary = nonempty_path(host_binary, HOST_BINARY)?;
    let wasmtime_cache_dir = nonempty_path(wasmtime_cache_dir, WASMTIME_CACHE_DIR)?;
    if !host_config_file.is_absolute() {
        return Err(DevConfigError::new(
            DevConfigErrorType::InvalidValue,
            HOST_CONFIG_FILE,
            "the host configuration file path must be absolute",
        ));
    }
    if let Some(reader) = &cdc_reader {
        let paths = [&reader.binary, &reader.cdc_sql_file];
        if paths.iter().any(|path| !path.is_absolute())
            || [&reader.system_database_url, &reader.cdc_url]
                .iter()
                .any(|value| value.is_empty())
        {
            return Err(DevConfigError::new(
                DevConfigErrorType::InvalidValue,
                CDC_READER,
                "the CDC reader needs absolute paths and nonempty URLs and username",
            ));
        }
    }

    validate_route_host(&route_host)?;

    Ok(DevConfig {
        target_database_url,
        target_privileges_file,
        target_template_database,
        target_database_acl_file,
        system_database_url,
        identity_database_url,
        connection_database_url,
        session_identity,
        guest_database_url,
        executor_platform_database_url,
        http_admitter_database_url,
        event_materializer_database_url,
        administration_database_url,
        scheduler_nats_url,
        nats_tls_ca,
        nats_tls_cert,
        nats_tls_key,
        stream_replicas,
        dup_window_secs,
        otel_exporter_otlp_endpoint,
        local_artifacts,
        condition_bearer_token,
        route_host,
        platform_domain: platform_domain.into_boxed_str(),
        package_sources,
        activation_identity,
        host_binary,
        wasmtime_cache_dir,
        host_config_file,
        cdc_reader,
        probes: Box::new([]),
    })
}

pub(super) fn validate_session_identity(config: &DevConfig) -> Result<(), DevConfigError> {
    if let Some(identity) = &config.session_identity {
        let valid =
            wamn_control_provision::identity_role::validate_identity_issuer(&identity.issuer)
                .is_ok()
                && wamn_control_provision::session_target::session_audience(
                    &wamn_registry::Triple::new(
                        &config.activation_identity.org,
                        config.activation_identity.environment.as_str(),
                        &config.activation_identity.project,
                    ),
                    &identity.instance_suffix,
                )
                .is_ok()
                && identity.ca.is_absolute();
        if !valid {
            return Err(DevConfigError::new(
                DevConfigErrorType::MalformedDocument,
                "session_identity",
                "issuer, instance suffix, or absolute CA path refused",
            ));
        }
    }
    Ok(())
}

/// Refuse a target database the development loop must not drop.
///
/// The loop recreates the target when its schema inputs change, so a system
/// database named as the target would be destroyed by the first run.
pub(super) fn validate_disposable_target_database(
    probe: &ReachabilityProbe,
    target: &DatabaseIdentity,
) -> Result<(), DevConfigError> {
    if POSTGRES_SYSTEM_DATABASES.contains(&target.database.as_ref()) {
        return Err(DevConfigError::endpoint(
            DevConfigErrorType::DatabaseCollision,
            TARGET_DATABASE_URL,
            probe.sanitized_endpoint.clone(),
            "set target_database_url to a disposable database other than postgres, template0, or template1",
        ));
    }
    Ok(())
}

pub(super) fn validate_runtime_database_credentials(
    privileged: &[(&ReachabilityProbe, &DatabaseIdentity)],
    runtime: &[(&ReachabilityProbe, &DatabaseIdentity)],
) -> Result<(), DevConfigError> {
    for (index, (probe, identity)) in runtime.iter().enumerate() {
        let collides_with_privileged = privileged
            .iter()
            .any(|(_, privileged)| identity.same_credential(privileged));
        let collides_with_runtime = runtime[..index]
            .iter()
            .any(|(_, prior)| identity.same_credential(prior));
        if collides_with_privileged || collides_with_runtime {
            return Err(DevConfigError::endpoint(
                DevConfigErrorType::DatabaseCollision,
                probe.key,
                probe.sanitized_endpoint.clone(),
                "runtime role must use its own database credential",
            ));
        }
    }
    Ok(())
}

pub(super) fn url_probe(
    key: &'static str,
    raw: &str,
    schemes: &[&str],
    default_port: u16,
    include_path: bool,
) -> Result<ReachabilityProbe, DevConfigError> {
    let parsed = parse_url(key, raw, schemes)?;
    probe_from_url(key, &parsed, default_port, include_path)
}

pub(super) fn parse_url(
    key: &'static str,
    raw: &str,
    schemes: &[&str],
) -> Result<Url, DevConfigError> {
    let parsed = Url::parse(raw).map_err(|source| {
        DevConfigError::endpoint(
            DevConfigErrorType::InvalidValue,
            key,
            "<malformed>",
            "endpoint URL is malformed",
        )
        .with_source(source)
    })?;
    if !schemes.contains(&parsed.scheme()) {
        return Err(DevConfigError::endpoint(
            DevConfigErrorType::InvalidValue,
            key,
            sanitized_url(&parsed, true, 0),
            "endpoint URL uses an unsupported scheme",
        ));
    }
    Ok(parsed)
}

pub(super) fn probe_from_url(
    key: &'static str,
    parsed: &Url,
    default_port: u16,
    include_path: bool,
) -> Result<ReachabilityProbe, DevConfigError> {
    let host = parsed.host_str().ok_or_else(|| {
        DevConfigError::endpoint(
            DevConfigErrorType::InvalidValue,
            key,
            "<malformed>",
            "endpoint URL has no host",
        )
    })?;
    let port = parsed.port().unwrap_or_else(|| {
        if parsed.scheme() == "http" {
            80
        } else {
            default_port
        }
    });
    Ok(ReachabilityProbe {
        key,
        host: host.into(),
        port,
        sanitized_endpoint: sanitized_url(parsed, include_path, port).into(),
    })
}

pub(super) fn sanitized_url(parsed: &Url, include_path: bool, default_port: u16) -> String {
    let Some(host) = parsed.host_str() else {
        return "<malformed>".to_owned();
    };
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let port = parsed.port().unwrap_or(default_port);
    let path = if include_path { parsed.path() } else { "" };
    format!("{}://{host}:{port}{path}", parsed.scheme())
}

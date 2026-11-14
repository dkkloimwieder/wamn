use super::{
    DeserializeOwned, DevActivationError, DevActivationErrorType, DevActivationIdentity,
    DevActivationRequest, FLOW_HTTP_NAME, FLOW_HTTP_WORKLOAD_ID, HOST_HOME, HOST_SHUTDOWN_TIMEOUT,
    HashMap, HostProcessSpec, LOOPBACK_HTTP_BIND, OPERATOR_API_PREFIX, v2,
};

pub(super) fn validate_request(
    request: &DevActivationRequest<'_>,
) -> Result<(), DevActivationError> {
    let identity = request.identity;
    for (field, value) in [
        ("catalog", identity.catalog.as_str()),
        ("environment", identity.environment.as_str()),
        ("org", identity.org.as_str()),
        ("project", identity.project.as_str()),
        ("host-group", identity.host_group.as_str()),
        ("host-name", identity.host_name.as_str()),
        ("runner", identity.runner.as_str()),
    ] {
        if value.is_empty() {
            return Err(DevActivationError::new(
                DevActivationErrorType::InvalidInput,
                "validate-identity",
                format!("supply the required activation identity {field}"),
            ));
        }
    }
    if request.release.artifact_base.is_empty() {
        return Err(DevActivationError::new(
            DevActivationErrorType::InvalidInput,
            "validate-release",
            "supply the release artifact base",
        ));
    }
    if request.host_binary.as_os_str().is_empty() {
        return Err(DevActivationError::new(
            DevActivationErrorType::InvalidInput,
            "validate-host",
            "supply the wamn-host executable path",
        ));
    }
    if request.wasmtime_cache_dir.as_os_str().is_empty() {
        return Err(DevActivationError::new(
            DevActivationErrorType::InvalidInput,
            "validate-host",
            "supply the Wasmtime cache directory",
        ));
    }
    Ok(())
}

/// The host namespace of the local host (docs/architecture/overview.md#environment-lifecycle):
/// what it reads from `WASMCLOUD_HOST_ENVIRONMENT`, names in its heartbeat,
/// and where its workloads are placed. A local session has no Kubernetes
/// namespace of its own, so its workloads take the environment name.
pub(super) fn host_namespace(identity: &DevActivationIdentity) -> &str {
    &identity.environment
}

/// The identity fields the host injects into the flow-http workload, paired
/// with the field of [`DevActivationIdentity`] each one carries.
///
/// This is the SINGLE SOURCE for the identity set. It exists because the set was
/// previously written out by hand in two places -- here and in the cluster
/// journey's workload renderer -- and the second copy had four of the five.
/// `wamn.catalog` was left at the template's placeholder, with no identity test, no
/// substitution and no assertion, so a second application would have deployed
/// under the first one's catalog and route resolution would have looked in
/// the wrong one. An enumeration maintained by hand on both sides misses
/// whatever the enumeration missed; nothing compared the two.
///
/// `deploy/platform/http-route-workload.example.yaml` is held equal to this
/// by a test in this module, so the template gaining or losing an identity fails
/// here rather than in a cluster.
pub fn flow_http_identity_fields(identity: &DevActivationIdentity) -> [(&'static str, String); 4] {
    [
        ("wamn.project_env_key", identity.project_env_key()),
        ("wamn.catalog", identity.catalog.clone()),
        ("wamn.environment", identity.environment.clone()),
        ("wamn.project", identity.project.clone()),
    ]
}

pub(super) fn flow_http_request(request: &DevActivationRequest<'_>) -> v2::WorkloadStartRequest {
    let platform: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "../../../../../../deploy/platform/release/values.yaml"
    ))
    .expect("the platform chart defaults are valid YAML");
    let http = &platform["platform"]["roles"]["http"];
    let identity = request.identity;
    let config = flow_http_identity_fields(identity)
        .into_iter()
        .map(|(identity, value)| (identity.to_owned(), value))
        .collect::<HashMap<_, _>>();
    let local_resources = v2::LocalResources {
        memory_limit_mb: 0,
        cpu_limit: 0,
        config,
        environment: HashMap::new(),
        volume_mounts: Vec::new(),
        allowed_hosts: Vec::new(),
        allowed_ip_name_lookups: Vec::new(),
        allowed_host_loopback_ports: Vec::new(),
    };
    v2::WorkloadStartRequest {
        workload_id: FLOW_HTTP_WORKLOAD_ID.to_owned(),
        workload: Some(v2::Workload {
            namespace: host_namespace(identity).to_owned(),
            name: FLOW_HTTP_NAME.to_owned(),
            annotations: HashMap::new(),
            service: None,
            wit_world: Some(v2::WitWorld {
                components: vec![v2::Component {
                    image: "flow-http.wasm".to_owned(),
                    local_resources: Some(local_resources),
                    pool_size: 0,
                    reclaim_window_seconds: 0,
                    reclaim_min_instances: 0,
                    max_invocations: 0,
                    image_pull_secret: None,
                    name: FLOW_HTTP_NAME.to_owned(),
                    image_pull_policy: v2::ImagePullPolicy::Always.into(),
                    max_concurrency: serde_yaml::from_value(http["maxConcurrency"].clone())
                        .expect("the HTTP store concurrency is an i32"),
                }],
                host_interfaces: vec![
                    v2::WitInterface {
                        namespace: "wasi".to_owned(),
                        package: "http".to_owned(),
                        version: "0.3.0".to_owned(),
                        interfaces: vec!["handler".to_owned()],
                        config: HashMap::from([(
                            "host".to_owned(),
                            request.config.route_host().to_owned(),
                        )]),
                        name: String::new(),
                    },
                    wit_interface("wamn", "flow-http-routing", "0.1.0", "routing"),
                    wit_interface("wamn", "router-delivery", "0.4.0", "delivery"),
                    wit_interface("wamn", "operation", "0.1.0", "operation"),
                    wit_interface("wamn", "postgres", "0.4.0", "statements"),
                ],
            }),
            volumes: Vec::new(),
        }),
    }
}

pub(super) fn wit_interface(
    namespace: &str,
    package: &str,
    version: &str,
    interface: &str,
) -> v2::WitInterface {
    v2::WitInterface {
        namespace: namespace.to_owned(),
        package: package.to_owned(),
        version: version.to_owned(),
        interfaces: vec![interface.to_owned()],
        config: HashMap::new(),
        name: String::new(),
    }
}

pub(super) fn heartbeat_matches(
    subject: &str,
    heartbeat: &v2::HostHeartbeat,
    identity: &DevActivationIdentity,
) -> bool {
    let prefix = format!("{OPERATOR_API_PREFIX}.heartbeat.");
    let Some(subject_host_id) = subject.strip_prefix(&prefix) else {
        return false;
    };
    !subject_host_id.is_empty()
        && !subject_host_id.contains('.')
        && subject_host_id == heartbeat.id
        && heartbeat.hostname == identity.host_name
        && heartbeat.environment == host_namespace(identity)
        && heartbeat.labels.get("hostgroup") == Some(&identity.host_group)
}

pub(super) fn decode_json<T>(
    payload: &[u8],
    step: &'static str,
    detail: &'static str,
) -> Result<T, DevActivationError>
where
    T: DeserializeOwned,
{
    serde_json::from_slice(payload).map_err(|source| {
        DevActivationError::with_source(
            DevActivationErrorType::ProtocolViolation,
            step,
            detail,
            source,
        )
    })
}

/// Whether a polled workload may still reach RUNNING. The host reports
/// NOT_FOUND until its own preload finishes and it starts the workload; any
/// other state but STARTING is final.
pub(super) fn workload_pending(status: Option<&v2::WorkloadStatus>) -> bool {
    status.is_none_or(|status| {
        [
            v2::WorkloadState::Unspecified,
            v2::WorkloadState::Starting,
            v2::WorkloadState::NotFound,
        ]
        .into_iter()
        .any(|state| status.workload_state == state as i32)
    })
}

pub(super) fn require_running(
    step: &'static str,
    status: Option<v2::WorkloadStatus>,
    workload_id: &str,
) -> Result<(), DevActivationError> {
    let status = status.ok_or_else(|| {
        DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            step,
            "response omitted workload_status",
        )
    })?;
    if status.workload_id != workload_id {
        return Err(DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            step,
            format!(
                "response named workload {:?}, expected {workload_id:?}",
                status.workload_id
            ),
        ));
    }
    let state = v2::WorkloadState::try_from(status.workload_state).map_err(|_| {
        DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            step,
            format!(
                "response carried unknown workload state {}",
                status.workload_state
            ),
        )
    })?;
    if state != v2::WorkloadState::Running {
        return Err(DevActivationError::new(
            DevActivationErrorType::WorkloadRefused,
            step,
            format!(
                "{workload_id} reached {}: {}",
                state.as_str_name(),
                status.message
            ),
        ));
    }
    Ok(())
}

pub(super) fn require_stopped(
    status: Option<v2::WorkloadStatus>,
    workload_id: &str,
) -> Result<(), DevActivationError> {
    let status = status.ok_or_else(|| {
        DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            "stop-workload",
            "response omitted workload_status",
        )
    })?;
    if status.workload_id != workload_id {
        return Err(DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            "stop-workload",
            "response named a different workload",
        ));
    }
    let state = v2::WorkloadState::try_from(status.workload_state).map_err(|_| {
        DevActivationError::new(
            DevActivationErrorType::ProtocolViolation,
            "stop-workload",
            "response carried an unknown workload state",
        )
    })?;
    if matches!(
        state,
        v2::WorkloadState::Stopping | v2::WorkloadState::NotFound
    ) {
        Ok(())
    } else {
        Err(DevActivationError::new(
            DevActivationErrorType::CleanupFailed,
            "stop-workload",
            format!("workload stop reached {}", state.as_str_name()),
        ))
    }
}

pub(super) fn host_process_spec(request: &DevActivationRequest<'_>) -> HostProcessSpec {
    let platform: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "../../../../../../deploy/platform/release/values.yaml"
    ))
    .expect("the platform chart defaults are valid YAML");
    let runtime = &platform["runtime-operator"]["runtime"];
    let identity = request.identity;
    let event_identity = request.config.activation_identity();
    let local = request.config.local_artifacts();
    let mut args = vec![
        "host".to_owned(),
        "--host-group".to_owned(),
        identity.host_group.clone(),
        "--scheduler-nats-url".to_owned(),
        request.config.scheduler_nats_url().to_owned(),
        "--data-nats-url".to_owned(),
        request.config.scheduler_nats_url().to_owned(),
        "--host-name".to_owned(),
        identity.host_name.clone(),
        "--runner".to_owned(),
        identity.runner.clone(),
        "--environment".to_owned(),
        identity.environment.clone(),
        "--applications".to_owned(),
        serde_json::to_string(request.applications).expect("application map serializes"),
        "--http-addr".to_owned(),
        LOOPBACK_HTTP_BIND.to_owned(),
        "--local-application".to_owned(),
        local.directory.display().to_string(),
        "--local-application-digest".to_owned(),
        request.release.manifest_digest.to_string(),
        "--wasmtime-cache-dir".to_owned(),
        request.wasmtime_cache_dir.display().to_string(),
        "--user-config".to_owned(),
        request.config.host_config_file().display().to_string(),
        "--project".to_owned(),
        identity.project.clone(),
        "--org".to_owned(),
        identity.org.clone(),
        "--drain-bound-seconds".to_owned(),
        HOST_SHUTDOWN_TIMEOUT.as_secs().to_string(),
    ];
    // One NATS and one development certificate for both connections
    // (docs/plan/alignment.md R2, R5).
    for plane in ["scheduler", "data"] {
        for (flag, path) in [
            ("ca", request.config.nats_tls_ca()),
            ("cert", request.config.nats_tls_cert()),
            ("key", request.config.nats_tls_key()),
        ] {
            args.extend([
                format!("--{plane}-nats-tls-{flag}"),
                path.display().to_string(),
            ]);
        }
    }
    for (field, flag) in [
        ("drainDelaySeconds", "--drain-delay"),
        ("natsConnectTimeoutSeconds", "--nats-connect-timeout"),
    ] {
        args.extend([
            flag.to_owned(),
            format!("{}s", runtime[field].as_u64().expect("chart timeout")),
        ]);
    }
    args.extend([
        "--socket-egress".to_owned(),
        wamn_release_control::release_chart::SOCKET_EGRESS.to_owned(),
    ]);
    if runtime["probes"]["endpoint"]["enabled"].as_bool() == Some(true) {
        args.extend(["--probe-addr".to_owned(), LOOPBACK_HTTP_BIND.to_owned()]);
    }
    let mut env = vec![
        (
            "WASMCLOUD_HOST_ENVIRONMENT".to_owned(),
            host_namespace(identity).to_owned(),
        ),
        (
            "WAMN_SYSTEM_URL".to_owned(),
            request.config.identity_database_url().to_owned(),
        ),
        (
            "WAMN_PG_URL".to_owned(),
            request.config.guest_database_url().to_owned(),
        ),
        (
            "WAMN_EXECUTOR_PLATFORM_PG_URL".to_owned(),
            request.config.executor_platform_database_url().to_owned(),
        ),
        (
            "WAMN_HTTP_ADMITTER_PG_URL".to_owned(),
            request.config.http_admitter_database_url().to_owned(),
        ),
        (
            "WAMN_EVENT_MATERIALIZER_PG_URL".to_owned(),
            request.config.event_materializer_database_url().to_owned(),
        ),
        (
            "WAMN_ADMINISTRATION_PG_URL".to_owned(),
            request.config.administration_database_url().to_owned(),
        ),
        (
            "WAMN_EVT_STREAM_REPLICAS".to_owned(),
            request.config.stream_replicas().to_string(),
        ),
        (
            "WAMN_EVT_DUP_WINDOW_SECS".to_owned(),
            request.config.dup_window_secs().to_string(),
        ),
        ("WAMN_EVT_ORG".to_owned(), event_identity.org.clone()),
        (
            "WAMN_EVT_PROJECT".to_owned(),
            event_identity.project.clone(),
        ),
        (
            "WAMN_EVT_ENV".to_owned(),
            event_identity.environment.clone(),
        ),
    ];
    env.push((
        "WAMN_MATERIALIZER_DRAIN_BOUND_SECONDS".to_owned(),
        wamn_release_control::release_chart::MATERIALIZER_DRAIN_BOUND_SECONDS.to_string(),
    ));
    env.extend(
        wamn_release_control::release_chart::TELEMETRY_VARIABLES.map(|(name, value)| {
            let value = if name == "OTEL_EXPORTER_OTLP_ENDPOINT" {
                request.config.otel_exporter_otlp_endpoint()
            } else {
                value
            };
            (name.to_owned(), value.to_owned())
        }),
    );
    env.push(("HOME".to_owned(), HOST_HOME.to_owned()));
    env.push((
        "WAMN_OAUTH_ENCRYPTION".to_owned(),
        r#"{"provider":"local"}"#.to_owned(),
    ));
    env.push((
        "WAMN_OAUTH_KEY_FILE".to_owned(),
        wamn_environment::platform::oauth::dev_key(&local.directory)
            .display()
            .to_string(),
    ));
    for variable in runtime["env"]
        .as_sequence()
        .expect("platform host environment")
    {
        env.push((
            variable["name"]
                .as_str()
                .expect("environment name")
                .to_owned(),
            variable["value"]
                .as_str()
                .expect("environment value")
                .to_owned(),
        ));
    }
    let resources = &runtime["resources"];
    for (name, value) in [
        ("WASH_DEFAULT_HEAP_MEMORY", &resources["defaultHeapMemory"]),
        ("WASH_CORE_INSTANCES", &resources["coreInstances"]),
        ("WASH_GUEST_MEMORY_MODE", &resources["guestMemoryMode"]),
        (
            "WASH_MAX_CONCURRENT_STARTS",
            &resources["maxConcurrentStarts"],
        ),
    ] {
        env.push((
            name.to_owned(),
            value.as_str().map_or_else(
                || value.as_u64().expect("host resource value").to_string(),
                str::to_owned,
            ),
        ));
    }
    if let Some(url) = request.config.connection_database_url() {
        env.push(("WAMN_CONNECTION_STORE_URL".to_owned(), url.to_owned()));
    }
    if let Some(identity) = request.config.session_identity() {
        env.extend([
            ("WAMN_SESSION_ISSUER".to_owned(), identity.issuer.clone()),
            (
                "WAMN_SESSION_JWKS_CA".to_owned(),
                identity.ca.display().to_string(),
            ),
            (
                "WAMN_SESSION_INSTANCE_SUFFIX".to_owned(),
                identity.instance_suffix.clone(),
            ),
        ]);
    }
    HostProcessSpec {
        program: request.host_binary.to_owned(),
        args: args.into_boxed_slice(),
        env: env.into_boxed_slice(),
    }
}

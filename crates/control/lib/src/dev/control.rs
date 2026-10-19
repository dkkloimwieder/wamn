//! Start the org control host and serve local built web clients.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize as _;
use serde_json::json;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};
use wash_runtime::washlet::types::v2;

use crate::provision_project_env::{
    OrgWorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction,
    run_org_workload_action, secret_value, write_secret_json,
};

/// Refuse missing web bytes before provisioning an environment.
pub(super) fn preflight(args: &super::up::DevUpRequest) -> anyhow::Result<()> {
    crate::platform_build::platform_ui_digest(&args.platform_ui)?;
    for package in &args.packages {
        if package.join("web").is_dir() {
            let dist = wamn_schema_generator::output_root(package).join("web");
            anyhow::ensure!(
                dist.join("index.html").is_file(),
                "build the application web client before dev up: {}",
                dist.display()
            );
        }
    }
    Ok(())
}

/// Start the deployed control binary with local HTTP component bytes.
pub(super) async fn start(
    args: &super::up::DevUpRequest,
    environment: &super::environment::DevEnvironment,
) -> anyhow::Result<String> {
    let root = &args.root;
    let org = &environment.identity.org;
    let database = url::Url::parse(&args.system_database_url)?;
    let secret = root.join("control.json");
    run_org_workload_action(&OrgWorkloadActionRequest {
        org: org.clone(),
        system_database_url: args.system_database_url.clone(),
        db_host: database.host_str().map(str::to_owned),
        db_port: database.port_or_known_default().unwrap_or(5432),
        namespace: "wamn-system".to_owned(),
        action: WorkloadGenerationAction {
            family: WorkloadRoleFamily::Control,
            verb: WorkloadActionVerb::Prepare,
            generation: CredentialGeneration::A,
        },
        secret: Some(secret.clone()),
    })
    .await?;
    let local = root.join("control-artifacts");
    std::fs::create_dir_all(&local)?;
    let bytes = std::fs::read(&args.flow_http_component)?;
    let digest = wamn_engine::component_admission::component_digest(&bytes);
    std::fs::write(
        wamn_engine::artifact_source::local_component_path(&local, &digest)?,
        bytes,
    )?;
    // Read the deployed interfaces instead of defining another HTTP contract.
    let deployment = serde_yaml::Deserializer::from_str(include_str!(
        "../../../../../deploy/gcp/control-service.yaml"
    ))
    .map(serde_json::Value::deserialize)
    .collect::<Result<Vec<_>, _>>()?;
    let spec = &deployment[1]["spec"]["template"]["spec"];
    let mut interfaces: Vec<v2::WitInterface> =
        serde_json::from_value(spec["hostInterfaces"].clone())?;
    let route_host = "control.localhost";
    for interface in &mut interfaces {
        if interface.namespace == "wasi" && interface.package == "http" {
            interface
                .config
                .insert("host".to_owned(), route_host.to_owned());
        }
    }
    let workload = v2::WorkloadStartRequest {
        workload_id: "wamn-dev-control-http".to_owned(),
        workload: Some(v2::Workload {
            namespace: "hosts".to_owned(),
            name: "flow-http".to_owned(),
            wit_world: Some(v2::WitWorld {
                host_interfaces: interfaces,
                components: vec![v2::Component {
                    name: "flow-http".to_owned(),
                    image: digest,
                    image_pull_policy: v2::ImagePullPolicy::Never.into(),
                    ..Default::default()
                }],
            }),
            ..Default::default()
        }),
    };
    std::fs::write(local.join("flow-http.json"), serde_json::to_vec(&workload)?)?;
    let administration = root.join("control-administration");
    std::fs::create_dir_all(&administration)?;
    let patch = crate::provision_project_env::read_json(
        &root.join("administration.control-administration-patch.json"),
    )?;
    for (name, value) in patch["stringData"]
        .as_object()
        .context("administration patch data")?
    {
        super::up::write_private(
            &administration.join(name),
            value.as_str().context("administration URL")?.as_bytes(),
        )?;
    }
    let issuer = &environment.issuer.args;
    let group: serde_json::Value = serde_yaml::from_str(include_str!(
        "../../../../../deploy/gcp/host-group-control.yaml"
    ))?;
    let group = &group["runtime-operator"]["runtime"]["hostGroups"][0];
    let mut env = serde_json::Map::new();
    for variable in group["env"]
        .as_array()
        .context("control host environment")?
    {
        if variable["name"] == "WAMN_DRAIN_BOUND_SECONDS" {
            env.insert(
                "WAMN_DRAIN_BOUND_SECONDS".to_owned(),
                variable["value"].clone(),
            );
        }
    }
    env.extend(
        json!({
            "HOME": "/tmp", "RUST_LOG": "info", "WASMCLOUD_HOST_ENVIRONMENT": "hosts",
            "WAMN_CONTROL_URL": secret_value(&secret, "url")?,
            "WASH_HOST_MAX_GUEST_MEMORY": group["resources"]["limits"]["memory"],
            "WAMN_SESSION_ISSUER": issuer.endpoint,
            "WAMN_SESSION_JWKS_CA": issuer.server_ca,
            "WAMN_PAT_ISSUER": issuer.endpoint,
            "WAMN_PAT_CLIENT_CERT": issuer.client_cert,
            "WAMN_PAT_CLIENT_KEY": issuer.client_key,
            "WAMN_PAT_SERVER_CA": issuer.server_ca,
            "WAMN_CONTROL_ADMINISTRATION_DIR": administration,
        })
        .as_object()
        .expect("object")
        .clone(),
    );
    for (name, value) in crate::release_chart::TELEMETRY_VARIABLES {
        env.insert(
            name.to_owned(),
            json!(if name == "OTEL_EXPORTER_OTLP_ENDPOINT" {
                args.otel_exporter_otlp_endpoint.as_str()
            } else {
                value
            }),
        );
    }
    let packages = args
        .packages
        .iter()
        .filter(|package| package.join("web").is_dir())
        .map(|package| {
            json!({"name": package.file_name().and_then(|name| name.to_str()),
            "dist": wamn_schema_generator::output_root(package).join("web")})
        })
        .collect::<Vec<_>>();
    let (_, readiness) = wamn_catalog::HostRouteSet::Control
        .attachments()
        .find(|(_, attachment)| attachment.route.operation == "control.mine")
        .context("control.mine host route")?;
    let config = root.join("web-process-config.json");
    write_secret_json(
        &config,
        &json!({
            "root": root, "org": org, "project": environment.identity.project,
            "platform_ui": args.platform_ui.canonicalize()?, "packages": packages,
            "issuer": issuer.endpoint, "ca": issuer.server_ca,
            "host": args.host_binary.canonicalize()?, "host_env": env,
            "host_args": ["host", "--control", "--org", org, "--scheduler-nats-url", &args.nats_url,
                "--host-group", "wamn-dev-control", "--local-control", local,
                "--wasmtime-cache-dir", root.join("dev-wasmtime-cache")],
            "route_host": route_host,
            "readiness": readiness.definition["route"]
        }),
    )?;
    let script = root.join("web.mjs");
    std::fs::write(&script, include_str!("web.mjs"))?;
    let log = std::fs::File::create(root.join("control-web.log"))?;
    let mut child = tokio::process::Command::new("pnpm")
        .args(["exec", "node"])
        .arg(script)
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .context("start local control and web services")?;
    let mut stdout = BufReader::new(child.stdout.take().context("web readiness pipe")?);
    let mut ready = String::new();
    let result = tokio::time::timeout(Duration::from_secs(120), stdout.read_line(&mut ready)).await;
    if !matches!(result, Ok(Ok(count)) if count > 0) {
        let _ = stop(root).await;
        let _ = child.kill().await;
        anyhow::bail!(
            "control and web startup failed; read {}/control-web.log",
            root.display()
        );
    }
    Ok(ready.trim().to_owned())
}

/// Stop the supervisor through its private socket and wait for child cleanup.
pub async fn stop(root: &Path) -> anyhow::Result<()> {
    let socket = root.join("web.sock");
    if !socket.exists() {
        return Ok(());
    }
    let mut stream = tokio::net::UnixStream::connect(&socket)
        .await
        .context("connect the owned web supervisor")?;
    stream.write_all(b"stop\n").await?;
    let mut result = String::new();
    tokio::time::timeout(Duration::from_secs(80), stream.read_to_string(&mut result))
        .await
        .context("control and web shutdown timed out")??;
    anyhow::ensure!(
        result == "stopped\n",
        "control and web cleanup failed: {result}"
    );
    Ok(())
}

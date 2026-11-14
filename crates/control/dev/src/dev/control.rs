//! Start the org control host and serve local built web clients.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use serde_json::json;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};
use wamn_control_provision::WorkloadRoleFamily;

use wamn_environment::provision_project_env::write_secret_json;

/// Refuse missing web bytes before provisioning an environment.
pub(super) fn preflight(args: &super::up::DevUpRequest) -> anyhow::Result<()> {
    anyhow::ensure!(
        args.platform_ui.join("index.html").is_file(),
        "build the platform UI before dev up: {} has no index.html",
        args.platform_ui.display()
    );
    for package in &args.packages {
        if package.join("web").is_dir() {
            let dist = wamn_package_control::web_artifact::artifact_output(package).join("dist");
            anyhow::ensure!(
                dist.join("index.html").is_file(),
                "run wamn web build --package {} before dev up",
                package.display()
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
    let oauth_key = root.join("oauth.key");
    wamn_environment::platform::oauth::local_key(&oauth_key)?;
    let org = &environment.identity.org;
    let coordinate = super::environment::JourneyTarget {
        org: org.clone(),
        project: environment.identity.project.clone(),
        environment: environment.identity.environment.clone(),
    };
    let control_url = super::credentials::workload(
        &args.system_database_url,
        &coordinate,
        WorkloadRoleFamily::Control,
    )
    .await?;
    let route_host = "control.wamn.localhost";
    let local = root.join("control-artifacts");
    let administration = root.join("control-administration");
    std::fs::create_dir_all(&administration)?;
    super::up::write_private(
        &administration.join(wamn_control_provision::control_administration_key(
            &coordinate.project,
            &coordinate.environment,
        )),
        environment.credentials.administration.as_bytes(),
    )?;
    let issuer = &environment.issuer.args;
    let group = wamn_environment::platform::control::host_values(org);
    let group = &group["runtime-operator"]["runtime"]["hostGroups"][0];
    let mut env = serde_json::Map::new();
    let platform: serde_json::Value = serde_yaml::from_str(include_str!(
        "../../../../../deploy/platform/release/values.yaml"
    ))?;
    for variable in platform["runtime-operator"]["runtime"]["env"]
        .as_array()
        .context("platform host environment")?
    {
        env.insert(
            variable["name"]
                .as_str()
                .context("environment name")?
                .to_owned(),
            variable["value"].clone(),
        );
    }
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
            "HOME": root, "RUST_LOG": "info", "WASMCLOUD_HOST_ENVIRONMENT": "hosts",
            "WAMN_OAUTH_ENCRYPTION": r#"{"provider":"local"}"#,
            "WAMN_OAUTH_KEY_FILE": oauth_key,
            "WAMN_CONTROL_URL": control_url,
            "WAMN_CONTROL_PUBLIC_ORIGIN": format!("http://{route_host}"),
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
    for (name, value) in wamn_release_control::release_chart::TELEMETRY_VARIABLES {
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
            "dist": wamn_package_control::web_artifact::artifact_output(package).join("dist")})
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
            "host_args": ["host", "--org", org,
                "--scheduler-nats-url", &args.nats.nats_url, "--data-nats-url", &args.nats.nats_url,
                "--scheduler-nats-tls-ca", &args.nats.nats_tls_ca,
                "--scheduler-nats-tls-cert", &args.nats.nats_tls_cert,
                "--scheduler-nats-tls-key", &args.nats.nats_tls_key,
                "--data-nats-tls-ca", &args.nats.nats_tls_ca,
                "--data-nats-tls-cert", &args.nats.nats_tls_cert,
                "--data-nats-tls-key", &args.nats.nats_tls_key,
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

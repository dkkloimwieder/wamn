use super::{
    BuildPlatformRequest, CLOUD_AUTH_SCRIPT, CLOUD_DOCKER_IMAGE, CLOUD_SDK_IMAGE, CloudBuild,
    IMAGES, Path, PathBuf, Plan, ROLES,
};
use anyhow::Context as _;

/// The local static bundle, independent of package build output.
pub fn platform_ui_dist(repository: &Path) -> PathBuf {
    repository.join("apps/platform_ui/dist")
}

pub(super) fn builds_platform(args: &[String]) -> bool {
    let Some(program) = args.first() else {
        return false;
    };
    if Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        != Some("wamn-ctl")
    {
        return false;
    }
    // Skip the CLI's global log option before finding its subcommand.
    let mut arguments = args[1..].iter();
    while let Some(argument) = arguments.next() {
        if argument == "--log-level" {
            arguments.next();
        } else if !argument.starts_with("--log-level=") {
            // Neither a stamp nor a cloud build runs Docker on this machine.
            return argument == "build-platform"
                && !arguments.any(|argument| argument == "--stamp-only" || argument == "--cloud");
        }
    }
    false
}

/// The `sha256:<hex>` digest of the manifest descriptor `json`.
pub(super) fn manifest_digest(json: &str) -> anyhow::Result<String> {
    let descriptor: serde_json::Value =
        serde_json::from_str(json).context("parse the manifest descriptor")?;
    descriptor["digest"]
        .as_str()
        .filter(|digest| digest.starts_with("sha256:"))
        .map(str::to_owned)
        .context("the manifest descriptor has no sha256 digest")
}

/// A loopback registry is reached over plain HTTP.
pub(super) fn loopback(registry: &str) -> bool {
    let host = registry.split([':', '/']).next().unwrap_or_default();
    matches!(host, "localhost" | "127.0.0.1")
}

/// The highest `x.y.z` tag below `current`.
pub(super) fn previous_version(tags: &[String], current: &str) -> Option<String> {
    let parse = |tag: &str| -> Option<(u64, u64, u64)> {
        let mut parts = tag.split('.').map(|part| part.parse().ok());
        let version = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    };
    let current = parse(current)?;
    tags.iter()
        .filter_map(|tag| parse(tag).map(|version| (version, tag)))
        .filter(|(version, _)| *version < current)
        .max()
        .map(|(_, tag)| tag.clone())
}

/// The digest of the single layer of a role manifest: the SHA-256 of its wasm.
pub(super) fn wasm_layer(manifest: &[u8]) -> anyhow::Result<String> {
    let value: serde_json::Value =
        serde_json::from_slice(manifest).context("parse the role manifest")?;
    match value["layers"].as_array().map(Vec::as_slice) {
        Some([layer]) => layer["digest"]
            .as_str()
            .map(str::to_owned)
            .context("the role layer has no digest"),
        _ => anyhow::bail!("a role manifest has one layer"),
    }
}

/// The tag that carries the input hash of an image.
pub(super) fn input_tag(registry: &str, target: &str, hash: &str) -> String {
    format!("{registry}/wamn-{target}:input-{hash}")
}

pub(super) fn cloud_minutes(build: &CloudBuild) -> anyhow::Result<u32> {
    match (build.start_time, build.finish_time) {
        (Some(start), Some(finish)) => {
            let nanos = (finish - start)
                .num_nanoseconds()
                .context("cloud build duration is too large")?;
            let nanos = u64::try_from(nanos).context("cloud build finished before it started")?;
            u32::try_from(nanos.div_ceil(60_000_000_000)).context("cloud build minutes overflow")
        }
        // No start means no worker ran, including a failure before execution.
        (None, _) => Ok(0),
        _ => anyhow::bail!(
            "Cloud Build {} lacks start or finish times; its spend cannot be recorded",
            build.id
        ),
    }
}

pub(super) fn stderr_tail(stderr: &[u8], lines: usize) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let all: Vec<_> = stderr.trim_end().lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

pub(super) fn cloud_configuration(
    request: &BuildPlatformRequest,
    supplement: &str,
    head: &str,
    plan: &Plan,
    key: &str,
) -> anyhow::Result<serde_json::Value> {
    use serde_json::json;
    let mut steps = Vec::new();
    let base = docker_arguments(key, "Dockerfile", &format!("{supplement}/apps"));
    // The token is captured in memory, then written privately to the shared
    // credential volume. Neither the token nor its encoded form is logged.
    // A token lasts an hour, so each push step gets a fresh one.
    let auth = CLOUD_AUTH_SCRIPT.replace('$', "$$");
    let volume = json!([{"name": "cloud-auth", "path": "/cloud-auth"}]);
    let auth_step = json!({
        "name": CLOUD_SDK_IMAGE, "entrypoint": "bash",
        "args": ["-c", auth, "bash", request.registry.split('/').next().context("registry host")?],
        "volumes": volume,
    });
    // The worker daemon's BuildKit lacks named build contexts, so buildx runs
    // its own BuildKit in a container (docker-container driver).
    steps.push(json!({
        "name": CLOUD_DOCKER_IMAGE,
        "args": ["buildx", "create", "--name", "wamn", "--driver", "docker-container"],
        "env": ["DOCKER_CONFIG=/cloud-auth"], "volumes": volume,
    }));
    let registry = &request.registry;
    let version = &request.version;
    for target in IMAGES.into_iter().chain(["roles"]) {
        // An image whose input tag exists is copied after the build.
        if plan.images.contains_key(target) {
            continue;
        }
        let mut args: Vec<String> = vec!["buildx".into(), "--builder".into(), "wamn".into()];
        args.extend(base.iter().cloned());
        args.extend(["--target".into(), target.into()]);
        if target == "roles" {
            args.extend(["--output".into(), "type=local,dest=/workspace/roles".into()]);
        } else {
            let hash = &plan.hashes[target];
            steps.push(auth_step.clone());
            args.extend([
                "--tag".into(),
                format!("{registry}/wamn-{target}:{version}"),
                "--tag".into(),
                input_tag(registry, target, hash),
                format!("--label=wamn.dev/source-head={head}"),
                "--label=wamn.dev/build-profile=release".into(),
                format!("--label=wamn.dev/input-hash={hash}"),
                // Owner ruling 2026-10-21: buildx pushes each image directly.
                "--push".into(),
            ]);
        }
        args.push(".".into());
        steps.push(json!({
            "name": CLOUD_DOCKER_IMAGE, "args": args,
            "env": ["DOCKER_CONFIG=/cloud-auth"], "volumes": volume,
        }));
    }
    steps.push(auth_step);
    for (file, role) in ROLES {
        steps.push(json!({
            // This release uses the same wash-runtime revision as Cargo.toml.
            "name": "ghcr.io/wasmcloud/wash:2.11.0", "entrypoint": "/usr/local/bin/wash",
            "args": ["oci", "push", format!("{registry}/wamn/{role}:{version}"), format!("/workspace/roles/{file}")],
            "env": ["DOCKER_CONFIG=/cloud-auth"], "volumes": volume,
        }));
    }
    // Cloud Logging is the log store that the agent can read.
    Ok(
        json!({"steps": steps, "options": {"machineType": "E2_HIGHCPU_8", "logging": "CLOUD_LOGGING_ONLY"}}),
    )
}

pub(super) fn docker_arguments(key: &str, file: &str, apps: &str) -> Vec<String> {
    vec![
        "build".into(),
        "--platform=linux/amd64".into(),
        "--provenance=false".into(),
        "--build-arg".into(),
        format!("WAMN_CHECKOUT_KEY={key}"),
        "--build-context".into(),
        format!("apps-target={apps}"),
        "--file".into(),
        file.into(),
    ]
}

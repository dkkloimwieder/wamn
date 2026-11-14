#[path = "platform_pools.rs"]
mod platform_pools;
#[path = "platform_project.rs"]
mod platform_project;
#[path = "platform_project_commands.rs"]
mod platform_project_commands;
#[path = "platform_tofu.rs"]
mod platform_tofu;
pub use platform_project_commands::{ProjectCommand, project};
// BuildKit publication and chart stamping.

use crate::process;
use anyhow::{Context as _, ensure};
use clap::Args as ClapArgs;
use docker_credential::{CredentialRetrievalError, DockerCredential};
use serde_json::Value;
use std::path::PathBuf;
use wamn_release_control::release_chart::{self, ImageSet};

const IMAGES: [&str; 5] = ["host", "control", "ctl", "identity", "edge-web"];
const ROLES: [(&str, &str); 1] = [("http-route", "http")];

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long)]
    registry: String,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value = ".")]
    repository: PathBuf,
    /// Stamp existing published images without building or pushing.
    #[arg(long)]
    stamp_only: bool,
    /// Select the platform UI when this cluster has a web bucket.
    #[arg(long)]
    cluster: Option<PathBuf>,
}

pub async fn run(args: Args) -> anyhow::Result<()> {
    let root = args.repository.canonicalize()?;
    ensure!(
        !args.out.exists(),
        "chart output {} exists",
        args.out.display()
    );
    let chart = root.join("deploy/platform/release");
    let version = release_chart::chart_version(&chart)?;
    let ui = args
        .cluster
        .as_deref()
        .map(wamn_platform_install::platform::document::ClusterDocument::compile)
        .transpose()?
        .is_some_and(|cluster| !cluster.buckets.web.is_empty());
    let components = tempfile::tempdir()?;
    let mut palette_pins = std::collections::BTreeMap::new();
    if !args.stamp_only {
        // Both roles use the same component build and validation path as packages.
        for (package, role) in ROLES {
            let path = wamn_build::build_guest(&root, &root.join("apps/Cargo.toml"), package, None)
                .await?;
            let bytes = std::fs::read(path)?;
            std::fs::write(components.path().join(format!("{role}.wasm")), &bytes)?;
        }
        for component in wamn_build::palette::build(&root).await? {
            let bytes = std::fs::read(&component.file)?;
            std::fs::write(
                components.path().join(format!(
                    "{}.wasm",
                    component
                        .declaration
                        .component_digest
                        .trim_start_matches("sha256:")
                )),
                &bytes,
            )?;
            let repository = format!("{}/wamn/palette/{}", args.registry, component.name);
            let reference = format!("{repository}:{version}");
            wash_runtime::oci::push_component(
                &reference,
                &bytes,
                wash_runtime::oci::OciConfig {
                    insecure: loopback(&args.registry),
                    ..Default::default()
                },
                Some(std::collections::HashMap::from([(
                    wamn_catalog::PALETTE_DESCRIPTOR_ANNOTATION.to_owned(),
                    serde_json::to_string(&component.declaration)?,
                )])),
            )
            .await?;
            let digest = inspect(&root, &reference).await?;
            palette_pins.insert(
                component.name,
                serde_json::json!({
                    "artifact": format!("{repository}@{digest}"),
                    "descriptor": component.declaration,
                }),
            );
        }
        let palette_directory = components.path().join("palette");
        std::fs::create_dir(&palette_directory)?;
        std::fs::write(
            palette_directory.join(wamn_catalog::PALETTE_FILE),
            wamn_digest::canonical_json_bytes(&serde_json::to_value(&palette_pins)?),
        )?;
        for pin in palette_pins.values() {
            let digest = pin["descriptor"]["component-digest"]
                .as_str()
                .context("palette digest missing")?;
            let name = format!("{}.wasm", digest.trim_start_matches("sha256:"));
            std::fs::copy(components.path().join(&name), palette_directory.join(name))?;
        }
        if ui {
            process::run(&root, "pnpm", &["--filter", "@wamn/platform-ui", "build"]).await?;
        }
        let key = wamn_digest::bytes_sha256(root.as_os_str().as_encoded_bytes());
        let pins: toml::Value = toml::from_str(&std::fs::read_to_string(root.join("mise.toml"))?)?;
        for image in IMAGES {
            let reference = format!("{}/wamn-{image}:{version}", args.registry);
            let cache = format!("{}/wamn-{image}:buildcache", args.registry);
            let mut command = tokio::process::Command::new("docker");
            command.current_dir(&root).args([
                "buildx",
                "build",
                "--platform",
                "linux/amd64",
                "--target",
                image,
                "--tag",
                &reference,
                "--push",
                "--cache-from",
                &format!("type=registry,ref={cache}"),
                "--cache-to",
                &format!("type=registry,ref={cache},mode=max"),
                "--build-arg",
                &format!("WAMN_CHECKOUT_KEY={}", &key[7..23]),
                "--build-context",
                &format!("components={}", components.path().display()),
            ]);
            // The version manager and Docker consume the same pinned downloads.
            for name in ["helm", "kcl"] {
                let pin = &pins["tools"][format!("http:{name}")];
                for (field, suffix) in [("url", "URL"), ("checksum", "CHECKSUM")] {
                    let value = pin[field].as_str().context("missing tool download pin")?;
                    command.args([
                        "--build-arg",
                        &format!("{}_{suffix}={value}", name.to_uppercase()),
                    ]);
                }
            }
            command.arg(".");
            process::execute(&mut command).await?;
        }
        for (_, role) in ROLES {
            let file = format!("{role}.wasm");
            let bytes = std::fs::read(components.path().join(file))?;
            wash_runtime::oci::push_component(
                &format!("{}/wamn/{role}:{version}", args.registry),
                &bytes,
                wash_runtime::oci::OciConfig {
                    insecure: loopback(&args.registry),
                    ..Default::default()
                },
                None,
            )
            .await?;
        }
    }
    if args.stamp_only {
        for name in wamn_build::palette::names(&root)? {
            let repository = format!("{}/wamn/palette/{name}", args.registry);
            let reference = format!("{repository}:{version}");
            let digest = inspect(&root, &reference).await?;
            let manifest = process::output(
                &root,
                "docker",
                &[
                    "buildx",
                    "imagetools",
                    "inspect",
                    "--raw",
                    &format!("{repository}@{digest}"),
                ],
            )
            .await?;
            let manifest: Value = serde_json::from_str(&manifest)?;
            let descriptor: wamn_catalog::ComponentDescriptor = serde_json::from_str(
                manifest["annotations"][wamn_catalog::PALETTE_DESCRIPTOR_ANNOTATION]
                    .as_str()
                    .context("published palette has no sealed descriptor")?,
            )?;
            let (bytes, _) = wash_runtime::oci::pull_component(
                &format!("{repository}@{digest}"),
                wash_runtime::oci::OciConfig {
                    insecure: loopback(&args.registry),
                    ..Default::default()
                },
                wash_runtime::oci::OciPullPolicy::Always,
            )
            .await?;
            std::fs::write(
                components.path().join(format!(
                    "{}.wasm",
                    descriptor.component_digest.trim_start_matches("sha256:")
                )),
                bytes,
            )?;
            palette_pins.insert(name, serde_json::json!({"artifact": format!("{repository}@{digest}"), "descriptor": descriptor}));
        }
    }

    let mut images = std::collections::BTreeMap::new();
    for image in IMAGES {
        let reference = format!("{}/wamn-{image}:{version}", args.registry);
        let digest = inspect(&root, &reference).await?;
        images.insert(image, format!("{reference}@{digest}"));
    }
    let mut roles = Vec::new();
    for (_, role) in ROLES {
        let reference = format!("{}/wamn/{role}:{version}", args.registry);
        let digest = inspect(&root, &reference).await?;
        let pinned = format!("{}/wamn/{role}@{digest}", args.registry);
        if args.stamp_only && role == "http" {
            let (bytes, _) = wash_runtime::oci::pull_component(
                &pinned,
                wash_runtime::oci::OciConfig {
                    insecure: loopback(&args.registry),
                    ..Default::default()
                },
                wash_runtime::oci::OciPullPolicy::Always,
            )
            .await?;
            std::fs::write(components.path().join("http.wasm"), bytes)?;
        }
        roles.push(pinned);
    }
    let ui_digest = if ui {
        if args.stamp_only {
            Some(
                inspect(
                    &root,
                    &format!("{}/wamn/platform-ui:{version}", args.registry),
                )
                .await?,
            )
        } else {
            let packed = wamn_publish::web_artifact::platform_ui::pack(
                &root.join("apps/platform_ui/dist"),
                &version,
            )?;
            let reference: oci_client::Reference =
                format!("{}/wamn/platform-ui:{}", args.registry, packed.tag).parse()?;
            let auth = match docker_credential::get_credential(reference.resolve_registry()) {
                Ok(DockerCredential::UsernamePassword(user, password)) => {
                    oci_client::secrets::RegistryAuth::Basic(user, password)
                }
                Ok(DockerCredential::IdentityToken(_)) => {
                    anyhow::bail!("an identity token is not a registry credential")
                }
                Err(
                    CredentialRetrievalError::ConfigNotFound
                    | CredentialRetrievalError::ConfigReadError
                    | CredentialRetrievalError::NoCredentialConfigured,
                ) => oci_client::secrets::RegistryAuth::Anonymous,
                Err(error) => return Err(error).context("read registry credential"),
            };
            let repository = wamn_package_artifact::Repository::with_auth(
                &wamn_package_artifact::PackageRegistry {
                    artifact_base: format!("{}/{}", reference.registry(), reference.repository()),
                    credentials: None,
                    registry_auth_file: Default::default(),
                    insecure_registry: loopback(&args.registry),
                    oci_ca_paths: Vec::new(),
                },
                reference.tag().context("platform UI tag is absent")?,
                auth,
            )?;
            Some(
                wamn_publish::web_artifact::push_archive(&packed, &repository)
                    .await?
                    .digest,
            )
        }
    } else {
        None
    };
    let stamped = release_chart::stamp(
        &chart,
        &ImageSet {
            host: images.remove("host").context("host image")?,
            http: roles.remove(0),
        },
        &args.out,
    )?;
    let path = stamped.join("values.yaml");
    let mut values: Value = serde_yaml::from_slice(&std::fs::read(&path)?)?;
    for (name, reference) in images {
        values["platform"][if name == "edge-web" { "edge" } else { name }]["image"] =
            reference.into();
    }
    values["platformUi"] = ui_digest.unwrap_or_default().into();
    std::fs::write(path, serde_yaml::to_string(&values)?)?;
    wamn_platform_install::platform::artifact_plan::prepare_payload(&root, &stamped)?;
    println!("{}", stamped.display());
    std::fs::copy(
        components.path().join("http.wasm"),
        args.out.join("http_route.wasm"),
    )?;
    for pin in palette_pins.values() {
        let digest = pin["descriptor"]["component-digest"]
            .as_str()
            .context("palette descriptor has no component digest")?;
        let name = format!("{}.wasm", digest.trim_start_matches("sha256:"));
        std::fs::copy(components.path().join(&name), args.out.join(name))?;
    }
    if !palette_pins.is_empty() {
        std::fs::write(
            args.out.join("palette.json"),
            wamn_digest::canonical_json_bytes(&serde_json::to_value(palette_pins)?),
        )?;
    }
    process::run(
        &root,
        "helm",
        &[
            "package",
            stamped.to_str().context("chart path")?,
            "--destination",
            args.out.to_str().context("chart output")?,
        ],
    )
    .await?;
    let archive = args.out.join(format!("release-{version}.tgz"));
    let destination = format!("oci://{}/wamn/charts", args.registry);
    let archive = archive.to_str().context("chart archive")?;
    let mut push = vec!["push", archive, destination.as_str()];
    if loopback(&args.registry) {
        push.push("--plain-http");
    }
    let registry_auth = std::env::var("WAMN_REGISTRY_AUTH_FILE").ok();
    if let Some(configuration) = &registry_auth {
        push.extend(["--registry-config", configuration]);
    }
    process::run(&root, "helm", &push).await?;
    let repository = format!("{}/wamn/charts/release", args.registry);
    let digest = inspect(&root, &format!("{repository}:{version}")).await?;
    println!("oci://{repository}@{digest}");
    Ok(())
}

async fn inspect(root: &std::path::Path, reference: &str) -> anyhow::Result<String> {
    let json = process::output(
        root,
        "docker",
        &[
            "buildx",
            "imagetools",
            "inspect",
            "--format",
            "{{json .Manifest}}",
            reference,
        ],
    )
    .await?;
    let value: Value = serde_json::from_str(&json)?;
    let digest = value["digest"]
        .as_str()
        .context("registry returned no digest")?;
    ensure!(
        digest
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())),
        "registry returned invalid digest for {reference}"
    );
    Ok(digest.to_owned())
}

fn loopback(registry: &str) -> bool {
    matches!(
        registry.split([':', '/']).next(),
        Some("localhost" | "127.0.0.1")
    ) || registry.starts_with("[::1]")
}

pub use self::platform_pools::{Direction, resize, work_directory};

/// Cloud node pool operations from the declared project.
#[derive(Debug, clap::Subcommand)]
pub enum CloudCommand {
    /// Set every node pool to its declared size.
    Wake { file: PathBuf },
    /// Set every node pool to zero nodes.
    Sleep { file: PathBuf },
}

pub async fn cloud(command: CloudCommand) -> anyhow::Result<()> {
    let (file, direction) = match command {
        CloudCommand::Wake { file } => (file, Direction::Wake),
        CloudCommand::Sleep { file } => (file, Direction::Sleep),
    };
    let document = wamn_platform_install::platform::document::ClusterDocument::compile(&file)?;
    resize(
        &document,
        &file.with_file_name("project.k"),
        direction,
        &work_directory()?,
        |line| {
            println!("{line}");
            Ok(())
        },
    )
    .await
}

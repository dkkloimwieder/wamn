//! File package assembly over an owned database.
use super::release_documents::{ORG, PROJECT};
use anyhow::{Context as _, ensure};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
pub const COMPONENT_INPUT: &str = "WAMN_BLOB_PUT_COMPONENT";

pub async fn build_fixture(
    work: &Path,
    kubeconfig: &Path,
    repository: &Path,
    database_url: &str,
) -> anyhow::Result<(
    PathBuf,
    Vec<wamn_package_control::component_admission::ComponentAdmission>,
)> {
    use wamn_compiler::build::generate_package;
    use wamn_compiler::{
        build::BuildRequest,
        build_record::{BUILD_RECORD_FILE, BuildComponent},
    };
    let source = repository.join("crates/control/environment/tests/support/file_package");
    let package = work.join("file-package/file_fixture");
    std::fs::create_dir_all(&package)?;
    std::fs::copy(source.join("kind.wamn.k.in"), package.join("wamn.k"))?;
    let palette = work.join("file-package/platform/execution/blob-put");
    std::fs::create_dir_all(&palette)?;
    std::fs::copy(
        source.join("publication/components/blob-put.json.in"),
        palette.join("declaration.json.in"),
    )?;
    let mut build_record = Box::pin(generate_package(&BuildRequest::new(
        &package,
        &work.join("file-package/target/wamn"),
        database_url,
    )))
    .await?;
    let output = wamn_compiler::output_root(&package);
    let mut resolve = wit_parser::Resolve::new();
    resolve.push_str(
        "node.wit",
        include_str!("../../../../../wit/wamn-node/package.wit"),
    )?;
    let (wit_package, _) = resolve.push_dir(output.join("wit"))?;
    let world = resolve.select_world(&[wit_package], Some("generated"))?;
    let mut module =
        wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )?;
    let fixture = wit_component::ComponentEncoder::default()
        .module(&module)?
        .validate(true)
        .encode()?;
    // Match wamn build: remove the ambient WASI CLI imports of the raw std guest.
    let target =
        repository.join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()));
    let blob = work.join("blob-put.wasm");
    run(
        kubeconfig,
        &target
            .join("debug/wamn-component-virtualizer")
            .to_string_lossy(),
        &[
            "--input",
            &std::env::var(COMPONENT_INPUT).context(COMPONENT_INPUT)?,
            "--output",
            &blob.to_string_lossy(),
        ],
    )
    .await?;
    let components = [("fixture", fixture), ("blob-put", std::fs::read(blob)?)];
    std::fs::create_dir_all(output.join("components"))?;
    for (name, bytes) in components {
        let path = format!("components/{name}.wasm");
        std::fs::write(output.join(&path), &bytes)?;
        build_record.outputs.components.push(BuildComponent {
            name: name.into(),
            path,
            sha256: wamn_digest::bytes_sha256(&bytes),
        });
    }
    std::fs::write(output.join(BUILD_RECORD_FILE), build_record.to_bytes())?;
    let coordinate = wamn_catalog::ComponentPackageCoordinate::new(
        wamn_registry::project_env_key(ORG, PROJECT, "files"),
        "file_fixture",
        "0.1.0",
    )?;
    let mut admissions = Vec::new();
    for name in ["fixture", "blob-put"] {
        let (declaration, platform_packages) = if name == "fixture" {
            (
                wamn_package_control::component_declaration::render_package_declaration(
                    &package,
                    "fixture",
                    &coordinate.project_env_key,
                )?,
                vec!["wamn:node".into(), "wamn:postgres".into()],
            )
        } else {
            let template = palette.join("declaration.json.in");
            let declaration =
                wamn_package_control::component_declaration::render_palette_declaration(
                    &template,
                    &coordinate,
                    None,
                )?;
            let packages = wamn_package_control::component_declaration::declared_platform_packages(
                &template,
                &declaration,
            )?;
            (declaration, packages)
        };
        let declaration_path = work.join(format!("file-{name}-declaration.json"));
        std::fs::write(&declaration_path, serde_json::to_vec(&declaration)?)?;
        admissions.push(wamn_package_control::component_admission::admit_component(
            wamn_package_control::component_admission::AdmitComponentRequest {
                package: package.clone(),
                component_bytes: output.join(format!("components/{name}.wasm")),
                declaration: declaration_path,
                admitted_platform_packages: platform_packages,
                base_digests: BTreeSet::new(),
            },
        )?);
    }
    Ok((package, admissions))
}
pub fn release_request()
-> anyhow::Result<wamn_release_control::publish_release::PublishReleaseRequest> {
    Ok(
        wamn_release_control::publish_release::PublishReleaseRequest {
            applications: [(
                "files".into(),
                wamn_catalog::ApplicationDefinition {
                    packages: ["file_fixture".into()].into(),
                    connections: BTreeMap::new(),
                },
            )]
            .into(),
            application: None,
            org: ORG.into(),
            project: PROJECT.into(),
            environment: "files".into(),
            packages: vec![wamn_catalog::PackageCoordinate::new(
                "file_fixture",
                "0.1.0",
            )?],
        },
    )
}
async fn run(kubeconfig: &Path, program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .env("KUBECONFIG", kubeconfig)
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("start {program}"))?;
    ensure!(
        output.status.success(),
        "{program} {} exited {}: {}",
        args.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

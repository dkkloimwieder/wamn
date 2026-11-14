//! Cargo compilation, byte validation and overlay composition.
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use tokio::process::Command;

const COMPILED_MANIFEST_NAME: &str = "wamn.json";
use crate::{BuildRequest, package_output};
use wamn_compiler::build_record::{self, BuildComponent, BuildRecord};
use wamn_compiler::manifest::PackageManifest;
use wamn_compiler::{ManifestError as GenerateError, ManifestErrorType as GenerateErrorType};
use wamn_digest::bytes_sha256;

/// Where the components lie in a package's build output.
const COMPONENTS: &str = "components";
const GUEST_TARGET: &str = "wasm32-wasip2";

/// One guest crate to build.
struct Guest {
    /// The Cargo manifest to build with.
    manifest: PathBuf,
    /// The Cargo package, when the manifest is a workspace root.
    package: String,
    /// The Cargo target directory, when the guest is its own workspace.
    target_directory: Option<PathBuf>,
}

/// One built guest.
struct Built {
    /// The Cargo package name.
    /// The built guest.
    file: PathBuf,
}

/// Compile only package-owned guests. Palette guests belong to the platform build.
pub(crate) async fn compile_components(
    request: &BuildRequest,
    package: &str,
) -> Result<Vec<BuildComponent>> {
    let output = package_output(&request.output_root, package);
    let manifest = PackageManifest::from_slice(&fs::read(output.join(COMPILED_MANIFEST_NAME))?)?;
    let workspace = wamn_compiler::package_workspace(&request.package_root);
    if !workspace.join("Cargo.toml").is_file() {
        ensure!(
            manifest.components.is_empty(),
            "package components require a Cargo workspace"
        );
        return Ok(Vec::new());
    }
    ensure!(
        output == wamn_compiler::output_root(&request.package_root),
        "component include paths require the package workspace output root"
    );
    let repository = workspace.join("..");
    let rustflags = guest_rustflags(&repository).await?;
    let bases = base_components(request, &manifest)?;
    let directory = output.join(COMPONENTS);
    fs::create_dir_all(&directory)?;
    let mut components = Vec::new();
    for name in manifest.components.keys() {
        let built = compile_guest(
            &repository,
            &rustflags,
            &Guest {
                manifest: workspace.join("Cargo.toml"),
                package: name.replace('_', "-"),
                target_directory: None,
            },
        )
        .await?;
        let bytes = fs::read(&built.file)?;
        let bytes = wamn_component_composer::compose(
            bytes,
            bases
                .iter()
                .map(fs::read)
                .collect::<std::io::Result<Vec<_>>>()?,
        )?;
        let path = format!("{COMPONENTS}/{name}.wasm");
        fs::write(output.join(&path), &bytes)?;
        components.push(BuildComponent {
            name: name.clone(),
            path,
            sha256: bytes_sha256(&bytes),
        });
    }
    Ok(components)
}

/// Derive guest path remapping from rustc and the current checkout.
pub async fn guest_rustflags(repository: &Path) -> Result<String> {
    let sysroot = Command::new("rustc")
        .current_dir(repository)
        .args(["--print", "sysroot"])
        .output()
        .await?;
    ensure!(sysroot.status.success(), "read rustc sysroot");
    let version = Command::new("rustc")
        .current_dir(repository)
        .arg("-vV")
        .output()
        .await?;
    ensure!(version.status.success(), "read rustc version");
    let version = String::from_utf8(version.stdout)?;
    let commit = version
        .lines()
        .find_map(|line| line.strip_prefix("commit-hash: "))
        .context("rustc has no commit hash")?;
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cargo")))
        .context("Cargo home is absent")?;
    let repository = repository.canonicalize()?;
    let sysroot = String::from_utf8(sysroot.stdout)?;
    let inherited = std::env::var("RUSTFLAGS").unwrap_or_default();
    Ok(format!(
        "{inherited} --remap-path-prefix={}=/wamn --remap-path-prefix={}=/cargo --remap-path-prefix={}/lib/rustlib/src/rust=/rustc/{commit}",
        repository.display(),
        cargo_home.display(),
        sysroot.trim()
    ))
}

pub async fn build_guest(
    repository: &Path,
    manifest: &Path,
    package: &str,
    target_directory: Option<&Path>,
) -> Result<PathBuf> {
    let rustflags = guest_rustflags(repository).await?;
    let guest = Guest {
        manifest: manifest.to_owned(),
        package: package.to_owned(),
        target_directory: target_directory.map(Path::to_owned),
    };
    let built = compile_guest(repository, &rustflags, &guest).await?;
    Ok(built.file)
}

/// Build one guest in its own Cargo invocation and test its bytes.
async fn compile_guest(repository: &Path, rustflags: &str, guest: &Guest) -> Result<Built> {
    let mut command = Command::new("cargo");
    command
        .current_dir(repository)
        .env("RUSTFLAGS", rustflags)
        .args([
            "build",
            "--locked",
            "--offline",
            "--release",
            "--target",
            GUEST_TARGET,
            "--message-format",
            "json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(&guest.manifest);
    command.args(["-p", &guest.package]);
    if let Some(target) = &guest.target_directory {
        command.env("CARGO_TARGET_DIR", target);
    }
    let output = command.output().await.context("run cargo build")?;
    let name = &guest.package;
    ensure!(
        output.status.success(),
        "cargo build of guest {name} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(&guest.manifest)
        .no_deps()
        .exec()?;
    let selected = metadata
        .packages
        .iter()
        .find(|package| package.name.as_str() == guest.package)
        .context("guest package is absent from Cargo metadata")?;
    let mut artifact = None;
    for message in cargo_metadata::Message::parse_stream(std::io::Cursor::new(output.stdout)) {
        if let cargo_metadata::Message::CompilerArtifact(message) = message?
            && message.package_id == selected.id
            && message.target.is_cdylib()
            && let Some(file) = message
                .filenames
                .iter()
                .find(|file| file.extension() == Some("wasm"))
        {
            artifact = Some(file.clone().into_std_path_buf());
        }
    }
    let file = artifact.context("cargo build gave no guest Wasm artifact")?;
    require_guest(
        repository,
        name,
        &file,
        selected.manifest_path.as_std_path(),
    )?;
    Ok(Built { file })
}

/// A built guest carries no local path, and it matches the pin beside its
/// crate when the crate has one.
fn require_guest(repository: &Path, name: &str, file: &Path, crate_manifest: &Path) -> Result<()> {
    let bytes = fs::read(file).with_context(|| format!("read {}", file.display()))?;
    ensure!(
        !bytes.is_empty(),
        "built guest {name} is empty: {}",
        file.display()
    );
    let cargo_home = std::env::var_os("CARGO_HOME").map_or_else(
        || {
            std::env::var_os("HOME")
                .map(|home| Path::new(&home).join(".cargo"))
                .unwrap_or_default()
        },
        PathBuf::from,
    );
    let repository = fs::canonicalize(repository)
        .with_context(|| format!("resolve {}", repository.display()))?;
    for local in [
        Path::new("/home/"),
        cargo_home.as_path(),
        repository.as_path(),
    ] {
        let local = local.as_os_str().as_encoded_bytes();
        ensure!(
            local.is_empty() || !bytes.windows(local.len()).any(|window| window == local),
            "built guest {name} embeds a local path: {} (see xtask guest-rustflags)",
            file.display()
        );
    }
    let artifact = file
        .file_name()
        .context("a built guest has a file name")?
        .to_string_lossy();
    let pin = crate_manifest.with_file_name(format!("{artifact}.sha256"));
    if let Ok(text) = fs::read_to_string(&pin) {
        let expected = text.split_whitespace().next().unwrap_or_default();
        let actual = bytes_sha256(&bytes);
        ensure!(
            actual.strip_prefix("sha256:") == Some(expected),
            "built guest {name} is {actual} but {} pins sha256:{expected}. If the change is intended, write the new digest to the pin.",
            pin.display()
        );
    }
    Ok(())
}

/// Resolve built base components and enforce authored pins unless the dev request permits drift.
pub(crate) fn base_components(
    request: &BuildRequest,
    manifest: &PackageManifest,
) -> Result<Vec<PathBuf>> {
    let mut bases = Vec::new();
    for dependency in manifest.base_dependencies.values() {
        let base_output = package_output(&request.output_root, &dependency.package);
        let base_build_record = build_record::read(&base_output).with_context(|| {
            format!(
                "read the build of base {}; build it first",
                dependency.package
            )
        })?;
        ensure!(
            base_build_record.package.version == dependency.version,
            "base {} is built at {}, not {}",
            dependency.package,
            base_build_record.package.version,
            dependency.version
        );
        let base_manifest = PackageManifest::from_slice(
            &fs::read(base_output.join(COMPILED_MANIFEST_NAME))
                .with_context(|| format!("read the wamn.json of base {}", dependency.package))?,
        )?;
        let [base_component] = base_manifest.components.keys().collect::<Vec<_>>()[..] else {
            bail!(
                "base {} must declare exactly one component",
                dependency.package
            );
        };
        let (file, digest) = component_file(&base_output, &base_build_record, base_component)?;
        if digest != dependency.digest && !request.allow_stale_base_pins {
            return Err(GenerateError::for_object(
                GenerateErrorType::InvalidComponent,
                format!(
                    "package {} pins base {} to {}, but its built component digest is {}",
                    manifest.package.id, dependency.package, dependency.digest, digest
                ),
                dependency.package.as_str(),
            )
            .into());
        }
        bases.push(file);
    }
    Ok(bases)
}

/// The file of the component `name` that a base's `build.json` records,
/// validated against its digest.
fn component_file(
    output: &Path,
    build_record: &BuildRecord,
    name: &str,
) -> Result<(PathBuf, String)> {
    let entry = build_record
        .outputs
        .components
        .iter()
        .find(|entry| entry.name == name)
        .with_context(|| {
            format!(
                "the build of {} records no component {name}",
                build_record.package.id
            )
        })?;
    let path = output.join(&entry.path);
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    let digest = bytes_sha256(&bytes);
    if digest != entry.sha256 {
        return Err(GenerateError::for_object(
            GenerateErrorType::InvalidComponent,
            format!(
                "{} differs from its build.json entry; build {} again",
                path.display(),
                build_record.package.id
            ),
            name,
        )
        .into());
    }
    Ok((path, digest))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        let root = std::env::temp_dir().join(format!(
            "wamn-compiler-components-{}-{name}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create the scratch directory");
        Scratch(root)
    }

    fn moved_base_pin(name: &str) -> (Scratch, BuildRequest, PackageManifest, PathBuf) {
        let scratch = scratch(name);
        let base = scratch.0.join("base");
        let output_root = scratch.0.join("target/wamn");
        let output = package_output(&output_root, "base");
        fs::create_dir_all(&base).unwrap();
        fs::create_dir_all(output.join(COMPONENTS)).unwrap();
        let mut manifest = serde_json::json!({
            "package": {"id": "base", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "policy", "state": "satisfied"},
            "models": {}, "connections": {},
            "components": {"base": {}}
        });
        fs::write(
            output.join(COMPILED_MANIFEST_NAME),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let bytes = wat::parse_str(
            r#"(component (instance $base) (export "test:base/op@1.0.0" (instance $base)))"#,
        )
        .unwrap();
        let file = output.join("components/base.wasm");
        fs::write(&file, &bytes).unwrap();
        let built = BuildRecord {
            schema: build_record::BUILD_RECORD_SCHEMA.to_owned(),
            package: build_record::BuildPackage {
                id: "base".into(),
                version: "1.0.0".into(),
            },
            dependencies: Vec::new(),
            generator: build_record::generator().unwrap(),
            outputs: build_record::BuildOutputs {
                manifest: bytes_sha256(&fs::read(output.join(COMPILED_MANIFEST_NAME)).unwrap()),
                files: BTreeMap::new(),
                components: vec![BuildComponent {
                    name: "base".into(),
                    path: "components/base.wasm".into(),
                    sha256: bytes_sha256(&bytes),
                }],
            },
        };
        fs::write(
            output.join(build_record::BUILD_RECORD_FILE),
            built.to_bytes(),
        )
        .unwrap();
        manifest["package"]["id"] = "overlay".into();
        manifest["components"] = serde_json::json!({"overlay": {}});
        manifest["base_dependencies"] = serde_json::json!({"base": {
            "package": "base", "version": "1.0.0", "digest": bytes_sha256(b"old base"),
            "operations": []
        }});
        let manifest =
            PackageManifest::from_slice(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        let request = BuildRequest::new(&scratch.0.join("overlay"), &output_root, "");
        (scratch, request, manifest, file)
    }

    #[test]
    fn a_default_request_refuses_a_moved_base_pin() {
        let (_scratch, request, manifest, _file) = moved_base_pin("default-pin");
        let error = base_components(&request, &manifest).unwrap_err();
        let refusal = error.downcast_ref::<GenerateError>().unwrap();
        assert_eq!(refusal.error_type(), GenerateErrorType::InvalidComponent);
        assert_eq!(refusal.object(), Some("base"));
    }

    #[test]
    fn a_dev_request_composes_over_the_built_base_after_its_pin_moves() {
        let (_scratch, mut request, manifest, file) = moved_base_pin("dev-pin");
        request.allow_stale_base_pins = true;
        let bases = base_components(&request, &manifest).unwrap();
        let overlay =
            wat::parse_str(r#"(component (import "test:base/op@1.0.0" (instance)))"#).unwrap();
        let composed = wamn_component_composer::compose(
            overlay,
            bases.iter().map(|path| fs::read(path).unwrap()).collect(),
        )
        .expect("compose over the built base");
        assert!(!composed.is_empty());

        fs::write(&file, b"changed after the build record").unwrap();
        let error = base_components(&request, &manifest).unwrap_err();
        let refusal = error.downcast_ref::<GenerateError>().unwrap();
        assert_eq!(refusal.error_type(), GenerateErrorType::InvalidComponent);
    }

    #[test]
    fn a_guest_with_a_local_path_or_off_its_pin_is_refused() {
        let scratch = scratch("guest");
        let repository = scratch.0.join("repository");
        let crate_directory = repository.join("guest");
        fs::create_dir_all(&crate_directory).unwrap();
        let manifest = crate_directory.join("Cargo.toml");
        fs::write(&manifest, "[package]\nname = \"guest-crate\"\n").unwrap();

        let file = scratch.0.join("guest.wasm");
        fs::write(&file, b"guest bytes").unwrap();
        require_guest(&repository, "guest", &file, &manifest).expect("a clean guest is accepted");

        let local = format!("bytes {}", fs::canonicalize(&repository).unwrap().display());
        fs::write(&file, local).unwrap();
        require_guest(&repository, "guest", &file, &manifest).unwrap_err();

        fs::write(&file, b"guest bytes").unwrap();
        fs::write(
            crate_directory.join("guest.wasm.sha256"),
            format!("{}  guest.wasm\n", "0".repeat(64)),
        )
        .unwrap();
        require_guest(&repository, "guest", &file, &manifest).unwrap_err();
    }
}

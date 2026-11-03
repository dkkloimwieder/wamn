//! Step 7 of `wamn build`
//! (docs/architecture/components.md#guest-build-identity): compile the
//! package's components and record them in `build.json`.
//!
//! The build compiles, each in its own Cargo invocation with the guest
//! RUSTFLAGS of `tools/guest-rustflags`, release profile, for
//! `wasm32-wasip2`: the components the package owns and the palette
//! components its wirings name. Cargo combines dependency features within one
//! invocation, which can change bytes, so no two guests share one.
//!
//! A built guest must carry no local path and must match the pin beside its
//! crate, `<artifact>.wasm.sha256`, when it has one. The owned components and
//! the crates that `tools/component-virtualization.json` lists are
//! virtualized. An overlay whose component declaration names a base operation
//! dependency is composed with each base component (read from the base's
//! `build.json`). Each owned component and palette
//! component is copied to `components/<name>.wasm` in the package's build
//! output, and `outputs.components` of `build.json` names it with its digest.
//!
//! A package outside the application workspace has no component to compile.
//! Top-level applications and nested platform packages share that workspace.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

use crate::authoring::COMPILED_MANIFEST_NAME;
use crate::build::{BuildRequest, package_output};
use crate::build_record::{self, BUILD_RECORD_FILE, BuildComponent, BuildRecord};
use wamn_catalog::manifest::PackageManifest;
use wamn_catalog::{ManifestError as GenerateError, ManifestErrorType as GenerateErrorType};
use wamn_execution_contract::bytes_sha256;

/// Where the components lie in a package's build output.
const COMPONENTS: &str = "components";
/// The virtualization allowlist, relative to the repository.
const VIRTUALIZATION: &str = "tools/component-virtualization.json";
/// The script that prints the guest RUSTFLAGS, relative to the repository.
const GUEST_RUSTFLAGS: &str = "tools/guest-rustflags";
/// The palette declarations, relative to the Cargo workspace of the packages.
const PLATFORM: &str = "platform";
const GUEST_TARGET: &str = "wasm32-wasip2";
const VIRTUALIZER: &str = "wamn-component-virtualizer";
const COMPOSER: &str = "wamn-component-composer";

/// `tools/component-virtualization.json`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Virtualization {
    output_subdirectory: String,
    packages: BTreeSet<String>,
}

/// One guest crate to build.
struct Guest {
    /// The Cargo manifest to build with.
    manifest: PathBuf,
    /// The Cargo package, when the manifest is a workspace root.
    package: Option<String>,
    /// The Cargo target directory, when the guest is its own workspace.
    target_directory: Option<PathBuf>,
}

/// One built guest.
struct Built {
    /// The Cargo package name.
    package: String,
    /// The release output: the raw guest, then the virtualized one when the
    /// allowlist names the package.
    file: PathBuf,
}

/// Compile the components of the package that `request` names, whose id is
/// `package`, after generation, then copy clients to their consumers.
/// Record the full input digest and compiled outputs in its `build.json`.
///
/// # Errors
///
/// When generated output is stale, when a guest does not build,
/// carries a local path or differs from its pin, when virtualization or
/// composition fails, or when the output cannot be written.
pub async fn build_components(request: &BuildRequest, package: &str) -> Result<BuildRecord> {
    let package_root = request.package_root.as_path();
    let output = package_output(&request.output_root, package);
    let mut build_record = build_record::read_generated(package_root, &output)?;
    build_record.inputs = build_record::inputs(package_root)?;
    build_record.outputs.components.clear();
    let path = output.join(BUILD_RECORD_FILE);
    fs::write(&path, build_record.to_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    let build_record = compile_components(request, package).await?;
    crate::materialize::materialize_client_consumers(
        package_root,
        package,
        &output.join("client-ts"),
    )?;
    fs::write(&path, build_record.to_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(build_record)
}

async fn compile_components(request: &BuildRequest, package: &str) -> Result<BuildRecord> {
    let package_root = request.package_root.as_path();
    let output = package_output(&request.output_root, package);
    let mut build_record = build_record::read_current(package_root, &output)?;
    let manifest = PackageManifest::from_slice(
        &fs::read(output.join(COMPILED_MANIFEST_NAME))
            .context("read the built wamn.json; run wamn build first")?,
    )
    .context("parse the built wamn.json")?;
    let workspace = crate::output::package_workspace(package_root);
    if !workspace.join("Cargo.toml").is_file() {
        return Ok(build_record);
    }
    let compiler_output = crate::output_root(package_root);
    if compiler_output != output {
        build_record::read_current(package_root, &compiler_output)?;
    }
    for dependency in manifest.base_dependencies.values() {
        let root = crate::output::dependency_root(package_root, &dependency.package);
        let compiler_output = crate::output_root(&root);
        if compiler_output != package_output(&request.output_root, &dependency.package) {
            build_record::read_current(&root, &compiler_output)?;
        }
    }
    let bases = base_components(request, &manifest)?;
    let repository = workspace.join("..");
    let rustflags = guest_rustflags(&repository).await?;
    let virtualization: Virtualization = read_json(&repository.join(VIRTUALIZATION))?;

    let mut allowlist = virtualization.packages.clone();
    let mut records = Vec::<(String, PathBuf)>::new();

    // The owned components.
    let mut owned = Vec::new();
    for name in manifest.components.keys() {
        let package = name.replace('_', "-");
        allowlist.insert(package.clone());
        let built = compile_guest(
            &repository,
            &rustflags,
            &Guest {
                manifest: workspace.join("Cargo.toml"),
                package: Some(package),
                target_directory: None,
            },
        )
        .await?;
        owned.push((name.clone(), built));
    }
    // The palette components that the wirings name.
    for name in wiring_components(package_root)? {
        if manifest.components.contains_key(&name) {
            continue;
        }
        let declaration = palette_declaration(&workspace, &name)?;
        let directory = declaration
            .parent()
            .context("a palette declaration has a directory")?;
        let built = compile_guest(
            &repository,
            &rustflags,
            &Guest {
                manifest: directory.join("Cargo.toml"),
                package: None,
                target_directory: None,
            },
        )
        .await?;
        let built = virtualize(&repository, &virtualization, &allowlist, built).await?;
        records.push((name, built.file));
    }
    for (name, built) in owned {
        let built = virtualize(&repository, &virtualization, &allowlist, built).await?;
        let file = compose(&repository, &manifest, &built.file, &bases).await?;
        records.push((name, file));
    }

    let directory = output.join(COMPONENTS);
    if directory.exists() {
        fs::remove_dir_all(&directory)
            .with_context(|| format!("remove {}", directory.display()))?;
    }
    fs::create_dir_all(&directory).with_context(|| format!("create {}", directory.display()))?;
    let mut components = Vec::new();
    for (name, file) in records {
        let bytes = fs::read(&file).with_context(|| format!("read {}", file.display()))?;
        let path = format!("{COMPONENTS}/{name}.wasm");
        let target = output.join(&path);
        fs::write(&target, &bytes).with_context(|| format!("write {}", target.display()))?;
        components.push(BuildComponent {
            name,
            path,
            sha256: bytes_sha256(&bytes),
        });
    }
    components.sort_by(|left, right| left.name.cmp(&right.name));
    build_record.outputs.components = components;
    let path = output.join(BUILD_RECORD_FILE);
    fs::write(&path, build_record.to_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(build_record)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("read {}", path.display()))?)
        .with_context(|| format!("parse {}", path.display()))
}

/// The guest RUSTFLAGS that `tools/guest-rustflags` prints.
async fn guest_rustflags(repository: &Path) -> Result<String> {
    let output = Command::new(repository.join(GUEST_RUSTFLAGS))
        .current_dir(repository)
        .output()
        .await
        .with_context(|| format!("run {GUEST_RUSTFLAGS}"))?;
    ensure!(
        output.status.success(),
        "{GUEST_RUSTFLAGS} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)
        .context("read the guest RUSTFLAGS")?
        .trim()
        .to_owned())
}

/// Build the guest `package` of the Cargo workspace at `manifest` the way step 7
/// builds a guest, and return its release output. This builds a guest that no
/// package build compiles, such as a platform role component. Its output lies
/// in `target_directory` when one is given.
///
/// # Errors
///
/// When the guest does not build, carries a local path, or differs from its
/// pin.
pub async fn build_guest(
    repository: &Path,
    manifest: &Path,
    package: &str,
    target_directory: Option<&Path>,
) -> Result<PathBuf> {
    let rustflags = guest_rustflags(repository).await?;
    let guest = Guest {
        manifest: manifest.to_owned(),
        package: Some(package.to_owned()),
        target_directory: target_directory.map(Path::to_owned),
    };
    Ok(compile_guest(repository, &rustflags, &guest).await?.file)
}

/// Build one guest in its own Cargo invocation and check its bytes.
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
    if let Some(package) = &guest.package {
        command.args(["-p", package]);
    }
    if let Some(target) = &guest.target_directory {
        command.env("CARGO_TARGET_DIR", target);
    }
    let output = command.output().await.context("run cargo build")?;
    let name = guest
        .package
        .as_deref()
        .map_or_else(|| guest.manifest.display().to_string(), str::to_owned);
    ensure!(
        output.status.success(),
        "cargo build of guest {name} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut built = Vec::new();
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let cdylib = message["target"]["crate_types"]
            .as_array()
            .is_some_and(|types| types.iter().any(|kind| kind == "cdylib"));
        if message["reason"] != "compiler-artifact" || !cdylib {
            continue;
        }
        let file = message["filenames"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .find(|file| {
                Path::new(file)
                    .extension()
                    .is_some_and(|extension| extension == "wasm")
            });
        if let (Some(file), Some(manifest)) = (file, message["manifest_path"].as_str()) {
            built.push((PathBuf::from(file), PathBuf::from(manifest)));
        }
    }
    // A dependency can be a cdylib too. Cargo finishes it before its
    // dependents, so the guest is the last one.
    let Some((file, crate_manifest)) = built.last() else {
        bail!("cargo build of guest {name} gave no cdylib output");
    };
    check_guest(repository, &name, file, crate_manifest)?;
    let package = match &guest.package {
        Some(package) => package.clone(),
        None => crate_package(crate_manifest)?,
    };
    Ok(Built {
        package,
        file: file.clone(),
    })
}

/// The package name of the crate at `manifest`.
fn crate_package(manifest: &Path) -> Result<String> {
    let text =
        fs::read_to_string(manifest).with_context(|| format!("read {}", manifest.display()))?;
    text.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("name = \"")
                .and_then(|rest| rest.strip_suffix('"'))
        })
        .map(str::to_owned)
        .with_context(|| format!("{} names no package", manifest.display()))
}

/// A built guest carries no local path, and it matches the pin beside its
/// crate when the crate has one.
fn check_guest(repository: &Path, name: &str, file: &Path, crate_manifest: &Path) -> Result<()> {
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
            "built guest {name} embeds a local path: {} (see {GUEST_RUSTFLAGS})",
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

/// Virtualize a built guest when the allowlist names its package, into
/// `<target>/<output_subdirectory>/` beside its release output.
async fn virtualize(
    repository: &Path,
    virtualization: &Virtualization,
    allowlist: &BTreeSet<String>,
    built: Built,
) -> Result<Built> {
    if !allowlist.contains(&built.package) {
        return Ok(built);
    }
    let target = built
        .file
        .ancestors()
        .nth(3)
        .context("a guest lies in <target>/wasm32-wasip2/release")?;
    let file_name = built.file.file_name().context("a guest has a file name")?;
    let output = target
        .join(&virtualization.output_subdirectory)
        .join(file_name);
    let parent = output.parent().context("an output has a directory")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let partial = output.with_extension(format!("wasm.partial.{}", std::process::id()));
    let mut command = tool(repository, VIRTUALIZER);
    command
        .arg("--input")
        .arg(&built.file)
        .arg("--output")
        .arg(&partial);
    run_tool(
        command,
        &partial,
        &output,
        &format!("virtualize {}", built.package),
    )
    .await?;
    Ok(Built {
        package: built.package,
        file: output,
    })
}

/// `cargo run` of one tool of the repository's root workspace.
fn tool(repository: &Path, package: &str) -> Command {
    let mut command = Command::new("cargo");
    command
        .current_dir(repository)
        .args(["run", "--locked", "--offline", "--manifest-path"])
        .arg(repository.join("Cargo.toml"))
        .args(["-p", package, "--"]);
    command
}

/// Run a tool that writes `partial`, then move it to `output`.
async fn run_tool(mut command: Command, partial: &Path, output: &Path, what: &str) -> Result<()> {
    let _ = fs::remove_file(partial);
    let result = command.output().await.with_context(|| what.to_owned())?;
    if !result.status.success() || fs::metadata(partial).map_or(true, |file| file.len() == 0) {
        let _ = fs::remove_file(partial);
        bail!(
            "{what} failed:\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    fs::rename(partial, output).with_context(|| format!("write {}", output.display()))
}

/// Resolve built base components and enforce authored pins unless the dev request permits drift.
pub(crate) fn base_components(
    request: &BuildRequest,
    manifest: &PackageManifest,
) -> Result<Vec<PathBuf>> {
    let mut bases = Vec::new();
    for dependency in manifest.base_dependencies.values() {
        let base_output = package_output(&request.output_root, &dependency.package);
        let base_root = crate::output::dependency_root(&request.package_root, &dependency.package);
        let base_build_record =
            build_record::read_current(&base_root, &base_output).with_context(|| {
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

/// Compose an overlay with the bases its manifest pins in
/// `base_dependencies`. The composer returns an overlay that calls no base
/// unchanged. A package without bases is returned as it is.
async fn compose(
    repository: &Path,
    manifest: &PackageManifest,
    file: &Path,
    bases: &[PathBuf],
) -> Result<PathBuf> {
    if bases.is_empty() {
        return Ok(file.to_owned());
    }
    let mut command = tool(repository, COMPOSER);
    command.arg("--overlay").arg(file);
    for base in bases {
        command.arg("--base").arg(base);
    }
    // The composed component replaces the overlay's output.
    let partial = file.with_extension(format!("wasm.composed.{}", std::process::id()));
    command.arg("--output").arg(&partial);
    run_tool(
        command,
        &partial,
        file,
        &format!("compose {}", manifest.package.id),
    )
    .await?;
    Ok(file.to_owned())
}

/// The file of the component `name` that a base's `build.json` records,
/// checked against its digest.
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
    ensure!(
        digest == entry.sha256,
        "{} differs from its build.json entry; build {} again",
        path.display(),
        build_record.package.id
    );
    Ok((path, digest))
}

/// The component names of the nodes of every wiring in `publication/wirings/`.
fn wiring_components(package_root: &Path) -> Result<BTreeSet<String>> {
    let directory = package_root.join("publication/wirings");
    let mut names = BTreeSet::new();
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(names),
        Err(error) => return Err(error).with_context(|| format!("read {}", directory.display())),
    };
    for entry in entries {
        let path = entry
            .with_context(|| format!("read {}", directory.display()))?
            .path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let wiring: Value = read_json(&path)?;
        for node in wiring["nodes"]
            .as_object()
            .into_iter()
            .flat_map(|nodes| nodes.values())
        {
            if node.get("operation-dependency").is_some() {
                continue;
            }
            let component = node["component"]
                .as_str()
                .with_context(|| format!("a node of {} names no component", path.display()))?;
            names.insert(component.to_owned());
        }
    }
    Ok(names)
}

/// The one `declaration.json.in` under `<workspace>/platform/*/*/` whose
/// `component` is `name`.
fn palette_declaration(workspace: &Path, name: &str) -> Result<PathBuf> {
    let platform = workspace.join(PLATFORM);
    let mut found = Vec::new();
    for area in fs::read_dir(&platform).with_context(|| format!("read {}", platform.display()))? {
        let area = area
            .with_context(|| format!("read {}", platform.display()))?
            .path();
        if !area.is_dir() {
            continue;
        }
        for member in fs::read_dir(&area).with_context(|| format!("read {}", area.display()))? {
            let declaration = member
                .with_context(|| format!("read {}", area.display()))?
                .path()
                .join("declaration.json.in");
            if !declaration.is_file() {
                continue;
            }
            let document: Value = read_json(&declaration)?;
            if document["component"] == name {
                found.push(declaration);
            }
        }
    }
    let [declaration] = found.as_slice() else {
        bail!(
            "{} platform declarations name component {name}, not one",
            found.len()
        );
    };
    Ok(declaration.clone())
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
            "wamn-schema-generator-components-{}-{name}",
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
            "components": {"base": {"connections": []}}
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
            inputs: build_record::inputs(&base).unwrap(),
            dependencies: Vec::new(),
            generator: build_record::generator(),
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
        fs::write(output.join(BUILD_RECORD_FILE), built.to_bytes()).unwrap();
        manifest["package"]["id"] = "overlay".into();
        manifest["components"] = serde_json::json!({"overlay": {"connections": []}});
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
        let (_scratch, request, manifest, file) = moved_base_pin("default-pin");
        let error = base_components(&request, &manifest).unwrap_err();
        let refusal = error.downcast_ref::<GenerateError>().unwrap();
        assert_eq!(refusal.error_type(), GenerateErrorType::InvalidComponent);
        assert_eq!(refusal.object(), Some("base"));
        assert_eq!(
            error.to_string(),
            format!(
                "InvalidComponent: package overlay pins base base to {}, but its built component digest is {}",
                manifest.base_dependencies["base"].digest,
                bytes_sha256(&fs::read(file).unwrap())
            )
        );
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
        let built_base = fs::read(&file).unwrap();
        assert!(
            composed
                .windows(built_base.len())
                .any(|bytes| bytes == built_base),
            "the composed component embeds the built base bytes"
        );

        fs::write(&file, b"changed after the build record").unwrap();
        let error = base_components(&request, &manifest).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("differs from its build.json entry")
        );
    }

    #[tokio::test]
    async fn package_build_does_not_run_the_web_build_or_require_web_output() {
        let scratch = scratch("no-web-build");
        let package = scratch.0.join("package");
        let output_root = scratch.0.join("target/wamn");
        let output = package_output(&output_root, "package");
        fs::create_dir_all(package.join("web")).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(package.join("wamn.k"), "# package\n").unwrap();
        fs::write(
            package.join("web/package.json"),
            r#"{"scripts":{"build":"node -e \"process.exit(1)\""}}"#,
        )
        .unwrap();
        let manifest = br#"{
            "package": {"id": "package", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "policy", "state": "satisfied"},
            "models": {}, "connections": {}, "components": {}
        }"#;
        fs::write(output.join(COMPILED_MANIFEST_NAME), manifest).unwrap();
        let record = BuildRecord {
            schema: build_record::BUILD_RECORD_SCHEMA.into(),
            package: build_record::BuildPackage {
                id: "package".into(),
                version: "1.0.0".into(),
            },
            inputs: build_record::inputs(&package).unwrap(),
            dependencies: Vec::new(),
            generator: build_record::generator(),
            outputs: build_record::BuildOutputs {
                manifest: bytes_sha256(manifest),
                files: BTreeMap::new(),
                components: Vec::new(),
            },
        };
        fs::write(output.join(BUILD_RECORD_FILE), record.to_bytes()).unwrap();
        let built = build_components(&BuildRequest::new(&package, &output_root, ""), "package")
            .await
            .unwrap();
        assert_eq!(built, record);
        assert!(
            crate::build::current_build(&package, &output_root)
                .unwrap()
                .is_some()
        );
        assert!(!output.join("web").exists());
    }

    #[tokio::test]
    async fn a_custom_output_cannot_hide_stale_macro_inputs() {
        let scratch = scratch("macro-inputs");
        let package = scratch.0.join("package");
        let selected = scratch.0.join("selected");
        let output = selected.join("package");
        let compiler_output = crate::output_root(&package);
        for directory in [&package, &output, &compiler_output] {
            fs::create_dir_all(directory).unwrap();
        }
        fs::write(scratch.0.join("Cargo.toml"), "[workspace]\n").unwrap();
        fs::write(package.join("wamn.k"), "# package\n").unwrap();
        fs::write(
            output.join(COMPILED_MANIFEST_NAME),
            br#"{
            "package": {"id": "package", "version": "1.0.0"},
            "required_platform_policy_contract": {"id": "policy", "state": "satisfied"},
            "models": {}, "connections": {}, "components": {}
        }"#,
        )
        .unwrap();
        let mut built = BuildRecord {
            schema: build_record::BUILD_RECORD_SCHEMA.to_owned(),
            package: build_record::BuildPackage {
                id: "package".into(),
                version: "1.0.0".into(),
            },
            inputs: build_record::inputs(&package).unwrap(),
            dependencies: Vec::new(),
            generator: build_record::generator(),
            outputs: build_record::BuildOutputs {
                manifest: "sha256:manifest".into(),
                files: BTreeMap::new(),
                components: Vec::new(),
            },
        };
        fs::write(
            output.join(build_record::BUILD_RECORD_FILE),
            built.to_bytes(),
        )
        .unwrap();
        built.inputs.digest = "sha256:old".into();
        fs::write(
            compiler_output.join(build_record::BUILD_RECORD_FILE),
            built.to_bytes(),
        )
        .unwrap();
        let error = build_components(&BuildRequest::new(&package, &selected, ""), "package")
            .await
            .expect_err("stale macro inputs were accepted");
        let error = format!("{error:#}");
        assert!(error.contains("is stale"), "{error}");
        assert!(error.contains("sha256:old"), "{error}");
    }

    #[test]
    fn wiring_discovery_leaves_cross_package_nodes_to_the_release() {
        let scratch = scratch("dependency");
        let directory = scratch.0.join("publication/wirings");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("pull.json"), r#"{
            "nodes": {
                "page": {"component": "http-page"},
                "consumer": {"component": "consumer", "operation-dependency": {"alias": "consumer", "operation": "item.record"}}
            }
        }"#).unwrap();
        assert_eq!(
            wiring_components(&scratch.0).unwrap(),
            BTreeSet::from(["http-page".into()])
        );
    }

    #[test]
    fn a_guest_with_a_local_path_or_off_its_pin_is_refused() {
        let scratch = scratch("check");
        let repository = scratch.0.join("repository");
        let crate_directory = repository.join("guest");
        fs::create_dir_all(&crate_directory).unwrap();
        let manifest = crate_directory.join("Cargo.toml");
        fs::write(&manifest, "[package]\nname = \"guest-crate\"\n").unwrap();
        assert_eq!(crate_package(&manifest).unwrap(), "guest-crate");

        let file = scratch.0.join("guest.wasm");
        fs::write(&file, b"guest bytes").unwrap();
        check_guest(&repository, "guest", &file, &manifest).expect("a clean guest is accepted");

        let local = format!("bytes {}", fs::canonicalize(&repository).unwrap().display());
        fs::write(&file, local).unwrap();
        let error = check_guest(&repository, "guest", &file, &manifest).unwrap_err();
        assert!(
            error.to_string().contains("embeds a local path"),
            "{error:#}"
        );

        fs::write(&file, b"guest bytes").unwrap();
        fs::write(
            crate_directory.join("guest.wasm.sha256"),
            format!("{}  guest.wasm\n", "0".repeat(64)),
        )
        .unwrap();
        let error = check_guest(&repository, "guest", &file, &manifest).unwrap_err();
        assert!(error.to_string().contains("pins sha256:"), "{error:#}");
    }
}

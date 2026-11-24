//! Cargo compilation, byte validation and overlay composition.
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, ensure};
use tokio::process::Command;

const COMPILED_MANIFEST_NAME: &str = "wamn.json";
use crate::{BuildMode, PreparedSource, package_output};
use wamn_codegen::{COMPONENTS_DIRECTORY, component_path, fingerprint};
use wamn_component_policy::PackageManifest;
use wamn_digest::{Digest, bytes_sha256};
use wamn_package::PackageName;
use wamn_package::manifest::{
    ManifestError as GenerateError, ManifestErrorType as GenerateErrorType,
};

/// Compilation target for an application or platform guest.
#[derive(Clone, Copy, Debug)]
pub enum GuestTarget {
    WasiP2,
    WasiP3,
}

impl GuestTarget {
    fn as_str(self) -> &'static str {
        match self {
            Self::WasiP2 => "wasm32-wasip2",
            Self::WasiP3 => "wasm32-wasip3",
        }
    }
}

/// One guest crate to build.
struct Guest {
    /// The Cargo manifest to build with.
    manifest: PathBuf,
    /// The Cargo package, when the manifest is a workspace root.
    package: String,
    target: GuestTarget,
}

/// One built guest.
struct Built {
    /// The Cargo package name.
    /// The built guest.
    file: PathBuf,
}

/// Compile only package-owned guests. Palette guests belong to the platform build.
pub(crate) async fn compile_components(
    root: &Path,
    output: &Path,
    manifest: &PackageManifest,
    bases: &[(PathBuf, Digest)],
    rustflags: &str,
) -> Result<BTreeMap<PathBuf, Digest>> {
    let workspace = wamn_codegen::package_workspace(root);
    if !workspace.join("Cargo.toml").is_file() {
        ensure!(
            manifest.components.is_empty(),
            "package components require a Cargo workspace"
        );
        return Ok(BTreeMap::new());
    }
    ensure!(
        output == wamn_codegen::output_root(root),
        "component include paths require the package workspace output root"
    );
    let repository = workspace.join("..");
    let directory = output.join(COMPONENTS_DIRECTORY);
    fs::create_dir_all(&directory)?;
    let mut components = BTreeMap::new();
    for name in manifest.components.keys() {
        let built = compile_guest(
            &repository,
            rustflags,
            &Guest {
                manifest: workspace.join("Cargo.toml"),
                package: name.as_str().replace('_', "-"),
                target: GuestTarget::WasiP2,
            },
        )
        .await?;
        let bytes = fs::read(&built.file)?;
        let bytes = crate::composer::compose(
            bytes,
            bases
                .iter()
                .map(|(path, digest)| {
                    let bytes =
                        fs::read(path).with_context(|| format!("read {}", path.display()))?;
                    ensure!(
                        bytes_sha256(&bytes) == *digest,
                        "{} changed during the overlay build; retry",
                        path.display()
                    );
                    Ok(bytes)
                })
                .collect::<Result<Vec<_>>>()?,
        )?;
        let path = component_path(name.as_str());
        fs::write(output.join(&path), &bytes)?;
        components.insert(path, bytes_sha256(&bytes));
    }
    Ok(components)
}

/// Derive guest path remapping from rustc and the current checkout.
pub async fn guest_rustflags(repository: &Path) -> Result<String> {
    Ok(guest_compiler(repository).await?.0)
}

pub(crate) async fn guest_compiler(repository: &Path) -> Result<(String, String)> {
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
    let flags = format!(
        "{inherited} --remap-path-prefix={}=/wamn --remap-path-prefix={}=/cargo --remap-path-prefix={}/lib/rustlib/src/rust=/rustc/{commit}",
        repository.display(),
        cargo_home.display(),
        sysroot.trim()
    );
    Ok((flags, version))
}

pub async fn build_guest(
    repository: &Path,
    manifest: &Path,
    package: &str,
    target: GuestTarget,
) -> Result<PathBuf> {
    let rustflags = guest_rustflags(repository).await?;
    let guest = Guest {
        manifest: manifest.to_owned(),
        package: package.to_owned(),
        target,
    };
    let built = compile_guest(repository, &rustflags, &guest).await?;
    Ok(built.file)
}

/// Build one guest in its own Cargo invocation and test its bytes.
async fn compile_guest(repository: &Path, rustflags: &str, guest: &Guest) -> Result<Built> {
    let repository = repository.canonicalize()?;
    let metadata = cargo_metadata::MetadataCommand::new()
        .manifest_path(&guest.manifest)
        .other_options(vec!["--offline".into(), "--locked".into()])
        .exec()?;
    let selected = metadata
        .packages
        .iter()
        .find(|package| {
            package.name.as_str() == guest.package
                && metadata.workspace_members.contains(&package.id)
        })
        .context("guest package is absent from Cargo metadata")?;
    let workspace = crate::guest_workspace::GuestWorkspace::new(&repository, &metadata)?;
    let rustflags = format!(
        "{rustflags} --remap-path-prefix={}=/wamn",
        workspace.root().display()
    );
    let mut command = Command::new("cargo");
    command
        .current_dir(workspace.root())
        .env("RUSTFLAGS", rustflags)
        .args([
            "build",
            "--locked",
            "--offline",
            "--release",
            "--target",
            guest.target.as_str(),
            "--message-format",
            "json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(workspace.manifest());
    command.args(["-p", &guest.package]);
    command.env("CARGO_TARGET_DIR", metadata.target_directory.as_std_path());
    let output = command.output().await.context("run cargo build")?;
    let name = &guest.package;
    ensure!(
        output.status.success(),
        "cargo build of guest {name} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest = workspace.root().join(
        selected
            .manifest_path
            .as_std_path()
            .strip_prefix(&repository)?,
    );
    let mut artifact = None;
    for message in cargo_metadata::Message::parse_stream(std::io::Cursor::new(output.stdout)) {
        if let cargo_metadata::Message::CompilerArtifact(message) = message?
            && message.manifest_path.as_std_path() == manifest
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
        &repository,
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
        || std::env::var_os("HOME").map_or_default(|home| Path::new(&home).join(".cargo")),
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
            actual == format!("sha256:{expected}").parse::<wamn_digest::Digest>()?,
            "built guest {name} is {actual} but {} pins sha256:{expected}. If the change is intended, write the new digest to the pin.",
            pin.display()
        );
    }
    Ok(())
}

#[derive(Debug, Default)]
pub(crate) struct Bases {
    pub bytes: Vec<(Digest, Digest)>,
    pub pins: BTreeMap<PackageName, Digest>,
    pub manifests: Vec<PackageManifest>,
    pub wasm: Vec<(PathBuf, Digest)>,
}

/// Resolve the exact base bytes and enforce the published component pins.
pub(crate) fn base_components(
    output_root: &Path,
    source: &PreparedSource,
    mode: BuildMode,
) -> Result<Bases> {
    let mut bases = Bases::default();
    for dependency in source.bases().values() {
        let output = package_output(output_root, &dependency.package);
        let marker = fingerprint::read(&output)
            .with_context(|| format!("build base {} first", dependency.package))?;
        let (bytes, manifest_digest) =
            fingerprint::read_artifact(&output, &marker, Path::new(COMPILED_MANIFEST_NAME))?;
        let manifest = PackageManifest::from_slice(&bytes)?;
        ensure!(
            manifest.package.id == dependency.package,
            "build output names a different base package"
        );
        ensure!(
            mode == BuildMode::Development || manifest.package.version == dependency.version,
            "base {} is built at {}, not {}",
            dependency.package,
            manifest.package.version,
            dependency.version
        );
        let mut components = manifest.components.keys();
        let component = components
            .next()
            .filter(|_| components.next().is_none())
            .with_context(|| {
                format!(
                    "base {} must declare exactly one component",
                    dependency.package
                )
            })?;
        let path = component_path(component.as_str());
        let (_, digest) = fingerprint::read_artifact(&output, &marker, &path)?;
        if digest != dependency.digest && mode == BuildMode::Published {
            return Err(GenerateError::for_object(
                GenerateErrorType::InvalidComponent,
                format!(
                    "package {} pins base {} to {}, but its built component digest is {}",
                    source.package().id,
                    dependency.package,
                    dependency.digest,
                    digest
                ),
                dependency.package.as_str(),
            )
            .into());
        }
        bases.bytes.push((manifest_digest, digest));
        bases.pins.insert(dependency.package.clone(), digest);
        bases.manifests.push(manifest);
        bases.wasm.push((output.join(path), digest));
    }
    Ok(bases)
}

#[cfg(test)]
mod tests {
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

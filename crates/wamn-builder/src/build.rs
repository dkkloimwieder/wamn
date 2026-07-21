//! 5.5b — the build executor: a cargo (Rust cdylib) or jco (JS/TS ES module)
//! toolchain run that produces a wasm32-wasip2 component, then screens it
//! through the 5.5a builder lint (`wamn_host::egress_guard`). Guest artifacts
//! are ALWAYS built `--release` — the executor itself, like every other guest
//! build in the tree.
//!
//! Real user-source INGESTION (fetching + unpacking untrusted source into the
//! sandbox) is out of scope for v0: the Job template
//! (`deploy/platform/builder-job.yaml`) builds a baked-in fixture source. This
//! module is the toolchain-invocation core the ingestion path will feed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use clap::{Args, ValueEnum};
use tokio::process::Command;
use wamn_host::egress_guard::screen_builder_component;
use wamn_host::engine::build_engine;

/// Which toolchain builds the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum BuildKind {
    /// A Rust cdylib node crate on `wamn-node-sdk` + `wamn-node-guest`,
    /// componentized by `export_node!` — `cargo build --release --target
    /// wasm32-wasip2 -p <package>`.
    Cargo,
    /// A single JS/TS ES module (no `package.json` dependencies) — `jco
    /// componentize`.
    Jco,
}

#[derive(Args, Debug)]
pub struct BuildArgs {
    /// The node SOURCE directory: a cargo node crate, or a jco JS/TS module dir.
    #[arg(long)]
    pub source: PathBuf,

    /// The build toolchain: `cargo` (a Rust cdylib node crate) or `jco` (a
    /// single JS/TS ES module).
    #[arg(long, value_enum)]
    pub kind: BuildKind,

    /// cargo: the package name to build (`-p`). Default: the `--source` dir name.
    #[arg(long)]
    pub package: Option<String>,

    /// The `.wasm` output path. cargo defaults to
    /// `<source>/target/wasm32-wasip2/release/<package>.wasm`; jco REQUIRES it.
    #[arg(long)]
    pub out: Option<PathBuf>,

    /// jco: the entry ES module, relative to `--source`. Default `node.js`.
    #[arg(long, default_value = "node.js")]
    pub entry: String,

    /// jco: the WIT directory, relative to `--source`. Default `wit`.
    #[arg(long, default_value = "wit")]
    pub wit: String,

    /// jco: the world name to componentize against. Default `node-bench` (the
    /// `components/samples/node-ts` fixture world).
    #[arg(long, default_value = "node-bench")]
    pub world: String,

    /// 5.5c: the dependency allowlist policy (TOML). Default: the built-in
    /// pinned policy (`policy/default-allowlist.toml`).
    #[arg(long)]
    pub allowlist: Option<PathBuf>,

    /// 5.5e: the OCI registry `host:port` to push to. When absent the build
    /// stops after the lint (no push). Plain HTTP (the in-cluster registry).
    #[arg(long)]
    pub registry: Option<String>,

    /// 5.5e: the repository path to push under (e.g. `wamn/sample-node`).
    /// Required when `--registry` is set.
    #[arg(long)]
    pub repository: Option<String>,

    /// 5.5e: the tag to push (`--registry`). Default `dev`.
    #[arg(long, default_value = "dev")]
    pub tag: String,

    /// 5.5e: the node-type slug for the manifest. cargo defaults it from
    /// `[package.metadata.wamn-node]` / the package name; jco (no cargo
    /// metadata) REQUIRES it to push.
    #[arg(long)]
    pub node_type: Option<String>,
}

impl BuildArgs {
    /// The cargo package name to build — `--package`, else the `--source` dir
    /// name. Shared by the allowlist stage and the build stage.
    pub fn cargo_package(&self) -> anyhow::Result<String> {
        self.package
            .clone()
            .or_else(|| source_dir_name(&self.source))
            .context("cargo build: pass --package or a named --source directory")
    }
}

/// A built node artifact: the wasm bytes and where they landed.
pub struct BuiltArtifact {
    /// The path the toolchain wrote the component to.
    pub wasm_path: PathBuf,
    /// The component bytes.
    pub wasm: Vec<u8>,
}

/// The cargo argv that builds a wasm32-wasip2 node crate. Always `--release`
/// (guest artifacts are release-only, the tree-wide rule).
pub fn cargo_build_argv(package: &str) -> Vec<String> {
    [
        "build",
        "--release",
        "--target",
        "wasm32-wasip2",
        "-p",
        package,
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// The jco argv that componentizes a single ES module — the exact invocation
/// docs/build-and-test.md uses for the node-ts fixture (`--disable http` /
/// `--disable fetch-event`, so a node exports only `wamn:node/handler`).
pub fn jco_componentize_argv(entry: &str, wit: &str, world: &str, out: &str) -> Vec<String> {
    [
        "componentize",
        entry,
        "--wit",
        wit,
        "--world-name",
        world,
        "--disable",
        "http",
        "--disable",
        "fetch-event",
        "-o",
        out,
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// The cdylib artifact filename cargo writes for `package`: the crate name with
/// `-` folded to `_` (sample-node → sample_node.wasm).
pub fn cargo_artifact_name(package: &str) -> String {
    format!("{}.wasm", package.replace('-', "_"))
}

/// Derive the cargo package name from the `--source` dir name when `--package`
/// is absent.
fn source_dir_name(source: &Path) -> Option<String> {
    source
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_string)
}

/// Run `program argv` in `cwd`, streaming its output; a non-zero exit is a hard
/// build failure.
async fn run_tool(program: &str, argv: &[String], cwd: &Path) -> anyhow::Result<()> {
    tracing::info!(program, ?argv, cwd = %cwd.display(), "builder: running toolchain");
    let status = Command::new(program)
        .args(argv)
        .current_dir(cwd)
        .status()
        .await
        .with_context(|| format!("spawn {program}"))?;
    if !status.success() {
        bail!("{program} {argv:?} failed with {status}");
    }
    Ok(())
}

/// Build the node artifact via the selected toolchain and read back its bytes.
/// Does NOT lint — [`run`] does that so the lint refusal is one place.
pub async fn build_artifact(args: &BuildArgs) -> anyhow::Result<BuiltArtifact> {
    let wasm_path = match args.kind {
        BuildKind::Cargo => {
            let package = args.cargo_package()?;
            run_tool("cargo", &cargo_build_argv(&package), &args.source).await?;
            args.out.clone().unwrap_or_else(|| {
                args.source
                    .join("target/wasm32-wasip2/release")
                    .join(cargo_artifact_name(&package))
            })
        }
        BuildKind::Jco => {
            let out = args
                .out
                .clone()
                .context("jco build requires --out (the componentized .wasm path)")?;
            let out_str = out
                .to_str()
                .context("jco --out path is not valid UTF-8")?
                .to_string();
            run_tool(
                "jco",
                &jco_componentize_argv(&args.entry, &args.wit, &args.world, &out_str),
                &args.source,
            )
            .await?;
            out
        }
    };
    let wasm = tokio::fs::read(&wasm_path)
        .await
        .with_context(|| format!("read built artifact {}", wasm_path.display()))?;
    Ok(BuiltArtifact { wasm_path, wasm })
}

/// Screen built bytes through the 5.5a builder lint (the package allowlist + the
/// interface tightening). A shared entry point so the E2E and the tests refuse
/// through the identical path.
pub fn lint_artifact(wasm: &[u8], label: &str) -> anyhow::Result<()> {
    let engine = build_engine(&[])?;
    screen_builder_component(engine.inner(), wasm, label)
        .context("built artifact failed the 5.5 builder import lint")
}

/// 5.5c — the dependency allowlist over a pre-fetched `cargo metadata` document
/// (cargo path). Refuses if the resolved package set carries an off-policy crate.
async fn enforce_cargo_allowlist(
    args: &BuildArgs,
    metadata_json: &str,
    package: &str,
) -> anyhow::Result<()> {
    let policy = match &args.allowlist {
        Some(path) => crate::allowlist::Policy::load(path).await?,
        None => crate::allowlist::Policy::default_policy(),
    };
    let resolved = crate::allowlist::closure_names(metadata_json, package)?;
    crate::allowlist::check_allowlist(&resolved, &policy, package)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    println!(
        "dependency allowlist: {} transitive package(s), all allowed",
        resolved.len()
    );
    Ok(())
}

/// Assemble the push target from the args (`None` when `--registry` is absent).
fn push_target(args: &BuildArgs) -> anyhow::Result<Option<crate::registry::RegistryRef>> {
    let Some(registry) = &args.registry else {
        return Ok(None);
    };
    let repository = args
        .repository
        .clone()
        .context("--registry requires --repository (e.g. wamn/sample-node)")?;
    Ok(Some(crate::registry::RegistryRef {
        registry: registry.clone(),
        repository,
        reference: args.tag.clone(),
        insecure: true,
    }))
}

/// The OCI annotations attached at push. 5.5e writes the `wamn.node.manifest`
/// annotation; 5.5d (signing / SBOM) adds its annotations to this map.
fn base_annotations(node_manifest: &wamn_node_manifest::NodeManifest) -> BTreeMap<String, String> {
    let mut annotations = BTreeMap::new();
    annotations.insert(
        wamn_node_manifest::ANNOTATION_KEY.to_string(),
        node_manifest.to_json(),
    );
    annotations
}

/// Push the built artifact with the `wamn.node.manifest` annotation (5.5e). The
/// manifest is built + validated (`is_valid`) BEFORE push.
async fn push_artifact(
    target: &crate::registry::RegistryRef,
    node_manifest: &wamn_node_manifest::NodeManifest,
    wasm: &[u8],
) -> anyhow::Result<()> {
    let annotations = base_annotations(node_manifest);
    let pushed = crate::registry::push(target, wasm, annotations).await?;
    println!(
        "pushed node artifact {} (manifest {} / layer {})",
        pushed.image, pushed.manifest_digest, pushed.layer_digest
    );
    Ok(())
}

/// The `build` verb: dependency allowlist (5.5c) → build (5.5b) → import lint
/// (5.5a) → optional OCI push with the `wamn.node.manifest` annotation (5.5e).
/// Signing / SBOM (5.5d) extend the push annotations.
pub async fn run(args: BuildArgs) -> anyhow::Result<()> {
    let target = push_target(&args)?;

    match args.kind {
        BuildKind::Cargo => {
            let package = args.cargo_package()?;
            let manifest_path = args.source.join("Cargo.toml");
            // One `cargo metadata` run, reused by the allowlist + the manifest.
            let metadata_json = crate::allowlist::cargo_metadata_json(&manifest_path).await?;
            enforce_cargo_allowlist(&args, &metadata_json, &package).await?;

            let artifact = build_artifact(&args).await?;
            lint_artifact(&artifact.wasm, &artifact.wasm_path.display().to_string())?;
            println!(
                "built + linted node artifact: {} ({} bytes)",
                artifact.wasm_path.display(),
                artifact.wasm.len()
            );

            if let Some(target) = target {
                let mut node_manifest =
                    crate::manifest_build::manifest_from_metadata(&metadata_json, &package)?;
                if let Some(node_type) = &args.node_type {
                    node_manifest.node_type = node_type.clone();
                }
                push_artifact(&target, &node_manifest, &artifact.wasm).await?;
            }
        }
        BuildKind::Jco => {
            crate::allowlist::assert_jco_single_module(&args.source).await?;
            let artifact = build_artifact(&args).await?;
            lint_artifact(&artifact.wasm, &artifact.wasm_path.display().to_string())?;
            println!(
                "built + linted node artifact: {} ({} bytes)",
                artifact.wasm_path.display(),
                artifact.wasm.len()
            );

            if let Some(target) = target {
                let node_type = args
                    .node_type
                    .clone()
                    .context("jco push requires --node-type (no cargo metadata to derive it)")?;
                let node_manifest = crate::manifest_build::minimal_manifest(
                    &node_type,
                    &node_type,
                    "0.1.0",
                    crate::manifest_build::DEFAULT_CONTRACT,
                )?;
                push_artifact(&target, &node_manifest, &artifact.wasm).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_build_argv_is_release_wasip2() {
        assert_eq!(
            cargo_build_argv("sample-node"),
            vec![
                "build".to_string(),
                "--release".to_string(),
                "--target".to_string(),
                "wasm32-wasip2".to_string(),
                "-p".to_string(),
                "sample-node".to_string(),
            ]
        );
    }

    #[test]
    fn jco_argv_matches_the_node_ts_invocation() {
        // The exact shape docs/build-and-test.md uses for the node-ts fixture.
        assert_eq!(
            jco_componentize_argv("node.js", "wit", "node-bench", "out/node-ts.wasm"),
            vec![
                "componentize",
                "node.js",
                "--wit",
                "wit",
                "--world-name",
                "node-bench",
                "--disable",
                "http",
                "--disable",
                "fetch-event",
                "-o",
                "out/node-ts.wasm",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn cdylib_artifact_name_folds_hyphens() {
        assert_eq!(cargo_artifact_name("sample-node"), "sample_node.wasm");
        assert_eq!(cargo_artifact_name("api_gateway"), "api_gateway.wasm");
    }

    #[test]
    fn build_kind_parses_lowercase() {
        use clap::ValueEnum as _;
        assert_eq!(
            BuildKind::from_str("cargo", true).unwrap(),
            BuildKind::Cargo
        );
        assert_eq!(BuildKind::from_str("jco", true).unwrap(), BuildKind::Jco);
        assert!(BuildKind::from_str("npm", true).is_err());
    }
}

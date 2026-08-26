//! Recorded wash-runtime feature and generated-workload inventory.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use url::Url;

const INVENTORY: &str = include_str!("../runtime-inventory.json");
const ALLOWED_WASH_RUNTIME_FEATURES: [&str; 5] = [
    "oci",
    "wasi-config",
    "wasi-otel",
    "washlet",
    "wasm_component_model_implements",
];
const CFG_TEST_MODULE: &str = "#[cfg(test)]\nmod tests {";
/// The one production store the execution host creates, and the file that holds
/// it. `18ba72b6` deleted the host plan-supply path this used to name, leaving
/// `crates/execution/host/src/lib.rs` a module-declaration file; the surviving
/// store is the router driver's, created per invocation.
const EXECUTION_HOST_STORE_CONSTRUCTOR: &str =
    "let mut store = Store::new(engine.inner(), SharedCtx::new(ctx));";
const EXECUTION_HOST_STORE_FILE: &str = "crates/execution/host/src/router_driver.rs";

/// The release-manifest weld construction call, deliberately truncated before the
/// `(` so it matches `load` and `load_from` alike — the guard counts
/// *construction*, not one spelling of it.
///
/// Counted as raw text, like every other marker here, so prose in a host file that
/// wrote this marker out in full would read as a second construction site. Host
/// doc comments name the type and the method separately for that reason.
const RELEASE_WELD_CONSTRUCTION: &str = "ReleaseManifestWeld::load";

/// The two host processes, and per process the two positions that must hold:
/// `(file, the text that reaches the weld, the first bind-capable text it must
/// precede)`.
///
/// wamn-0h0g.15.101 rules one weld instance PER PROCESS: the wash host serves
/// flow-http routing and jetstream delivery, the executor serves the durable
/// queue. Separate processes cannot share one object, so each constructs exactly
/// once — and must do so before anything binds a component, because under ruling
/// wamn-0h0g.15.102 the verified manifest is the sole carrier of the
/// `(release version, manifest digest)` pair a claim records. A component that
/// bound first would have no pair.
///
/// The second process used to be the in-process run host, reached through
/// `load_plan_release`; `18ba72b6` deleted host plan supply and that symbol with
/// it, so this entry named a function that existed nowhere and the guard proved
/// nothing (wamn-nguw). Both surviving processes call the weld directly.
const HOST_WELD_SITES: [(&str, &str, &str); 2] = [
    (
        "services/host/src/host.rs",
        "let release = load_release(",
        "ClusterHostBuilder::default()",
    ),
    (
        "services/executor/src/lib.rs",
        "let release = load_release(",
        "RouterDriver::new(",
    ),
];

/// The production construction of a claim's release pair.
///
/// wamn-0h0g.15.103 struck the per-workload config keys that used to assert this
/// pair at bind time, leaving the verified manifest as its sole carrier. The old
/// guard pinned ONE `plugin.set_release_identity(` call in one file; production
/// no longer has one such site, and `WamnPostgres::set_release_identity` is a
/// pass-through that builds the struct from its own parameters rather than a
/// source of the pair.
///
/// So the invariant that survives is not the COUNT but the SOURCE: wherever
/// production builds a `ReleaseIdentity`, both halves are read off the weld. A
/// site that invented either half from anywhere else would restore the
/// dual-representation bug the ruling closed — two carriers with nothing
/// reconciling them, so a pod could stamp one release onto a run while resolving
/// plans against another.
const RELEASE_IDENTITY_CONSTRUCTION: &str = "ReleaseIdentity {";

/// The two production sites that build the pair, and the weld expression each
/// one must read both halves off.
///
/// The per-run site is shared by both host processes through `RouterDriver`; the
/// executor's is its queue-claim session scope. Two sites, one source.
const RELEASE_IDENTITY_SOURCE_SITES: [(&str, &str); 2] = [
    (
        "crates/execution/host/src/router_driver.rs",
        "self.release.release()",
    ),
    ("services/executor/src/lib.rs", "release.release()"),
];

/// The host's one `RouterDeliveryBridge` opts into its meter.
///
/// wamn-0h0g.24.4 shipped `wamn.router.delivery.attempts` and
/// `wamn.router.delivery.errors`, but the bridge's `new` defaults its meter to
/// `None` so a test can own its own provider — which means the series exist and
/// stay permanently silent until the construction site opts in. `host::run`
/// needs NATS and a release weld, so there is no runtime proof to take; this
/// pins the wiring as raw text, the way the weld sites above are pinned.
///
/// `with_metrics` exists on no other type in this file's reach, so counting the
/// call alone is enough — the builder is `#[must_use]` and consumed straight
/// into the plugin, so it cannot be called on anything else or dropped.
const METERED_DELIVERY_BRIDGE: (&str, &str) = ("services/host/src/host.rs", ".with_metrics(");

/// The two struck config keys, and every file that could plausibly re-read them.
///
/// Spelled here rather than imported so the guard fails if the constants are
/// reintroduced under any name at all.
const STRUCK_RELEASE_IDENTITY_KEYS: [&str; 2] = ["wamn.release-version", "wamn.manifest-digest"];

/// Files whose text must not carry a struck key: both host construction sites and
/// the plugin whose bind path used to read them.
const STRUCK_KEY_SITES: [&str; 3] = [
    "crates/platform/runtime/src/plugins/wamn_postgres/mod.rs",
    "services/executor/src/lib.rs",
    "services/host/src/host.rs",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum WorkloadAbi {
    P2Components,
    P2CliService,
    P3Service,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimePin {
    cargo_tree_root: String,
    default_features: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Consumer {
    package: String,
    manifest: String,
    features: BTreeSet<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AbiEvidence {
    path: String,
    required_marker: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkloadManifest {
    path: String,
    deployment_state: String,
    abi: WorkloadAbi,
    abi_evidence: Option<AbiEvidence>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    schema_version: String,
    wash_runtime: RuntimePin,
    live_store_paths: BTreeSet<String>,
    consumers: Vec<Consumer>,
    workload_manifests: Vec<WorkloadManifest>,
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    name: String,
    manifest_path: String,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn inventory() -> Inventory {
    serde_json::from_str(INVENTORY).expect("runtime-inventory.json must be valid")
}

fn cargo_tree(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["tree", "--locked", "--offline"])
        .args(arguments)
        .output()
        .expect("run cargo tree for runtime inventory");
    assert!(
        output.status.success(),
        "cargo tree {} failed:\n{}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("cargo tree output must be UTF-8")
}

fn wash_runtime_source(root: &Path) -> PathBuf {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .args(["metadata", "--locked", "--offline", "--format-version", "1"])
        .output()
        .expect("run cargo metadata for wash-runtime source");
    assert!(
        output.status.success(),
        "cargo metadata failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: CargoMetadata =
        serde_json::from_slice(&output.stdout).expect("cargo metadata must be valid JSON");
    let manifest = metadata
        .packages
        .iter()
        .find(|package| package.name == "wash-runtime")
        .expect("resolved graph must contain wash-runtime");
    Path::new(&manifest.manifest_path)
        .parent()
        .expect("wash-runtime manifest must have a parent")
        .to_path_buf()
}

fn workspace_wash_runtime_declaration(root: &Path) -> String {
    let manifest = fs::read_to_string(root.join("Cargo.toml")).expect("read root Cargo.toml");
    let mut workspace_dependencies = false;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            workspace_dependencies = trimmed == "[workspace.dependencies]";
            continue;
        }
        if workspace_dependencies && let Some(declaration) = trimmed.strip_prefix("wash-runtime =")
        {
            return declaration.split_whitespace().collect();
        }
    }
    panic!("root [workspace.dependencies] must declare wash-runtime");
}

fn validate_one(source: &str, marker: &str, seam: &str) -> Result<(), String> {
    let observed = source.matches(marker).count();
    if observed == 1 {
        Ok(())
    } else {
        Err(format!(
            "{seam} must retain exactly one `{marker}` marker; found {observed}"
        ))
    }
}

fn assert_one(source: &str, marker: &str, seam: &str) {
    validate_one(source, marker, seam).unwrap_or_else(|error| panic!("{error}"));
}

fn production_execution_host_source(source: &str) -> Result<&str, String> {
    let test_modules = source.matches(CFG_TEST_MODULE).count();
    if test_modules != 1 {
        return Err(format!(
            "ExecutionHost source must retain exactly one terminal `{CFG_TEST_MODULE}` module; \
             found {test_modules}"
        ));
    }
    let (production, _) = source
        .split_once(CFG_TEST_MODULE)
        .expect("the counted cfg(test) module must split");
    Ok(production)
}

fn validate_execution_host_store_constructor(source: &str) -> Result<(), String> {
    let production = production_execution_host_source(source)?;
    validate_one(
        production,
        EXECUTION_HOST_STORE_CONSTRUCTOR,
        "production ExecutionHost store constructor",
    )
}

/// Everything before a file's terminal `#[cfg(test)] mod tests {`, or the whole
/// file when it has none.
///
/// Unlike [`production_execution_host_source`], which pins a seam to a file that
/// must always carry a test module, both shapes are legitimate for the host weld
/// sites: the wash host carries no test module and the execution host carries one.
fn production_half<'a>(source: &'a str, seam: &str) -> Result<&'a str, String> {
    match source.matches(CFG_TEST_MODULE).count() {
        0 => Ok(source),
        1 => Ok(source
            .split_once(CFG_TEST_MODULE)
            .expect("the counted cfg(test) module must split")
            .0),
        found => Err(format!(
            "{seam} must carry at most one terminal `{CFG_TEST_MODULE}` module; found {found}"
        )),
    }
}

fn validate_one_weld_site(source: &str, seam: &str) -> Result<(), String> {
    let production = production_half(source, seam)?;
    validate_one(production, RELEASE_WELD_CONSTRUCTION, seam)
}

fn validate_weld_precedes_bind(
    source: &str,
    entry: &str,
    bind: &str,
    seam: &str,
) -> Result<(), String> {
    let production = production_half(source, seam)?;
    let Some(entry_at) = production.find(entry) else {
        return Err(format!("{seam} must reach its weld through `{entry}`"));
    };
    let Some(bind_at) = production.find(bind) else {
        return Err(format!(
            "{seam} must still bind components through `{bind}`"
        ));
    };
    if entry_at < bind_at {
        Ok(())
    } else {
        Err(format!(
            "{seam} must reach `{entry}` before `{bind}`; a component that binds first \
             carries no release identity for its claim to record"
        ))
    }
}

/// Every half of a production `ReleaseIdentity` is read off the weld.
///
/// The literal's body is taken as the text between `ReleaseIdentity {` and the
/// next `}` — every production construction is a flat struct literal of two
/// scalar fields, so no nesting can hide inside it — and both field
/// initializers must name `weld`, the expression that reaches this file's weld.
fn validate_release_identity_from_weld(source: &str, weld: &str, seam: &str) -> Result<(), String> {
    let production = production_half(source, seam)?;
    validate_one(production, RELEASE_IDENTITY_CONSTRUCTION, seam)?;
    let opened = production
        .find(RELEASE_IDENTITY_CONSTRUCTION)
        .expect("the counted construction must locate")
        + RELEASE_IDENTITY_CONSTRUCTION.len();
    let Some(closed) = production[opened..].find('}').map(|end| opened + end) else {
        return Err(format!(
            "{seam} must close its `{RELEASE_IDENTITY_CONSTRUCTION}` literal"
        ));
    };
    let body = &production[opened..closed];
    for field in ["release_version", "manifest_digest"] {
        let initializer = format!("{field}: {weld}.{field}");
        if !body.contains(&initializer) {
            return Err(format!(
                "{seam} must initialize `{field}` as `{initializer}`; a pair read from \
                 anywhere but the weld is a second carrier of the release identity the \
                 verified manifest was made sole owner of (wamn-0h0g.15.102)"
            ));
        }
    }
    Ok(())
}

fn validate_no_struck_key(source: &str, seam: &str) -> Result<(), String> {
    for key in STRUCK_RELEASE_IDENTITY_KEYS {
        let observed = source.matches(key).count();
        if observed != 0 {
            return Err(format!(
                "{seam} must carry no `{key}`; found {observed}. Release identity has one \
                 carrier, the mounted manifest (wamn-0h0g.15.102)"
            ));
        }
    }
    Ok(())
}

fn host_source(root: &Path, path: &str) -> String {
    let full = root.join(path);
    fs::read_to_string(&full).unwrap_or_else(|error| panic!("read {}: {error}", full.display()))
}

fn observed_store_paths(root: &Path, wash_runtime: &Path) -> BTreeSet<String> {
    let wash_manifest_path = wash_runtime.join("Cargo.toml");
    let wash_manifest = fs::read_to_string(&wash_manifest_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", wash_manifest_path.display()));
    assert_one(
        &wash_manifest,
        "host-component-plugins = []",
        "host-component plugin feature",
    );
    let plugin_module_path = wash_runtime.join("src/plugin/mod.rs");
    let plugin_module = fs::read_to_string(&plugin_module_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", plugin_module_path.display()));
    assert_one(
        &plugin_module,
        "#[cfg(all(feature = \"host-component-plugins\", feature = \"oci\"))]",
        "host-component plugin module gate",
    );

    let linked_call_path = wash_runtime.join("src/engine/linked_call.rs");
    let linked_call = fs::read_to_string(&linked_call_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", linked_call_path.display()));
    assert_one(
        &linked_call,
        "pub(crate) async fn new_store_from_templates(",
        "runtime store constructor",
    );
    assert_one(
        &linked_call,
        "let mut store = wasmtime::Store::new(engine, shared_ctx);",
        "runtime store constructor",
    );

    let component_plugin_path = wash_runtime.join("src/plugin/component_host/mod.rs");
    let component_plugin = fs::read_to_string(&component_plugin_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", component_plugin_path.display()));
    assert_one(
        &component_plugin,
        "Store::new(engine.inner(), SharedCtx::new(ctx).with_resource_registry())",
        "feature-gated host-component plugin store",
    );

    let pool_path = wash_runtime.join("src/engine/instance_pool.rs");
    let pool = fs::read_to_string(&pool_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", pool_path.display()));
    assert!(
        pool.contains("Some(pool_size) => Self::Warm"),
        "nonzero pool_size must remain the warm-store activation seam"
    );

    let execution_path = root.join(EXECUTION_HOST_STORE_FILE);
    let execution = fs::read_to_string(&execution_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", execution_path.display()));
    validate_execution_host_store_constructor(&execution).unwrap_or_else(|error| panic!("{error}"));

    BTreeSet::from([
        "runtime: new_store_from_templates (single production site)".to_string(),
        "wamn: ExecutionHost store (crates/execution/host)".to_string(),
    ])
}

fn resolved_features(tree: &str) -> BTreeSet<String> {
    tree.lines()
        .filter_map(|line| {
            line.split_once("wash-runtime feature \"")
                .and_then(|(_, suffix)| suffix.split_once('"'))
                .map(|(feature, _)| feature.to_string())
        })
        .collect()
}

fn service_consumers(root: &Path) -> BTreeSet<String> {
    let services = root.join("services");
    fs::read_dir(&services)
        .unwrap_or_else(|error| panic!("read {}: {error}", services.display()))
        .filter_map(|entry| {
            let directory = entry.expect("read services entry").path();
            let manifest = directory.join("Cargo.toml");
            if !manifest.is_file() {
                return None;
            }
            let source = fs::read_to_string(&manifest)
                .unwrap_or_else(|error| panic!("read {}: {error}", manifest.display()));
            let directly_declares_runtime = source.lines().any(|line| {
                let line = line.trim();
                !line.starts_with('#') && line.starts_with("wash-runtime =")
            });
            directly_declares_runtime.then(|| {
                manifest
                    .strip_prefix(root)
                    .expect("service manifest must be repository-relative")
                    .to_string_lossy()
                    .to_string()
            })
        })
        .collect()
}

fn workload_manifests(root: &Path) -> BTreeSet<String> {
    let platform = root.join("deploy/platform");
    fs::read_dir(&platform)
        .unwrap_or_else(|error| panic!("read {}: {error}", platform.display()))
        .filter_map(|entry| {
            let path = entry.expect("read deploy/platform entry").path();
            let extension = path.extension()?.to_str()?;
            if !matches!(extension, "yaml" | "yml") {
                return None;
            }
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            if !source
                .lines()
                .any(|line| line.trim() == "kind: WorkloadDeployment")
            {
                return None;
            }
            Some(
                path.strip_prefix(root)
                    .expect("platform manifest must be repository-relative")
                    .to_string_lossy()
                    .to_string(),
            )
        })
        .collect()
}

fn yaml_i32(source: &str, key: &str) -> Vec<i32> {
    source
        .lines()
        .filter_map(|line| {
            let value = line.trim().strip_prefix(key)?.trim();
            Some(
                value
                    .parse()
                    .unwrap_or_else(|_| panic!("{key} must be an integer, got `{value}`")),
            )
        })
        .collect()
}

fn yaml_blocks<'a>(lines: &'a [&'a str], marker: &str) -> Vec<&'a [&'a str]> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim() == marker)
        .map(|(index, line)| {
            let block_indent = line.len() - line.trim_start().len();
            let block_end = lines[index + 1..]
                .iter()
                .position(|line| {
                    let trimmed = line.trim();
                    !trimmed.is_empty()
                        && !trimmed.starts_with('#')
                        && line.len() - line.trim_start().len() <= block_indent
                })
                .map_or(lines.len(), |offset| index + 1 + offset);
            &lines[index + 1..block_end]
        })
        .collect()
}

fn yaml_scalar(raw: &str) -> &str {
    let value = raw.split_once(" #").map_or(raw, |(value, _)| value).trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn validate_no_component_database_urls(path: &str, source: &str) -> Result<(), String> {
    let lines: Vec<_> = source.lines().collect();
    for local_resources in yaml_blocks(&lines, "localResources:") {
        for environment in yaml_blocks(local_resources, "environment:") {
            for line in environment {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
                    continue;
                };
                let key = yaml_scalar(raw_key);
                let value = yaml_scalar(raw_value);
                if key == "DATABASE_URL" || key.ends_with("_PG_URL") {
                    return Err(format!(
                        "{path}: component localResources.environment key `{key}` may not carry a database URL"
                    ));
                }
                if value.is_empty() {
                    continue;
                }
                if let Ok(url) = Url::parse(value)
                    && matches!(url.scheme(), "postgres" | "postgresql")
                {
                    return Err(format!(
                        "{path}: component localResources.environment key `{key}` may not carry a {} URL",
                        url.scheme()
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_feature_policy(features: &BTreeSet<String>) -> Result<(), String> {
    if features.contains("host-component-plugins") {
        return Err(
            "host-component-plugins enables plugin-owned stores beyond the three recorded paths"
                .to_string(),
        );
    }

    let allowed = ALLOWED_WASH_RUNTIME_FEATURES
        .into_iter()
        .map(str::to_string)
        .collect();
    let unknown: Vec<_> = features.difference(&allowed).cloned().collect();
    if !unknown.is_empty() {
        return Err(format!(
            "unreviewed wash-runtime features may widen live store paths: {}",
            unknown.join(", ")
        ));
    }
    Ok(())
}

fn validate_ip_name_lookup_defaults(path: &str, source: &str) -> Result<(), String> {
    let lines: Vec<_> = source.lines().collect();
    let local_resources = yaml_blocks(&lines, "localResources:");

    for block in &local_resources {
        let child_indent = block
            .iter()
            .filter(|line| {
                let trimmed = line.trim();
                !trimmed.is_empty() && !trimmed.starts_with('#')
            })
            .map(|line| line.len() - line.trim_start().len())
            .min();
        let values: Vec<_> = block
            .iter()
            .filter(|line| {
                child_indent.is_some_and(|indent| line.len() - line.trim_start().len() == indent)
            })
            .filter_map(|line| {
                line.trim()
                    .strip_prefix("allowedIpNameLookups:")
                    .map(str::trim)
            })
            .collect();

        if values.len() != 1 {
            return Err(format!(
                "{path}: each localResources block must contain exactly one allowedIpNameLookups field; found {}",
                values.len()
            ));
        }
        if values[0] != "[]" {
            return Err(format!(
                "{path}: allowedIpNameLookups must default to [], got `{}`",
                values[0]
            ));
        }
    }

    if local_resources.is_empty() {
        return Err(format!(
            "{path}: workload must expose localResources.allowedIpNameLookups with default []"
        ));
    }
    Ok(())
}

fn validate_workload_policy(path: &str, source: &str, abi: &WorkloadAbi) -> Result<(), String> {
    let pool_sizes = yaml_i32(source, "poolSize:");
    if let Some(pool_size) = pool_sizes.into_iter().find(|pool_size| *pool_size != 0) {
        return Err(format!(
            "{path}: poolSize {pool_size} enables reusable component stores"
        ));
    }

    validate_ip_name_lookup_defaults(path, source)?;
    validate_no_component_database_urls(path, source)?;

    let has_components = source.lines().any(|line| line == "      components:");
    // maxInvocations has no effect when poolSize is absent or zero.
    let _max_invocations = yaml_i32(source, "maxInvocations:");

    let has_workload_service = source.lines().any(|line| line == "      service:");
    match abi {
        WorkloadAbi::P2Components if has_workload_service || !has_components => {
            return Err(format!(
                "{path}: recorded as P2 components but its components/service shape disagrees"
            ));
        }
        WorkloadAbi::P2CliService if !has_workload_service => {
            return Err(format!(
                "{path}: recorded as a P2 CLI service but has no workload service"
            ));
        }
        WorkloadAbi::P3Service => {
            return Err(format!(
                "{path}: P3 service workload deployment is excluded"
            ));
        }
        WorkloadAbi::P2Components | WorkloadAbi::P2CliService => {}
    }
    Ok(())
}

#[test]
fn resolved_feature_and_deployed_workload_inventory_is_current() {
    let root = repository_root();
    let inventory = inventory();
    assert_eq!(inventory.schema_version, "0.1");
    let declaration = workspace_wash_runtime_declaration(&root);
    assert!(
        declaration.contains("default-features=false"),
        "root wash-runtime dependency must explicitly set default-features = false"
    );
    assert!(
        !inventory.wash_runtime.default_features,
        "recorded wash-runtime default-feature policy drifted"
    );
    assert_eq!(
        inventory.live_store_paths,
        observed_store_paths(&root, &wash_runtime_source(&root)),
        "the inventory must retain both live store paths"
    );
    assert_eq!(
        inventory.consumers.len(),
        3,
        "the inventory must retain all three production consumers"
    );

    let recorded_manifests = inventory
        .consumers
        .iter()
        .map(|consumer| consumer.manifest.clone())
        .collect();
    assert_eq!(
        service_consumers(&root),
        recorded_manifests,
        "production services consuming wash-runtime drifted"
    );

    let mut all_features = BTreeSet::new();
    for consumer in &inventory.consumers {
        let tree = cargo_tree(
            &root,
            &[
                "-p",
                &consumer.package,
                "-e",
                "features",
                "-i",
                "wash-runtime",
            ],
        );
        assert_eq!(
            tree.lines().next(),
            Some(inventory.wash_runtime.cargo_tree_root.as_str()),
            "{} resolved a different wash-runtime identity",
            consumer.package
        );
        let actual = resolved_features(&tree);
        assert_eq!(
            actual, consumer.features,
            "{} wash-runtime feature inventory drifted",
            consumer.package
        );
        all_features.extend(actual);
    }
    validate_feature_policy(&all_features).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        all_features.contains("wasm_component_model_implements"),
        "the shipped host and executor must enable named component-model imports"
    );

    let recorded_workloads = inventory
        .workload_manifests
        .iter()
        .map(|workload| workload.path.clone())
        .collect();
    assert_eq!(
        workload_manifests(&root),
        recorded_workloads,
        "generated WorkloadDeployment manifest inventory drifted"
    );
    assert_eq!(
        inventory.workload_manifests.len(),
        2,
        "the inventory must retain both generated workload manifests"
    );
    for workload in &inventory.workload_manifests {
        let expected_state = if workload.path.contains(".example.") {
            "template"
        } else {
            "active"
        };
        assert_eq!(
            workload.deployment_state, expected_state,
            "{} deployment-state classification drifted",
            workload.path
        );
        let path = root.join(&workload.path);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        validate_workload_policy(&workload.path, &source, &workload.abi)
            .unwrap_or_else(|error| panic!("{error}"));
        match (&workload.abi, &workload.abi_evidence) {
            (WorkloadAbi::P2CliService, Some(evidence)) => {
                let evidence_path = root.join(&evidence.path);
                let evidence_source = fs::read_to_string(&evidence_path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", evidence_path.display()));
                assert!(
                    evidence_source.contains(&evidence.required_marker),
                    "{} no longer proves the P2 CLI service classification for {}",
                    evidence.path,
                    workload.path
                );
                let component = evidence_path
                    .parent()
                    .expect("component Cargo.toml must have a parent");
                assert!(
                    component.join("src/main.rs").is_file(),
                    "{} must remain a command component",
                    evidence.path
                );
                let world_path = component.join("wit/world.wit");
                let world = fs::read_to_string(&world_path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", world_path.display()));
                assert!(
                    !world.contains("wasi:http/handler") && !world.contains("wasmcloud:messaging"),
                    "{} adopted a P3 service export",
                    world_path.display()
                );
            }
            (WorkloadAbi::P2Components, None) => {}
            _ => panic!(
                "{} ABI classification has missing or unexpected evidence",
                workload.path
            ),
        }
    }
}

#[test]
fn host_component_plugins_mutation_is_rejected() {
    let mut features = BTreeSet::from(["oci".to_string(), "washlet".to_string()]);
    features.insert("host-component-plugins".to_string());
    let error = validate_feature_policy(&features).expect_err("mutation must fail closed");
    assert!(error.contains("plugin-owned stores beyond the three recorded paths"));
}

#[test]
fn nonzero_pool_size_mutation_is_rejected() {
    let mutant = "components:\n  - name: mutant\n    poolSize: 1\n    maxInvocations: 10\n";
    let error =
        validate_workload_policy("pool-size-mutant.yaml", mutant, &WorkloadAbi::P2Components)
            .expect_err("mutation must fail closed");
    assert!(error.contains("poolSize 1 enables reusable component stores"));
}

#[test]
fn nonempty_ip_name_lookup_default_mutation_is_rejected() {
    let mutant = "      components:\n        - name: mutant\n          localResources:\n            allowedIpNameLookups: [\"example.com\"]\n";
    let error = validate_workload_policy("lookup-mutant.yaml", mutant, &WorkloadAbi::P2Components)
        .expect_err("nonempty allowedIpNameLookups default must fail closed");
    assert!(error.contains("allowedIpNameLookups must default to []"));
}

#[test]
fn missing_misspelled_or_duplicate_ip_name_lookup_defaults_are_rejected() {
    let mutants = [
        (
            "missing",
            "      components:\n        - name: mutant\n          localResources:\n            config: {}\n",
        ),
        (
            "wrong-allow-prefix",
            "      components:\n        - name: mutant\n          localResources:\n            allowIpNameLookups: []\n",
        ),
        (
            "legacy-singular",
            "      components:\n        - name: mutant\n          localResources:\n            allowIpNameLookup: []\n",
        ),
        (
            "duplicate",
            "      components:\n        - name: mutant\n          localResources:\n            allowedIpNameLookups: []\n            allowedIpNameLookups: []\n",
        ),
    ];

    for (name, mutant) in mutants {
        let error =
            validate_workload_policy("lookup-mutant.yaml", mutant, &WorkloadAbi::P2Components)
                .expect_err("invalid allowedIpNameLookups structure must fail closed");
        assert!(
            error.contains("must contain exactly one allowedIpNameLookups field"),
            "{name} mutation failed for an unexpected reason: {error}"
        );
    }
}

fn component_environment_fixture(entry: &str) -> String {
    format!(
        "      components:\n\
         \x20       - name: mutant\n\
         \x20         localResources:\n\
         \x20           allowedIpNameLookups: []\n\
         \x20           environment:\n\
         \x20             config:\n\
         \x20               {entry}\n"
    )
}

#[test]
fn component_environment_pg_url_suffix_mutation_is_rejected() {
    let mutant = component_environment_fixture("WAMN_READER_PG_URL: not-a-url");
    let error = validate_workload_policy(
        "component-pg-url-key-mutant.yaml",
        &mutant,
        &WorkloadAbi::P2Components,
    )
    .expect_err("a component environment *_PG_URL key must fail closed");
    assert!(
        error.contains("key `WAMN_READER_PG_URL`"),
        "the refusal must name the forbidden key: {error}"
    );
}

#[test]
fn component_environment_database_url_mutation_is_rejected() {
    let mutant = component_environment_fixture("DATABASE_URL:");
    let error = validate_workload_policy(
        "component-database-url-key-mutant.yaml",
        &mutant,
        &WorkloadAbi::P2Components,
    )
    .expect_err("an empty component environment DATABASE_URL placeholder must fail closed");
    assert!(
        error.contains("key `DATABASE_URL`"),
        "the refusal must name the forbidden key: {error}"
    );
}

#[test]
fn component_environment_postgres_url_value_mutation_is_rejected() {
    for (name, url) in [
        ("postgres", "postgres://guest:secret@database/wamn"),
        (
            "postgresql",
            "postgresql://guest:secret@database/wamn?sslmode=require",
        ),
    ] {
        let mutant = component_environment_fixture(&format!("WAMN_ENDPOINT: \"{url}\""));
        let error = validate_workload_policy(
            "component-postgres-url-value-mutant.yaml",
            &mutant,
            &WorkloadAbi::P2Components,
        )
        .expect_err("a postgres URL under a neutral component environment key must fail closed");
        assert!(
            error.contains(&format!("may not carry a {name} URL")),
            "{name} value mutation failed for an unexpected reason: {error}"
        );
        assert!(
            !error.contains("guest:secret"),
            "the refusal must not echo credential material: {error}"
        );
    }
}

#[test]
fn database_url_names_and_values_outside_component_environment_are_allowed() {
    let control = "      env:\n\
                   \x20       - name: DATABASE_URL\n\
                   \x20         value: postgres://host:secret@database/wamn\n\
                   \x20     components:\n\
                   \x20       - name: control\n\
                   \x20         localResources:\n\
                   \x20           allowedIpNameLookups: []\n\
                   \x20           environment:\n\
                   \x20             config:\n\
                   \x20               WAMN_MODE: safe\n\
                   \x20               WAMN_OPTIONAL:\n\
                   ---\n\
                   apiVersion: v1\n\
                   kind: Secret\n\
                   stringData:\n\
                   \x20 DATABASE_URL: postgresql://secret:secret@database/wamn\n";
    validate_workload_policy(
        "component-environment-boundary-control.yaml",
        control,
        &WorkloadAbi::P2Components,
    )
    .expect("host environment and arbitrary Secret fields are outside this guard");
}

#[test]
fn execution_host_inventory_ignores_cfg_test_store_constructor() {
    let source = format!(
        "{EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         {CFG_TEST_MODULE}\n\
             {EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         }}\n"
    );
    assert_eq!(
        source.matches(EXECUTION_HOST_STORE_CONSTRUCTOR).count(),
        2,
        "fixture must contain identical production and test-only constructors"
    );
    validate_execution_host_store_constructor(&source)
        .expect("the cfg(test) constructor must not widen the production inventory");
}

#[test]
fn execution_host_inventory_rejects_removed_or_duplicated_production_constructor() {
    let test_module = format!(
        "{CFG_TEST_MODULE}\n\
             {EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         }}\n"
    );
    let removed = validate_execution_host_store_constructor(&test_module)
        .expect_err("removing the production ExecutionHost constructor must fail");
    assert!(
        removed.ends_with("found 0"),
        "removed-constructor failure must report the production count: {removed}"
    );

    let duplicated = format!(
        "{EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         {EXECUTION_HOST_STORE_CONSTRUCTOR}\n\
         {test_module}"
    );
    let duplicate = validate_execution_host_store_constructor(&duplicated)
        .expect_err("duplicating the production ExecutionHost constructor must fail");
    assert!(
        duplicate.ends_with("found 2"),
        "duplicate-constructor failure must report the production count: {duplicate}"
    );
}

/// wamn-0h0g.15.101: one release-manifest weld per host process, constructed
/// before that process can bind a component.
#[test]
fn one_release_manifest_weld_construction_site_per_host_process() {
    let root = repository_root();
    for (path, entry, bind) in HOST_WELD_SITES {
        let source = host_source(&root, path);
        validate_one_weld_site(&source, path).unwrap_or_else(|error| panic!("{error}"));
        validate_weld_precedes_bind(&source, entry, bind, path)
            .unwrap_or_else(|error| panic!("{error}"));
    }
}

#[test]
fn the_host_router_delivery_bridge_is_metered() {
    let (path, marker) = METERED_DELIVERY_BRIDGE;
    let source = host_source(&repository_root(), path);
    let production = production_half(&source, path).unwrap_or_else(|error| panic!("{error}"));
    validate_one(production, marker, path).unwrap_or_else(|error| {
        panic!("{error}. Without it both wamn.router.delivery series stay silent")
    });
}

#[test]
fn every_production_release_identity_is_read_off_the_weld() {
    let root = repository_root();
    for (path, weld) in RELEASE_IDENTITY_SOURCE_SITES {
        let source = host_source(&root, path);
        validate_release_identity_from_weld(&source, weld, path)
            .unwrap_or_else(|error| panic!("{error}"));
    }
}

#[test]
fn the_struck_release_identity_config_keys_do_not_return() {
    let root = repository_root();
    for path in STRUCK_KEY_SITES {
        let source = host_source(&root, path);
        validate_no_struck_key(&source, path).unwrap_or_else(|error| panic!("{error}"));
    }
}

/// The fixture the mutants below are cut from: one flat construction whose two
/// halves both come off `release.release()`.
fn welded_release_identity() -> String {
    format!(
        "let release = load_release(base, digest)?;\n\
         let identity = {RELEASE_IDENTITY_CONSTRUCTION}\n\
         \x20   release_version: release.release().release_version,\n\
         \x20   manifest_digest: release.release().manifest_digest.clone(),\n\
         }};\n"
    )
}

#[test]
fn release_identity_inventory_accepts_the_welded_shape() {
    validate_release_identity_from_weld(&welded_release_identity(), "release.release()", "seam")
        .expect("both halves read off the weld must pass");
}

#[test]
fn release_identity_inventory_rejects_a_removed_or_duplicated_construction() {
    let duplicated = format!("{}{}", welded_release_identity(), welded_release_identity());
    for source in [String::new(), duplicated] {
        let error = validate_release_identity_from_weld(&source, "release.release()", "seam")
            .expect_err("a missing or duplicated construction must be rejected");
        assert!(
            error.contains("exactly one"),
            "the refusal must name the count it required: {error}"
        );
    }
}

#[test]
fn release_identity_inventory_rejects_a_half_read_from_elsewhere() {
    for stolen in ["release_version", "manifest_digest"] {
        let mutant = welded_release_identity().replace(
            &format!("{stolen}: release.release()."),
            &format!("{stolen}: config.get("),
        );
        let error = validate_release_identity_from_weld(&mutant, "release.release()", "seam")
            .expect_err("a half read from anywhere but the weld must be rejected");
        assert!(
            error.contains("second carrier"),
            "the refusal must name why a second source matters: {error}"
        );
    }
}

#[test]
fn release_identity_inventory_rejects_a_returning_config_key() {
    for key in STRUCK_RELEASE_IDENTITY_KEYS {
        let source = format!("config.get(\"{key}\")");
        let error = validate_no_struck_key(&source, "seam")
            .expect_err("a reintroduced config key must be rejected");
        assert!(
            error.contains(key),
            "the refusal must name the key it found: {error}"
        );
    }
}

#[test]
fn weld_inventory_ignores_cfg_test_construction_sites() {
    // The weld's own unit tests and the plan-supply tests construct welds from
    // fixture directories; only production sites are the subject of the one-per-
    // process rule.
    let source = format!(
        "{RELEASE_WELD_CONSTRUCTION}_from(root)\n\
         {CFG_TEST_MODULE}\n\
             {RELEASE_WELD_CONSTRUCTION}_from(fixture)\n\
             {RELEASE_WELD_CONSTRUCTION}()\n\
         }}\n"
    );
    assert_eq!(
        source.matches(RELEASE_WELD_CONSTRUCTION).count(),
        3,
        "fixture must carry one production and two test-only construction sites"
    );
    validate_one_weld_site(&source, "weld-mutant.rs")
        .expect("cfg(test) construction must not widen the production inventory");
}

#[test]
fn weld_inventory_rejects_removed_or_duplicated_construction_site() {
    let test_module =
        format!("{CFG_TEST_MODULE}\n    {RELEASE_WELD_CONSTRUCTION}_from(fixture)\n}}\n");

    let removed = validate_one_weld_site(&test_module, "weld-mutant.rs")
        .expect_err("removing the production weld construction must fail");
    assert!(
        removed.ends_with("found 0"),
        "removed-weld failure must report the production count: {removed}"
    );

    // Two production sites in one process is the exact drift this guard exists to
    // catch: two loaded manifests where the ruling allows one.
    let duplicated = format!(
        "{RELEASE_WELD_CONSTRUCTION}_from(root)\n\
         {RELEASE_WELD_CONSTRUCTION}()\n\
         {test_module}"
    );
    let duplicate = validate_one_weld_site(&duplicated, "weld-mutant.rs")
        .expect_err("a second production weld construction must fail");
    assert!(
        duplicate.ends_with("found 2"),
        "duplicate-weld failure must report the production count: {duplicate}"
    );
}

#[test]
fn weld_inventory_rejects_construction_after_the_first_bind() {
    let ordered = "let release = load_release(root)?;\nClusterHostBuilder::default()\n";
    validate_weld_precedes_bind(
        ordered,
        "let release = load_release(",
        "ClusterHostBuilder::default()",
        "weld-mutant.rs",
    )
    .expect("construction ahead of the builder must pass");

    let inverted = "ClusterHostBuilder::default()\nlet release = load_release(root)?;\n";
    let error = validate_weld_precedes_bind(
        inverted,
        "let release = load_release(",
        "ClusterHostBuilder::default()",
        "weld-mutant.rs",
    )
    .expect_err("constructing the weld after the host builder must fail");
    assert!(
        error.contains("carries no release identity for its claim to record"),
        "ordering failure must name the consequence: {error}"
    );

    for (name, mutant) in [
        ("weld-unreachable", "ClusterHostBuilder::default()\n"),
        ("bind-removed", "let release = load_release(root)?;\n"),
    ] {
        let error = validate_weld_precedes_bind(
            mutant,
            "let release = load_release(",
            "ClusterHostBuilder::default()",
            "weld-mutant.rs",
        )
        .expect_err("a missing anchor must fail closed");
        assert!(
            error.starts_with("weld-mutant.rs must "),
            "{name} mutation failed for an unexpected reason: {error}"
        );
    }
}

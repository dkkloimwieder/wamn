//! Runtime authority and deployed workload policy checks.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;

const CFG_TEST_MODULE: &str = "#[cfg(test)]\nmod tests {";
/// The release manifest load call, deliberately truncated before the
/// `(` so it matches `load` and `load_from` alike — the guard counts
/// *construction*, not one spelling of it.
///
/// Counted as raw text, like every other marker here, so prose in a host file that
/// wrote this marker out in full would read as a second construction site. Host
/// doc comments name the type and the method separately for that reason.
const RELEASE_LOAD_CONSTRUCTION: &str = "LoadedRelease::load";

/// The two host processes, and per process the two positions that must hold:
/// `(file, the text that reaches the loaded release, the first bind-capable text it must
/// precede)`.
///
/// wamn-0h0g.15.101 rules one loaded release instance PER PROCESS: the wash host serves
/// flow-http routing and jetstream delivery, the executor serves the durable
/// queue. Separate processes cannot share one object, so each constructs exactly
/// once — and must do so before anything binds a component, because under ruling
/// wamn-0h0g.15.102 the verified manifest is the sole carrier of the
/// `(effective release id, manifest digest)` pair a claim records. A component that
/// bound first would have no pair.
///
/// The second process used to be the in-process run host, reached through
/// `load_plan_release`; `18ba72b6` deleted host plan supply and that symbol with
/// it, so this entry named a function that existed nowhere and the guard proved
/// nothing (wamn-nguw). Both surviving processes call the loaded release directly.
const HOST_RELEASE_LOAD_SITES: [(&str, &str, &str); 2] = [
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
/// production builds a `ReleaseIdentity`, both halves are read off the loaded release. A
/// site that invented either half from anywhere else would restore the
/// dual-representation bug the ruling closed — two carriers with nothing
/// reconciling them, so a pod could stamp one release onto a run while resolving
/// plans against another.
const RELEASE_IDENTITY_CONSTRUCTION: &str = "ReleaseIdentity {";

/// The two production sites that build the pair, and the loaded release expression each
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
/// needs NATS and a loaded release. This test reads the construction source
/// directly, as the release-load tests above do.
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

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
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

/// Everything before a file's terminal `#[cfg(test)] mod tests {`, or the whole
/// file when it has none. Both shapes are valid for the host loaded release sites.
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

fn validate_one_release_load_site(source: &str, seam: &str) -> Result<(), String> {
    let production = production_half(source, seam)?;
    validate_one(production, RELEASE_LOAD_CONSTRUCTION, seam)
}

fn validate_release_load_precedes_bind(
    source: &str,
    entry: &str,
    bind: &str,
    seam: &str,
) -> Result<(), String> {
    let production = production_half(source, seam)?;
    let Some(entry_at) = production.find(entry) else {
        return Err(format!("{seam} must reach its loaded release through `{entry}`"));
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

/// Every half of a production `ReleaseIdentity` is read off the loaded release.
///
/// The literal's body is taken as the text between `ReleaseIdentity {` and the
/// next `}` — every production construction is a flat struct literal of two
/// scalar fields, so no nesting can hide inside it — and both field
/// initializers must name `loaded_release`, the expression that reaches this file's loaded release.
fn validate_release_identity_from_loaded_release(source: &str, loaded_release: &str, seam: &str) -> Result<(), String> {
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
    for field in ["effective_release_id", "manifest_digest"] {
        let initializer = body
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("{field}:")));
        if !initializer.is_some_and(|line| line.contains(loaded_release) && line.contains(field)) {
            return Err(format!(
                "{seam} must initialize `{field}` from `{loaded_release}.{field}`. A pair read from \
                 anywhere but the loaded release is a second carrier of the release identity the \
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

#[test]
fn only_positive_pool_sizes_keep_instances_warm() {
    use wash_runtime::engine::InstancePolicy;
    use wash_runtime::types::Component;

    for pool_size in [i32::MIN, -1, 0, 1, i32::MAX] {
        let component = Component {
            pool_size,
            ..Default::default()
        };
        assert_eq!(
            InstancePolicy::from_component(&component).keeps_instances_warm(),
            pool_size > 0,
            "pool_size {pool_size} must preserve the native fresh-store boundary"
        );
    }
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

fn validate_workload_policy(path: &str, source: &str) -> Result<(), String> {
    let pool_sizes = yaml_i32(source, "poolSize:");
    if let Some(pool_size) = pool_sizes.into_iter().find(|pool_size| *pool_size != 0) {
        return Err(format!(
            "{path}: poolSize {pool_size} enables reusable component stores"
        ));
    }

    validate_ip_name_lookup_defaults(path, source)?;
    validate_no_component_database_urls(path, source)?;

    Ok(())
}

#[test]
fn deployed_workloads_preserve_runtime_policy() {
    let root = repository_root();
    let workloads = workload_manifests(&root);
    assert!(
        !workloads.is_empty(),
        "deploy/platform must contain a workload"
    );
    for path in workloads {
        let source = host_source(&root, &path);
        validate_workload_policy(&path, &source).unwrap_or_else(|error| panic!("{error}"));
    }
}

#[test]
fn materializer_keeps_its_command_export() {
    let root = repository_root();
    let component = root.join("apps/platform/execution/materializer");
    assert!(
        component.join("src/main.rs").is_file(),
        "materializer must remain a command component"
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

#[test]
fn nonzero_pool_size_mutation_is_rejected() {
    let mutant = "components:\n  - name: mutant\n    poolSize: 1\n    maxInvocations: 10\n";
    let error = validate_workload_policy("pool-size-mutant.yaml", mutant)
        .expect_err("mutation must fail closed");
    assert!(error.contains("poolSize 1 enables reusable component stores"));
}

#[test]
fn nonempty_ip_name_lookup_default_mutation_is_rejected() {
    let mutant = "      components:\n        - name: mutant\n          localResources:\n            allowedIpNameLookups: [\"example.com\"]\n";
    let error = validate_workload_policy("lookup-mutant.yaml", mutant)
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
        let error = validate_workload_policy("lookup-mutant.yaml", mutant)
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
    let error = validate_workload_policy("component-pg-url-key-mutant.yaml", &mutant)
        .expect_err("a component environment *_PG_URL key must fail closed");
    assert!(
        error.contains("key `WAMN_READER_PG_URL`"),
        "the refusal must name the forbidden key: {error}"
    );
}

#[test]
fn component_environment_database_url_mutation_is_rejected() {
    let mutant = component_environment_fixture("DATABASE_URL:");
    let error = validate_workload_policy("component-database-url-key-mutant.yaml", &mutant)
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
        let error = validate_workload_policy("component-postgres-url-value-mutant.yaml", &mutant)
            .expect_err(
                "a postgres URL under a neutral component environment key must fail closed",
            );
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
    validate_workload_policy("component-environment-boundary-control.yaml", control)
        .expect("host environment and arbitrary Secret fields are outside this guard");
}

/// wamn-0h0g.15.101: one loaded release manifest per host process, constructed
/// before that process can bind a component.
#[test]
fn one_release_load_site_per_host_process() {
    let root = repository_root();
    for (path, entry, bind) in HOST_RELEASE_LOAD_SITES {
        let source = host_source(&root, path);
        validate_one_release_load_site(&source, path).unwrap_or_else(|error| panic!("{error}"));
        validate_release_load_precedes_bind(&source, entry, bind, path)
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
fn every_production_release_identity_is_read_off_the_loaded_release() {
    let root = repository_root();
    for (path, loaded_release) in RELEASE_IDENTITY_SOURCE_SITES {
        let source = host_source(&root, path);
        validate_release_identity_from_loaded_release(&source, loaded_release, path)
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
fn loaded_release_identity() -> String {
    format!(
        "let release = load_release(base, digest)?;\n\
         let identity = {RELEASE_IDENTITY_CONSTRUCTION}\n\
         \x20   effective_release_id: release.release().effective_release_id,\n\
         \x20   manifest_digest: release.release().manifest_digest.clone(),\n\
         }};\n"
    )
}

#[test]
fn release_identity_list_accepts_the_loaded_shape() {
    validate_release_identity_from_loaded_release(&loaded_release_identity(), "release.release()", "seam")
        .expect("both halves read off the loaded release must pass");
}

#[test]
fn release_identity_list_rejects_a_removed_or_duplicated_construction() {
    let duplicated = format!("{}{}", loaded_release_identity(), loaded_release_identity());
    for source in [String::new(), duplicated] {
        let error = validate_release_identity_from_loaded_release(&source, "release.release()", "seam")
            .expect_err("a missing or duplicated construction must be rejected");
        assert!(
            error.contains("exactly one"),
            "the refusal must name the count it required: {error}"
        );
    }
}

#[test]
fn release_identity_list_rejects_a_half_read_from_elsewhere() {
    for stolen in ["effective_release_id", "manifest_digest"] {
        let mutant = loaded_release_identity().replace(
            &format!("{stolen}: release.release()."),
            &format!("{stolen}: config.get("),
        );
        let error = validate_release_identity_from_loaded_release(&mutant, "release.release()", "seam")
            .expect_err("a half read from anywhere but the loaded release must be rejected");
        assert!(
            error.contains("second carrier"),
            "the refusal must name why a second source matters: {error}"
        );
    }
}

#[test]
fn release_identity_list_rejects_a_returning_config_key() {
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
fn release_load_ignores_cfg_test_construction_sites() {
    // Unit tests and plan-supply tests load releases from fixture directories.
    // The one-instance rule applies only to production sites.
    let source = format!(
        "{RELEASE_LOAD_CONSTRUCTION}_from(root)\n\
         {CFG_TEST_MODULE}\n\
             {RELEASE_LOAD_CONSTRUCTION}_from(fixture)\n\
             {RELEASE_LOAD_CONSTRUCTION}()\n\
         }}\n"
    );
    assert_eq!(
        source.matches(RELEASE_LOAD_CONSTRUCTION).count(),
        3,
        "fixture must carry one production and two test-only construction sites"
    );
    validate_one_release_load_site(&source, "release-load-mutant.rs")
        .expect("cfg(test) construction must not widen the production list");
}

#[test]
fn release_load_rejects_removed_or_duplicated_construction_site() {
    let test_module =
        format!("{CFG_TEST_MODULE}\n    {RELEASE_LOAD_CONSTRUCTION}_from(fixture)\n}}\n");

    let removed = validate_one_release_load_site(&test_module, "release-load-mutant.rs")
        .expect_err("removing the production release load must fail");
    assert!(
        removed.ends_with("found 0"),
        "removed-release-load failure must report the production count: {removed}"
    );

    // Two production sites in one process is the exact drift this guard exists to
    // catch: two loaded manifests where the ruling allows one.
    let duplicated = format!(
        "{RELEASE_LOAD_CONSTRUCTION}_from(root)\n\
         {RELEASE_LOAD_CONSTRUCTION}()\n\
         {test_module}"
    );
    let duplicate = validate_one_release_load_site(&duplicated, "release-load-mutant.rs")
        .expect_err("a second production release load must fail");
    assert!(
        duplicate.ends_with("found 2"),
        "duplicate-release-load failure must report the production count: {duplicate}"
    );
}

#[test]
fn release_load_rejects_construction_after_the_first_bind() {
    let ordered = "let release = load_release(root)?;\nClusterHostBuilder::default()\n";
    validate_release_load_precedes_bind(
        ordered,
        "let release = load_release(",
        "ClusterHostBuilder::default()",
        "release-load-mutant.rs",
    )
    .expect("construction ahead of the builder must pass");

    let inverted = "ClusterHostBuilder::default()\nlet release = load_release(root)?;\n";
    let error = validate_release_load_precedes_bind(
        inverted,
        "let release = load_release(",
        "ClusterHostBuilder::default()",
        "release-load-mutant.rs",
    )
    .expect_err("constructing the loaded release after the host builder must fail");
    assert!(
        error.contains("carries no release identity for its claim to record"),
        "ordering failure must name the consequence: {error}"
    );

    for (name, mutant) in [
        ("release-load-unreachable", "ClusterHostBuilder::default()\n"),
        ("bind-removed", "let release = load_release(root)?;\n"),
    ] {
        let error = validate_release_load_precedes_bind(
            mutant,
            "let release = load_release(",
            "ClusterHostBuilder::default()",
            "release-load-mutant.rs",
        )
        .expect_err("a missing anchor must fail closed");
        assert!(
            error.starts_with("release-load-mutant.rs must "),
            "{name} mutation failed for an unexpected reason: {error}"
        );
    }
}

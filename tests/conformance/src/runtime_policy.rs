//! Runtime authority and deployed workload policy checks.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use url::Url;

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
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

//! Dependency-based test selection and declared input preparation.

use crate::{
    process,
    tasks::{self, Mode},
};
use anyhow::{Context as _, ensure};
use clap::Args as ClapArgs;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use tokio::process::Command;

const WORKSPACES: [(&str, &str); 2] = [("root", "Cargo.toml"), ("apps", "apps/Cargo.toml")];

#[derive(Debug, ClapArgs)]
pub struct Args {
    #[arg(long)]
    base: Option<String>,
    #[arg(long, value_delimiter = ',')]
    tier: Vec<String>,
    #[arg(long)]
    name: Option<String>,
    #[arg(long, value_delimiter = ',')]
    suite: Vec<String>,
    #[arg(value_enum, default_value = "run")]
    mode: Mode,
}

#[derive(Debug, Deserialize)]
struct Policy {
    version: u32,
    documentation: Vec<String>,
    root_features: Vec<String>,
    rules: Vec<Rule>,
    targets: Vec<TierAssignment>,
    suites: Vec<Suite>,
    preparation: Vec<Preparation>,
}
#[derive(Debug, Deserialize)]
struct Rule {
    inputs: Vec<String>,
    #[serde(default)]
    workspaces: Vec<String>,
    #[serde(default)]
    application: bool,
}
#[derive(Debug, Deserialize)]
struct TierAssignment {
    workspace: String,
    package: String,
    target: String,
    tier: String,
}
#[derive(Debug, Clone, Deserialize)]
struct Suite {
    id: String,
    tier: String,
    workspace: String,
    package: String,
    target: Vec<String>,
    tests: Vec<String>,
    features: Vec<String>,
    prerequisites: Vec<String>,
    inputs: Vec<String>,
    execution: String,
    mode: String,
    resource_lock: String,
    always: Option<bool>,
    #[serde(default)]
    ignored: bool,
    #[serde(default)]
    filter: String,
    #[serde(default)]
    select_packages: Vec<String>,
    #[serde(default)]
    commands: Vec<Vec<String>>,
}
#[derive(Debug, Deserialize)]
struct Preparation {
    id: Option<String>,
    #[serde(default)]
    packages: Vec<String>,
    #[serde(default)]
    target: Vec<String>,
    #[serde(default)]
    suites: Vec<String>,
    #[serde(default)]
    prerequisites: Vec<String>,
    #[serde(default)]
    features: Vec<String>,
    #[serde(default)]
    outputs: BTreeMap<String, String>,
    #[serde(default)]
    aliases: BTreeMap<String, String>,
    #[serde(default)]
    applications: Vec<String>,
    #[serde(default)]
    before_applications: Vec<Vec<String>>,
    #[serde(default)]
    commands: Vec<Vec<String>>,
    #[serde(default)]
    after_applications: bool,
}
#[derive(Debug)]
struct Selected {
    workspace: usize,
    package: String,
    target: String,
    features: BTreeSet<String>,
}

fn matches(patterns: &[String], path: &str) -> anyhow::Result<bool> {
    for pattern in patterns {
        if regex::Regex::new(pattern)?.is_match(path) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn target_name(target: &cargo_metadata::Target) -> String {
    format!("{}:{}", target.kind[0], target.name)
}

pub async fn run(root: &Path, args: &Args) -> anyhow::Result<()> {
    let tiers: Vec<_> = if args.tier.is_empty() {
        vec!["unit", "contract", "live-postgres", "live-nats"]
    } else {
        args.tier.iter().map(String::as_str).collect()
    };
    ensure!(
        tiers
            .iter()
            .all(|tier| ["unit", "contract", "live-postgres", "live-nats", "kind"].contains(tier)),
        "unknown test tier"
    );
    let cluster = tiers.contains(&"kind");
    ensure!(
        args.name.is_none() || cluster,
        "--name requires --tier kind"
    );
    let base = match &args.base {
        Some(base) => base.clone(),
        None => process::output(root, "git", &["merge-base", "HEAD", "main"])
            .await?
            .trim()
            .to_owned(),
    };
    let base = process::output(
        root,
        "git",
        &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
    )
    .await?
    .trim()
    .to_owned();
    let mut changed = process::output(
        root,
        "git",
        &["diff", "--name-only", "--no-renames", "-z", &base, "--"],
    )
    .await?;
    changed.push_str(
        &process::output(
            root,
            "git",
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )
        .await?,
    );
    let policy_text = std::fs::read_to_string(root.join("tests/selection.toml"))?;
    let policy: Policy = toml::from_str(&policy_text)?;
    ensure!(policy.version == 1, "unsupported selection schema");
    let mut ids = BTreeSet::new();
    for suite in &policy.suites {
        ensure!(ids.insert(&suite.id), "duplicate suite {}", suite.id);
        ensure!(
            WORKSPACES.iter().any(|(name, _)| *name == suite.workspace),
            "invalid workspace in {}",
            suite.id
        );
        ensure!(
            ["commands", "libtest"].contains(&suite.execution.as_str()),
            "invalid execution in {}",
            suite.id
        );
    }
    for id in &args.suite {
        ensure!(ids.contains(id), "unknown suite {id}");
    }
    let mut row_suites = None;
    let mut policy_tests_changed = false;
    if changed
        .split('\0')
        .any(|path| path == "tests/selection.toml")
    {
        let previous = process::output(
            root,
            "git",
            &["show", &format!("{base}:tests/selection.toml")],
        )
        .await?;
        let before: toml::Value = toml::from_str(&previous)?;
        let after: toml::Value = toml::from_str(&policy_text)?;
        let shared = |value: &toml::Value| {
            let mut value = value.clone();
            for field in ["suites", "targets", "exclusions"] {
                value.as_table_mut().expect("selection table").remove(field);
            }
            value
        };
        let previous_rows = before["suites"].as_array().context("previous suites")?;
        let current_rows = after["suites"].as_array().context("current suites")?;
        for removed in previous_rows
            .iter()
            .filter_map(|row| row["id"].as_str())
            .filter(|id| !ids.iter().any(|current| current.as_str() == *id))
        {
            let mut command = Command::new("git");
            command.current_dir(root).args([
                "grep",
                "-w",
                "-l",
                "-F",
                "-e",
                removed,
                "--",
                ".",
                ":!tests/selection.toml",
                ":!docs/history/**",
                ":!docs/audit/**",
            ]);
            let references = command.output().await?;
            ensure!(
                references.status.code().is_some_and(|code| code <= 1),
                "cannot inspect removed suite {removed}"
            );
            ensure!(
                references.stdout.is_empty(),
                "removed suite {removed} is still referenced: {}",
                String::from_utf8_lossy(&references.stdout)
            );
        }
        if !changed
            .split('\0')
            .any(|path| path.starts_with("xtask/src/selection") || path == "tools/test-changes")
            && shared(&before) == shared(&after)
            && shared_comments(&previous) == shared_comments(&policy_text)
        {
            let rows: BTreeSet<String> = current_rows
                .iter()
                .filter(|row| !previous_rows.iter().any(|old| old == *row))
                .filter_map(|row| row["id"].as_str().map(str::to_owned))
                .collect();
            policy_tests_changed = !rows.is_empty()
                || before["targets"] != after["targets"]
                || before.get("exclusions") != after.get("exclusions");
            row_suites = Some(rows);
        }
    }
    let mut paths = Vec::new();
    for path in changed.split('\0').filter(|path| !path.is_empty()) {
        if !(matches(&policy.documentation, path)?
            || path == "tests/selection.toml" && row_suites.is_some())
        {
            paths.push(path);
        }
    }
    paths.sort_unstable();
    paths.dedup();
    println!("working-directory: {}\nbase: {base}", root.display());
    if paths.is_empty() && args.suite.is_empty() && !policy_tests_changed {
        println!("selected: nothing");
        ensure!(args.name.is_none(), "no tests match name filter");
        return Ok(());
    }
    let mut metadata = Vec::new();
    for (_, manifest) in WORKSPACES {
        metadata.push(
            cargo_metadata::MetadataCommand::new()
                .current_dir(root)
                .manifest_path(root.join(manifest))
                .other_options(vec!["--locked".into(), "--offline".into()])
                .exec()?,
        );
    }
    // A path belongs to the deepest member, including members of nested workspaces.
    let mut seeds = BTreeSet::new();
    let mut full = BTreeSet::new();
    let mut unknown = false;
    for path in &paths {
        if let Some(index) = WORKSPACES.iter().position(|(_, manifest)| {
            *path == *manifest || *path == manifest.replace(".toml", ".lock")
        }) {
            full.insert(index);
            continue;
        }
        let owner = metadata
            .iter()
            .flat_map(cargo_metadata::Metadata::workspace_packages)
            .filter(|package| {
                let directory = package
                    .manifest_path
                    .parent()
                    .expect("manifest parent")
                    .strip_prefix(root)
                    .expect("workspace member below root");
                path.starts_with(&format!("{directory}/"))
            })
            .max_by_key(|package| package.manifest_path.as_str().len());
        if let Some(package) = owner {
            seeds.insert(package.manifest_path.to_string());
        }
        let mut matched = false;
        let mut application = false;
        for rule in &policy.rules {
            if !matches(&rule.inputs, path)? {
                continue;
            }
            for workspace in &rule.workspaces {
                full.insert(
                    WORKSPACES
                        .iter()
                        .position(|(name, _)| name == workspace)
                        .context("rule workspace")?,
                );
                matched = true;
            }
            application |= rule.application;
        }
        if owner.is_none() && application {
            let prefix = path.split('/').take(2).collect::<Vec<_>>().join("/") + "/";
            for package in metadata
                .iter()
                .flat_map(cargo_metadata::Metadata::workspace_packages)
            {
                if package
                    .manifest_path
                    .strip_prefix(root)
                    .expect("member path")
                    .as_str()
                    .starts_with(&prefix)
                {
                    seeds.insert(package.manifest_path.to_string());
                    matched = true;
                }
            }
        }
        if owner.is_none() && !matched {
            unknown = true;
        }
    }
    let mut selected_packages = BTreeSet::new();
    let mut selected = Vec::new();
    for (index, md) in metadata.iter().enumerate() {
        let mut dependents: BTreeSet<_> = md
            .packages
            .iter()
            .filter(|package| seeds.contains(package.manifest_path.as_str()))
            .map(|package| package.id.clone())
            .collect();
        let graph = md
            .resolve
            .as_ref()
            .context("Cargo metadata has no dependency graph")?;
        loop {
            let old = dependents.len();
            for node in &graph.nodes {
                if node.deps.iter().any(|dep| {
                    dependents.contains(&dep.pkg)
                        && dep
                            .dep_kinds
                            .iter()
                            .any(|edge| edge.kind != cargo_metadata::DependencyKind::Development)
                }) {
                    dependents.insert(node.id.clone());
                }
            }
            if old == dependents.len() {
                break;
            }
        }
        let development: Vec<_> = graph
            .nodes
            .iter()
            .filter(|node| {
                node.deps.iter().any(|dep| {
                    dependents.contains(&dep.pkg)
                        && dep
                            .dep_kinds
                            .iter()
                            .any(|edge| edge.kind == cargo_metadata::DependencyKind::Development)
                })
            })
            .map(|node| node.id.clone())
            .collect();
        dependents.extend(development);
        for package in md.workspace_packages() {
            if !(unknown
                || full.contains(&index)
                || dependents.contains(&package.id)
                || policy_tests_changed && package.name.as_str() == "xtask")
            {
                continue;
            }
            selected_packages.insert(package.name.to_string());
            let mut features: BTreeSet<_> = package
                .targets
                .iter()
                .flat_map(|target| &target.required_features)
                .filter(|feature| feature.as_str() != "cluster" || cluster)
                .map(|feature| {
                    if feature.contains('/') {
                        feature.clone()
                    } else {
                        format!("{}/{feature}", package.name)
                    }
                })
                .collect();
            for prep in &policy.preparation {
                if prep
                    .packages
                    .iter()
                    .any(|name| name == package.name.as_str())
                {
                    features.extend(
                        prep.features
                            .iter()
                            .map(|feature| format!("{}/{feature}", package.name)),
                    );
                }
            }
            if cluster && package.features.contains_key("cluster") {
                features.insert(format!("{}/cluster", package.name));
            }
            if index == 0 && (unknown || full.contains(&0)) {
                features.extend(
                    policy
                        .root_features
                        .iter()
                        .filter(|feature| feature.starts_with(&format!("{}/", package.name)))
                        .cloned(),
                );
            }
            for target in package
                .targets
                .iter()
                .filter(|target| target.test && !target.is_custom_build())
            {
                let name = target_name(target);
                if !args.tier.is_empty()
                    && !policy.targets.iter().any(|row| {
                        row.workspace == WORKSPACES[index].0
                            && row.package == package.name.as_str()
                            && row.target == name
                            && tiers.contains(&row.tier.as_str())
                    })
                {
                    continue;
                }
                selected.push(Selected {
                    workspace: index,
                    package: package.name.to_string(),
                    target: name,
                    features: features.clone(),
                });
            }
        }
    }
    let mut suites = Vec::new();
    for suite in &policy.suites {
        let include = if args.suite.is_empty() {
            (suite.mode == "always" || tiers.contains(&suite.tier.as_str()))
                && suite.always != Some(false)
                && (unknown
                    || row_suites
                        .as_ref()
                        .is_some_and(|rows| rows.contains(&suite.id))
                    || suite.always == Some(true)
                    || paths
                        .iter()
                        .any(|path| matches(&suite.inputs, path).unwrap_or(false))
                    || suite
                        .select_packages
                        .iter()
                        .any(|name| selected_packages.contains(name)))
        } else {
            args.suite.contains(&suite.id)
        };
        if include {
            let mut suite = suite.clone();
            if suite.tier == "kind"
                && !suite.tests.is_empty()
                && let Some(filter) = &args.name
            {
                suite.tests.retain(|name| name.contains(filter));
                if suite.tests.is_empty() {
                    continue;
                }
            }
            suites.push(suite);
        }
    }
    if !args.suite.is_empty() {
        selected.clear();
    }
    println!(
        "suites: {}",
        suites
            .iter()
            .map(|suite| suite.id.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    for suite in &suites {
        let workspace = WORKSPACES
            .iter()
            .position(|(name, _)| *name == suite.workspace)
            .context("suite workspace")?;
        let package = metadata[workspace]
            .workspace_packages()
            .into_iter()
            .find(|package| package.name.as_str() == suite.package);
        if suite.execution == "libtest" {
            let package = package.with_context(|| {
                format!("suite {}: missing package {}", suite.id, suite.package)
            })?;
            for target in &suite.target {
                ensure!(
                    package
                        .targets
                        .iter()
                        .any(|row| target_name(row) == *target),
                    "suite {}: missing target {target}",
                    suite.id
                );
            }
            for target in &mut selected {
                if target.workspace == workspace && target.package == suite.package {
                    target.features.extend(
                        suite
                            .features
                            .iter()
                            .map(|feature| format!("{}/{feature}", suite.package)),
                    );
                }
            }
        }
    }
    let preparation_suites = suites.clone();
    suites.retain(|suite| {
        if suite.execution != "libtest"
            || suite.ignored
            || suite.tier == "kind"
            || !suite.filter.is_empty()
        {
            return true;
        }
        let workspace = WORKSPACES
            .iter()
            .position(|(name, _)| *name == suite.workspace)
            .expect("suite workspace");
        let package = metadata[workspace]
            .workspace_packages()
            .into_iter()
            .find(|package| package.name.as_str() == suite.package)
            .expect("validated suite package");
        let targets: Vec<_> = package
            .targets
            .iter()
            .filter(|target| {
                target.test
                    && (suite.target.is_empty() || suite.target.contains(&target_name(target)))
            })
            .map(target_name)
            .collect();
        !targets.iter().all(|target| {
            selected.iter().any(|row| {
                row.workspace == workspace && row.package == suite.package && row.target == *target
            })
        })
    });
    let mut runner = Runner {
        root,
        mode: args.mode,
        env: BTreeMap::new(),
    };
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let target = if target.is_absolute() {
        target
    } else {
        root.join(target)
    };
    runner
        .env
        .insert("CARGO_TARGET_DIR".into(), target.display().to_string());
    runner.env.insert(
        "PATH".into(),
        format!("{}/debug:{}", target.display(), std::env::var("PATH")?),
    );
    let required = prepare(&mut runner, &policy, &selected, &preparation_suites).await?;
    // Exact suite selectors are resolved before execution.
    for suite in &preparation_suites {
        if suite.execution != "libtest" {
            continue;
        }
        for name in &suite.tests {
            let mut command = runner.cargo_suite(suite);
            command
                .arg(name)
                .args(["--", "--exact", "--list", "--format", "terse"]);
            if suite.ignored {
                command.arg("--ignored");
            }
            if matches!(args.mode, Mode::Run) {
                let listed = process::execute(&mut command).await?;
                ensure!(
                    listed.lines().any(|line| line == format!("{name}: test")),
                    "suite {}: missing exact test {name}",
                    suite.id
                );
            } else {
                println!("{}", tasks::describe(&command));
            }
        }
    }
    let mut failed = Vec::new();
    for suite in suites.iter().filter(|suite| suite.mode == "always") {
        if let Err(error) = runner.suite(suite, args.name.as_deref()).await {
            failed.push(format!("{}: {error:#}", suite.id));
            if suite.tier == "kind" {
                anyhow::bail!("{}", failed.join("\n"));
            }
        }
    }
    let mut generic_results: BTreeMap<usize, bool> = BTreeMap::new();
    for target in &selected {
        let owned = suites.iter().any(|suite| {
            suite.execution == "libtest"
                && suite.workspace == WORKSPACES[target.workspace].0
                && suite.package == target.package
                && (suite.target.is_empty() || suite.target.contains(&target.target))
                && (suite.tier == "kind"
                    || (!suite.ignored && suite.filter.is_empty() && suite.tests.is_empty()))
        });
        if owned {
            continue;
        }
        let mut command = runner.command("cargo");
        command.args([
            "test",
            "--locked",
            "--offline",
            "--no-fail-fast",
            "--manifest-path",
            WORKSPACES[target.workspace].1,
            "-p",
            &target.package,
        ]);
        add_target(&mut command, &target.target);
        if !target.features.is_empty() {
            command.args([
                "--features",
                &target
                    .features
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(","),
            ]);
        }
        if let Some(name) = &args.name {
            command.arg(name);
        }
        if cluster {
            command.args([
                "--",
                if tiers.len() == 1 {
                    "--ignored"
                } else {
                    "--include-ignored"
                },
                "--test-threads=1",
            ]);
        }
        let result = if cluster {
            runner.test(&mut command, None).await.map(|()| true)
        } else {
            runner
                .native_test(&mut command, target, &metadata[target.workspace], &suites)
                .await
        };
        match result {
            Ok(executed) => {
                *generic_results.entry(target.workspace).or_default() |= executed;
            }
            Err(error) => {
                failed.push(format!("{} {}: {error:#}", target.package, target.target));
                if cluster {
                    anyhow::bail!("{}", failed.join("\n"));
                }
            }
        }
    }
    for (workspace, executed) in generic_results {
        if !executed && matches!(args.mode, Mode::Run) {
            failed.push(format!(
                "{}: no executed passing cases",
                WORKSPACES[workspace].0
            ));
        }
    }
    let _nats = if required.contains("private-nats") {
        start_nats(&mut runner).await?
    } else {
        None
    };
    for suite in suites.iter().filter(|suite| suite.mode == "local") {
        if let Err(error) = runner.suite(suite, args.name.as_deref()).await {
            failed.push(format!("{}: {error:#}", suite.id));
            if suite.tier == "kind" {
                break;
            }
        }
    }
    ensure!(failed.is_empty(), "{}", failed.join("\n"));
    Ok(())
}

fn add_target(command: &mut Command, target: &str) {
    let (category, name) = target.split_once(':').expect("Cargo target selector");
    match category {
        "lib" | "cdylib" | "staticlib" | "rlib" | "dylib" => {
            command.arg("--lib");
        }
        _ => {
            command.args([&format!("--{category}"), name]);
        }
    }
}

struct Runner<'a> {
    root: &'a Path,
    mode: Mode,
    env: BTreeMap<String, String>,
}
impl Runner<'_> {
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.current_dir(self.root).envs(&self.env);
        command
    }
    fn value(&self, name: &str) -> Option<String> {
        self.env
            .get(name)
            .cloned()
            .or_else(|| std::env::var(name).ok())
            .filter(|value| !value.is_empty())
    }
    async fn expand(&self, value: &str) -> anyhow::Result<String> {
        let mut value = value
            .replace("{root}", &self.root.display().to_string())
            .replace("{target}", &self.env["CARGO_TARGET_DIR"]);
        if value.contains("{guest-rustflags}") {
            value = value.replace(
                "{guest-rustflags}",
                &crate::guest_rustflags(self.root).await?,
            );
        }
        Ok(value)
    }
    async fn commands(&self, commands: &[Vec<String>]) -> anyhow::Result<()> {
        for args in commands {
            let mut expanded = Vec::new();
            for arg in args {
                expanded.push(self.expand(arg).await?);
            }
            let mut command = self.command(expanded.first().context("empty input command")?);
            command.args(&expanded[1..]);
            tasks::step(&mut command, self.mode).await?;
        }
        Ok(())
    }
    fn cargo_suite(&self, suite: &Suite) -> Command {
        let mut command = self.command("cargo");
        let manifest = WORKSPACES
            .iter()
            .find(|(name, _)| *name == suite.workspace)
            .expect("suite workspace")
            .1;
        command.args([
            "test",
            "--locked",
            "--offline",
            "--no-fail-fast",
            "--manifest-path",
            manifest,
            "-p",
            &suite.package,
        ]);
        for target in &suite.target {
            add_target(&mut command, target);
        }
        if !suite.features.is_empty() {
            command.args(["--features", &suite.features.join(",")]);
        }
        command
    }
    async fn test(&self, command: &mut Command, expected: Option<&str>) -> anyhow::Result<()> {
        if matches!(self.mode, Mode::DryRun) {
            println!("{}", tasks::describe(command));
            return Ok(());
        }
        process::validate(&process::execute(command).await?, expected, false)
    }
    async fn suite(&self, suite: &Suite, filter: Option<&str>) -> anyhow::Result<()> {
        let lock = if suite.resource_lock.is_empty() || matches!(self.mode, Mode::DryRun) {
            None
        } else {
            let directory = Path::new(&self.env["CARGO_TARGET_DIR"]).join("test-locks");
            std::fs::create_dir_all(&directory)?;
            let file = std::fs::File::create(directory.join(&suite.resource_lock))?;
            file.lock()?;
            Some(file)
        };
        let result = match suite.execution.as_str() {
            "commands" => {
                let mut errors = Vec::new();
                for args in &suite.commands {
                    if let Err(error) = self.commands(&[args[1..].to_vec()]).await {
                        errors.push(format!("{error:#}"));
                    }
                }
                ensure!(errors.is_empty(), "{}", errors.join("\n"));
                Ok(())
            }
            "libtest" => {
                if suite.tests.is_empty() {
                    let mut command = self.cargo_suite(suite);
                    if !suite.filter.is_empty() {
                        command.arg(&suite.filter);
                    }
                    if suite.ignored {
                        command.args(["--", "--ignored", "--test-threads=1"]);
                    }
                    self.test(&mut command, None).await
                } else {
                    let mut errors = Vec::new();
                    for name in suite
                        .tests
                        .iter()
                        .filter(|name| filter.is_none_or(|filter| name.contains(filter)))
                    {
                        let mut command = self.cargo_suite(suite);
                        command
                            .arg(name)
                            .args(["--", "--exact", "--test-threads=1"]);
                        if suite.ignored {
                            command.arg("--ignored");
                        }
                        if let Err(error) = self.test(&mut command, Some(name)).await {
                            errors.push(format!("{error:#}"));
                            if suite.tier == "kind" {
                                break;
                            }
                        }
                    }
                    ensure!(errors.is_empty(), "{}", errors.join("\n"));
                    Ok(())
                }
            }
            _ => anyhow::bail!("invalid suite execution"),
        };
        drop(lock);
        result
    }
    async fn native_test(
        &self,
        command: &mut Command,
        target: &Selected,
        md: &cargo_metadata::Metadata,
        suites: &[Suite],
    ) -> anyhow::Result<bool> {
        command.arg("--message-format=json-render-diagnostics");
        if matches!(self.mode, Mode::DryRun) {
            println!("{}", tasks::describe(command));
            return Ok(false);
        }
        let text = process::execute(command).await?;
        let mut results = text.clone();
        // Libtest's ignore reason declares whether this runner supplies its resources.
        let artifacts =
            cargo_metadata::Message::parse_stream(text.as_bytes()).filter_map(|message| {
                match message {
                    Ok(cargo_metadata::Message::CompilerArtifact(artifact))
                        if artifact.profile.test
                            && target_name(&artifact.target) == target.target =>
                    {
                        Some(artifact)
                    }
                    _ => None,
                }
            });
        let ignored = regex::Regex::new(r"(?m)^test (.+) \.\.\. ignored, requires: (.+)$")?;
        for artifact in artifacts {
            let Some(executable) = artifact.executable.as_deref() else {
                continue;
            };
            let manifest = artifact.manifest_path.as_str();
            let Some(package) = md.packages.iter().find(|package| {
                package.manifest_path.as_str() == manifest
                    && package.name.as_str() == target.package
            }) else {
                continue;
            };
            for row in ignored.captures_iter(&text) {
                let name = &row[1];
                let owned = suites.iter().any(|suite| {
                    suite.ignored
                        && suite.package == target.package
                        && (suite.target.is_empty() || suite.target.contains(&target.target))
                        && (suite.tests.iter().any(|test| test == name)
                            || (suite.tests.is_empty() && name.contains(&suite.filter)))
                });
                if owned || !row[2].split(", ").all(|item| supplied(self, item)) {
                    continue;
                }
                let mut ignored_command = self.command(executable.as_str());
                ignored_command
                    .current_dir(package.manifest_path.parent().expect("package directory"))
                    .env(
                        "CARGO_MANIFEST_DIR",
                        package.manifest_path.parent().expect("package directory"),
                    )
                    .env("CARGO_MANIFEST_PATH", &package.manifest_path)
                    .args([name, "--ignored", "--exact", "--test-threads=1"]);
                let result = process::execute(&mut ignored_command).await?;
                process::validate(&result, Some(name), false)?;
                results.push_str(&result);
            }
        }
        // Empty libraries may own only ignored tests. Require a result across all executions.
        Ok(process::validate(&results, None, false).is_ok())
    }
}

fn supplied(runner: &Runner<'_>, requirement: &str) -> bool {
    if let Some(name) = requirement.strip_prefix("env:") {
        return runner.value(name).is_some();
    }
    if let Some(name) = requirement.strip_prefix("bin:") {
        return runner.value("PATH").is_some_and(|path| {
            use std::os::unix::fs::PermissionsExt as _;
            std::env::split_paths(&path).any(|directory| {
                std::fs::metadata(directory.join(name)).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
        });
    }
    false
}

async fn prepare(
    runner: &mut Runner<'_>,
    policy: &Policy,
    targets: &[Selected],
    suites: &[Suite],
) -> anyhow::Result<BTreeSet<String>> {
    let mut required: BTreeSet<_> = suites
        .iter()
        .flat_map(|suite| suite.prerequisites.iter().cloned())
        .collect();
    for prep in &policy.preparation {
        if prep.id.is_none()
            && (targets.iter().any(|target| {
                target.workspace == 0
                    && prep.packages.contains(&target.package)
                    && (prep.target.is_empty() || prep.target.contains(&target.target))
            }) || suites.iter().any(|suite| {
                prep.suites.contains(&suite.id)
                    || (suite.tests.is_empty()
                        && suite.filter.is_empty()
                        && prep.packages.contains(&suite.package)
                        && (prep.target.is_empty()
                            || suite.target.is_empty()
                            || prep
                                .target
                                .iter()
                                .any(|target| suite.target.contains(target))))
            }))
        {
            required.extend(prep.prerequisites.iter().cloned());
        }
    }
    loop {
        let old = required.len();
        for prep in &policy.preparation {
            if prep.id.as_ref().is_some_and(|id| required.contains(id)) && needs(runner, prep) {
                required.extend(prep.prerequisites.iter().cloned());
            }
        }
        if old == required.len() {
            break;
        }
    }
    let mut applications = Vec::new();
    let mut seen = BTreeSet::new();
    if required.contains("fixture") {
        for app in ["apps/platform_fixture", "apps/platform_fixture_overlay"] {
            application(runner.root, app, &mut seen, &mut applications)?;
        }
    }
    let mut deferred = Vec::new();
    for prep in &policy.preparation {
        let Some(id) = prep.id.as_deref().filter(|id| required.contains(*id)) else {
            continue;
        };
        if !needs(runner, prep) {
            continue;
        }
        if id == "nats" {
            let binary = runner
                .value("WAMN_NATIVE_C_NATS_BIN")
                .unwrap_or_else(|| "nats-server".into());
            runner.env.insert("WAMN_NATIVE_C_NATS_BIN".into(), binary);
            continue;
        }
        if id == "authored-applications" {
            let files =
                process::output(runner.root, "rg", &["--files", "apps", "-g", "wamn.k"]).await?;
            for file in files.lines() {
                application(
                    runner.root,
                    file.trim_end_matches("/wamn.k"),
                    &mut seen,
                    &mut applications,
                )?;
            }
        }
        for app in &prep.applications {
            application(runner.root, app, &mut seen, &mut applications)?;
        }
        runner.commands(&prep.before_applications).await?;
        if prep.after_applications {
            deferred.push(prep);
        } else {
            runner.commands(&prep.commands).await?;
        }
        for (name, value) in &prep.outputs {
            if runner.value(name).is_none() {
                let value = runner.expand(value).await?;
                runner.env.insert(name.clone(), value);
            }
        }
    }
    for prep in &policy.preparation {
        if !prep.id.as_ref().is_some_and(|id| required.contains(id)) {
            continue;
        }
        for (name, source) in &prep.aliases {
            if runner.value(name).is_none()
                && let Some(value) = runner.value(source)
            {
                runner.env.insert(name.clone(), value);
            }
        }
    }
    if !applications.is_empty() {
        runner
            .commands(&[vec![
                "cargo",
                "build",
                "--locked",
                "--offline",
                "-p",
                "wamn-ctl",
                "--bin",
                "wamn",
                "-p",
                "wamn-test-infrastructure",
                "--bins",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()])
            .await?;
        let bin = PathBuf::from(&runner.env["CARGO_TARGET_DIR"]).join("debug");
        let mut command = runner.command(
            bin.join("wamn-test-postgres")
                .to_str()
                .context("test postgres path")?,
        );
        command
            .args([
                "--database",
                "prekind_build",
                "--url-env",
                "DATABASE_URL",
                "--",
            ])
            .arg(bin.join("wamn"))
            .arg("build")
            .args(&applications);
        tasks::step(&mut command, runner.mode).await?;
        for app in &applications {
            if runner.root.join(app).join("web/package.json").is_file() {
                tasks::step(
                    runner
                        .command(bin.join("wamn").to_str().context("wamn path")?)
                        .args(["web", "build", "--package", app]),
                    runner.mode,
                )
                .await?;
            }
        }
    }
    for prep in deferred {
        runner.commands(&prep.commands).await?;
    }
    Ok(required)
}

fn needs(runner: &Runner<'_>, prep: &Preparation) -> bool {
    prep.outputs.is_empty() || prep.outputs.keys().any(|name| runner.value(name).is_none())
}
fn application(
    root: &Path,
    app: &str,
    seen: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) -> anyhow::Result<()> {
    if !seen.insert(app.to_owned()) {
        return Ok(());
    }
    let source = std::fs::read_to_string(root.join(app).join("wamn.k"))?;
    let authored = std::process::Command::new("rg")
        .current_dir(root)
        .args(["--files", "apps", "-g", "wamn.k"])
        .output()
        .context("find authored packages")?;
    ensure!(
        authored.status.success(),
        "find authored packages: {}",
        String::from_utf8_lossy(&authored.stderr)
    );
    let authored = String::from_utf8(authored.stdout).context("authored package paths")?;
    let dependency = regex::Regex::new(r#"(?m)^\s*package = "([^" ]+)""#)?;
    for capture in dependency.captures_iter(&source) {
        if let Some(path) = authored
            .lines()
            .map(Path::new)
            .filter_map(Path::parent)
            .find(|path| path.file_name().is_some_and(|name| name == &capture[1]))
        {
            application(
                root,
                path.to_str().context("authored package path")?,
                seen,
                ordered,
            )?;
        }
    }
    ordered.push(app.to_owned());
    Ok(())
}

struct Nats {
    child: tokio::process::Child,
    _directory: tempfile::TempDir,
}
impl Drop for Nats {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
async fn start_nats(runner: &mut Runner<'_>) -> anyhow::Result<Option<Nats>> {
    if matches!(runner.mode, Mode::DryRun) {
        println!("input jetstream: private nats-server -js -a 127.0.0.1 -p -1");
        return Ok(None);
    }
    let directory = tempfile::tempdir()?;
    let log = directory.path().join("server.log");
    let mut command = runner.command(
        &runner
            .value("WAMN_NATIVE_C_NATS_BIN")
            .context("prebuilt nats-server executable")?,
    );
    command
        .args(["-js", "-a", "127.0.0.1", "-p", "-1", "-sd"])
        .arg(directory.path().join("data"))
        .stdout(std::fs::File::create(&log)?)
        .stderr(std::fs::File::options().append(true).open(&log)?)
        .kill_on_drop(true);
    let mut owned = Nats {
        child: command.spawn()?,
        _directory: directory,
    };
    let port = regex::Regex::new(r"Listening for client connections on 127\.0\.0\.1:(\d+)")?;
    for _ in 0..100 {
        let text = std::fs::read_to_string(&log)?;
        if text.contains("Server is ready") {
            let port = port.captures(&text).context("NATS reports no port")?[1].to_owned();
            runner.env.insert(
                "WAMN_EVT_NATS_URL".into(),
                format!("nats://127.0.0.1:{port}"),
            );
            return Ok(Some(owned));
        }
        ensure!(
            owned.child.try_wait()?.is_none(),
            "private NATS exited: {text}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    anyhow::bail!("private NATS did not become ready")
}

fn shared_comments(source: &str) -> Vec<&str> {
    let mut inside_suites = false;
    let mut braces = 0_i32;
    let mut comments = Vec::new();
    for line in source.lines() {
        if line.starts_with("[[") {
            inside_suites = line == "[[suites]]";
        }
        if !inside_suites && braces == 0 && line.trim_start().starts_with('#') {
            comments.push(line);
        }
        if !line.trim_start().starts_with('#') {
            braces += i32::try_from(line.bytes().filter(|b| *b == b'{').count())
                .expect("line length fits i32");
            braces -= i32::try_from(line.bytes().filter(|b| *b == b'}').count())
                .expect("line length fits i32");
        }
    }
    comments
}

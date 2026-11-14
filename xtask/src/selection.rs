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

#[derive(Debug, Default)]
struct Case {
    needs: BTreeSet<(String, String)>,
    integration: bool,
    ignored: bool,
    database: bool,
}

impl Case {
    fn tier(&self) -> &'static str {
        wamn_test_postgres::test_tier(
            self.needs
                .iter()
                .map(|(type_, name)| (type_.as_str(), name.as_str())),
            self.integration,
        )
    }
}

fn attribute_text(attribute: &syn::Attribute) -> Option<String> {
    let syn::Meta::NameValue(value) = &attribute.meta else {
        return None;
    };
    let syn::Expr::Lit(syn::ExprLit {
        lit: syn::Lit::Str(value),
        ..
    }) = &value.value
    else {
        return None;
    };
    Some(value.value())
}

// Follow the modules of this Cargo target, including shared test modules.
fn cases(source: &Path, integration: bool) -> anyhow::Result<BTreeMap<String, Case>> {
    fn items(
        source: &Path,
        directory: &Path,
        explicit_directory: &Path,
        prefix: &str,
        syntax: Vec<syn::Item>,
        integration: bool,
        cases: &mut BTreeMap<String, Case>,
    ) -> anyhow::Result<()> {
        for item in syntax {
            match item {
                syn::Item::Fn(function)
                    if function.attrs.iter().any(|attr| {
                        attr.path()
                            .segments
                            .last()
                            .is_some_and(|segment| segment.ident == "test")
                    }) =>
                {
                    let mut case = Case {
                        integration,
                        ..Case::default()
                    };
                    for attribute in &function.attrs {
                        if attribute.path().is_ident("ignore") {
                            case.ignored = true;
                            let reason = attribute_text(attribute)
                                .context("ignored test must declare its needs")?;
                            case.needs.extend(
                                wamn_test_postgres::parse_prerequisites(&reason)
                                    .map_err(anyhow::Error::msg)?
                                    .into_iter()
                                    .map(|(type_, name)| (type_.to_owned(), name.to_owned())),
                            );
                        }
                        if attribute
                            .path()
                            .segments
                            .first()
                            .is_some_and(|segment| segment.ident == "sqlx")
                        {
                            case.database = true;
                            case.needs.insert(("service".into(), "postgres".into()));
                        }
                    }
                    cases.insert(format!("{prefix}{}", function.sig.ident), case);
                }
                syn::Item::Mod(module) => {
                    let prefix = format!("{prefix}{}::", module.ident);
                    if let Some((_, content)) = module.content {
                        items(
                            source,
                            &directory.join(module.ident.to_string()),
                            &directory.join(module.ident.to_string()),
                            &prefix,
                            content,
                            integration,
                            cases,
                        )?;
                    } else {
                        let explicit = module
                            .attrs
                            .iter()
                            .find(|attr| attr.path().is_ident("path"))
                            .and_then(attribute_text);
                        let path = if let Some(path) = explicit {
                            explicit_directory.join(path)
                        } else {
                            let path = directory.join(format!("{}.rs", module.ident));
                            if path.is_file() {
                                path
                            } else {
                                directory.join(module.ident.to_string()).join("mod.rs")
                            }
                        };
                        if path.is_file() {
                            let directory = if path.file_name().is_some_and(|name| name == "mod.rs")
                            {
                                path.parent().context("module parent")?.to_owned()
                            } else {
                                path.with_extension("")
                            };
                            items(
                                &path,
                                &directory,
                                path.parent().context("module parent")?,
                                &prefix,
                                syn::parse_file(&std::fs::read_to_string(&path)?)
                                    .with_context(|| format!("parse {}", path.display()))?
                                    .items,
                                integration,
                                cases,
                            )?;
                        }
                    }
                }
                syn::Item::Macro(item) if item.mac.path.is_ident("include") => {
                    if let Ok(path) = syn::parse2::<syn::LitStr>(item.mac.tokens) {
                        let path = source.parent().context("source parent")?.join(path.value());
                        if !path.is_file() {
                            continue;
                        }
                        items(
                            &path,
                            path.parent().context("include parent")?,
                            path.parent().context("include parent")?,
                            prefix,
                            syn::parse_file(&std::fs::read_to_string(&path)?)?.items,
                            integration,
                            cases,
                        )?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
    let mut cases = BTreeMap::new();
    items(
        source,
        source.parent().context("target source parent")?,
        source.parent().context("target source parent")?,
        "",
        syn::parse_file(&std::fs::read_to_string(source)?)?.items,
        integration,
        &mut cases,
    )?;
    Ok(cases)
}

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
    documentation: Vec<String>,
    rules: Vec<Rule>,
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
    cases: BTreeMap<String, Case>,
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
    let mut tiers: Vec<_> = if args.tier.is_empty() {
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
    if !args.suite.is_empty() && args.tier.is_empty() {
        tiers = policy
            .suites
            .iter()
            .filter(|suite| args.suite.contains(&suite.id))
            .map(|suite| suite.tier.as_str())
            .collect();
    }
    let cluster = tiers.contains(&"kind");
    ensure!(
        args.name.is_none() || cluster,
        "--name requires --tier kind"
    );
    let mut paths = Vec::new();
    for path in changed.split('\0').filter(|path| !path.is_empty()) {
        if !matches(&policy.documentation, path)? {
            paths.push(path);
        }
    }
    let policy_changed = paths.contains(&"tests/selection.toml");
    paths.sort_unstable();
    paths.dedup();
    println!("working-directory: {}\nbase: {base}", root.display());
    if paths.is_empty() && args.suite.is_empty() {
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
            if !(unknown || full.contains(&index) || dependents.contains(&package.id)) {
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
            for target in package
                .targets
                .iter()
                .filter(|target| target.test && !target.is_custom_build())
            {
                let name = target_name(target);
                let cases = cases(target.src_path.as_std_path(), target.is_test())?;
                let eligible = if cases.is_empty() {
                    tiers.contains(&wamn_test_postgres::test_tier([], target.is_test()))
                } else {
                    cases.values().any(|case| tiers.contains(&case.tier()))
                };
                if !eligible {
                    continue;
                }
                selected.push(Selected {
                    workspace: index,
                    package: package.name.to_string(),
                    target: name,
                    features: features.clone(),
                    cases,
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
                    || policy_changed
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
        if include && (args.name.is_none() || suite.execution == "libtest") {
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
    let mut runner = Runner {
        root,
        mode: args.mode,
        env: BTreeMap::new(),
        skipped_missing: false,
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
    prepare(
        &mut runner,
        &policy,
        &selected,
        &suites,
        &metadata,
        &CaseSelection {
            suites: &suites,
            tiers: &tiers,
            name: args.name.as_deref(),
            only: None,
            explicit: !args.suite.is_empty(),
        },
    )
    .await?;
    let mut failed = Vec::new();
    let mut executed = false;
    for suite in suites.iter().filter(|suite| suite.mode == "always") {
        match runner
            .suite(
                suite,
                args.name.as_deref(),
                &metadata,
                &tiers,
                !args.suite.is_empty(),
            )
            .await
        {
            Ok(ran) => executed |= ran,
            Err(error) => {
                failed.push(format!("{}: {error:#}", suite.id));
                if suite.tier == "kind" {
                    anyhow::bail!("{}", failed.join("\n"));
                }
            }
        }
    }
    for target in &selected {
        let mut command = runner.command("cargo");
        command.args([
            "test",
            "--locked",
            "--offline",
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
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(","),
            ]);
        }
        match runner
            .native_test(
                &mut command,
                target,
                &metadata[target.workspace],
                &CaseSelection {
                    suites: &suites,
                    tiers: &tiers,
                    name: args.name.as_deref(),
                    only: None,
                    explicit: false,
                },
            )
            .await
        {
            Ok(ran) => executed |= ran,
            Err(error) => {
                failed.push(format!("{} {}: {error:#}", target.package, target.target));
                if cluster {
                    anyhow::bail!("{}", failed.join("\n"));
                }
            }
        }
    }
    for suite in suites.iter().filter(|suite| suite.mode == "local") {
        match runner
            .suite(
                suite,
                args.name.as_deref(),
                &metadata,
                &tiers,
                !args.suite.is_empty(),
            )
            .await
        {
            Ok(ran) => executed |= ran,
            Err(error) => {
                failed.push(format!("{}: {error:#}", suite.id));
                if suite.tier == "kind" {
                    break;
                }
            }
        }
    }
    if matches!(args.mode, Mode::Run) && !executed && failed.is_empty() {
        if let Some(name) = &args.name {
            failed.push(if runner.skipped_missing {
                format!("tests matching name filter {name:?} were skipped for missing inputs")
            } else {
                format!("no tests match name filter {name:?}")
            });
        } else if !selected.is_empty() {
            failed.push("no executed test cases for selection".into());
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
    skipped_missing: bool,
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
    async fn suite(
        &mut self,
        suite: &Suite,
        name: Option<&str>,
        metadata: &[cargo_metadata::Metadata],
        tiers: &[&str],
        explicit: bool,
    ) -> anyhow::Result<bool> {
        let mut executed = false;
        let lock = if suite.resource_lock.is_empty() || matches!(self.mode, Mode::DryRun) {
            None
        } else {
            let directory = Path::new(&self.env["CARGO_TARGET_DIR"]).join("test-locks");
            std::fs::create_dir_all(&directory)?;
            let file = std::fs::File::create(directory.join(&suite.resource_lock))?;
            file.lock()?;
            Some(file)
        };
        let mut errors = Vec::new();
        if suite.execution == "commands" {
            for args in &suite.commands {
                if let Err(error) = self.commands(&[args[1..].to_vec()]).await {
                    errors.push(format!("{error:#}"));
                }
            }
        } else {
            let workspace = WORKSPACES
                .iter()
                .position(|(name, _)| *name == suite.workspace)
                .context("suite workspace")?;
            let md = &metadata[workspace];
            let package = md
                .workspace_packages()
                .into_iter()
                .find(|package| package.name.as_str() == suite.package)
                .context("suite package")?;
            for target in package.targets.iter().filter(|target| {
                target.test
                    && !target.is_custom_build()
                    && (suite.target.is_empty() || suite.target.contains(&target_name(target)))
            }) {
                let selected = Selected {
                    workspace,
                    package: suite.package.clone(),
                    target: target_name(target),
                    features: BTreeSet::new(),
                    cases: cases(target.src_path.as_std_path(), target.is_test())?,
                };
                let mut command = self.cargo_suite(suite);
                if !target.required_features.is_empty() {
                    command.args(["--features", &target.required_features.join(",")]);
                }
                // Each executable is discovered and run separately.
                if suite.target.is_empty() {
                    add_target(&mut command, &selected.target);
                }
                let selection = CaseSelection {
                    suites: &[],
                    tiers,
                    name,
                    only: Some(suite),
                    explicit,
                };
                match self
                    .native_test(&mut command, &selected, md, &selection)
                    .await
                {
                    Ok(ran) => executed |= ran,
                    Err(error) => {
                        errors.push(format!("{error:#}"));
                        if suite.tier == "kind" {
                            break;
                        }
                    }
                }
            }
            if explicit && !executed && matches!(self.mode, Mode::Run) && errors.is_empty() {
                errors.push(format!("suite {}: no selected passing cases", suite.id));
            }
        }
        drop(lock);
        ensure!(errors.is_empty(), "{}", errors.join("\n"));
        Ok(executed)
    }

    async fn native_test(
        &mut self,
        command: &mut Command,
        target: &Selected,
        md: &cargo_metadata::Metadata,
        selection: &CaseSelection<'_>,
    ) -> anyhow::Result<bool> {
        if selection.only.is_none_or(|suite| suite.tests.is_empty())
            && !target.cases.is_empty()
            && !target
                .cases
                .iter()
                .any(|(name, case)| selection.includes(target, name, case))
        {
            return Ok(false);
        }
        command.args(["--no-run", "--message-format=json-render-diagnostics"]);
        if matches!(self.mode, Mode::DryRun) {
            println!("{}", tasks::describe(command));
            for (name, case) in &target.cases {
                if selection.includes(target, name, case) {
                    if !self.available(target, name, case, selection)? {
                        self.skipped_missing = true;
                        continue;
                    }
                    println!(
                        "  {} {name}{}",
                        case.tier(),
                        if case.database {
                            " (private PostgreSQL)"
                        } else {
                            ""
                        }
                    );
                }
            }
            return Ok(false);
        }
        let text = process::execute(command).await?;
        let mut executed = false;
        let mut skipped_missing = false;
        let mut failures = Vec::new();
        for message in cargo_metadata::Message::parse_stream(text.as_bytes()) {
            let Ok(cargo_metadata::Message::CompilerArtifact(artifact)) = message else {
                continue;
            };
            if !artifact.profile.test || target_name(&artifact.target) != target.target {
                continue;
            }
            let Some(executable) = artifact.executable.as_deref() else {
                continue;
            };
            let Some(package) = md.packages.iter().find(|package| {
                package.manifest_path == artifact.manifest_path
                    && package.name.as_str() == target.package
            }) else {
                continue;
            };
            let directory = package
                .manifest_path
                .parent()
                .context("package directory")?;
            let binary = |database| {
                let mut command = if database {
                    let mut command = self.command(&format!(
                        "{}/debug/wamn-test-postgres",
                        self.env["CARGO_TARGET_DIR"]
                    ));
                    command.args(["--database", "selection", "--url-env", "DATABASE_URL", "--"]);
                    command
                } else {
                    self.command("env")
                };
                if let Ok(runner) = std::env::var("CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER") {
                    command.args(runner.split_whitespace());
                }
                command
                    .arg(executable.as_str())
                    .current_dir(directory)
                    .env("CARGO_MANIFEST_DIR", directory)
                    .env("CARGO_MANIFEST_PATH", &package.manifest_path);
                command
            };
            let listed =
                process::execute(binary(false).args(["--list", "--format", "terse"])).await?;
            let ignored =
                process::execute(binary(false).args(["--list", "--ignored", "--format", "terse"]))
                    .await?;
            let ignored: BTreeSet<_> = ignored
                .lines()
                .filter_map(|line| line.strip_suffix(": test"))
                .collect();
            let names: Vec<_> = listed
                .lines()
                .filter_map(|line| line.strip_suffix(": test"))
                .collect();
            if let Some(suite) = selection.only {
                for name in &suite.tests {
                    ensure!(
                        names.contains(&name.as_str()),
                        "suite {}: missing exact test {name}",
                        suite.id
                    );
                }
            }
            let default = Case {
                integration: artifact.target.is_test(),
                ..Case::default()
            };
            let mut skip = Vec::new();
            let mut database = false;
            let mut run = Vec::new();
            for name in names {
                ensure!(
                    !ignored.contains(name) || target.cases.contains_key(name),
                    "{name}: ignored test has no readable needs declaration"
                );
                let case = target.cases.get(name).unwrap_or(&default);
                if !selection.includes(target, name, case) {
                    skip.push(name);
                } else if !self.available(target, name, case, selection)? {
                    skipped_missing = true;
                    skip.push(name);
                } else {
                    database |= case.database;
                    run.push(name);
                }
            }
            if run.is_empty() {
                continue;
            }
            if selection.tiers.contains(&"kind") {
                for name in run {
                    let database = target.cases.get(name).is_some_and(|case| case.database);
                    let output = process::execute(binary(database).args([
                        name,
                        "--include-ignored",
                        "--exact",
                        "--test-threads=1",
                    ]))
                    .await?;
                    process::validate(&output, Some(name), false)?;
                    executed = true;
                }
                continue;
            }
            let mut command = binary(database);
            command.args(["--include-ignored", "--exact", "--test-threads=1"]);
            for name in skip {
                command.args(["--skip", name]);
            }
            let result = match process::execute(&mut command).await {
                Ok(output) => process::validate(&output, None, false),
                Err(error) => Err(error),
            };
            match result {
                Ok(()) => executed = true,
                Err(error) => failures.push(format!("{error:#}")),
            }
        }
        self.skipped_missing |= skipped_missing;
        ensure!(failures.is_empty(), "{}", failures.join("\n"));
        Ok(executed)
    }

    fn available(
        &self,
        target: &Selected,
        name: &str,
        case: &Case,
        selection: &CaseSelection<'_>,
    ) -> anyhow::Result<bool> {
        let missing = case
            .needs
            .iter()
            .filter(|(type_, name)| !supplied(self, type_, name))
            .map(|(type_, name)| format!("{type_}:{name}"))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(true);
        }
        let missing = missing.join(", ");
        ensure!(
            case.ignored && !selection.explicit,
            "{} {name}: missing {missing}",
            target.package
        );
        println!("skipped {} {name}: missing {missing}", target.package);
        Ok(false)
    }
}

struct CaseSelection<'a> {
    suites: &'a [Suite],
    tiers: &'a [&'a str],
    name: Option<&'a str>,
    only: Option<&'a Suite>,
    explicit: bool,
}
impl CaseSelection<'_> {
    fn includes(&self, target: &Selected, name: &str, case: &Case) -> bool {
        let matches = |suite: &Suite| {
            case.tier() == suite.tier
                && (suite.tests.is_empty() || suite.tests.iter().any(|test| test == name))
                && (suite.filter.is_empty() || name.contains(&suite.filter))
        };
        self.tiers.contains(&case.tier())
            && self.name.is_none_or(|filter| name.contains(filter))
            && if let Some(suite) = self.only {
                matches(suite)
            } else {
                !self.suites.iter().any(|suite| {
                    suite.execution == "libtest"
                        && suite.workspace == WORKSPACES[target.workspace].0
                        && suite.package == target.package
                        && (suite.target.is_empty() || suite.target.contains(&target.target))
                        && matches(suite)
                })
            }
    }
}

fn supplied(runner: &Runner<'_>, type_: &str, name: &str) -> bool {
    if matches!(type_, "service" | "input") {
        return true;
    }
    if type_ == "env" {
        return runner.value(name).is_some();
    }
    if type_ == "bin" {
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
    metadata: &[cargo_metadata::Metadata],
    selection: &CaseSelection<'_>,
) -> anyhow::Result<()> {
    let mut required: BTreeSet<_> = suites
        .iter()
        .filter(|suite| suite.execution == "commands" && selection.name.is_none())
        .flat_map(|suite| suite.prerequisites.iter().cloned())
        .collect();
    let mut inputs = BTreeSet::new();
    let mut postgres = false;
    let mut collect = |case: &Case| {
        postgres |= case.database;
        for (type_, name) in &case.needs {
            if type_ == "env" {
                inputs.insert(name.clone());
            }
            if type_ == "input" {
                required.insert(name.clone());
            }
        }
    };
    for target in targets {
        for (name, case) in &target.cases {
            if selection.includes(target, name, case) {
                collect(case);
            }
        }
    }
    for suite in suites.iter().filter(|suite| suite.execution == "libtest") {
        let workspace = WORKSPACES
            .iter()
            .position(|(name, _)| *name == suite.workspace)
            .context("suite workspace")?;
        let md = &metadata[workspace];
        let package = md
            .workspace_packages()
            .into_iter()
            .find(|package| package.name.as_str() == suite.package)
            .context("suite package")?;
        let suite_selection = CaseSelection {
            suites: &[],
            tiers: selection.tiers,
            name: selection.name,
            only: Some(suite),
            explicit: selection.explicit,
        };
        for target in package.targets.iter().filter(|target| {
            target.test
                && !target.is_custom_build()
                && (suite.target.is_empty() || suite.target.contains(&target_name(target)))
        }) {
            let target = Selected {
                workspace,
                package: suite.package.clone(),
                target: target_name(target),
                features: BTreeSet::new(),
                cases: cases(target.src_path.as_std_path(), target.is_test())?,
            };
            for (name, case) in &target.cases {
                if suite_selection.includes(&target, name, case) {
                    collect(case);
                }
            }
        }
    }
    if postgres {
        runner
            .commands(&[vec![
                "cargo",
                "build",
                "--locked",
                "--offline",
                "-p",
                "wamn-test-postgres",
                "--bin",
                "wamn-test-postgres",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()])
            .await?;
    }
    for prep in &policy.preparation {
        if let Some(id) = &prep.id
            && prep
                .outputs
                .keys()
                .chain(prep.aliases.keys())
                .any(|name| inputs.contains(name))
        {
            required.insert(id.clone());
        }
    }
    ensure!(
        required.iter().all(|id| policy
            .preparation
            .iter()
            .any(|prep| prep.id.as_ref() == Some(id))),
        "selected case names an unknown preparation input"
    );
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
        for app in &prep.applications {
            application(runner.root, app, &mut seen, &mut applications)?;
        }
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
                "-p",
                "wamn-test-postgres",
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
    Ok(())
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

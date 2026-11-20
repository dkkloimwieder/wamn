//! Repository tasks, platform publication, and benchmark commands.

mod bench;
mod bench_cloud;
mod bench_data;
mod kind;
mod netsuite;
mod platform;
mod process;
mod repo_policy;
mod submodule_pins;
mod tasks;

use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};

/// Repository task arguments.
#[derive(Debug, Parser)]
#[command(name = "cargo xtask")]
pub struct Args {
    #[command(subcommand)]
    command: Task,
}

#[derive(Debug, Subcommand)]
enum Task {
    /// Own and reap the descendants of one task command.
    #[command(hide = true)]
    Supervise {
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<std::ffi::OsString>,
    },
    /// Refuse undeclared or backward submodule pins.
    SubmodulePins {
        #[arg(default_value = "main")]
        main_ref: String,
    },
    /// Run the two NetSuite operator workflows with private credentials.
    NetsuiteLive(netsuite::Args),
    /// Install the pinned KCL CLI.
    InstallKcl,
    /// Build complete package artifacts.
    BuildPackage {
        #[arg(required = true)]
        packages: Vec<PathBuf>,
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: Option<String>,
        /// Recreate the committed SQL describe evidence from PostgreSQL.
        #[arg(long, requires = "database_url", conflicts_with = "check_describe")]
        refresh_describe: bool,
        /// Compare committed SQL describe evidence with PostgreSQL.
        #[arg(long, requires = "database_url")]
        check_describe: bool,
    },
    /// Build one component through the shared guest compiler.
    BuildComponent { manifest: PathBuf },
    /// Print guest compiler flags.
    GuestRustflags,
    /// Build, push, and stamp one platform chart.
    BuildPlatform(platform::Args),
    /// Write the two platform UI OpenAPI documents.
    PlatformUiOpenapi,
    /// Run the deployed application benchmark.
    Bench(bench::Args),
    /// Measure the deployed benchmark tier.
    BenchTier(bench_cloud::TierArgs),
    /// Measure PostgreSQL replays on the selected cloud deployment.
    BenchPgMinimum(bench_cloud::PgArgs),
    /// Summarize retained benchmark results.
    BenchResult {
        work: PathBuf,
        output: PathBuf,
        concurrency: usize,
    },
    /// Summarize PostgreSQL replay results.
    BenchPgResult {
        work: PathBuf,
        output: PathBuf,
        concurrency: usize,
    },
    /// Subtract PostgreSQL samples from one measured interval.
    BenchDatabaseDelta { work: PathBuf, call: String },
    /// Summarize perf call stacks.
    BenchProfileSummary { source: PathBuf, output: PathBuf },
    /// Restore complete names in guest call stacks.
    BenchNameGuests { artifacts: PathBuf, stacks: PathBuf },
    /// Summarize sampled application traces.
    BenchAppResult {
        work: PathBuf,
        output: PathBuf,
        http: String,
        #[arg(action = clap::ArgAction::Set)]
        edge: bool,
        scale: usize,
    },
    /// Require a passing, nonempty libtest result.
    RequireTestResult(process::TestArgs),
    /// Run repository formatting, Clippy, and policy checks.
    RepoLint(tasks::RunArgs),
    /// Generate or check the authored environment JSON Schema.
    EnvironmentSchema {
        #[arg(long)]
        check: bool,
    },
    /// Check dependency boundaries.
    RepoLintDependencies,
    /// Compile each package without other members' features.
    CompilePackagesAlone { manifest: Option<PathBuf> },
    /// Run the HTTP component contract tests.
    ContractDiff(tasks::RunArgs),
    /// Run the upstream release acceptance tests.
    WasmcloudReleaseTests(tasks::UpstreamArgs),
    /// Render the local collector from the deployment configuration.
    DevCollector { root: PathBuf },
    /// Create or reuse the local kind cluster and its OCI registry.
    KindClusterCreate,
    /// Delete a disposable kind cluster and inspect its resources.
    KindClusterDelete { cluster: String },
    /// Print the environment for an isolated checkout.
    LaneStart {
        #[arg(long)]
        print_env: bool,
        worktree: PathBuf,
    },
    /// Build native debug programs with Cranelift.
    Cranelift {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
}

/// Execute one repository task.
pub async fn run(args: Args) -> anyhow::Result<()> {
    let root = root();
    match args.command {
        Task::Supervise { command } => std::process::exit(process::supervise(&command).await?),
        Task::SubmodulePins { main_ref } => submodule_pins::run(&root, &main_ref),
        Task::NetsuiteLive(args) => netsuite::run(&args).await,
        Task::InstallKcl => {
            println!("{}", wamn_build::kcl::install(&root).await?.display());
            Ok(())
        }
        Task::BuildPackage {
            packages,
            database_url,
            refresh_describe,
            check_describe,
        } => {
            let describe = wamn_build::DescribeMode::from_options(
                database_url.as_deref(),
                refresh_describe,
                check_describe,
            )?;
            for package in packages {
                let output = wamn_build::default_output_root(&package);
                let built = wamn_build::build_package(&wamn_build::BuildRequest::new(
                    &package, &output, describe,
                ))
                .await?;
                println!(
                    "{} {} {}",
                    built.package.id, built.package.version, built.manifest_digest
                );
            }
            Ok(())
        }
        Task::BuildComponent { manifest } => {
            println!(
                "{}",
                wamn_build::build_component(&root, &manifest)
                    .await?
                    .display()
            );
            Ok(())
        }
        Task::GuestRustflags => {
            println!("{}", guest_rustflags(&root).await?);
            Ok(())
        }
        Task::BuildPlatform(args) => platform::run(args).await,
        Task::PlatformUiOpenapi => platform::write_ui_openapi(&root),
        Task::Bench(args) => bench::run(args).await,
        Task::BenchTier(args) => bench_cloud::tier(&root, &args).await,
        Task::BenchPgMinimum(args) => bench_cloud::pg(&root, &args).await,
        Task::BenchResult {
            work,
            output,
            concurrency,
        } => bench_data::result(&work, &output, concurrency),
        Task::BenchPgResult {
            work,
            output,
            concurrency,
        } => bench_data::pg_result(&work, &output, concurrency),
        Task::BenchDatabaseDelta { work, call } => bench_data::database_delta(&work, &call),
        Task::BenchProfileSummary { source, output } => {
            bench_data::profile_summary(&source, &output)
        }
        Task::BenchNameGuests { artifacts, stacks } => {
            bench_data::name_guests(&root, &artifacts, &stacks).await
        }
        Task::BenchAppResult {
            work,
            output,
            http,
            edge,
            scale,
        } => bench_data::app_result(&work, &output, &http, edge, scale),
        Task::RequireTestResult(args) => process::require(&std::env::current_dir()?, &args).await,
        Task::RepoLint(args) => tasks::lint(&root, args.mode).await,
        Task::EnvironmentSchema { check } => repo_policy::environment_schema(&root, check),
        Task::RepoLintDependencies => tasks::dependencies(&root).await,
        Task::CompilePackagesAlone { manifest } => {
            tasks::packages(&root, manifest.as_deref()).await
        }
        Task::ContractDiff(args) => tasks::contract(&root, args.mode).await,
        Task::WasmcloudReleaseTests(args) => tasks::upstream(&root, &args).await,
        Task::DevCollector { root: output } => tasks::collector(&root, &output),
        Task::KindClusterCreate => kind::create_cluster(&root).await,
        Task::KindClusterDelete { cluster } => tasks::delete_cluster(&root, &cluster).await,
        Task::LaneStart {
            print_env,
            worktree,
        } => tasks::lane(&worktree, print_env).await,
        Task::Cranelift { arguments } => tasks::cranelift(&root, &arguments).await,
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the repository")
        .to_owned()
}

async fn guest_rustflags(root: &Path) -> anyhow::Result<String> {
    wamn_build::guest_rustflags(root).await
}

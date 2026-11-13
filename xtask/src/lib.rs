//! Repository tasks, platform publication, and benchmark commands.

mod bench;
mod bench_cloud;
mod bench_data;
mod bench_local;
mod platform;
mod process;
mod selection;
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
    /// Install the pinned KCL CLI.
    InstallKcl,
    /// Build complete package artifacts.
    BuildPackage {
        #[arg(required = true)]
        packages: Vec<PathBuf>,
        #[arg(long, env = "DATABASE_URL", hide_env_values = true)]
        database_url: String,
    },
    /// Generate TypeScript clients for native host routes.
    HostRouteClients,
    /// Print guest compiler flags.
    GuestRustflags,
    /// Build, push, and stamp one platform chart.
    BuildPlatform(platform::Args),
    /// Run the deployed application benchmark.
    Bench(bench::Args),
    /// Measure a disposable local development deployment.
    BenchLocal(bench_local::Args),
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
        materializer: String,
        #[arg(action = clap::ArgAction::Set)]
        edge: bool,
        scale: usize,
    },
    /// Select tests from Cargo dependencies and repository inputs.
    TestChanges(selection::Args),
    /// Require a passing, nonempty libtest result.
    RequireTestResult(process::TestArgs),
    /// Run repository formatting, Clippy, and policy checks.
    RepoLint(tasks::RunArgs),
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
        Task::InstallKcl => {
            println!("{}", wamn_build::kcl::install(&root).await?.display());
            Ok(())
        }
        Task::BuildPackage {
            packages,
            database_url,
        } => {
            for package in packages {
                let output = wamn_build::default_output_root(&package);
                let record = wamn_build::build_package(&wamn_build::BuildRequest::new(
                    &package,
                    &output,
                    &database_url,
                ))
                .await?;
                println!(
                    "{} {} {}",
                    record.package.id, record.package.version, record.outputs.manifest
                );
            }
            Ok(())
        }
        Task::HostRouteClients => wamn_compiler::materialize_host_route_client(
            &wamn_compiler::host_route_client_output_root(&root),
        ),
        Task::GuestRustflags => {
            println!("{}", guest_rustflags(&root).await?);
            Ok(())
        }
        Task::BuildPlatform(args) => platform::run(args).await,
        Task::Bench(args) => bench::run(args).await,
        Task::BenchLocal(args) => bench_local::run(&root, &args).await,
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
            materializer,
            edge,
            scale,
        } => bench_data::app_result(&work, &output, &http, &materializer, edge, scale),
        Task::TestChanges(args) => selection::run(&root, &args).await,
        Task::RequireTestResult(args) => process::require(&std::env::current_dir()?, &args).await,
        Task::RepoLint(args) => tasks::lint(&root, args.mode).await,
        Task::RepoLintDependencies => tasks::dependencies(&root).await,
        Task::CompilePackagesAlone { manifest } => {
            tasks::packages(&root, manifest.as_deref()).await
        }
        Task::ContractDiff(args) => tasks::contract(&root, args.mode).await,
        Task::WasmcloudReleaseTests(args) => tasks::upstream(&root, &args).await,
        Task::DevCollector { root: output } => tasks::collector(&root, &output),
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

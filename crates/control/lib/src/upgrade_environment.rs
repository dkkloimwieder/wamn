//! `wamn-ctl upgrade-environment`: one installed environment to the bytes of
//! one commit (docs/plan/upgrade-environment.md).
//!
//! Each stage of §4.2 is a function over the run record. A stage reads the
//! state first and records `already_done` when the state is the target. The
//! verb keeps its work under `<work root>/<commit>`: the checkout, the target
//! directories, the command logs and one run record per environment. Two
//! environments at one commit share the checkout, the builds and the pushed
//! images.
//!
//! A failure that is not a stop condition of §2 waits for Ready nodes and
//! platform pods, and its stage runs once more (§4.3). A second failure stops
//! the run, and the record names the step.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use serde_json::Value;
use tokio::io::AsyncBufReadExt as _;
use tokio::process::{Child, Command};

use crate::delivery::environment::EnvironmentFile;
use crate::delivery::run_record::{RunArguments, RunRecord, Stage, StepResult};

/// The images that the verb builds by source identity. The gates image is
/// pinned in the environment file and never built (`wamn-1s38`).
const BUILT_IMAGES: [&str; 3] = ["host", "identity", "ctl"];
/// The tag that `tools/journey-image-cache` relabels to, and its lease name.
const RELABEL_TAG: &str = "gcp";
const LEASE: &str = "gcp-wamn";
/// The workload guests of `docs/operations/gcp.md` §3.20: file, repository.
const WORKLOAD_GUESTS: [(&str, &str); 2] = [
    ("http_route.wasm", "flow-http"),
    ("materializer.wasm", "materializer"),
];
const BROKER_POD: &str = "evt-nats-0";
const TAP_JOB: &str = "evt-nats-tap-stream";
const TAP_CHECK_POD: &str = "evt-nats-check";
const TAP_PRESENT: &str = r#""config":{"name":"WAMN_TAP""#;
const COMMAND_TIMEOUT: Duration = Duration::from_hours(3);

#[derive(Clone, Debug)]
pub struct UpgradeEnvironmentRequest {
    /// The repository that holds `--commit`. The verb never builds in its
    /// working tree.
    pub repository: PathBuf,
    /// The parent of the private work directory of each commit.
    pub work_root: PathBuf,
    pub arguments: RunArguments,
}

/// Runs every built stage that the run record does not show as finished.
pub async fn upgrade_environment(request: &UpgradeEnvironmentRequest) -> anyhow::Result<RunRecord> {
    let work = request.work_root.join(&request.arguments.commit);
    private_directory(&work)?;
    private_directory(&work.join("runs"))?;
    private_directory(&work.join("logs"))?;
    let arguments = &request.arguments;
    let path = work.join("runs").join(format!(
        "{}--{}--{}.json",
        arguments.org, arguments.project, arguments.environment
    ));
    let mut record = RunRecord::open(&path, arguments.clone())?;
    let run = Run {
        repository: request.repository.clone(),
        checkout: work.join("checkout"),
        work,
        arguments: arguments.clone(),
    };
    while let Some(stage) = record.next_stage() {
        record.start(&path, stage, run.inputs(stage))?;
        match run.stage(stage, &record).await {
            Ok((result, outputs)) => record.finish(&path, result, outputs)?,
            Err(error) => {
                let cause = redact(&format!("{error:#}"));
                record.finish(&path, StepResult::Failed { cause }, BTreeMap::new())?;
                // §4.3: a failure that is not a stop condition waits for
                // Ready nodes and platform pods, and the stage runs once
                // more. A second failure stops the run.
                if error.downcast_ref::<StopRun>().is_some()
                    || !retried(stage)
                    || attempts(&record, stage) > 1
                {
                    return Err(error.context(format!(
                        "the stage {stage:?} failed, and the run record is {}",
                        path.display()
                    )));
                }
                run.wait_ready().await?;
            }
        }
    }
    Ok(record)
}

impl Run {
    async fn stage(&self, stage: Stage, record: &RunRecord) -> Outcome {
        match stage {
            Stage::Source => self.source().await,
            Stage::Build => self.build().await,
            Stage::Images => self.images().await,
            Stage::Guests => self.guests(record).await,
            Stage::Preflight => self.preflight().await,
            Stage::Schema => self.schema().await,
            Stage::Packages => self.packages(record).await,
            Stage::Qualify => self.qualify(record).await,
            Stage::PublishAndSelect => self.publish_and_select(record).await,
            Stage::Deploy => self.deploy(record).await,
            Stage::CheckAndRetire => self.check_and_retire(record).await,
            Stage::Record => self.finish_record(record),
        }
    }
}

/// A refusal that changes what runs: the run stops at once and is not
/// retried (docs/plan/upgrade-environment.md §2).
#[derive(Debug)]
struct StopRun(&'static str);

impl std::fmt::Display for StopRun {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}, so the run stops", self.0)
    }
}

/// The stages that touch the environment, which the retry of §4.3 covers.
fn retried(stage: Stage) -> bool {
    !matches!(
        stage,
        Stage::Source | Stage::Build | Stage::Images | Stage::Guests | Stage::Record
    )
}

/// The failed steps of `stage` since the last step of another stage.
fn attempts(record: &RunRecord, stage: Stage) -> usize {
    record
        .steps
        .iter()
        .rev()
        .take_while(|step| step.stage == stage)
        .count()
}

type Outcome = anyhow::Result<(StepResult, BTreeMap<String, String>)>;

struct Run {
    repository: PathBuf,
    work: PathBuf,
    checkout: PathBuf,
    arguments: RunArguments,
}

impl Run {
    fn inputs(&self, stage: Stage) -> BTreeMap<String, String> {
        let mut inputs = BTreeMap::from([("commit".to_owned(), self.arguments.commit.clone())]);
        if stage == Stage::Source {
            inputs.insert(
                "repository".to_owned(),
                self.repository.display().to_string(),
            );
        } else {
            inputs.insert(
                "environment_file".to_owned(),
                EnvironmentFile::path(
                    &self.checkout,
                    &self.arguments.org,
                    &self.arguments.project,
                    &self.arguments.environment,
                )
                .display()
                .to_string(),
            );
        }
        inputs
    }

    fn environment(&self) -> anyhow::Result<EnvironmentFile> {
        EnvironmentFile::read(
            &self.checkout,
            &self.arguments.org,
            &self.arguments.project,
            &self.arguments.environment,
        )
    }

    fn programs(&self) -> PathBuf {
        self.work.join("programs")
    }

    fn delivery(&self) -> PathBuf {
        self.work.join("delivery")
    }

    fn guest_file(&self, name: &str) -> PathBuf {
        self.delivery().join("wasm32-wasip2/release").join(name)
    }

    /// Runs one command in the checkout. Its output goes to a log file of the
    /// work directory, and a failure names that file.
    async fn command(&self, label: &str, command: &mut Command) -> anyhow::Result<String> {
        let log = self.work.join("logs").join(format!("{label}.log"));
        let output = command
            .current_dir(&self.checkout)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        let output = tokio::time::timeout(COMMAND_TIMEOUT, output)
            .await
            .with_context(|| format!("{label} ran longer than {COMMAND_TIMEOUT:?}"))?
            .with_context(|| format!("start {label}"))?;
        fs::write(
            &log,
            [output.stdout.as_slice(), output.stderr.as_slice()].concat(),
        )
        .with_context(|| format!("write {}", log.display()))?;
        ensure!(
            output.status.success(),
            "{label} exited {}; its output is in {}",
            output.status,
            log.display()
        );
        String::from_utf8(output.stdout).with_context(|| format!("{label} printed no UTF-8"))
    }

    /// Stage 1: a clean checkout of the commit in the work directory.
    async fn source(&self) -> Outcome {
        let existed = self.checkout.exists();
        if !existed {
            git(
                &self.repository,
                &[
                    "cat-file",
                    "-e",
                    &format!("{}^{{commit}}", self.arguments.commit),
                ],
            )
            .await
            .with_context(|| format!("the repository has no commit {}", self.arguments.commit))?;
            let checkout = self.checkout.display().to_string();
            git(
                &self.repository,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    &checkout,
                    &self.arguments.commit,
                ],
            )
            .await?;
        }
        let head = git(&self.checkout, &["rev-parse", "HEAD"]).await?;
        ensure!(
            head.trim() == self.arguments.commit,
            "the checkout {} is at {}, not at {}",
            self.checkout.display(),
            head.trim(),
            self.arguments.commit
        );
        let status = git(
            &self.checkout,
            &["status", "--porcelain", "--untracked-files=all"],
        )
        .await?;
        ensure!(
            status.is_empty(),
            "the checkout {} is not clean:\n{status}",
            self.checkout.display()
        );
        self.environment()?;
        let outputs =
            BTreeMap::from([("checkout".to_owned(), self.checkout.display().to_string())]);
        Ok((done(existed), outputs))
    }

    /// Stage 2: the programs, the native delivery binaries and the guests,
    /// before any candidate exists. Cargo decides what is fresh.
    async fn build(&self) -> Outcome {
        let programs = self.programs();
        let delivery = self.delivery();
        self.command(
            "build-programs",
            Command::new("cargo")
                .args([
                    "build",
                    "--locked",
                    "-p",
                    "wamn-ctl",
                    "--features",
                    "ops",
                    "--bins",
                ])
                .args(["-p", "wamn-scenario-worker"])
                .env("CARGO_TARGET_DIR", &programs),
        )
        .await?;
        self.command(
            "build-native",
            Command::new(self.checkout.join("tools/delivery-owned"))
                .arg("build-native")
                .arg(&delivery),
        )
        .await?;
        self.command(
            "build-components",
            Command::new(self.checkout.join("tools/build-components"))
                .arg("all")
                .env("CARGO_TARGET_DIR", &delivery),
        )
        .await?;
        let mut outputs = BTreeMap::new();
        let guests = delivery.join("wasm32-wasip2/release");
        let mut files: Vec<PathBuf> = fs::read_dir(&guests)
            .with_context(|| format!("list {}", guests.display()))?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<_, _>>()?;
        files.sort();
        for file in files
            .iter()
            .filter(|file| file.extension().is_some_and(|ext| ext == "wasm"))
        {
            let name = file
                .file_name()
                .and_then(|name| name.to_str())
                .context("guest name")?;
            outputs.insert(format!("guest {name}"), sha256_file(file)?);
        }
        for (file, _) in WORKLOAD_GUESTS {
            ensure!(
                outputs.contains_key(&format!("guest {file}")),
                "the build made no workload guest {file}"
            );
        }
        Ok((StepResult::Done, outputs))
    }

    /// Stage 3: host, identity and ctl by source identity, pushed once under
    /// the `src-` tag. The gates image comes from the environment file.
    async fn images(&self) -> Outcome {
        let environment = self.environment()?;
        let registry = Registry::login(&environment.registry, &self.work).await?;
        let mut pushed = false;
        let mut outputs = BTreeMap::new();
        for name in BUILT_IMAGES {
            let built = self
                .command(
                    &format!("image-{name}"),
                    Command::new(self.checkout.join("tools/journey-image-cache"))
                        .arg("ensure")
                        .arg(&self.checkout)
                        .args([name, name, &self.arguments.commit, RELABEL_TAG, LEASE]),
                )
                .await?;
            let identity = built
                .trim()
                .strip_prefix(&format!("wamn-{name}:src-"))
                .with_context(|| format!("the image cache printed {built}"))?
                .to_owned();
            let repository = format!("wamn-{name}");
            let tag = format!("src-{identity}");
            let reference = format!("{}/{repository}:{tag}", environment.registry);
            let digest = if let Some(digest) = registry.digest(&repository, &tag).await? {
                digest
            } else {
                let relabeled = format!("wamn-{name}:{RELABEL_TAG}");
                self.command(
                    &format!("tag-{name}"),
                    Command::new("docker").args(["tag", &relabeled, &reference]),
                )
                .await?;
                self.command(
                    &format!("push-{name}"),
                    registry.docker().args(["push", &reference]),
                )
                .await?;
                pushed = true;
                registry
                    .digest(&repository, &tag)
                    .await?
                    .with_context(|| format!("the registry shows no {reference} after its push"))?
            };
            outputs.insert(name.to_owned(), format!("{reference}@{digest}"));
        }
        outputs.insert("gates".to_owned(), environment.gates_image);
        Ok((done(!pushed), outputs))
    }

    /// Stage 4: each workload guest once under the sha256 hex of its bytes.
    async fn guests(&self, record: &RunRecord) -> Outcome {
        let environment = self.environment()?;
        let registry = Registry::login(&environment.registry, &self.work).await?;
        let wash = self
            .command(
                "install-wash",
                &mut Command::new(self.checkout.join("tools/install-wash")),
            )
            .await?;
        let mut pushed = false;
        let mut outputs = BTreeMap::new();
        for (file, name) in WORKLOAD_GUESTS {
            let path = self.guest_file(file);
            let hex = sha256_file(&path)?;
            ensure!(
                hex == record.output(Stage::Build, &format!("guest {file}"))?,
                "the guest {} changed after the build stage",
                path.display()
            );
            let repository = format!("components/{name}");
            let reference = format!("{}/{repository}", environment.registry);
            let digest = if let Some(digest) = registry.digest(&repository, &hex).await? {
                digest
            } else {
                let mut push = registry.command(wash.trim());
                push.args(["-o", "json", "oci", "push", &format!("{reference}:{hex}")])
                    .arg(&path);
                self.command(&format!("push-{name}"), &mut push).await?;
                pushed = true;
                registry.digest(&repository, &hex).await?.with_context(|| {
                    format!("the registry shows no {reference}:{hex} after its push")
                })?
            };
            outputs.insert(name.to_owned(), format!("{reference}@{digest}"));
        }
        Ok((done(!pushed), outputs))
    }

    /// Stage 5: capacity, broker, `WAMN_TAP` and the next free release id,
    /// before any change to the environment.
    async fn preflight(&self) -> Outcome {
        let environment = self.environment()?;
        let kubectl = Kubectl(environment.context.clone());
        let mut outputs = BTreeMap::new();

        let values = host_values(&self.checkout)?;
        let groups = host_group_requests(&values)?;
        let nodes = kubectl.json(&["get", "nodes"]).await?;
        let pods = kubectl.json(&["get", "pods", "--all-namespaces"]).await?;
        let free = free_capacity(&nodes, &pods)?;
        place(&groups, free)?;
        outputs.insert(
            "host groups".to_owned(),
            groups
                .iter()
                .map(|(name, request)| format!("{name} {}m {}B", request.cpu, request.memory))
                .collect::<Vec<_>>()
                .join(", "),
        );

        let tap = self.broker(&kubectl).await?;
        outputs.insert("WAMN_TAP".to_owned(), tap.to_owned());

        let databases = Databases::open(&environment, &kubectl, &self.arguments).await?;
        let release = next_release(&databases, &self.arguments).await?;
        outputs.insert("tenant".to_owned(), databases.tenant.clone());
        outputs.insert("database".to_owned(), databases.database.clone());
        outputs.insert(
            "attested releases".to_owned(),
            release
                .attested
                .iter()
                .map(|(id, commit)| format!("{id} {commit}"))
                .collect::<Vec<_>>()
                .join(", "),
        );
        outputs.insert("release id".to_owned(), release.next.to_string());
        Ok((StepResult::Done, outputs))
    }

    fn release_files(&self) -> PathBuf {
        self.work.join("releases").join(format!(
            "{}--{}--{}",
            self.arguments.org, self.arguments.project, self.arguments.environment
        ))
    }

    /// The credentials of the gate, kept until stage 11 retires the old
    /// generation, then deleted.
    fn secrets(&self) -> PathBuf {
        self.work.join("secrets").join(format!(
            "{}--{}--{}",
            self.arguments.org, self.arguments.project, self.arguments.environment
        ))
    }

    fn release_id(record: &RunRecord) -> anyhow::Result<u32> {
        record
            .output(Stage::Preflight, "release id")?
            .parse()
            .context("the release id of the preflight")
    }

    /// Stage 6: every pending system and project migration.
    async fn schema(&self) -> Outcome {
        let environment = self.environment()?;
        let kubectl = Kubectl(environment.context.clone());
        let databases = Databases::open(&environment, &kubectl, &self.arguments).await?;
        for admin_database_url in [None, Some(databases.project_url())] {
            crate::upgrade_schema::upgrade_schema(&crate::upgrade_schema::UpgradeSchemaRequest {
                system_database_url: databases.system_url(),
                admin_database_url,
                baseline: None,
                confirm: true,
            })
            .await
            .context(StopRun("a migration refused"))?;
        }
        let outputs = BTreeMap::from([
            (
                "system database".to_owned(),
                databases.system_database.clone(),
            ),
            ("project database".to_owned(), databases.database.clone()),
        ]);
        Ok((StepResult::Done, outputs))
    }

    /// Stage 7: packages, components, gated wirings, the release under the
    /// preflight id, and the bindings of the current release copied to it.
    async fn packages(&self, record: &RunRecord) -> Outcome {
        use crate::release_composition::{
            ComponentBuildPlan, PackageInput, load_wirings, select_component_artifacts,
            select_palette_artifacts, wiring_store_alias,
        };
        let environment = self.environment()?;
        let kubectl = Kubectl(environment.context.clone());
        let databases = Databases::open(&environment, &kubectl, &self.arguments).await?;
        let release_id = Self::release_id(record)?;
        let tenant = databases.tenant.clone();
        let project_url = databases.project_url();
        let system_url = databases.system_url();
        let mut outputs = BTreeMap::new();
        private_directory(&self.release_files())?;

        let roots: Vec<PathBuf> = environment
            .packages
            .iter()
            .map(|root| self.checkout.join(root))
            .collect();
        let mut packages = Vec::new();
        for root in &roots {
            let path = wamn_schema_generator::package_manifest_path(root);
            let manifest = wamn_schema_generator::PackageManifest::from_slice(
                &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
            )
            .with_context(|| format!("parse {}", path.display()))?;
            packages.push(PackageInput {
                root: root.clone(),
                manifest,
            });
        }
        // The release composition of the dev loop, over the stage 2 build.
        let mut plan = Command::new(self.checkout.join("tools/build-components"));
        plan.args(["build-only", "app"])
            .args(&roots)
            .env("CARGO_TARGET_DIR", self.delivery());
        let plan: ComponentBuildPlan =
            serde_json::from_str(&self.command("build-plan", &mut plan).await?)
                .context("decode the build plan")?;
        let artifacts = select_component_artifacts(&packages, &plan.virtualization.artifacts)?;
        let palette = select_palette_artifacts(&packages, &plan.palette)?;
        let wirings = load_wirings(&packages)?;

        for root in &roots {
            let applied =
                crate::apply_package::apply_package(crate::apply_package::ApplyPackageRequest {
                    package: root.clone(),
                    database_url: project_url.clone(),
                    tenant: tenant.clone(),
                })
                .await?;
            outputs.insert(
                format!("package {}", applied.package_id),
                format!(
                    "{} ({} migrations applied)",
                    applied.package_version, applied.migrations_applied
                ),
            );
        }
        crate::reconcile_package_data_access::reconcile_package_data_access(
            crate::reconcile_package_data_access::ReconcilePackageDataAccessRequest {
                packages: roots.clone(),
                database_url: project_url.clone(),
                tenant: tenant.clone(),
            },
        )
        .await?;
        for root in &roots {
            crate::reconcile_replica_identity::reconcile_package_replica_identity(
                crate::reconcile_replica_identity::ReconcileReplicaIdentityRequest {
                    admin_database_url: project_url.clone(),
                    package: root.clone(),
                    dry_run: false,
                },
            )
            .await?;
        }

        let registry = Registry::login(&environment.registry, &self.work).await?;
        let publish = || crate::push_component::PublishAdmittedComponentRequest {
            artifact_base: format!("{}/components", environment.registry),
            registry_auth_file: registry.auth_file(),
            insecure_registry: false,
            oci_ca_paths: Vec::new(),
            project_database_url: project_url.clone(),
            control_database_url: system_url.clone(),
        };
        let mut pushed = BTreeMap::<String, String>::new();
        for artifact in &artifacts {
            let package = packages
                .iter()
                .find(|package| package.manifest.package.id == *artifact.package_id)
                .context("a selected artifact has no package")?;
            let template = package
                .root
                .join("publication/components")
                .join(format!("{}.json.in", artifact.component));
            let base = crate::component_declaration::authored_base_digests(&package.root)?;
            let document = crate::component_declaration::render_declaration_document(
                &template, &tenant, &base,
            )?;
            let declaration = self
                .release_files()
                .join(format!("{}.declaration.json", artifact.component));
            fs::write(&declaration, serde_json::to_vec(&document)?)?;
            let outcome = crate::push_component::push_component(
                crate::push_component::AdmitComponentRequest {
                    package: package.root.clone(),
                    component_bytes: artifact.path.clone(),
                    declaration,
                    admitted_platform_packages: vec![
                        "wamn:node".to_owned(),
                        "wamn:postgres".to_owned(),
                    ],
                },
                publish(),
            )
            .await?;
            pushed.insert(artifact.component.to_string(), outcome.component_digest);
        }
        for selected in &palette {
            let artifact = &selected.artifact;
            let package = packages
                .iter()
                .find(|package| package.manifest.package.id == *artifact.package_id)
                .context("a palette artifact has no package")?;
            let scope = wamn_catalog::ComponentPackageScope {
                tenant_id: tenant.clone(),
                package_id: artifact.package_id.to_string(),
                package_version: artifact.package_version.to_string(),
            };
            let alias = wiring_store_alias(&wirings, artifact)?;
            let document = crate::component_declaration::render_palette_declaration(
                &selected.declaration,
                &scope,
                alias.as_deref(),
            )?;
            let admitted = crate::component_declaration::declared_platform_packages(
                &selected.declaration,
                &document,
            )?;
            let declaration = self
                .release_files()
                .join(format!("{}.declaration.json", artifact.component));
            fs::write(&declaration, serde_json::to_vec(&document)?)?;
            let outcome = crate::push_component::push_component(
                crate::push_component::AdmitComponentRequest {
                    package: package.root.clone(),
                    component_bytes: artifact.path.clone(),
                    declaration,
                    admitted_platform_packages: admitted,
                },
                publish(),
            )
            .await?;
            pushed.insert(artifact.component.to_string(), outcome.component_digest);
        }
        for (component, digest) in &pushed {
            outputs.insert(format!("component {component}"), digest.clone());
        }

        // The gate of every wiring, with a new generation and a new PAT.
        let gate = GateCredentials::prepare(self, &kubectl, &databases, record).await?;
        outputs.insert(
            "control-author generation".to_owned(),
            gate.control_author.as_str().to_owned(),
        );
        outputs.insert(
            "management-admitter generation".to_owned(),
            gate.management_admitter.as_str().to_owned(),
        );
        outputs.insert(
            "management-author PAT prefix".to_owned(),
            gate.pat_prefix.clone(),
        );
        let mut targets = Vec::new();
        if !wirings.is_empty() {
            let service = GateService::start(
                self,
                &gate.control_author_url,
                &gate.management_admitter_url,
                &databases,
            )
            .await?;
            for input in &wirings {
                let package = packages
                    .iter()
                    .find(|package| package.manifest.package.id == *input.package_id)
                    .context("a wiring has no package")?;
                let file = input.path.clone();
                ensure!(
                    file.starts_with(&package.root),
                    "a wiring lies outside its package"
                );
                let report = service
                    .gate(
                        &gate,
                        input,
                        &self.arguments.project,
                        &self.arguments.environment,
                    )
                    .await?;
                outputs.insert(format!("gate {}", input.wiring.wiring_id), report);
                crate::author_wiring::author_wiring_document(
                    crate::author_wiring::AuthorWiringDocumentRequest {
                        database_url: project_url.clone(),
                        control_database_url: system_url.clone(),
                        tenant: tenant.clone(),
                        package_id: input.package_id.to_string(),
                        package_version: input.package_version.to_string(),
                        wiring_document: file,
                    },
                )
                .await?;
                targets.push(crate::publish_release::ReleaseWiringTarget {
                    package_id: input.package_id.to_string(),
                    package_version: input.package_version.to_string(),
                    wiring_id: input.wiring.wiring_id.clone(),
                    wiring_version: input.wiring.version,
                });
            }
        }

        let digest = crate::publish_release::publish_release(
            crate::publish_release::PublishReleaseRequest {
                database_url: project_url.clone(),
                control_database_url: system_url.clone(),
                org: self.arguments.org.clone(),
                project: self.arguments.project.clone(),
                tenant: tenant.clone(),
                effective_release_id: release_id,
                environment: self.arguments.environment.clone(),
                verified_publisher_principal: format!(
                    "wamn-management-author-{}--{}--{}",
                    self.arguments.org, self.arguments.project, self.arguments.environment
                ),
                run_schema: "wamn_run".to_owned(),
                packages: packages
                    .iter()
                    .map(|package| {
                        wamn_catalog::PackageCoordinate::new(
                            &package.manifest.package.id,
                            &package.manifest.package.version,
                        )
                    })
                    .collect::<Result<_, _>>()?,
                wirings: targets,
                attachments: roots
                    .iter()
                    .map(|root| root.join("publication/attachments.json"))
                    .filter(|path| path.is_file())
                    .collect(),
                route_host: Some(environment.route_host.clone()),
                package_manifests: roots
                    .iter()
                    .map(|root| wamn_schema_generator::package_manifest_path(root))
                    .collect(),
            },
        )
        .await?;
        outputs.insert("release id".to_owned(), release_id.to_string());
        outputs.insert("release digest".to_owned(), digest.to_string());

        for copied in copy_bindings(self, &databases, release_id, &pushed).await? {
            outputs.insert(format!("binding {}", copied.0), copied.1);
        }
        Ok((StepResult::Done, outputs))
    }

    /// Stage 8: the candidate from the stage 2 target and `qualify-release`
    /// at the commit. A failed qualification stops the run.
    async fn qualify(&self, record: &RunRecord) -> Outcome {
        let environment = self.environment()?;
        let kubectl = Kubectl(environment.context.clone());
        self.broker(&kubectl).await?;
        let databases = Databases::open(&environment, &kubectl, &self.arguments).await?;
        let release_id = Self::release_id(record)?;
        let files = self.release_files();
        private_directory(&files)?;
        let manifest = files.join("manifest.json");
        let candidate = files.join("candidate.json");
        let qualification = files.join("qualification.json");
        for path in [&manifest, &candidate, &qualification] {
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        crate::delivery::prepare(crate::delivery::PrepareReleaseRequest {
            database_url: databases.project_url(),
            org: self.arguments.org.clone(),
            project: self.arguments.project.clone(),
            tenant: databases.tenant.clone(),
            effective_release_id: release_id,
            artifact_base: format!("{}/releases", environment.registry),
            target_directory: self.delivery(),
            manifest_output: manifest.clone(),
            candidate_output: candidate.clone(),
            host_image: record.output(Stage::Images, "host")?.to_owned(),
            gates_image: Some(record.output(Stage::Images, "gates")?.to_owned()),
            identity_image: Some(record.output(Stage::Images, "identity")?.to_owned()),
            native_registry_endpoint: None,
            native_registry_insecure: false,
            deployment_files: Vec::new(),
        })
        .await?;
        // The kind cases fetch the pinned images with the login of this
        // machine (tools/registry-image-archive), so the qualification runs
        // as its own process with the private Docker configuration.
        let registry = Registry::login(&environment.registry, &self.work).await?;
        let mut qualify =
            registry.command(&self.programs().join("debug/wamn-ctl").display().to_string());
        qualify
            .arg("qualify-release")
            .arg("--repository")
            .arg(&self.checkout)
            .args(["--revision", &self.arguments.commit])
            .arg("--candidate")
            .arg(&candidate)
            .arg("--result")
            .arg(&qualification);
        self.command(&format!("qualify-{}", self.arguments.project), &mut qualify)
            .await
            .context(StopRun("the qualification failed"))?;
        let outputs = BTreeMap::from([
            ("candidate".to_owned(), candidate.display().to_string()),
            (
                "qualification".to_owned(),
                qualification.display().to_string(),
            ),
        ]);
        Ok((StepResult::Done, outputs))
    }

    /// Stage 9: the qualified publication and the selection of the
    /// environment that the run changes.
    async fn publish_and_select(&self, record: &RunRecord) -> Outcome {
        let environment = self.environment()?;
        let kubectl = Kubectl(environment.context.clone());
        let databases = Databases::open(&environment, &kubectl, &self.arguments).await?;
        let qualification = PathBuf::from(record.output(Stage::Qualify, "qualification")?);
        let registry = Registry::login(&environment.registry, &self.work).await?;
        let request = crate::push_release_manifest::PushReleaseManifestRequest {
            database_url: databases.project_url(),
            org: self.arguments.org.clone(),
            project: self.arguments.project.clone(),
            tenant: databases.tenant.clone(),
            effective_release_id: Self::release_id(record)?,
            artifact_base: format!("{}/releases", environment.registry),
            registry_auth_file: registry.auth_file(),
            insecure_registry: false,
            oci_ca_paths: Vec::new(),
            control_database_url: databases.system_url(),
        };
        let pushed = crate::delivery::publication::publish(&qualification, &request)
            .await
            .context(StopRun("the qualified publication refused the bytes"))?;
        let selected = crate::delivery::deployment::select(
            &crate::delivery::selection::QualificationSource::File(qualification.clone()),
            &request,
        )
        .await?;
        let outputs = BTreeMap::from([
            ("published".to_owned(), format!("{pushed:?}")),
            (
                "selected release".to_owned(),
                format!("{:?}", selected.release),
            ),
            (
                "manifest digest".to_owned(),
                selected.manifest_digest.clone(),
            ),
        ]);
        Ok((StepResult::Done, outputs))
    }

    /// The broker check of stage 5, which runs again before stages 8 and 10
    /// (§4.3): the broker is Ready, and the tap Job runs when `WAMN_TAP` is
    /// missing.
    async fn broker(&self, kubectl: &Kubectl) -> anyhow::Result<&'static str> {
        let broker = kubectl
            .json(&["-n", "platform", "get", "pod", BROKER_POD])
            .await?;
        ensure!(ready(&broker), "the broker pod {BROKER_POD} is not Ready");
        if self.tap_exists(kubectl).await? {
            return Ok("present");
        }
        self.run_tap_job(kubectl).await?;
        ensure!(
            self.tap_exists(kubectl).await?,
            "WAMN_TAP is still missing after the tap Job"
        );
        Ok("made by the tap Job")
    }

    /// The wait of §4.3 before a retry: Ready nodes and Ready platform pods.
    async fn wait_ready(&self) -> anyhow::Result<()> {
        let kubectl = Kubectl(self.environment()?.context);
        kubectl
            .run(&[
                "wait",
                "--for=condition=Ready",
                "nodes",
                "--all",
                "--timeout=15m",
            ])
            .await?;
        kubectl
            .run(&[
                "-n",
                "platform",
                "wait",
                "--for=condition=Ready",
                "pods",
                "--all",
                "--field-selector=status.phase!=Succeeded",
                "--timeout=15m",
            ])
            .await?;
        Ok(())
    }

    fn rendered(&self) -> PathBuf {
        self.release_files().join("rendered")
    }

    /// Stage 10: the web client, the edge, the URL map, the hosts and the
    /// workloads, with every host group and workload Ready.
    async fn deploy(&self, record: &RunRecord) -> Outcome {
        let environment = self.environment()?;
        let kubectl = Kubectl(environment.context.clone());
        self.broker(&kubectl).await?;
        let databases = Databases::open(&environment, &kubectl, &self.arguments).await?;
        let digest = record
            .output(Stage::PublishAndSelect, "manifest digest")?
            .to_owned();
        let hex = digest
            .strip_prefix("sha256:")
            .context("the selected manifest digest has no sha256 prefix")?
            .to_owned();
        let rendered = self.rendered();
        private_directory(&rendered)?;
        let mut outputs = BTreeMap::new();
        let project = gcp_project(&environment.registry)?;

        // The web client of each package that has one, create-only.
        let mut client_package = None;
        for root in &environment.packages {
            let root = self.checkout.join(root);
            let manifest: Value = serde_json::from_slice(&fs::read(
                wamn_schema_generator::package_manifest_path(&root),
            )?)?;
            if manifest.pointer("/client_package/name").is_none() {
                continue;
            }
            let package = manifest
                .pointer("/package/id")
                .and_then(Value::as_str)
                .context("a package manifest names no package id")?
                .to_owned();
            let index = format!(
                "gs://{}/{}/{package}/{hex}/index.html",
                environment.web_client.bucket, environment.web_client.prefix
            );
            let present = Command::new("gcloud")
                .args(["storage", "ls", &index, "--project", &project])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await?
                .success();
            if present {
                outputs.insert(format!("web client {package}"), format!("{index} present"));
            } else {
                self.command(
                    "pnpm-install",
                    Command::new("pnpm").args(["install", "--frozen-lockfile"]),
                )
                .await?;
                let mut upload = Command::new(self.programs().join("debug/wamn"));
                upload
                    .args(["web", "upload"])
                    .arg(&root)
                    .args(["--release", &digest, "--org", &self.arguments.org])
                    .arg("--bucket")
                    .arg(format!(
                        "gs://{}/{}",
                        environment.web_client.bucket, environment.web_client.prefix
                    ))
                    .env("WAMN_WEB_DATABASE_URL", databases.project_url());
                self.command(&format!("web-upload-{package}"), &mut upload)
                    .await?;
                outputs.insert(format!("web client {package}"), format!("{index} uploaded"));
            }
            client_package.get_or_insert(package);
        }

        // The edge and the URL map, from their live state, for this route
        // host only, so another environment keeps what it serves.
        if let Some(package) = &client_package {
            let bucket_path = format!(
                "{}/{}/{package}/{hex}",
                environment.web_client.bucket, environment.web_client.prefix
            );
            let live = helm(
                &environment.context,
                &[
                    "get",
                    "values",
                    &environment.edge.release,
                    "-n",
                    &environment.edge.namespace,
                    "-o",
                    "json",
                ],
            )
            .await?;
            let mut values: Value = serde_json::from_str(&live)?;
            if set_bucket_path(&mut values, &environment.route_host, &bucket_path)? {
                let file = rendered.join("values-edge.yaml");
                fs::write(&file, serde_yaml::to_string(&values)?)?;
                let chart = self.checkout.join("deploy/platform/edge");
                helm(
                    &environment.context,
                    &[
                        "upgrade",
                        &environment.edge.release,
                        &chart.display().to_string(),
                        "-n",
                        &environment.edge.namespace,
                        "-f",
                        &file.display().to_string(),
                        "--wait",
                        "--timeout",
                        "3m",
                    ],
                )
                .await?;
                outputs.insert(
                    "edge".to_owned(),
                    format!("{} serves {bucket_path}", environment.route_host),
                );
            } else {
                outputs.insert("edge".to_owned(), "already serves the release".to_owned());
            }
            let exported = rendered.join("url-map-live.yaml");
            self.command(
                "url-map-export",
                Command::new("gcloud")
                    .args(["compute", "url-maps", "export", &environment.url_map.name])
                    .args(["--global", "--project", &project, "--destination"])
                    .arg(&exported),
            )
            .await?;
            let mut map: serde_yaml::Value = serde_yaml::from_str(&fs::read_to_string(&exported)?)?;
            let prefix = format!("/{}/{package}/", environment.web_client.prefix);
            if rewrite_url_map(&mut map, &environment.route_host, &prefix, &hex)? {
                let file = rendered.join("url-map.yaml");
                fs::write(&file, serde_yaml::to_string(&map)?)?;
                self.command(
                    "url-map-import",
                    Command::new("gcloud")
                        .args(["compute", "url-maps", "import", &environment.url_map.name])
                        .args(["--global", "--project", &project, "--quiet", "--source"])
                        .arg(&file),
                )
                .await?;
                outputs.insert(
                    "url map".to_owned(),
                    format!("{} rewrites to {hex}", environment.route_host),
                );
            } else {
                outputs.insert(
                    "url map".to_owned(),
                    "already rewrites to the release".to_owned(),
                );
            }
        }

        // The host values from the stage 3 host image, with this group's new
        // release and the current release of every other group.
        ensure!(
            HOST_VALUES_GROUPS.contains(&environment.host_group.as_str()),
            "the host values program renders no host group {}",
            environment.host_group
        );
        let live = helm(
            &environment.context,
            &[
                "get",
                "values",
                HOST_RELEASE,
                "-n",
                HOST_NAMESPACE,
                "-o",
                "json",
            ],
        )
        .await?;
        let live: Value = serde_json::from_str(&live)?;
        let mut digests = Vec::new();
        for group in HOST_VALUES_GROUPS {
            digests.push(if group == environment.host_group {
                digest.clone()
            } else {
                release_digest(&live, group)?
            });
        }
        let host_image = record.output(Stage::Images, "host")?.to_owned();
        let mut values = Command::new("cargo");
        values
            .args(["run", "--locked", "-p", "wamn-test-infrastructure"])
            .args(["--example", "host_values_files", "--"])
            .arg(&rendered)
            .arg(&host_image)
            .arg(format!("{}/releases", environment.registry))
            .args(&digests)
            .env("CARGO_TARGET_DIR", self.programs());
        self.command("host-values", &mut values).await?;
        let flow = record.output(Stage::Guests, "flow-http")?.to_owned();
        let materializer = record.output(Stage::Guests, "materializer")?.to_owned();
        let mut workloads = Command::new("cargo");
        workloads
            .args(["run", "--locked", "-p", "wamn-test-infrastructure"])
            .args(["--example", "workload_files", "--"])
            .arg(&rendered)
            .args([&flow, &materializer, &flow, &materializer])
            .env("CARGO_TARGET_DIR", self.programs());
        self.command("workload-files", &mut workloads).await?;
        helm(
            &environment.context,
            &[
                "upgrade",
                HOST_RELEASE,
                HOST_CHART,
                "--version",
                HOST_CHART_VERSION,
                "-n",
                HOST_NAMESPACE,
                "-f",
                &rendered.join("values-host-base.yaml").display().to_string(),
                "-f",
                &rendered.join("values-host.yaml").display().to_string(),
                "--wait",
                "--timeout",
                "10m",
            ],
        )
        .await?;
        outputs.insert("host image".to_owned(), host_image);
        let mut names = Vec::new();
        for workload in &environment.workloads {
            let file = rendered.join(
                workload
                    .file_name()
                    .context("a workload file has no name")?,
            );
            kubectl
                .run(&["apply", "-f", &file.display().to_string()])
                .await?;
            names.extend(workload_names(&fs::read_to_string(&file)?)?);
        }
        for name in &names {
            kubectl
                .run(&[
                    "-n",
                    HOST_NAMESPACE,
                    "wait",
                    "--for=condition=Ready",
                    &format!("workloaddeployment/{name}"),
                    "--timeout=240s",
                ])
                .await?;
        }
        outputs.insert("workloads".to_owned(), names.join(", "));
        Ok((StepResult::Done, outputs))
    }

    /// Stage 11: the serve check, then the retirement of the old gate
    /// generation and the revocation of every older management-author PAT of
    /// the environment.
    async fn check_and_retire(&self, record: &RunRecord) -> Outcome {
        use crate::provision_project_env::{
            WorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction,
            run_workload_action,
        };
        use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};
        let environment = self.environment()?;
        let kubectl = Kubectl(environment.context.clone());
        let mut outputs = BTreeMap::new();

        let manifest_file = self.release_files().join("manifest.json");
        let manifest =
            wamn_catalog::ServingManifest::from_canonical_bytes(&fs::read(&manifest_file)?)
                .context("admit the release manifest")?;
        let routes = serve_routes(&manifest.0)?;
        let forward = ServiceForward::open(
            &kubectl,
            HOST_NAMESPACE,
            &format!("hostgroup-{}", environment.host_group),
        )
        .await?;
        let checked = serve_check(forward.port, &environment.route_host, &routes)
            .await
            .context(StopRun("the serve check failed after the hosts changed"))?;
        drop(forward);
        outputs.insert("serve check".to_owned(), checked);

        let databases = Databases::open(&environment, &kubectl, &self.arguments).await?;
        let current = |name: &str| -> anyhow::Result<CredentialGeneration> {
            match record.output(Stage::Packages, name)? {
                "a" => Ok(CredentialGeneration::A),
                "b" => Ok(CredentialGeneration::B),
                other => bail!("the packages stage recorded the generation {other}"),
            }
        };
        let control_author = current("control-author generation")?;
        let management_admitter = current("management-admitter generation")?;
        let secrets = self.secrets();
        let control_url = databases.local(&secret_url(&secrets.join("control-author.json"))?)?;
        let admitter_url =
            databases.local(&secret_url(&secrets.join("management-admitter.json"))?)?;
        redact_later(&control_url);
        redact_later(&admitter_url);
        let system = databases.system().await?;
        let mut retire = Vec::new();
        for (family, generation, database, target) in [
            (
                WorkloadRoleFamily::ControlAuthor,
                control_author,
                databases.system_database.clone(),
                None,
            ),
            (
                WorkloadRoleFamily::ManagementAdmitter,
                management_admitter,
                databases.database.clone(),
                Some(databases.project_url()),
            ),
        ] {
            let old = generation.other();
            let role = match family {
                WorkloadRoleFamily::ControlAuthor => {
                    wamn_control_provision::control_author_generation_role(
                        &self.arguments.org,
                        &self.arguments.project,
                        &self.arguments.environment,
                        &database,
                        old,
                    )
                }
                _ => wamn_control_provision::management_admitter_generation_role(
                    &self.arguments.org,
                    &self.arguments.project,
                    &self.arguments.environment,
                    &database,
                    old,
                ),
            };
            let active: Option<bool> = system
                .query_opt(
                    "SELECT rolcanlogin FROM pg_roles WHERE rolname = $1",
                    &[&role],
                )
                .await?
                .map(|row| row.get(0));
            if active == Some(true) {
                retire.push((family, old, target));
            } else {
                outputs.insert(
                    format!("retire {}", family_file(family)),
                    format!("generation {} is not active", old.as_str()),
                );
            }
        }
        if !retire.is_empty() {
            // A retire needs a live session of the replacement generation
            // (owner ruling on wamn-ld93.25), so the gate service holds one.
            let service = GateService::start(self, &control_url, &admitter_url, &databases).await?;
            tokio::time::sleep(Duration::from_secs(3)).await;
            for (family, old, target) in retire {
                run_workload_action(&WorkloadActionRequest {
                    org: self.arguments.org.clone(),
                    project: self.arguments.project.clone(),
                    env: self.arguments.environment.clone(),
                    tenant: Some(databases.tenant.clone()),
                    system_database_url: Some(databases.system_url()),
                    target_admin_database_url: target,
                    cluster: None,
                    db_host: Some(databases.cluster_host.clone()),
                    db_port: 5432,
                    namespace: databases.secret_namespace.clone(),
                    action: WorkloadGenerationAction {
                        family,
                        verb: WorkloadActionVerb::Retire,
                        generation: old,
                    },
                    secret: None,
                    emit_role_sql: None,
                    control_administration_patch: None,
                })
                .await?;
                outputs.insert(
                    format!("retire {}", family_file(family)),
                    format!("generation {} retired", old.as_str()),
                );
            }
            drop(service);
        }

        // Only the management-author PATs of the environment, which upgrades
        // issue (owner ruling of 2026-10-02 on stage 11).
        let pat = record
            .output(Stage::Packages, "management-author PAT prefix")?
            .to_owned();
        let subject = format!(
            "wamn-management-author-{}--{}--{}",
            self.arguments.org, self.arguments.project, self.arguments.environment
        );
        let older: Vec<String> = system
            .query(
                "SELECT p.token_prefix FROM identity.pats AS p
                   JOIN identity.principals AS r ON r.id = p.principal_id
                  WHERE r.subject = $1 AND p.revoked_at IS NULL AND p.token_prefix <> $2
                  ORDER BY p.token_prefix",
                &[&subject, &pat],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        for prefix in &older {
            crate::provision_project_env::revoke_provisioning_pat(&databases.system_url(), prefix)
                .await?;
        }
        outputs.insert("revoked PATs".to_owned(), older.join(", "));
        outputs.insert("current PAT".to_owned(), pat);
        if secrets.exists() {
            fs::remove_dir_all(&secrets)?;
        }
        Ok((StepResult::Done, outputs))
    }

    /// Stage 12: the run record is complete. It names its own file and the
    /// start of its first step.
    #[expect(
        clippy::unnecessary_wraps,
        reason = "every stage has the same result type"
    )]
    fn finish_record(&self, record: &RunRecord) -> Outcome {
        let first = record
            .steps
            .first()
            .map(|step| step.started_at.clone())
            .unwrap_or_default();
        let file = self.work.join("runs").join(format!(
            "{}--{}--{}.json",
            self.arguments.org, self.arguments.project, self.arguments.environment
        ));
        let outputs = BTreeMap::from([
            ("first step started".to_owned(), first),
            ("run record".to_owned(), file.display().to_string()),
        ]);
        Ok((StepResult::Done, outputs))
    }

    /// Runs the check pod of `deploy/gcp/evt-nats-check.yaml` and reads its log.
    async fn tap_exists(&self, kubectl: &Kubectl) -> anyhow::Result<bool> {
        let manifest = self.checkout.join("deploy/gcp/evt-nats-check.yaml");
        let manifest = manifest.display().to_string();
        kubectl
            .run(&[
                "-n",
                "platform",
                "delete",
                "pod",
                TAP_CHECK_POD,
                "--ignore-not-found",
            ])
            .await?;
        kubectl.run(&["apply", "-f", &manifest]).await?;
        let waited = kubectl
            .run(&[
                "-n",
                "platform",
                "wait",
                &format!("pod/{TAP_CHECK_POD}"),
                "--for=jsonpath={.status.phase}=Succeeded",
                "--timeout=120s",
            ])
            .await;
        let log = kubectl
            .run(&["-n", "platform", "logs", TAP_CHECK_POD])
            .await;
        let saved = self.work.join("logs").join("evt-nats-check.log");
        fs::write(&saved, log.as_deref().unwrap_or_default())?;
        kubectl
            .run(&[
                "-n",
                "platform",
                "delete",
                "pod",
                TAP_CHECK_POD,
                "--ignore-not-found",
            ])
            .await?;
        waited?;
        Ok(log?.contains(TAP_PRESENT))
    }

    /// Runs the tap-stream Job of `deploy/gcp/nats-jetstream.yaml` alone, so
    /// that no other document of that file changes.
    async fn run_tap_job(&self, kubectl: &Kubectl) -> anyhow::Result<()> {
        let file = self.checkout.join("deploy/gcp/nats-jetstream.yaml");
        let job = tap_job(
            &fs::read_to_string(&file).with_context(|| format!("read {}", file.display()))?,
        )?;
        let path = self.work.join("tap-job.yaml");
        fs::write(&path, job)?;
        let saved = self.work.join("logs").join("tap-job-previous.log");
        let previous = kubectl
            .run(&["-n", "platform", "logs", &format!("job/{TAP_JOB}")])
            .await;
        fs::write(&saved, previous.unwrap_or_default())?;
        kubectl
            .run(&[
                "-n",
                "platform",
                "delete",
                "job",
                TAP_JOB,
                "--ignore-not-found",
            ])
            .await?;
        kubectl
            .run(&["apply", "-f", &path.display().to_string()])
            .await?;
        let waited = kubectl
            .run(&[
                "-n",
                "platform",
                "wait",
                "--for=condition=complete",
                &format!("job/{TAP_JOB}"),
                "--timeout=180s",
            ])
            .await;
        let log = kubectl
            .run(&["-n", "platform", "logs", &format!("job/{TAP_JOB}")])
            .await;
        fs::write(
            self.work.join("logs").join("tap-job.log"),
            log.unwrap_or_default(),
        )?;
        waited.map(|_| ())
    }
}

/// The credentials of the gate service for one run: a new generation of the
/// control-author and management-admitter logins, and a new management-author
/// PAT (owner ruling of 2026-10-02 on `wamn-m511.5`).
struct GateCredentials {
    control_author: wamn_control_provision::CredentialGeneration,
    management_admitter: wamn_control_provision::CredentialGeneration,
    control_author_url: String,
    management_admitter_url: String,
    pat_token: String,
    pat_prefix: String,
}

impl GateCredentials {
    async fn prepare(
        run: &Run,
        kubectl: &Kubectl,
        databases: &Databases,
        record: &RunRecord,
    ) -> anyhow::Result<Self> {
        use crate::provision_project_env::{
            WorkloadActionRequest, WorkloadActionVerb, WorkloadGenerationAction,
            run_workload_action,
        };
        use wamn_control_provision::{CredentialGeneration, WorkloadRoleFamily};
        let secrets = run.secrets();
        private_directory(&secrets)?;
        let arguments = &run.arguments;
        let system = databases.system().await?;
        let mut generations = Vec::new();
        for (family, database, target) in [
            (
                WorkloadRoleFamily::ControlAuthor,
                databases.system_database.clone(),
                None,
            ),
            (
                WorkloadRoleFamily::ManagementAdmitter,
                databases.database.clone(),
                Some(databases.project_url()),
            ),
        ] {
            let role = |generation| match family {
                WorkloadRoleFamily::ControlAuthor => {
                    wamn_control_provision::control_author_generation_role(
                        &arguments.org,
                        &arguments.project,
                        &arguments.environment,
                        &database,
                        generation,
                    )
                }
                _ => wamn_control_provision::management_admitter_generation_role(
                    &arguments.org,
                    &arguments.project,
                    &arguments.environment,
                    &database,
                    generation,
                ),
            };
            let generation = next_generation(
                &system,
                &role(CredentialGeneration::A),
                &role(CredentialGeneration::B),
            )
            .await?;
            let file = secrets.join(format!("{}.json", family_file(family)));
            run_workload_action(&WorkloadActionRequest {
                org: arguments.org.clone(),
                project: arguments.project.clone(),
                env: arguments.environment.clone(),
                tenant: Some(databases.tenant.clone()),
                system_database_url: Some(databases.system_url()),
                target_admin_database_url: target,
                cluster: None,
                db_host: Some(databases.cluster_host.clone()),
                db_port: 5432,
                namespace: databases.secret_namespace.clone(),
                action: WorkloadGenerationAction {
                    family,
                    verb: WorkloadActionVerb::Prepare,
                    generation,
                },
                secret: Some(file.clone()),
                emit_role_sql: None,
                control_administration_patch: None,
            })
            .await?;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
            let url = secret_url(&file)?;
            redact_later(&url);
            generations.push((generation, databases.local(&url)?));
        }
        let [
            (control_author, control_author_url),
            (management_admitter, management_admitter_url),
        ] = <[_; 2]>::try_from(generations).map_err(|_| anyhow::anyhow!("two generations"))?;
        let (pat_token, pat_prefix) = mint_pat(run, kubectl, databases, record).await?;
        Ok(Self {
            control_author,
            management_admitter,
            control_author_url,
            management_admitter_url,
            pat_token,
            pat_prefix,
        })
    }
}

fn family_file(family: wamn_control_provision::WorkloadRoleFamily) -> &'static str {
    match family {
        wamn_control_provision::WorkloadRoleFamily::ControlAuthor => "control-author",
        _ => "management-admitter",
    }
}

/// The URL of an emitted credential Secret file.
fn secret_url(file: &Path) -> anyhow::Result<String> {
    use base64::Engine as _;
    let secret: Value = serde_json::from_slice(&fs::read(file)?)?;
    if let Some(url) = secret["stringData"]["url"].as_str() {
        return Ok(url.to_owned());
    }
    let encoded = secret["data"]["url"]
        .as_str()
        .with_context(|| format!("{} holds no url", file.display()))?;
    Ok(String::from_utf8(
        base64::engine::general_purpose::STANDARD.decode(encoded)?,
    )?)
}

/// The generation to prepare: the other one of the current generation, which
/// is the active login with the later expiry. With neither role present it is
/// `a`.
async fn next_generation(
    system: &tokio_postgres::Client,
    role_a: &str,
    role_b: &str,
) -> anyhow::Result<wamn_control_provision::CredentialGeneration> {
    use wamn_control_provision::CredentialGeneration;
    let rows = system
        .query(
            "SELECT rolname::text, rolcanlogin AND coalesce(rolvaliduntil > now(), true),
                    coalesce(extract(epoch FROM rolvaliduntil)::float8, 'infinity'::float8)
               FROM pg_roles WHERE rolname IN ($1, $2)",
            &[&role_a, &role_b],
        )
        .await?;
    let roles: Vec<(String, bool, f64)> = rows
        .iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect();
    choose_generation(&roles, role_a).map(|current| match current {
        None => CredentialGeneration::A,
        Some(current) => current.other(),
    })
}

/// The current generation among the roles of `a` and `b`, or none when
/// neither exists. Roles that exist but are all inactive refuse.
fn choose_generation(
    roles: &[(String, bool, f64)],
    role_a: &str,
) -> anyhow::Result<Option<wamn_control_provision::CredentialGeneration>> {
    use wamn_control_provision::CredentialGeneration;
    if roles.is_empty() {
        return Ok(None);
    }
    let current = roles
        .iter()
        .filter(|(_, active, _)| *active)
        .max_by(|a, b| a.2.total_cmp(&b.2))
        .with_context(|| format!("no generation of {role_a} is active"))?;
    Ok(Some(if current.0 == role_a {
        CredentialGeneration::A
    } else {
        CredentialGeneration::B
    }))
}

/// Issues a new management-author PAT through the Job of
/// `deploy/gcp/operator/mint-pat.yaml` with the ctl image of stage 3
/// (docs/operations/gcp.md §3.16). The PAT Secret file stays at mode 0600
/// in the private directory of the run.
async fn mint_pat(
    run: &Run,
    kubectl: &Kubectl,
    databases: &Databases,
    record: &RunRecord,
) -> anyhow::Result<(String, String)> {
    let arguments = &run.arguments;
    let template = fs::read_to_string(run.checkout.join("deploy/gcp/operator/mint-pat.yaml"))?;
    let job = mint_pat_job(
        &template,
        record.output(Stage::Images, "ctl")?,
        arguments,
        &databases.tenant,
        &databases.secret_namespace,
    )?;
    let previous = kubectl
        .run(&["-n", "identity", "logs", "job/mint-pat"])
        .await;
    fs::write(
        run.work.join("logs").join("mint-pat-previous.log"),
        previous.unwrap_or_default(),
    )?;
    kubectl
        .run(&[
            "-n",
            "identity",
            "delete",
            "job",
            "mint-pat",
            "--ignore-not-found",
            "--wait=true",
        ])
        .await?;
    let file = run.secrets().join("mint-pat.yaml");
    fs::write(&file, job)?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    let applied = kubectl
        .run(&["apply", "-f", &file.display().to_string()])
        .await;
    fs::remove_file(&file)?;
    applied?;
    let uid = kubectl
        .run(&[
            "-n",
            "identity",
            "get",
            "job",
            "mint-pat",
            "-o",
            "jsonpath={.metadata.uid}",
        ])
        .await?;
    // `kubectl create`, because `kubectl apply` copies the data into an annotation.
    let secret = serde_json::json!({
        "apiVersion": "v1",
        "kind": "Secret",
        "metadata": {
            "name": "wamn-system-admin",
            "namespace": "identity",
            "ownerReferences": [{"apiVersion": "batch/v1", "kind": "Job", "name": "mint-pat", "uid": uid.trim()}],
        },
        "stringData": {"url": databases.system_url_in_cluster()},
    });
    kubectl
        .input(&["create", "-f", "-"], &serde_json::to_vec(&secret)?)
        .await?;
    let mut ready = false;
    for _ in 0..60 {
        if kubectl
            .run(&[
                "-n",
                "identity",
                "exec",
                "job/mint-pat",
                "--",
                "test",
                "-e",
                "/out/pat.json",
            ])
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    ensure!(
        ready,
        "the mint-pat Job wrote no PAT Secret file within 120 s"
    );
    let pat = kubectl
        .run(&[
            "-n",
            "identity",
            "exec",
            "job/mint-pat",
            "--",
            "sh",
            "-c",
            "cat /out/pat.json && rm /out/pat.json",
        ])
        .await?;
    let path = run.secrets().join("management-author-pat.json");
    fs::write(&path, &pat)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let waited = kubectl
        .run(&[
            "-n",
            "identity",
            "wait",
            "--for=condition=complete",
            "job/mint-pat",
            "--timeout=60s",
        ])
        .await;
    let log = kubectl
        .run(&["-n", "identity", "logs", "job/mint-pat"])
        .await;
    fs::write(
        run.work.join("logs").join("mint-pat.log"),
        log.unwrap_or_default(),
    )?;
    kubectl
        .run(&[
            "-n",
            "identity",
            "delete",
            "job",
            "mint-pat",
            "--ignore-not-found",
        ])
        .await?;
    waited?;
    let secret: Value = serde_json::from_str(&pat).context("parse the PAT Secret file")?;
    let token = secret["stringData"]["token"]
        .as_str()
        .context("the PAT Secret file holds no token")?
        .to_owned();
    redact_later(&token);
    let prefix = secret["metadata"]["annotations"]["wamn.io/pat-prefix"]
        .as_str()
        .context("the PAT Secret file names no prefix")?
        .to_owned();
    Ok((token, prefix))
}

/// The mint-pat Job with the run values and the ctl image of the commit.
fn mint_pat_job(
    template: &str,
    ctl_image: &str,
    arguments: &RunArguments,
    tenant: &str,
    namespace: &str,
) -> anyhow::Result<String> {
    let mut images = 0;
    let lines: Vec<String> = template
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("image: ") {
                images += 1;
                let indent = &line[..line.len() - line.trim_start().len()];
                format!("{indent}image: {ctl_image}")
            } else {
                line.to_owned()
            }
        })
        .collect();
    ensure!(
        images == 1,
        "the mint-pat Job names {images} images, not one"
    );
    let job = lines
        .join("\n")
        .replace("__ORG__", &arguments.org)
        .replace("__PROJECT__", &arguments.project)
        .replace("__ENV__", &arguments.environment)
        .replace("__TENANT__", tenant)
        .replace("__NAMESPACE__", namespace)
        .replace("__PAT_FLAG__", "--emit-management-author-pat-secret");
    let body: String = job
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    ensure!(!body.contains("__"), "the mint-pat Job keeps a placeholder");
    Ok(job)
}

/// The gate service on this machine, from the stage 2 build. It ends with
/// this value.
struct GateService {
    port: u16,
    http: reqwest::Client,
    _child: Child,
}

impl GateService {
    async fn start(
        run: &Run,
        control_author_url: &str,
        management_admitter_url: &str,
        databases: &Databases,
    ) -> anyhow::Result<Self> {
        let arguments = &run.arguments;
        let reader = wamn_control_provision::workload_secret_name(
            wamn_control_provision::WorkloadRoleFamily::IdentityReader,
            &arguments.org,
            &arguments.project,
            &arguments.environment,
        );
        let kubectl_context = run.environment()?.context;
        let reader_url = Kubectl(kubectl_context)
            .run(&[
                "-n",
                &databases.secret_namespace,
                "get",
                "secret",
                &reader,
                "-o",
                "go-template={{.data.url | base64decode}}",
            ])
            .await?;
        redact_later(&reader_url);
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let log = run
            .work
            .join("logs")
            .join(format!("gate-{}.log", arguments.project));
        let output = fs::File::create(&log)?;
        let child = Command::new(run.programs().join("debug/wamn-scenario-worker"))
            .args(["serve", "--bind", &format!("127.0.0.1:{port}")])
            .env("WAMN_SYSTEM_URL", databases.local(&reader_url)?)
            .env("WAMN_CONTROL_AUTHORING_PG_URL", control_author_url)
            .env("WAMN_MANAGEMENT_ADMISSION_PG_URL", management_admitter_url)
            .env("WAMN_MANAGEMENT_ORG", &arguments.org)
            .env("WAMN_MANAGEMENT_PROJECT", &arguments.project)
            .env("WAMN_MANAGEMENT_ENVIRONMENT", &arguments.environment)
            .env("WAMN_MANAGEMENT_TENANT", &databases.tenant)
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(output)
            .kill_on_drop(true)
            .spawn()
            .context("start the gate service")?;
        for _ in 0..60 {
            if fs::read_to_string(&log)?.contains("listening") {
                return Ok(Self {
                    port,
                    http: reqwest::Client::new(),
                    _child: child,
                });
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        bail!(
            "the gate service did not listen within 60 s; its log is {}",
            log.display()
        )
    }

    /// Gates one wiring and returns its report id.
    async fn gate(
        &self,
        gate: &GateCredentials,
        input: &crate::release_composition::WiringInput,
        project: &str,
        environment: &str,
    ) -> anyhow::Result<String> {
        use wamn_authoring_model::{
            AuthoringCommand, AuthoringDocument, AuthoringRequest, AuthoringRequestEnvelope,
            AuthoringScope, Gate, SCHEMA_VERSION,
        };
        let request = AuthoringDocument::Request(Box::new(AuthoringRequestEnvelope::Command(
            AuthoringRequest {
                schema_version: SCHEMA_VERSION.to_owned(),
                command_id: format!(
                    "gate-{}-{}-{}",
                    input.package_id, input.package_version, input.wiring.wiring_id
                ),
                command: AuthoringCommand::Gate(Gate {
                    scope: AuthoringScope {
                        project_id: project.to_owned(),
                        environment: environment.to_owned(),
                    },
                    package_id: input.package_id.to_string(),
                    package_version: input.package_version.to_string(),
                    document: input.document.clone(),
                }),
            },
        )));
        let reply: Value = self
            .http
            .post(format!("http://127.0.0.1:{}/authoring", self.port))
            .bearer_auth(&gate.pat_token)
            .json(&request)
            .send()
            .await
            .context("post the gate request")?
            .json()
            .await
            .context("read the gate reply")?;
        let outcome = &reply["body"]["outcome"];
        ensure!(
            outcome["status"] == "completed",
            "the gate of {} answered {}",
            input.wiring.wiring_id,
            redact(&outcome.to_string())
        );
        outcome["value"]["result"]["report-id"]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("the gate of {} gave no report-id", input.wiring.wiring_id))
    }
}

/// Copies each active binding of the current release of the environment to
/// the new release id, with the digest that this run pushed for the same
/// component (owner ruling of 2026-10-02 on `wamn-m511.5`).
async fn copy_bindings(
    run: &Run,
    databases: &Databases,
    release_id: u32,
    pushed: &BTreeMap<String, String>,
) -> anyhow::Result<Vec<(String, String)>> {
    let client = databases.project().await?;
    let rows = client
        .query(
            "SELECT b.effective_release_id, b.component_digest, b.store_alias, b.instance_id,
                    i.requirement_type, g.definition_json::text, g.credential_set_handle,
                    (SELECT l.component FROM catalog.component_library AS l
                      WHERE l.tenant_id = b.tenant_id AND l.component_digest = b.component_digest
                      ORDER BY l.admitted_at DESC LIMIT 1)
               FROM catalog.effective_release_heads AS h
               JOIN catalog.connection_bindings AS b
                 ON b.tenant_id = h.tenant_id AND b.effective_release_id = h.effective_release_id
               JOIN catalog.connection_instances AS i
                 ON i.tenant_id = b.tenant_id AND i.environment = b.environment
                AND i.instance_id = b.instance_id
               JOIN catalog.connection_generations AS g
                 ON g.tenant_id = i.tenant_id AND g.environment = i.environment
                AND g.instance_id = i.instance_id AND g.generation = i.active_generation
              WHERE h.tenant_id = $1 AND h.environment = $2 AND b.binding_status = 'active'
              ORDER BY b.instance_id, b.store_alias",
            &[&databases.tenant, &run.arguments.environment],
        )
        .await?;
    let mut copied = Vec::new();
    for row in rows {
        let current: i32 = row.get(0);
        let old_digest: String = row.get(1);
        let store_alias: String = row.get(2);
        let instance_id: String = row.get(3);
        let requirement: String = row.get(4);
        let definition: String = row.get(5);
        let credential_handle: Option<String> = row.get(6);
        let component: Option<String> = row.get(7);
        let component = component
            .with_context(|| format!("no component of the tenant has the digest {old_digest}"))?;
        let new_digest = pushed.get(&component).with_context(|| {
            format!(
                "release {current} binds {instance_id} to the component {component}, which this run did not push"
            )
        })?;
        let file = run
            .release_files()
            .join(format!("{instance_id}.definition.json"));
        fs::write(&file, &definition)?;
        let bound = crate::bind_connection::bind(&crate::bind_connection::BindConnectionRequest {
            database_url: databases.project_url(),
            tenant: databases.tenant.clone(),
            environment: run.arguments.environment.clone(),
            instance_id: instance_id.clone(),
            requirement_type: serde_json::from_value(Value::String(requirement))?,
            definition: file,
            credential_handle,
            effective_release_id: release_id,
            component_digest: new_digest.clone(),
            store_alias: store_alias.clone(),
        })
        .await?;
        copied.push((
            instance_id,
            format!(
                "{store_alias}: release {current} {old_digest} to release {release_id} {new_digest}, generation {}",
                bound.generation
            ),
        ));
    }
    Ok(copied)
}

/// The Helm release of the hosts (docs/operations/gcp.md §3.14).
const HOST_RELEASE: &str = "wamn-host";
const HOST_NAMESPACE: &str = "hosts";
const HOST_CHART: &str = "oci://ghcr.io/wasmcloud/charts/runtime-operator";
const HOST_CHART_VERSION: &str = "2.10.0";
/// The host groups that `host_values_files` renders, in the order of its
/// release digest arguments.
const HOST_VALUES_GROUPS: [&str; 2] = ["default", "wms"];

async fn helm(context: &str, arguments: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("helm")
        .args(["--kube-context", context])
        .args(arguments)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .context("start helm")?;
    ensure!(
        output.status.success(),
        "helm {} exited {}: {}",
        arguments.join(" "),
        output.status,
        redact(&String::from_utf8_lossy(&output.stderr))
    );
    Ok(String::from_utf8(output.stdout)?)
}

/// The Google Cloud project of an Artifact Registry path.
fn gcp_project(registry: &str) -> anyhow::Result<String> {
    registry
        .split('/')
        .nth(1)
        .map(str::to_owned)
        .with_context(|| format!("the registry {registry} names no project"))
}

/// Sets the bucket path of the edge application of `host`. It reports
/// whether the values changed.
fn set_bucket_path(values: &mut Value, host: &str, bucket_path: &str) -> anyhow::Result<bool> {
    let application = values["applications"]
        .as_array_mut()
        .context("the edge values have no applications")?
        .iter_mut()
        .find(|application| application["host"] == host)
        .with_context(|| format!("the edge serves no host {host}"))?;
    if application["bucketPath"] == bucket_path {
        return Ok(false);
    }
    application["bucketPath"] = Value::from(bucket_path);
    Ok(true)
}

/// Points every rewrite of the path matcher of `host` that names a release
/// under `prefix` at the release `hex`. It reports whether the map changed.
fn rewrite_url_map(
    map: &mut serde_yaml::Value,
    host: &str,
    prefix: &str,
    hex: &str,
) -> anyhow::Result<bool> {
    let matcher = map["hostRules"]
        .as_sequence()
        .context("the URL map has no hostRules")?
        .iter()
        .find(|rule| {
            rule["hosts"]
                .as_sequence()
                .is_some_and(|hosts| hosts.iter().any(|name| name.as_str() == Some(host)))
        })
        .and_then(|rule| rule["pathMatcher"].as_str())
        .with_context(|| format!("the URL map has no host rule for {host}"))?
        .to_owned();
    let matcher = map["pathMatchers"]
        .as_sequence_mut()
        .context("the URL map has no pathMatchers")?
        .iter_mut()
        .find(|candidate| candidate["name"].as_str() == Some(matcher.as_str()))
        .with_context(|| format!("the URL map has no path matcher {matcher}"))?;
    let mut changed = false;
    let mut found = 0;
    for rule in matcher["routeRules"]
        .as_sequence_mut()
        .into_iter()
        .flatten()
    {
        for key in ["pathPrefixRewrite", "pathTemplateRewrite"] {
            let rewrite = &mut rule["routeAction"]["urlRewrite"][key];
            let Some(path) = rewrite.as_str() else {
                continue;
            };
            let Some(rest) = path.strip_prefix(prefix) else {
                continue;
            };
            let (old, tail) = rest
                .split_at_checked(64)
                .context("a rewrite names a short release")?;
            ensure!(
                old.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "the rewrite {path} names no release digest"
            );
            found += 1;
            if old != hex {
                *rewrite = serde_yaml::Value::from(format!("{prefix}{hex}{tail}"));
                changed = true;
            }
        }
    }
    ensure!(
        found > 0,
        "the path matcher of {host} rewrites to no release under {prefix}"
    );
    Ok(changed)
}

/// The release manifest digest in the live host values of one host group.
fn release_digest(values: &Value, group: &str) -> anyhow::Result<String> {
    values["runtime"]["hostGroups"]
        .as_array()
        .context("the live host values have no runtime.hostGroups")?
        .iter()
        .find(|candidate| candidate["name"] == group)
        .with_context(|| format!("the live host values have no host group {group}"))?["extraArgs"]
        .as_array()
        .into_iter()
        .flatten()
        .find_map(|argument| {
            argument
                .as_str()?
                .strip_prefix("--release-manifest-digest=")
        })
        .map(str::to_owned)
        .with_context(|| format!("the host group {group} names no release manifest digest"))
}

/// The names of the WorkloadDeployment documents of a workload file.
fn workload_names(file: &str) -> anyhow::Result<Vec<String>> {
    let mut names = Vec::new();
    for document in serde_yaml::Deserializer::from_str(file) {
        let value = <serde_yaml::Value as serde::Deserialize>::deserialize(document)?;
        if value["kind"] == "WorkloadDeployment" {
            names.push(
                value["metadata"]["name"]
                    .as_str()
                    .context("a WorkloadDeployment has no name")?
                    .to_owned(),
            );
        }
    }
    ensure!(
        !names.is_empty(),
        "a workload file holds no WorkloadDeployment"
    );
    Ok(names)
}

/// Each HTTP route of the release with its method and path.
fn serve_routes(manifest: &wamn_catalog::ServingManifest) -> anyhow::Result<Vec<(String, String)>> {
    let mut routes = Vec::new();
    for (name, attachment) in &manifest.attachments {
        let Some(path) = attachment.definition["route"]["path"].as_str() else {
            continue;
        };
        let method = match attachment.definition["route"]["method"].as_str() {
            Some(method) => method.to_owned(),
            None => manifest
                .routes
                .iter()
                .find(|route| route.operation == attachment.operation)
                .map(|route| route.type_.http_method().to_owned())
                .with_context(|| format!("the attachment {name} calls no route of the release"))?,
        };
        routes.push((method, path.to_owned()));
    }
    ensure!(!routes.is_empty(), "the release has no HTTP route");
    Ok(routes)
}

/// The path of the serve check that no release serves.
const UNKNOWN_PATH: &str = "/wamn-upgrade-unknown-path";

/// Each released route answers 401 without a credential, and an unknown
/// path answers 404.
async fn serve_check(port: u16, host: &str, routes: &[(String, String)]) -> anyhow::Result<String> {
    let http = reqwest::Client::new();
    let mut answers = Vec::new();
    let unknown = ("GET".to_owned(), UNKNOWN_PATH.to_owned());
    for (method, path) in routes.iter().chain([&unknown]) {
        let expected = if path == UNKNOWN_PATH { 404 } else { 401 };
        let status = http
            .request(method.parse()?, format!("http://127.0.0.1:{port}{path}"))
            .header(reqwest::header::HOST, host)
            .send()
            .await
            .with_context(|| format!("{method} {path}"))?
            .status()
            .as_u16();
        ensure!(
            status == expected,
            "{method} {path} on {host} answered {status}, not {expected}"
        );
        answers.push(format!("{method} {path} {status}"));
    }
    Ok(answers.join(", "))
}

/// A port-forward to port 80 of a Service, on a free local port.
struct ServiceForward {
    port: u16,
    _child: Child,
}

impl ServiceForward {
    async fn open(kubectl: &Kubectl, namespace: &str, service: &str) -> anyhow::Result<Self> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let mut child = kubectl
            .command(&[
                "-n",
                namespace,
                "port-forward",
                &format!("svc/{service}"),
                &format!("{port}:80"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start kubectl port-forward")?;
        let stdout = child.stdout.take().context("port-forward output")?;
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let first = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .context("the port-forward did not start within 30 s")??;
        ensure!(
            first.is_some_and(|line| line.starts_with("Forwarding from")),
            "the port-forward to {service} did not start"
        );
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Ok(Self {
            port,
            _child: child,
        })
    }
}

fn done(already: bool) -> StepResult {
    if already {
        StepResult::AlreadyDone
    } else {
        StepResult::Done
    }
}

fn private_directory(path: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("set the mode of {}", path.display()))
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(hex::encode(ring::digest::digest(
        &ring::digest::SHA256,
        &bytes,
    )))
}

async fn git(directory: &Path, arguments: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .await
        .context("start git")?;
    ensure!(
        output.status.success(),
        "git {} exited {}: {}",
        arguments.join(" "),
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

/// kubectl with the explicit context of the environment file.
struct Kubectl(String);

impl Kubectl {
    fn command(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new("kubectl");
        command
            .args(["--context", &self.0])
            .args(arguments)
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    async fn run(&self, arguments: &[&str]) -> anyhow::Result<String> {
        let output = self
            .command(arguments)
            .output()
            .await
            .context("start kubectl")?;
        ensure!(
            output.status.success(),
            "kubectl {} exited {}: {}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(String::from_utf8(output.stdout)?)
    }

    /// Runs kubectl with `input` on its standard input. The input never
    /// reaches the command line or a log.
    async fn input(&self, arguments: &[&str], input: &[u8]) -> anyhow::Result<String> {
        use tokio::io::AsyncWriteExt as _;
        let mut child = self
            .command(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start kubectl")?;
        let mut stdin = child.stdin.take().context("kubectl input")?;
        stdin.write_all(input).await?;
        drop(stdin);
        let output = child.wait_with_output().await?;
        ensure!(
            output.status.success(),
            "kubectl {} exited {}: {}",
            arguments.join(" "),
            output.status,
            redact(&String::from_utf8_lossy(&output.stderr))
        );
        Ok(String::from_utf8(output.stdout)?)
    }

    async fn json(&self, arguments: &[&str]) -> anyhow::Result<Value> {
        let mut arguments = arguments.to_vec();
        arguments.extend(["-o", "json"]);
        Ok(serde_json::from_str(&self.run(&arguments).await?)?)
    }
}

/// A login to the registry of the environment file with a token of
/// `gcloud auth print-access-token`. The Docker configuration lives in a
/// private directory of the work directory and is removed with this value.
struct Registry {
    host: String,
    /// The `<project>/<repository>` path below the host.
    path: String,
    token: String,
    config: PathBuf,
    http: reqwest::Client,
}

impl Registry {
    async fn login(registry: &str, work: &Path) -> anyhow::Result<Self> {
        let (host, path) = registry
            .split_once('/')
            .with_context(|| format!("the registry {registry} has no path"))?;
        let project = path
            .split('/')
            .next()
            .with_context(|| format!("the registry {registry} names no project"))?;
        let output = Command::new("gcloud")
            .args(["auth", "print-access-token", "--project", project])
            .stdin(Stdio::null())
            .output()
            .await
            .context("start gcloud")?;
        ensure!(
            output.status.success(),
            "gcloud auth print-access-token exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
        let token = String::from_utf8(output.stdout)?.trim().to_owned();
        redact_later(&token);
        let config = work.join(format!("docker-{}", std::process::id()));
        private_directory(&config)?;
        let registry = Self {
            host: host.to_owned(),
            path: path.to_owned(),
            token,
            config,
            http: reqwest::Client::new(),
        };
        let file = registry.config.join("config.json");
        fs::write(&file, docker_config(&registry.host, &registry.token))?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        Ok(registry)
    }

    /// A command that reads the login from the private Docker configuration.
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.env("DOCKER_CONFIG", &self.config);
        command
    }

    fn docker(&self) -> Command {
        self.command("docker")
    }

    /// The `.dockerconfigjson` file that the push verbs read.
    fn auth_file(&self) -> PathBuf {
        self.config.join("config.json")
    }

    /// The manifest digest of a tag, or none when the tag does not exist.
    async fn digest(&self, repository: &str, tag: &str) -> anyhow::Result<Option<String>> {
        let url = format!(
            "https://{}/v2/{}/{repository}/manifests/{tag}",
            self.host, self.path
        );
        let response = self
            .http
            .head(&url)
            .basic_auth("oauth2accesstoken", Some(&self.token))
            .header(
                reqwest::header::ACCEPT,
                "application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, \
                 application/vnd.docker.distribution.manifest.list.v2+json, \
                 application/vnd.docker.distribution.manifest.v2+json",
            )
            .send()
            .await
            .with_context(|| format!("ask the registry for {url}"))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status().is_success(),
            "the registry answered {} for {url}",
            response.status()
        );
        let digest = response
            .headers()
            .get("docker-content-digest")
            .and_then(|value| value.to_str().ok())
            .with_context(|| format!("the registry named no digest for {url}"))?;
        ensure!(
            digest.starts_with("sha256:") && digest.len() == 71,
            "the registry named the digest {digest} for {url}"
        );
        Ok(Some(digest.to_owned()))
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.config);
    }
}

fn docker_config(host: &str, token: &str) -> String {
    use base64::Engine as _;
    let auth =
        base64::engine::general_purpose::STANDARD.encode(format!("oauth2accesstoken:{token}"));
    let mut auths = serde_json::Map::new();
    auths.insert(host.to_owned(), serde_json::json!({ "auth": auth }));
    serde_json::json!({ "auths": auths }).to_string()
}

fn host_values(checkout: &Path) -> anyhow::Result<serde_yaml::Value> {
    let path = checkout.join("deploy/gcp/values-host.yaml");
    serde_yaml::from_str(
        &fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))
}

/// CPU in millicores and memory in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Resources {
    cpu: u64,
    memory: u64,
}

/// One request per replica of each host group of the host values. One Helm
/// upgrade changes every group, and each Deployment starts one new pod
/// before it stops an old one.
fn host_group_requests(values: &serde_yaml::Value) -> anyhow::Result<Vec<(String, Resources)>> {
    let groups = values["runtime"]["hostGroups"]
        .as_sequence()
        .context("the host values have no runtime.hostGroups")?;
    groups
        .iter()
        .map(|group| {
            let name = group["name"].as_str().context("a host group has no name")?;
            let requests = &group["resources"]["requests"];
            Ok((
                name.to_owned(),
                Resources {
                    cpu: cpu(requests["cpu"]
                        .as_str()
                        .with_context(|| format!("{name} requests no cpu"))?)?,
                    memory: memory(
                        requests["memory"]
                            .as_str()
                            .with_context(|| format!("{name} requests no memory"))?,
                    )?,
                },
            ))
        })
        .collect()
}

/// The allocatable capacity of each Ready node less the requests of the
/// pods that run on it.
fn free_capacity(nodes: &Value, pods: &Value) -> anyhow::Result<Vec<(String, Resources)>> {
    let mut free = Vec::new();
    for node in nodes["items"].as_array().context("no node list")? {
        if !ready(node) || node["spec"]["unschedulable"].as_bool() == Some(true) {
            continue;
        }
        let name = node["metadata"]["name"]
            .as_str()
            .context("a node has no name")?;
        let allocatable = &node["status"]["allocatable"];
        let mut left = Resources {
            cpu: cpu(allocatable["cpu"].as_str().context("no allocatable cpu")?)?,
            memory: memory(
                allocatable["memory"]
                    .as_str()
                    .context("no allocatable memory")?,
            )?,
        };
        for pod in pods["items"].as_array().context("no pod list")? {
            let phase = pod["status"]["phase"].as_str().unwrap_or_default();
            if pod["spec"]["nodeName"].as_str() != Some(name)
                || matches!(phase, "Succeeded" | "Failed")
            {
                continue;
            }
            for container in pod["spec"]["containers"].as_array().into_iter().flatten() {
                let requests = &container["resources"]["requests"];
                if let Some(value) = requests["cpu"].as_str() {
                    left.cpu = left.cpu.saturating_sub(cpu(value)?);
                }
                if let Some(value) = requests["memory"].as_str() {
                    left.memory = left.memory.saturating_sub(memory(value)?);
                }
            }
        }
        free.push((name.to_owned(), left));
    }
    ensure!(
        !free.is_empty(),
        "the cluster has no Ready schedulable node"
    );
    Ok(free)
}

/// Places one new pod of each host group, largest CPU first, on the node
/// with the most free CPU that holds it. A pod that fits no node refuses.
fn place(groups: &[(String, Resources)], mut free: Vec<(String, Resources)>) -> anyhow::Result<()> {
    let mut groups = groups.to_vec();
    groups.sort_by(|a, b| b.1.cpu.cmp(&a.1.cpu).then(b.1.memory.cmp(&a.1.memory)));
    for (group, request) in &groups {
        let summary = format!("{free:?}");
        let node = free
            .iter_mut()
            .filter(|(_, left)| left.cpu >= request.cpu && left.memory >= request.memory)
            .max_by_key(|(_, left)| left.cpu)
            .with_context(|| {
                format!(
                    "no node has room for a new pod of host group {group} ({}m CPU, {} bytes); free: {summary}",
                    request.cpu, request.memory
                )
            })?;
        node.1.cpu -= request.cpu;
        node.1.memory -= request.memory;
    }
    Ok(())
}

fn ready(object: &Value) -> bool {
    object["status"]["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|condition| condition["type"] == "Ready" && condition["status"] == "True")
}

/// A Kubernetes CPU quantity in millicores.
fn cpu(quantity: &str) -> anyhow::Result<u64> {
    if let Some(milli) = quantity.strip_suffix('m') {
        return milli
            .parse()
            .with_context(|| format!("the CPU quantity {quantity}"));
    }
    let cores: f64 = quantity
        .parse()
        .with_context(|| format!("the CPU quantity {quantity}"))?;
    ensure!(
        (0.0..1e9).contains(&cores),
        "the CPU quantity {quantity} is out of range"
    );
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "the range check above bounds the value"
    )]
    let millicores = (cores * 1000.0).round() as u64;
    Ok(millicores)
}

/// A Kubernetes memory quantity in bytes.
fn memory(quantity: &str) -> anyhow::Result<u64> {
    const UNITS: [(&str, u64); 8] = [
        ("Ki", 1 << 10),
        ("Mi", 1 << 20),
        ("Gi", 1 << 30),
        ("Ti", 1 << 40),
        ("k", 1_000),
        ("M", 1_000_000),
        ("G", 1_000_000_000),
        ("T", 1_000_000_000_000),
    ];
    let (number, unit) = UNITS
        .iter()
        .find_map(|(suffix, unit)| quantity.strip_suffix(suffix).map(|number| (number, *unit)))
        .unwrap_or((quantity, 1));
    let number: u64 = number
        .parse()
        .with_context(|| format!("the memory quantity {quantity}"))?;
    number
        .checked_mul(unit)
        .with_context(|| format!("the memory quantity {quantity} is too large"))
}

/// The Job document of the tap stream, alone.
fn tap_job(file: &str) -> anyhow::Result<String> {
    let mut found = None;
    for document in serde_yaml::Deserializer::from_str(file) {
        let value = <serde_yaml::Value as serde::Deserialize>::deserialize(document)?;
        if value["kind"] == "Job" && value["metadata"]["name"] == TAP_JOB {
            ensure!(found.is_none(), "the file has two Jobs {TAP_JOB}");
            found = Some(serde_yaml::to_string(&value)?);
        }
    }
    found.with_context(|| format!("the file has no Job {TAP_JOB}"))
}

/// The superuser connections of the system database and the project
/// database of one environment, through one pod port-forward. The password
/// stays in memory, and every recorded cause is redacted of it.
struct Databases {
    forward: PortForward,
    password: String,
    system_database: String,
    /// The in-cluster host that an emitted credential names.
    cluster_host: String,
    database: String,
    tenant: String,
    /// `registry.project_envs.secret_namespace`, where the credential Secrets live.
    secret_namespace: String,
}

impl Databases {
    async fn open(
        environment: &EnvironmentFile,
        kubectl: &Kubectl,
        arguments: &RunArguments,
    ) -> anyhow::Result<Self> {
        let system = &environment.system_database;
        let forward = PortForward::open(kubectl, &system.namespace, &system.cluster).await?;
        let password = kubectl
            .run(&[
                "-n",
                &system.namespace,
                "get",
                "secret",
                &format!("{}-superuser", system.cluster),
                "-o",
                "go-template={{.data.password | base64decode}}",
            ])
            .await?;
        redact_later(&password);
        let client = forward.connect(&system.database, &password).await?;
        let row = client
            .query_opt(
                "SELECT o.placement_type, o.pool_cluster, e.instance_suffix,
                        coalesce(e.secret_namespace, '')
                   FROM registry.project_envs AS e JOIN registry.orgs AS o ON o.id = e.org
                  WHERE e.org = $1 AND e.project = $2 AND e.env = $3",
                &[&arguments.org, &arguments.project, &arguments.environment],
            )
            .await?
            .with_context(|| {
                format!(
                    "registry.project_envs has no environment {}/{}/{}",
                    arguments.org, arguments.project, arguments.environment
                )
            })?;
        let placement: String = row.get(0);
        let pool: Option<String> = row.get(1);
        let suffix: String = row.get(2);
        let secret_namespace: String = row.get(3);
        ensure!(
            placement == "pooled" && pool.as_deref() == Some(system.cluster.as_str()),
            "the org {} is {placement} on {pool:?}; the verb reaches only the cluster {}",
            arguments.org,
            system.cluster
        );
        ensure!(
            !secret_namespace.is_empty(),
            "registry.project_envs names no secret namespace for the environment"
        );
        let database = wamn_control_provision::project_env_database_name(
            &arguments.org,
            &arguments.project,
            &arguments.environment,
            &suffix,
        );
        // The control store copy of the tenant lives in the system database
        // (deploy/sql/control-portable-store.sql), not in the project database.
        let tenant: String = client
            .query_one(
                "SELECT tenant_id FROM catalog.tenant_environments
                  WHERE org = $1 AND project = $2 AND env = $3",
                &[&arguments.org, &arguments.project, &arguments.environment],
            )
            .await
            .with_context(|| format!("read the tenant of {database}"))?
            .get(0);
        Ok(Self {
            password,
            system_database: system.database.clone(),
            cluster_host: format!(
                "{}-rw.{}.svc.cluster.local",
                system.cluster, system.namespace
            ),
            database,
            tenant,
            secret_namespace,
            forward,
        })
    }

    fn url(&self, database: &str) -> String {
        format!(
            "postgresql://postgres:{}@127.0.0.1:{}/{database}?sslmode=disable",
            percent_encode(&self.password),
            self.forward.port
        )
    }

    fn system_url(&self) -> String {
        self.url(&self.system_database)
    }

    fn project_url(&self) -> String {
        self.url(&self.database)
    }

    /// The superuser URL of the system database as a pod in the cluster
    /// reaches it.
    fn system_url_in_cluster(&self) -> String {
        format!(
            "postgresql://postgres:{}@{}:5432/{}",
            percent_encode(&self.password),
            self.cluster_host,
            self.system_database
        )
    }

    async fn system(&self) -> anyhow::Result<tokio_postgres::Client> {
        self.forward
            .connect(&self.system_database, &self.password)
            .await
    }

    async fn project(&self) -> anyhow::Result<tokio_postgres::Client> {
        self.forward.connect(&self.database, &self.password).await
    }

    /// An emitted credential URL that names the in-cluster host, pointed at
    /// the forward for a process on this machine.
    fn local(&self, url: &str) -> anyhow::Result<String> {
        let in_cluster = format!("@{}:5432/", self.cluster_host);
        ensure!(
            url.matches(&in_cluster).count() == 1,
            "a credential URL does not name the host {}",
            self.cluster_host
        );
        Ok(url.replace(&in_cluster, &format!("@127.0.0.1:{}/", self.forward.port)))
    }
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// Every credential that this process read. A recorded cause never shows one.
static SECRETS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

fn redact_later(secret: &str) {
    if secret.len() >= 8
        && let Ok(mut secrets) = SECRETS.lock()
    {
        secrets.push(secret.to_owned());
        secrets.push(percent_encode(secret));
    }
}

fn redact(text: &str) -> String {
    let mut text = text.to_owned();
    if let Ok(secrets) = SECRETS.lock() {
        for secret in secrets.iter() {
            text = text.replace(secret.as_str(), "[redacted]");
        }
    }
    text
}

struct NextRelease {
    /// Each attested release id with its source commit.
    attested: Vec<(i32, String)>,
    next: i32,
}

/// The attestations and releases of the tenant. The next free id is one
/// above every release of the tenant.
async fn next_release(
    databases: &Databases,
    arguments: &RunArguments,
) -> anyhow::Result<NextRelease> {
    // The attestations live in the control store of the system database
    // (deploy/sql/control-portable-store.sql), the releases in the project database.
    let attested = databases
        .system()
        .await?
        .query(
            "SELECT effective_release_id, coalesce(source_commit, '') FROM catalog.deployment_attestations
              WHERE tenant_id = $1 AND org_id = $2 AND project_id = $3 AND environment = $4
              ORDER BY effective_release_id",
            &[&databases.tenant, &arguments.org, &arguments.project, &arguments.environment],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    let highest: i32 = databases
        .project()
        .await?
        .query_one(
            "SELECT coalesce(max(effective_release_id), 0) FROM catalog.effective_releases
              WHERE tenant_id = $1",
            &[&databases.tenant],
        )
        .await?
        .get(0);
    Ok(NextRelease {
        attested,
        next: highest + 1,
    })
}

/// A port-forward to the primary pod of a CloudNativePG cluster, on a free
/// local port. A pod forward, because a Service forward resets connections
/// (`docs/plan/kind-to-type.md` B0). The forward ends with this value.
struct PortForward {
    port: u16,
    _child: Child,
}

impl PortForward {
    async fn open(kubectl: &Kubectl, namespace: &str, cluster: &str) -> anyhow::Result<Self> {
        let primary = kubectl
            .run(&[
                "-n",
                namespace,
                "get",
                "pods",
                "-l",
                &format!("cnpg.io/cluster={cluster},cnpg.io/instanceRole=primary"),
                "-o",
                "jsonpath={.items[*].metadata.name}",
            ])
            .await?;
        let primary: Vec<&str> = primary.split_whitespace().collect();
        let [primary] = primary.as_slice() else {
            bail!("the cluster {cluster} has {} primary pods", primary.len());
        };
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let mut child = kubectl
            .command(&[
                "-n",
                namespace,
                "port-forward",
                &format!("pod/{primary}"),
                &format!("{port}:5432"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start kubectl port-forward")?;
        let stdout = child.stdout.take().context("port-forward output")?;
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let first = tokio::time::timeout(Duration::from_secs(30), lines.next_line())
            .await
            .context("the port-forward did not start within 30 s")??;
        ensure!(
            first.is_some_and(|line| line.starts_with("Forwarding from")),
            "the port-forward to {primary} did not start"
        );
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
        Ok(Self {
            port,
            _child: child,
        })
    }

    /// A superuser connection without TLS: the forward is the authenticated
    /// kubectl stream.
    async fn connect(
        &self,
        database: &str,
        password: &str,
    ) -> anyhow::Result<tokio_postgres::Client> {
        let (client, connection) = tokio_postgres::Config::new()
            .host("127.0.0.1")
            .port(self.port)
            .user("postgres")
            .password(password)
            .dbname(database)
            .ssl_mode(tokio_postgres::config::SslMode::Disable)
            .connect(tokio_postgres::NoTls)
            .await
            .with_context(|| format!("connect to {database}"))?;
        tokio::spawn(connection);
        Ok(client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quantities_read_in_millicores_and_bytes() {
        assert_eq!(cpu("500m").unwrap(), 500);
        assert_eq!(cpu("2").unwrap(), 2000);
        assert_eq!(cpu("1.5").unwrap(), 1500);
        assert_eq!(memory("256Mi").unwrap(), 256 << 20);
        assert_eq!(memory("5933980Ki").unwrap(), 5_933_980 << 10);
        assert_eq!(memory("1G").unwrap(), 1_000_000_000);
        assert_eq!(memory("1024").unwrap(), 1024);
        assert!(cpu("x").is_err() && memory("1Xi").is_err());
    }

    fn node(name: &str, cpu: &str, memory: &str, ready: &str) -> Value {
        json!({
            "metadata": {"name": name},
            "status": {
                "allocatable": {"cpu": cpu, "memory": memory},
                "conditions": [{"type": "Ready", "status": ready}],
            },
        })
    }

    fn pod(node: &str, cpu: &str, memory: &str, phase: &str) -> Value {
        json!({
            "spec": {"nodeName": node, "containers": [{"resources": {"requests": {"cpu": cpu, "memory": memory}}}]},
            "status": {"phase": phase},
        })
    }

    #[test]
    fn free_capacity_counts_running_pods_of_ready_nodes() {
        let nodes = json!({"items": [
            node("a", "1930m", "4Gi", "True"),
            node("b", "1930m", "4Gi", "False"),
        ]});
        let pods = json!({"items": [
            pod("a", "500m", "1Gi", "Running"),
            pod("a", "900m", "1Gi", "Succeeded"),
            pod("b", "100m", "1Gi", "Running"),
        ]});
        assert_eq!(
            free_capacity(&nodes, &pods).unwrap(),
            [(
                "a".to_owned(),
                Resources {
                    cpu: 1430,
                    memory: 3 << 30
                }
            )]
        );
    }

    #[test]
    fn every_host_group_needs_room_for_one_new_pod() {
        let group = |name: &str| {
            (
                name.to_owned(),
                Resources {
                    cpu: 500,
                    memory: 256 << 20,
                },
            )
        };
        let groups = [group("default"), group("wms"), group("control")];
        let node = |name: &str, cpu| {
            (
                name.to_owned(),
                Resources {
                    cpu,
                    memory: 1 << 30,
                },
            )
        };
        assert!(place(&groups, vec![node("a", 1000), node("b", 500)]).is_ok());
        let error = place(&groups, vec![node("a", 900), node("b", 600)]).unwrap_err();
        assert!(
            format!("{error:#}").contains("no node has room"),
            "{error:#}"
        );
    }

    #[test]
    fn the_host_values_of_wamn_dev_name_three_groups() {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let groups = host_group_requests(&host_values(checkout).unwrap()).unwrap();
        let names: Vec<&str> = groups.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["default", "wms", "control"]);
    }

    #[test]
    fn the_tap_job_is_taken_alone_from_its_file() {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let file = fs::read_to_string(checkout.join("deploy/gcp/nats-jetstream.yaml")).unwrap();
        let job: serde_yaml::Value = serde_yaml::from_str(&tap_job(&file).unwrap()).unwrap();
        assert_eq!(job["kind"], "Job");
        assert_eq!(job["metadata"]["name"], TAP_JOB);
        assert!(tap_job("kind: ConfigMap\n").is_err());
    }

    #[test]
    fn the_docker_configuration_holds_only_the_registry_login() {
        let config: Value =
            serde_json::from_str(&docker_config("us-central1-docker.pkg.dev", "token")).unwrap();
        assert_eq!(
            config,
            json!({"auths": {"us-central1-docker.pkg.dev": {"auth": "b2F1dGgyYWNjZXNzdG9rZW46dG9rZW4="}}})
        );
    }

    #[test]
    fn the_next_generation_follows_the_active_login_with_the_later_expiry() {
        use wamn_control_provision::CredentialGeneration;
        let role = |name: &str, active, until| (name.to_owned(), active, until);
        assert_eq!(choose_generation(&[], "r_a").unwrap(), None);
        assert_eq!(
            choose_generation(&[role("r_a", true, 10.0), role("r_b", false, 0.0)], "r_a").unwrap(),
            Some(CredentialGeneration::A)
        );
        assert_eq!(
            choose_generation(&[role("r_a", true, 10.0), role("r_b", true, 20.0)], "r_a").unwrap(),
            Some(CredentialGeneration::B)
        );
        assert!(choose_generation(&[role("r_a", false, 0.0)], "r_a").is_err());
    }

    #[test]
    fn the_mint_job_takes_the_run_values_and_the_ctl_image_of_the_commit() {
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let template =
            fs::read_to_string(checkout.join("deploy/gcp/operator/mint-pat.yaml")).unwrap();
        let arguments = RunArguments::new("dkk", "wms", "dev", &"a".repeat(40)).unwrap();
        let image = "us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-ctl:src-x@sha256:00";
        let job = mint_pat_job(&template, image, &arguments, "wms", "hosts").unwrap();
        let document: serde_yaml::Value = serde_yaml::from_str(&job).unwrap();
        let container = &document["spec"]["template"]["spec"]["containers"][0];
        assert_eq!(container["image"], image);
        let script = container["args"][0].as_str().unwrap();
        assert!(
            script.contains("--org dkk --project wms --env dev --tenant wms"),
            "{script}"
        );
        assert!(script.contains("--namespace hosts"), "{script}");
        assert!(
            script.contains("--emit-management-author-pat-secret /out/pat.json"),
            "{script}"
        );
    }

    #[test]
    fn a_recorded_cause_never_shows_a_credential() {
        redact_later("pass/word:with@signs");
        let url = format!(
            "postgresql://postgres:{}@127.0.0.1:1/x",
            percent_encode("pass/word:with@signs")
        );
        assert_eq!(
            redact(&format!("connect to {url} failed for pass/word:with@signs")),
            "connect to postgresql://postgres:[redacted]@127.0.0.1:1/x failed for [redacted]"
        );
    }

    #[test]
    fn an_emitted_secret_names_its_url_in_string_data_or_data() {
        let directory =
            std::env::temp_dir().join(format!("wamn-upgrade-secret-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let plain = directory.join("plain.json");
        fs::write(
            &plain,
            r#"{"stringData":{"url":"postgresql://u@h:5432/d"}}"#,
        )
        .unwrap();
        assert_eq!(secret_url(&plain).unwrap(), "postgresql://u@h:5432/d");
        let encoded = directory.join("encoded.json");
        fs::write(
            &encoded,
            r#"{"data":{"url":"cG9zdGdyZXNxbDovL3VAaDo1NDMyL2Q="}}"#,
        )
        .unwrap();
        assert_eq!(secret_url(&encoded).unwrap(), "postgresql://u@h:5432/d");
        fs::remove_dir_all(&directory).unwrap();
    }

    fn repository() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap()
    }

    #[test]
    fn the_edge_and_the_url_map_change_only_the_route_host_of_the_run() {
        let edge: serde_yaml::Value = serde_yaml::from_str(
            &fs::read_to_string(repository().join("deploy/gcp/values-edge.yaml")).unwrap(),
        )
        .unwrap();
        let mut edge: Value = serde_json::to_value(edge).unwrap();
        let before = edge.clone();
        let path = format!("wamn-dev-web/clients/wamn_wms/{}", "c".repeat(64));
        assert!(set_bucket_path(&mut edge, "wms.wamn.dev", &path).unwrap());
        assert!(!set_bucket_path(&mut edge, "wms.wamn.dev", &path).unwrap());
        assert_eq!(edge["applications"][0], before["applications"][0]);
        assert_eq!(edge["applications"][1]["bucketPath"], path.as_str());
        assert!(set_bucket_path(&mut edge, "absent.wamn.dev", &path).is_err());

        let mut map: serde_yaml::Value = serde_yaml::from_str(
            &fs::read_to_string(repository().join("deploy/gcp/url-map.yaml")).unwrap(),
        )
        .unwrap();
        let receiving = serde_yaml::to_string(&map["pathMatchers"][0]).unwrap();
        let hex = "c".repeat(64);
        assert!(rewrite_url_map(&mut map, "wms.wamn.dev", "/clients/wamn_wms/", &hex).unwrap());
        assert!(!rewrite_url_map(&mut map, "wms.wamn.dev", "/clients/wamn_wms/", &hex).unwrap());
        assert_eq!(
            serde_yaml::to_string(&map["pathMatchers"][0]).unwrap(),
            receiving
        );
        let wms = serde_yaml::to_string(&map["pathMatchers"][1]).unwrap();
        // /assets/, /config.json (wamn-l2fi), / and every other page.
        assert_eq!(wms.matches(&hex).count(), 4, "{wms}");
    }

    #[test]
    fn the_live_host_values_name_each_group_release() {
        let values: serde_yaml::Value = serde_yaml::from_str(
            &fs::read_to_string(repository().join("deploy/gcp/values-host.yaml")).unwrap(),
        )
        .unwrap();
        let values = serde_json::to_value(values).unwrap();
        for group in HOST_VALUES_GROUPS {
            let digest = release_digest(&values, group).unwrap();
            assert!(
                digest.starts_with("sha256:") && digest.len() == 71,
                "{digest}"
            );
        }
        assert!(release_digest(&values, "control").is_err());
    }

    #[test]
    fn a_workload_file_names_its_workload_deployments() {
        let file = fs::read_to_string(repository().join("deploy/gcp/flow-http.yaml")).unwrap();
        assert_eq!(workload_names(&file).unwrap(), ["flow-http"]);
        assert!(workload_names("kind: Service\n").is_err());
    }

    #[test]
    fn a_stage_retries_once_and_never_a_stop_condition() {
        let directory = std::env::temp_dir().join(format!(
            "wamn-upgrade-retry-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("run.json");
        let mut record = RunRecord::open(
            &path,
            RunArguments::new("dkk", "wms", "dev", &"a".repeat(40)).unwrap(),
        )
        .unwrap();
        for stage in [Stage::Source, Stage::Build, Stage::Images, Stage::Guests] {
            record.start(&path, stage, BTreeMap::new()).unwrap();
            record
                .finish(&path, StepResult::Done, BTreeMap::new())
                .unwrap();
        }
        record
            .start(&path, Stage::Preflight, BTreeMap::new())
            .unwrap();
        assert_eq!(attempts(&record, Stage::Preflight), 1);
        record
            .finish(
                &path,
                StepResult::Failed {
                    cause: "reset".to_owned(),
                },
                BTreeMap::new(),
            )
            .unwrap();
        record
            .start(&path, Stage::Preflight, BTreeMap::new())
            .unwrap();
        assert_eq!(attempts(&record, Stage::Preflight), 2);
        assert!(retried(Stage::Preflight) && retried(Stage::CheckAndRetire));
        assert!(!retried(Stage::Build) && !retried(Stage::Record));
        let stop = anyhow::anyhow!("refused").context(StopRun("the qualification failed"));
        assert!(stop.downcast_ref::<StopRun>().is_some());
        assert_eq!(
            format!("{stop:#}"),
            "the qualification failed, so the run stops: refused"
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    fn git_sync(directory: &Path, arguments: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git output")
    }

    #[tokio::test]
    async fn the_source_stage_checks_out_the_commit_once_and_refuses_a_dirty_checkout() {
        let root = std::env::temp_dir().join(format!(
            "wamn-upgrade-source-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().expect("time")
        ));
        let repository = root.join("repository");
        let real = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let file = "deploy/gcp/environments/dkk--receiving--dev.json";
        let environment: Value =
            serde_json::from_str(&fs::read_to_string(real.join(file)).unwrap()).unwrap();
        let mut named = vec![
            file.to_owned(),
            environment["edge"]["values"].as_str().unwrap().to_owned(),
            environment["url_map"]["file"].as_str().unwrap().to_owned(),
        ];
        for workload in environment["workloads"].as_array().unwrap() {
            named.push(workload.as_str().unwrap().to_owned());
        }
        for package in environment["packages"].as_array().unwrap() {
            named.push(format!("{}/README.md", package.as_str().unwrap()));
        }
        for path in &named {
            let target = repository.join(path);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::copy(real.join(path), target).unwrap();
        }
        git_sync(&repository, &["init", "--quiet"]);
        git_sync(
            &repository,
            &["config", "user.email", "test@example.invalid"],
        );
        git_sync(&repository, &["config", "user.name", "test"]);
        git_sync(&repository, &["add", "."]);
        git_sync(&repository, &["commit", "--quiet", "-m", "one"]);
        let commit = git_sync(&repository, &["rev-parse", "HEAD"])
            .trim()
            .to_owned();
        let work = root.join("work");
        let run = Run {
            repository: repository.clone(),
            checkout: work.join("checkout"),
            work,
            arguments: RunArguments::new("dkk", "receiving", "dev", &commit).unwrap(),
        };

        let (first, outputs) = run.source().await.expect("first source");
        assert_eq!(first, StepResult::Done);
        assert_eq!(outputs["checkout"], run.checkout.display().to_string());
        let (second, _) = run.source().await.expect("second source");
        assert_eq!(second, StepResult::AlreadyDone);
        fs::write(run.checkout.join("stray"), "x").unwrap();
        let error = run.source().await.expect_err("dirty checkout");
        assert!(format!("{error:#}").contains("is not clean"), "{error:#}");

        let absent = Run {
            arguments: RunArguments::new("dkk", "receiving", "dev", &"0".repeat(40)).unwrap(),
            checkout: root.join("absent"),
            ..run
        };
        let error = absent.source().await.expect_err("absent commit");
        assert!(format!("{error:#}").contains("has no commit"), "{error:#}");
        fs::remove_dir_all(&root).unwrap();
    }
}

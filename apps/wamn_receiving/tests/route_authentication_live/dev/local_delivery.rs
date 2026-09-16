//! Saved-edit acceptance through the existing Receiving developer command.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, BufReader, Lines};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};

use super::super::{
    DevSourceState, GitSource, PLATFORM_DOMAIN, ScratchRoot, TENANT, connect,
    environment::seed_receiving_business_rows, repository_root, required_journey,
    required_journey_path, write_dev_config,
};
use super::{DEV_COMMAND_TIMEOUT, DevJourneyInputs, current_database_acl};

const CODE: &str = "apps/wamn_receiving/component/src/lib.rs";
const SQL: &str = "apps/wamn_receiving/query/location.sql";
const MANIFEST: &str = "apps/wamn_receiving/wamn.json";
const MIGRATIONS: &str = "apps/wamn_receiving/migrations";
const APPENDED: &str = "apps/wamn_receiving/migrations/0002_location_note.sql";
const APPENDED_SQL: &[u8] = b"ALTER TABLE receiving.location ADD COLUMN note text;\n";
const CODE_BEFORE: &str =
    "invoke_operation(wamn_receiving_data_access::operation::location_list(&input))";
const CODE_AFTER: &str = "invoke_operation(wamn_receiving_data_access::operation::location_list(&input.replace(\"timing-original\", \"timing-edited\")))";
const MANIFEST_BEFORE: &str = "\"table\": \"location\",\n      \"owner\": \"wamn_receiving\",\n      \"server_owned_fields\": [\n        \"id\"\n      ],";
const MANIFEST_AFTER: &str = "\"table\": \"location\",\n      \"owner\": \"wamn_receiving\",\n      \"server_owned_fields\": [\n        \"id\",\n        \"note\"\n      ],";
const OWNER_BEFORE: &str = "\"id\",\n        \"note\"\n      ],";
const OWNER_AFTER: &str = "\"id\",\n        \"note\"\n      ],\n      \"field_owners\": {\n        \"note\": \"wamn_receiving\"\n      },";

#[tokio::test]
#[ignore = "requires: WAMN_LOCAL_DEV_EDIT_ROOT, WAMN_RECEIVING_DEV_BIN, WAMN_DEV_ENV_FLOW_HTTP_COMPONENT, WAMN_RECEIVING_DEV_HOST_BIN, WAMN_RECEIVING_DEV_NATS_URL, WAMN_EVT_NATS_URL, WAMN_EVT_NATS_USERNAME, WAMN_EVT_NATS_PASSWORD_FILE, WAMN_EVT_STREAM_REPLICAS, WAMN_EVT_DUP_WINDOW_SECS, WAMN_RECEIVING_DEV_TEMPO_QUERY_URL, WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT, WAMN_ROUTE_HOST, WAMN_DEV_ENV_EVENT_PROVISIONING_USERNAME, WAMN_DEV_ENV_EVENT_PROVISIONING_PASSWORD_FILE, cargo-sqlx, jq"]
async fn local_watch_preserves_data_refuses_bad_sql_and_recreates_schema() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "WAMN_LOCAL_DEV_EDIT_ROOT",
        "WAMN_RECEIVING_DEV_BIN",
        "WAMN_DEV_ENV_FLOW_HTTP_COMPONENT",
        "WAMN_RECEIVING_DEV_HOST_BIN",
        "WAMN_RECEIVING_DEV_NATS_URL",
        "WAMN_EVT_NATS_URL",
        "WAMN_EVT_NATS_USERNAME",
        "WAMN_EVT_NATS_PASSWORD_FILE",
        "WAMN_EVT_STREAM_REPLICAS",
        "WAMN_EVT_DUP_WINDOW_SECS",
        "WAMN_RECEIVING_DEV_TEMPO_QUERY_URL",
        "WAMN_RECEIVING_DEV_OTEL_EXPORTER_OTLP_ENDPOINT",
        "WAMN_ROUTE_HOST",
        "WAMN_DEV_ENV_EVENT_PROVISIONING_USERNAME",
        "WAMN_DEV_ENV_EVENT_PROVISIONING_PASSWORD_FILE",
        "cargo-sqlx",
        "jq",
    ]);
    let repository = repository_root()?;
    ensure!(
        required_journey_path("WAMN_LOCAL_DEV_EDIT_ROOT")?
            .canonicalize()
            .context("resolve WAMN_LOCAL_DEV_EDIT_ROOT")?
            == repository
            && repository.join(".git").is_file(),
        "WAMN_LOCAL_DEV_EDIT_ROOT must name this explicitly owned linked worktree"
    );
    let git = GitSource::discover(&repository).await?;
    let initial = git.snapshot().await?;
    ensure!(
        initial.state() == DevSourceState::Clean,
        "the owned edit worktree must start clean"
    );
    // The environment resets the control store of the whole server, so the test starts its own.
    let mut server = wamn_test_infrastructure::postgres::start(&[])?;
    let system_url = server.create_database("wamn_system")?.url().to_owned();
    let mut inputs = DevJourneyInputs::required()?;
    ensure!(
        inputs
            .environment
            .local_artifacts
            .flow_http_component
            .is_file(),
        "WAMN_DEV_ENV_FLOW_HTTP_COMPONENT must name the local flow-http component file"
    );
    let mut original = SavedSource::capture(&repository)?;
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    inputs.environment.local_artifacts.directory = root.join("local-artifacts");

    let (admin, admin_task) = connect(&system_url).await?;
    let environment =
        wamn_ctl::dev::environment::provision(&system_url, admin.as_ref(), root, PLATFORM_DOMAIN)
            .await?;
    let system_acl = current_database_acl(admin.as_ref()).await?;
    let credentials = wamn_test_infrastructure::event_broker::Credentials {
        username: required_journey("WAMN_DEV_ENV_EVENT_PROVISIONING_USERNAME")?,
        password_file: required_journey_path("WAMN_DEV_ENV_EVENT_PROVISIONING_PASSWORD_FILE")?,
    };
    let provisioning = wamn_test_infrastructure::event_broker::connect(
        &credentials,
        &inputs.environment.event_nats_url,
    )
    .await?;
    let scope = wamn_control_registry::Triple::new(
        &environment.identity.org,
        &environment.identity.project,
        environment.identity.environment.as_str(),
    );
    wamn_control::event_streams::provision(
        &async_nats::jetstream::new(provisioning.clone()),
        &scope,
        inputs.environment.stream_replicas,
        Duration::from_secs(inputs.environment.dup_window_secs),
        &[],
    )
    .await?;
    let config = write_dev_config(
        root,
        &system_url,
        &environment.template,
        &environment.route,
        &environment.credentials,
        &inputs.environment,
        &environment.identity,
    )?;
    let mut watch = Watch::start(&inputs.wamn_binary, &repository, &config)?;
    let result = async {
        let first = watch.served().await?;
        ensure!(!first.skipped.contains("migrate") && !first.skipped.contains("generate"), "a new local session must prepare its schema and generated files");
        let (project, task) = connect(&environment.route.database_url).await?;
        seed_receiving_business_rows(project.as_ref()).await?;
        project.execute("INSERT INTO receiving.location(id,location_code) VALUES ('00000000-0000-0000-0000-000000000202','DOCK-2')", &[]).await?;
        task.abort();
        let token = &environment.route.token;
        let denied = http_client()?.post(format!("{}/location/list", first.url))
            .header("Host", &first.host).json(&json!([{"request_id":"denied"}])).send().await?;
        ensure!(denied.status() == reqwest::StatusCode::UNAUTHORIZED, "the local route must require authentication");
        first.locations(token, "timing-original", &["DOCK-1", "DOCK-2"]).await?;
        let updated = first.request(token, "/purchase_order/update", json!([{
            "request_id":"local-update", "id":"00000000-0000-0000-0000-000000000301",
            "expected_row_version":"1", "change":{"supplier_id":"00000000-0000-0000-0000-000000000402"}
        }])).await?;
        ensure!(updated[0]["value"]["row_version"] == "2", "the authenticated command did not commit its revision");
        require_local_facts(root, admin.as_ref(), &environment.route.database_url).await?;

        original.replace(CODE, CODE_BEFORE, CODE_AFTER)?;
        let code = watch.served().await?;
        code.retained(&first, &["migrate", "introspect", "generate", "acl"])?;
        code.locations(token, "timing-edited", &["DOCK-1", "DOCK-2"]).await?;
        require_revision(&environment.route.database_url).await?;

        original.replace(SQL, "location.location_code ASC", "location.location_code DESC")?;
        let sql = watch.served().await?;
        sql.retained(&code, &["migrate", "introspect"])?;
        ensure!(!sql.skipped.contains("generate"), "a named SQL edit cannot skip generation");
        sql.locations(token, "timing-edited", &["DOCK-2", "DOCK-1"]).await?;
        require_revision(&environment.route.database_url).await?;

        let valid_sql = fs::read(repository.join(SQL))?;
        fs::write(repository.join(SQL), b"SELECT invalid_delivery_sql FROM receiving.location;\n")?;
        watch.refused_generation().await?;
        sql.locations(token, "timing-edited", &["DOCK-2", "DOCK-1"]).await?;
        require_revision(&environment.route.database_url).await?;
        fs::write(repository.join(SQL), valid_sql)?;
        let repaired = watch.served().await?;
        repaired.retained(&sql, &["migrate", "introspect"])?;

        fs::write(repository.join(APPENDED), APPENDED_SQL)?;
        let appended = watch.served().await?;
        appended.retained(&repaired, &[])?;
        ensure!(!appended.skipped.contains("migrate"), "an appended migration must reach the kept target");
        ensure!(location_note(&environment.route.database_url).await?, "the kept target lacks the appended column");
        appended.locations(token, "timing-edited", &["DOCK-2", "DOCK-1"]).await?;
        require_revision(&environment.route.database_url).await?;

        original.replace(MANIFEST, MANIFEST_BEFORE, MANIFEST_AFTER)?;
        let declared = watch.served().await?;
        declared.retained(&appended, &[])?;
        declared.locations(token, "timing-edited", &["DOCK-2", "DOCK-1"]).await?;
        require_revision(&environment.route.database_url).await?;

        original.replace(MANIFEST, OWNER_BEFORE, OWNER_AFTER)?;
        let reset = watch.served().await?;
        ensure!(reset.instance != first.instance && !reset.skipped.contains("migrate"), "a declared definition owner must create a new target instance");
        reset.locations(token, "timing-edited", &[]).await?;
        ensure!(location_note(&environment.route.database_url).await?, "the recreated target lacks the appended migration");
        require_local_facts(root, admin.as_ref(), &environment.route.database_url).await?;
        ensure!(current_database_acl(admin.as_ref()).await? == system_acl, "the local loop changed the system database ACL");
        Ok::<_, anyhow::Error>(())
    }.await;
    let stopped = watch.stop().await;
    let result = result.with_context(|| {
        format!(
            "local developer stderr tail:\n{}",
            watch
                .diagnostics
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    drop(watch);
    let restored = original.restore();
    admin_task.abort();
    provisioning.drain().await?;
    result?;
    stopped?;
    restored?;
    let final_source = git.snapshot().await?;
    ensure!(
        final_source.state() == DevSourceState::Clean
            && final_source.source_commit() == initial.source_commit(),
        "the exact source was not restored"
    );
    Ok(())
}

async fn require_revision(url: &str) -> anyhow::Result<()> {
    let (client, task) = connect(url).await?;
    let version: i64 = client.query_one("SELECT row_version FROM receiving.purchase_order WHERE id='00000000-0000-0000-0000-000000000301'", &[]).await?.get(0);
    task.abort();
    ensure!(
        version == 2,
        "a compatible save lost or replayed the authenticated mutation"
    );
    Ok(())
}

async fn location_note(url: &str) -> anyhow::Result<bool> {
    let (client, task) = connect(url).await?;
    let present: bool = client.query_one("SELECT count(*) = 1 FROM information_schema.columns WHERE table_schema = 'receiving' AND table_name = 'location' AND column_name = 'note'", &[]).await?.get(0);
    task.abort();
    Ok(present)
}

async fn require_local_facts(
    root: &Path,
    control: &tokio_postgres::Client,
    target_url: &str,
) -> anyhow::Result<()> {
    let (manifest, _) = wamn_catalog::ServingManifest::from_canonical_bytes(&fs::read(
        root.join("local-artifacts")
            .join(wamn_catalog::RELEASE_MANIFEST_FILE_NAME),
    )?)?;
    ensure!(
        manifest.release.tenant_id == TENANT,
        "the local manifest names another tenant"
    );
    let published: i64 = control.query_one("SELECT count(*) FROM catalog.authoring_command_audit WHERE tenant_id=$1 AND command_kind='publish'", &[&TENANT]).await?.get(0);
    let attestations: i64 = control
        .query_one(
            "SELECT count(*) FROM catalog.deployment_attestations WHERE tenant_id=$1",
            &[&TENANT],
        )
        .await?
        .get(0);
    let (target, task) = connect(target_url).await?;
    let publications = target
        .query_one(
            "SELECT \
             (SELECT count(*) FROM catalog.release_manifest_v3_snapshots WHERE tenant_id=$1), \
             (SELECT count(*) FROM catalog.component_library WHERE tenant_id=$1), \
             (SELECT count(*) FROM catalog.wirings WHERE tenant_id=$1), \
             (SELECT count(*) FROM catalog.release_components WHERE tenant_id=$1)",
            &[&TENANT],
        )
        .await?;
    // The local run plane needs a disposable release FK and package membership,
    // while the manifest and component/wiring facts remain local files.
    let release_id = i32::try_from(manifest.release.effective_release_id.get())?;
    let environment: String = target
        .query_one(
            "SELECT environment FROM catalog.effective_releases \
             WHERE tenant_id=$1 AND effective_release_id=$2",
            &[&TENANT, &release_id],
        )
        .await?
        .get(0);
    let packages = target
        .query(
            "SELECT package_id, package_version FROM catalog.effective_release_packages \
             WHERE tenant_id=$1 AND effective_release_id=$2",
            &[&TENANT, &release_id],
        )
        .await?
        .into_iter()
        .map(|row| (row.get::<_, String>(0), row.get::<_, String>(1)))
        .collect::<BTreeSet<_>>();
    task.abort();
    ensure!(
        published == 0
            && attestations == 0
            && (0..4_usize).all(|column| publications.get::<_, i64>(column) == 0),
        "local execution wrote permanent publication facts"
    );
    ensure!(
        environment == manifest.release.environment
            && packages
                == manifest
                    .release
                    .packages
                    .iter()
                    .map(|package| (
                        package.package_id().to_owned(),
                        package.package_version().to_owned(),
                    ))
                    .collect::<BTreeSet<_>>(),
        "the disposable run-plane identity differs from the local manifest"
    );
    Ok(())
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()?)
}

struct Served {
    url: String,
    host: String,
    instance: String,
    skipped: BTreeSet<String>,
}

impl Served {
    fn retained(&self, previous: &Self, reused: &[&str]) -> anyhow::Result<()> {
        ensure!(
            self.instance == previous.instance,
            "a compatible edit recreated the application database"
        );
        ensure!(
            reused.iter().all(|stage| self.skipped.contains(*stage)),
            "the local loop repeated unchanged stages: {:?}",
            self.skipped
        );
        Ok(())
    }

    async fn request(&self, token: &str, path: &str, body: Value) -> anyhow::Result<Value> {
        Ok(http_client()?
            .post(format!("{}{path}", self.url))
            .header("Host", &self.host)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn locations(
        &self,
        token: &str,
        request_id: &str,
        expected: &[&str],
    ) -> anyhow::Result<()> {
        let body = self
            .request(
                token,
                "/location/list",
                json!([{"request_id":"timing-original"}]),
            )
            .await?;
        ensure!(
            body[0]["request_id"] == request_id,
            "the served component did not reflect its source edit"
        );
        let rows = body[0]["value"]["rows"]
            .as_array()
            .context("the authenticated location result has rows")?;
        let codes = rows
            .iter()
            .map(|row| row["location_code"].as_str())
            .collect::<Vec<_>>();
        ensure!(
            codes == expected.iter().copied().map(Some).collect::<Vec<_>>(),
            "the served SQL or retained rows differ: {body}"
        );
        Ok(())
    }
}

#[tokio::test]
async fn watch_reports_startup_failure_after_stdout_closes() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let scratch = ScratchRoot::create()?;
    let binary = scratch.path().join("failed-watch");
    fs::write(
        &binary,
        "#!/bin/sh\nexec 1>&-\nsleep 0.05\nprintf 'dev-stage-failed at generate: watch startup diagnostic\\nwatch compiler diagnostic\\n' >&2\nexit 23\n",
    )?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))?;
    let mut watch = Watch::start(&binary, scratch.path(), &scratch.path().join("config"))?;
    let error = watch
        .served()
        .await
        .err()
        .context("the child must refuse")?;
    ensure!(
        error.to_string().contains("watch startup diagnostic"),
        "the startup error lost its child diagnostic: {error:#}"
    );
    watch
        .stop()
        .await
        .expect_err("the child exits unsuccessfully");
    ensure!(
        watch
            .diagnostics
            .iter()
            .any(|line| line == "watch compiler diagnostic"),
        "cleanup must retain the rest of a multiline failure"
    );
    Ok(())
}

fn retain_diagnostic(diagnostics: &mut VecDeque<String>, line: String) {
    // Keep startup failures available without retaining an unbounded build log.
    if diagnostics.len() == 32 {
        diagnostics.pop_front();
    }
    diagnostics.push_back(line);
}

struct Watch {
    child: Child,
    process_group: Option<u32>,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr: Lines<BufReader<ChildStderr>>,
    stdout_open: bool,
    stderr_open: bool,
    diagnostics: VecDeque<String>,
}

impl Watch {
    fn start(binary: &Path, repository: &Path, config: &Path) -> anyhow::Result<Self> {
        let mut child = Command::new(binary)
            .current_dir(repository)
            .args(["dev", "--config"])
            .arg(config)
            .arg("--overlay-root")
            .arg(repository.join("apps/client_acme_receiving"))
            .args(["--watch", "--hold"])
            .process_group(0)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(Self {
            process_group: child.id(),
            stdout: BufReader::new(child.stdout.take().context("watch stdout")?).lines(),
            stderr: BufReader::new(child.stderr.take().context("watch stderr")?).lines(),
            child,
            stdout_open: true,
            stderr_open: true,
            diagnostics: VecDeque::new(),
        })
    }

    async fn line(&mut self) -> anyhow::Result<(bool, String)> {
        loop {
            ensure!(
                self.stdout_open || self.stderr_open,
                "the dev watch closed its output before its result (status {:?}): {}",
                self.child.try_wait()?,
                self.diagnostics
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
            );
            let (stderr, line) = tokio::select! {
                line = self.stdout.next_line(), if self.stdout_open => (false, line?),
                line = self.stderr.next_line(), if self.stderr_open => (true, line?),
            };
            let Some(line) = line else {
                if stderr {
                    self.stderr_open = false;
                } else {
                    self.stdout_open = false;
                }
                continue;
            };
            if stderr {
                retain_diagnostic(&mut self.diagnostics, line.clone());
            }
            return Ok((stderr, line));
        }
    }

    async fn served(&mut self) -> anyhow::Result<Served> {
        tokio::time::timeout(DEV_COMMAND_TIMEOUT, async {
            let mut skipped = BTreeSet::new();
            loop {
                let (stderr, line) = self.line().await?;
                ensure!(
                    !stderr || !line.contains("dev-stage-failed"),
                    "local watch refused a valid edit: {line}"
                );
                if let Some((_, stages)) = line.split_once(" skipped: unchanged ") {
                    skipped = stages.split(',').map(str::to_owned).collect();
                }
                if let Some(endpoint) = line.strip_prefix("run served: ") {
                    let (url, rest) = endpoint
                        .split_once(" host=")
                        .context("the served endpoint names its host")?;
                    let (host, instance) = rest
                        .split_once(" target_instance=")
                        .context("the served endpoint names its database creation")?;
                    ensure!(
                        url.starts_with("http://127.0.0.1:") && !instance.is_empty(),
                        "the local endpoint is not an explicit loopback target"
                    );
                    return Ok(Served {
                        url: url.to_owned(),
                        host: host.to_owned(),
                        instance: instance.to_owned(),
                        skipped,
                    });
                }
            }
        })
        .await
        .context("the local watch did not serve its candidate within the product bound")?
    }

    async fn refused_generation(&mut self) -> anyhow::Result<()> {
        tokio::time::timeout(DEV_COMMAND_TIMEOUT, async {
            loop {
                let (stderr, line) = self.line().await?;
                ensure!(
                    !line.starts_with("run served: "),
                    "invalid SQL reached activation"
                );
                if stderr && line.contains("dev-stage-failed at generate") {
                    return Ok(());
                }
            }
        })
        .await
        .context("the local watch did not refuse invalid SQL within the product bound")?
    }

    async fn stop(&mut self) -> anyhow::Result<()> {
        if let Some(pid) = self.child.id() {
            let _ = Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .output()
                .await?;
        }
        let child = &mut self.child;
        let stdout = &mut self.stdout;
        let stderr = &mut self.stderr;
        let diagnostics = &mut self.diagnostics;
        let completed = tokio::time::timeout(Duration::from_secs(60), async {
            let (status, (), ()) = tokio::try_join!(
                child.wait(),
                async {
                    while stdout.next_line().await?.is_some() {}
                    Ok::<_, std::io::Error>(())
                },
                async {
                    while let Some(line) = stderr.next_line().await? {
                        retain_diagnostic(diagnostics, line);
                    }
                    Ok::<_, std::io::Error>(())
                }
            )?;
            Ok::<_, std::io::Error>(status)
        })
        .await;
        let status = match completed {
            Ok(status) => status?,
            Err(_) => {
                self.kill_group();
                self.child.kill().await?;
                anyhow::bail!("the owned developer process did not stop within its cleanup bound");
            }
        };
        self.kill_group();
        ensure!(
            status.success(),
            "the developer process failed during cleanup: {status}"
        );
        Ok(())
    }

    fn kill_group(&mut self) {
        if let Some(pid) = self.process_group.take() {
            let _ = std::process::Command::new("kill")
                .args(["-KILL", "--", &format!("-{pid}")])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.kill_group();
    }
}

struct SavedSource {
    repository: PathBuf,
    files: BTreeMap<PathBuf, (Vec<u8>, fs::Permissions)>,
    directories: BTreeMap<PathBuf, fs::Permissions>,
    outputs: Vec<PathBuf>,
    restored: bool,
}

impl SavedSource {
    fn capture(repository: &Path) -> anyhow::Result<Self> {
        let mut files = BTreeMap::new();
        let mut directories = BTreeMap::new();
        for relative in [CODE, SQL, MANIFEST] {
            capture_file(repository, &repository.join(relative), &mut files)?;
        }
        let mut outputs = Vec::new();
        // The appended migration is a new file, so restore owns the whole directory.
        capture_tree(
            repository,
            &repository.join(MIGRATIONS),
            &mut files,
            &mut directories,
        )?;
        outputs.push(PathBuf::from(MIGRATIONS));
        for package in ["wamn_receiving", "client_acme_receiving"] {
            for output in ["generated", "tests/.sqlx"] {
                let relative = PathBuf::from("apps").join(package).join(output);
                capture_tree(
                    repository,
                    &repository.join(&relative),
                    &mut files,
                    &mut directories,
                )?;
                outputs.push(relative);
            }
        }
        Ok(Self {
            repository: repository.to_owned(),
            files,
            directories,
            outputs,
            restored: false,
        })
    }

    fn replace(&self, relative: &str, before: &str, after: &str) -> anyhow::Result<()> {
        let path = self.repository.join(relative);
        let bytes = fs::read_to_string(&path)?;
        ensure!(
            bytes.matches(before).count() == 1,
            "the existing edit anchor moved: {relative}"
        );
        fs::write(path, bytes.replace(before, after))?;
        Ok(())
    }

    fn restore(&mut self) -> anyhow::Result<()> {
        if self.restored {
            return Ok(());
        }
        for relative in &self.outputs {
            let path = self.repository.join(relative);
            if path.exists() {
                fs::remove_dir_all(path)?;
            }
        }
        for relative in self.directories.keys() {
            fs::create_dir_all(self.repository.join(relative))?;
        }
        for (relative, (bytes, permissions)) in &self.files {
            let path = self.repository.join(relative);
            fs::create_dir_all(path.parent().context("the owned file has a parent")?)?;
            fs::write(&path, bytes)?;
            fs::set_permissions(path, permissions.clone())?;
        }
        for (relative, permissions) in &self.directories {
            fs::set_permissions(self.repository.join(relative), permissions.clone())?;
        }
        self.restored = true;
        Ok(())
    }
}

impl Drop for SavedSource {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("restore the owned local test source: {error:#}");
        }
    }
}

fn capture_file(
    repository: &Path,
    path: &Path,
    files: &mut BTreeMap<PathBuf, (Vec<u8>, fs::Permissions)>,
) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_file(),
        "the owned source backup requires ordinary files"
    );
    files.insert(
        path.strip_prefix(repository)?.to_owned(),
        (fs::read(path)?, metadata.permissions()),
    );
    Ok(())
}

fn capture_tree(
    repository: &Path,
    directory: &Path,
    files: &mut BTreeMap<PathBuf, (Vec<u8>, fs::Permissions)>,
    directories: &mut BTreeMap<PathBuf, fs::Permissions>,
) -> anyhow::Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    ensure!(
        fs::symlink_metadata(directory)?.is_dir(),
        "generated outputs must be owned directories"
    );
    directories.insert(
        directory.strip_prefix(repository)?.to_owned(),
        fs::metadata(directory)?.permissions(),
    );
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            capture_tree(repository, &entry.path(), files, directories)?;
        } else {
            capture_file(repository, &entry.path(), files)?;
        }
    }
    Ok(())
}

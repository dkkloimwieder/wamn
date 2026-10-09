//! Live test of the provisioning worker `wamn-ctl serve` (wamn-zua8.3,
//! `docs/plan/platform-ui.md` §5.5). One create-environment saga for the WMS
//! package runs all 15 steps on this machine, with no Kubernetes cluster.
//!
//! An author environment from the dev loop's `provision` applies WMS, pushes
//! each listed component with `push_component`, and pushes the package with
//! `push_package`, as an author does. The registry is a local `registry:2`,
//! the web client bucket a local MinIO, and the event broker a local
//! `nats-server`. A fake `kubectl` on `PATH` runs this test executable again
//! at its ignored entry [`fake_kubectl`]: it keeps the applied objects in a
//! state directory, creates the database of a `Database` CR and the
//! publication of a `Publication` CR as the superuser, as CloudNativePG does,
//! and records every applied document. The recorded documents are compared
//! byte for byte with `tests/fixtures/serve_live/manifests.json`, after the
//! instance suffix is substituted, the tenant-key label is checked against
//! the key of the tenant in its database and masked, every Secret value is
//! replaced with one fixed marker, and the PAT annotations that each run mints
//! anew are replaced with another. Then `read-source` and `copy-roles` of a
//! copy saga read the new environment as their source, and copy its roles
//! into the author database. The prerequisites are in `docs/operations/running-tests.md`.

use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context as _, bail, ensure};
use base64::Engine as _;
use serde_json::{Value, json};
use tokio_postgres::{Client, NoTls};
use wamn_control::package_artifact::{
    COMPONENT_LIST, ListedComponent, PackageRegistry, PackageSource, PushPackageRequest,
    open_package_source, push_package,
};
use wamn_control::provision_system::{EmitProvisionerRequest, emit_provisioner_credential};
use wamn_control::serve::{ServeConfig, run_saga};
use wamn_control_provision::saga::{
    ConnectionReplacement, ConnectionRequest, EnvironmentRequest, PackageReference, STEPS,
    SourceRead, create_environment_saga_sql, environment_target,
};
use wamn_control_registry::Triple;
use wamn_test_infrastructure::scratch::ScratchRoot;

const ORG: &str = "acme";
const PROJECT: &str = "wms";
const ENVIRONMENT: &str = "dev";
/// The tenant the saga derives from the coordinate.
const TENANT: &str = "acme--wms--dev";
const PLATFORM_DOMAIN: &str = "wamn.example.test";
const UI_BUCKET: &str = "wamn-ui";
const MINIO_USER: &str = "wamn-serve-live";
const MINIO_PASSWORD: &str = "wamn-serve-live-password";
/// The state directory of the fake `kubectl`.
const STATE_ENV: &str = "WAMN_FAKE_KUBECTL_STATE";
/// The file with the arguments of one fake `kubectl` call, one per line.
const ARGS_ENV: &str = "WAMN_FAKE_KUBECTL_ARGS";
/// The file that receives the standard output of one fake `kubectl` call.
const OUT_ENV: &str = "WAMN_FAKE_KUBECTL_OUT";
/// The value that replaces every Secret value in the recorded documents.
const SECRET_MARKER: &str = "<secret>";
const INSTANCE_MARKER: &str = "<instance>";
/// The value that replaces the tenant key, which is a digest of the tenant and
/// the database name, and so of the instance suffix.
const TENANT_KEY_MARKER: &str = "<tenant-key>";
/// The value that replaces each annotation in [`RUN_ANNOTATIONS`].
const RUN_MARKER: &str = "<run>";
/// The PAT Secret annotations that each run mints anew: the expiry counts from
/// the mint time, and the prefix and the principal id are random. They are
/// not secrets.
const RUN_ANNOTATIONS: [&str; 3] = [
    "wamn.io/pat-expires-at",
    "wamn.io/pat-prefix",
    "wamn.io/principal-id",
];

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn wms_package() -> PathBuf {
    repository().join("apps/wamn_wms")
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/serve_live/manifests.json")
}

async fn connect(url: &str) -> anyhow::Result<Client> {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .context("connect to the disposable database")?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok(client)
}

fn database_url(url: &str, database: &str) -> anyhow::Result<String> {
    let mut url = url::Url::parse(url).context("parse the database URL")?;
    url.set_path(&format!("/{database}"));
    Ok(url.to_string())
}

/// One container started with `docker run -d --rm`, removed on drop.
struct Container {
    name: String,
}

impl Container {
    fn run(name: String, arguments: &[&str]) -> anyhow::Result<Self> {
        let run = Command::new("docker")
            .args(["run", "-d", "--rm", "--name", &name])
            .args(arguments)
            .output()
            .context("run docker")?;
        ensure!(
            run.status.success(),
            "start the container {name}: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        Ok(Self { name })
    }

    /// The `127.0.0.1:<port>` that the container publishes for `port`.
    fn address(&self, port: &str) -> anyhow::Result<String> {
        let output = Command::new("docker")
            .args(["port", &self.name, port])
            .output()
            .context("run docker port")?;
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .with_context(|| format!("the container {} publishes no {port}", self.name))?
            .trim()
            .to_owned())
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .output();
    }
}

async fn wait_http(url: &str) -> anyhow::Result<()> {
    for _ in 0..100 {
        if reqwest::get(url).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("{url} did not answer")
}

/// A private file, mode 0600.
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?
        .write_all(bytes)
        .with_context(|| format!("write {}", path.display()))
}

#[tokio::test]
#[ignore = "requires: Docker, WAMN_NATIVE_C_NATS_BIN, WAMN_IDENTITY_BINARY, \
            WAMN_SCENARIO_WORKER_BIN, built WMS components and web/dist"]
async fn a_create_environment_saga_runs_all_fifteen_steps() -> anyhow::Result<()> {
    wamn_test_postgres::require_prerequisites(&[
        "docker",
        "WAMN_NATIVE_C_NATS_BIN",
        "WAMN_IDENTITY_BINARY",
        "WAMN_SCENARIO_WORKER_BIN",
    ]);
    let scratch = ScratchRoot::create()?;
    let root = scratch.path();
    let mut server = wamn_test_postgres::start(&[("wal_level", "logical")])?;
    let system = server.create_database("wamn_system")?;
    let system_url = system.url().to_owned();
    let admin = connect(&system_url).await?;

    // The author environment, with WMS applied in it.
    let author_root = root.join("author");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&author_root)?;
    let author = wamn_control::dev::environment::provision(
        &system_url,
        &admin,
        &author_root,
        PLATFORM_DOMAIN,
        &[wms_package()],
    )
    .await
    .context("provision the author environment")?;
    let author_tenant = wamn_control::dev::environment::TENANT;
    // The author's owner URL of the author database. The dev provision creates
    // the database as the superuser, and a `Database` CR names wamn_db_owner,
    // which apply-package narrows to for the package DDL.
    let author_database = url::Url::parse(&author.route.database_url)?
        .path()
        .trim_start_matches('/')
        .to_owned();
    admin
        .batch_execute(&format!(
            "ALTER DATABASE {} OWNER TO {}",
            quote(&author_database),
            quote(wamn_control_provision::DB_OWNER_ROLE)
        ))
        .await
        .context("give the author database the owner of a Database CR")?;
    let author_url = database_url(&system_url, &author_database)?;
    wamn_control::apply_package::apply_package(wamn_control::apply_package::ApplyPackageRequest {
        package: wms_package(),
        database_url: author_url.clone(),
        tenant: author_tenant.to_owned(),
    })
    .await
    .context("apply WMS in the author environment")?;

    // The registry and the web client bucket.
    let id = std::process::id();
    let registry = Container::run(
        format!("wamn-serve-live-registry-{id}"),
        &["-p", "127.0.0.1::5000", "registry:2"],
    )?;
    let registry_address = registry.address("5000/tcp")?;
    wait_http(&format!("http://{registry_address}/v2/")).await?;
    let auth_file = root.join("registry-auth.json");
    write_private(
        &auth_file,
        format!(r#"{{"auths":{{"{registry_address}":{{"username":"wamn","password":"wamn"}}}}}}"#)
            .as_bytes(),
    )?;
    let minio = Container::run(
        format!("wamn-serve-live-minio-{id}"),
        &[
            "-p",
            "127.0.0.1::9000",
            "-e",
            &format!("MINIO_ROOT_USER={MINIO_USER}"),
            "-e",
            &format!("MINIO_ROOT_PASSWORD={MINIO_PASSWORD}"),
            "minio/minio:RELEASE.2025-09-07T16-13-09Z",
            "server",
            "/data",
        ],
    )?;
    let minio_address = minio.address("9000/tcp")?;
    wait_http(&format!("http://{minio_address}/minio/health/live")).await?;
    for arguments in [
        vec![
            "alias",
            "set",
            "local",
            "http://127.0.0.1:9000",
            MINIO_USER,
            MINIO_PASSWORD,
        ],
        vec!["mb", "local/wamn-ui"],
    ] {
        let output = Command::new("docker")
            .args(["exec", &minio.name, "mc"])
            .args(&arguments)
            .output()
            .context("run mc")?;
        ensure!(
            output.status.success(),
            "mc {}: {}",
            arguments[0],
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // The author pushes each listed component, then the package.
    let package_registry = PackageRegistry {
        artifact_base: format!("{registry_address}/wamn/packages"),
        registry_auth_file: auth_file.clone(),
        insecure_registry: true,
        oci_ca_paths: Vec::new(),
        control_database_url: system_url.clone(),
    };
    let pushed = push_package(&PushPackageRequest {
        package: wms_package(),
        registry: package_registry.clone(),
        source_commit: None,
    })
    .await
    .context("push the WMS package")?;
    let component_base = format!("{registry_address}/wamn/components");
    push_components(
        &pushed.tag,
        &package_registry,
        &component_base,
        &auth_file,
        author_tenant,
        &author_url,
        &system_url,
    )
    .await?;

    // The operator's part: the worker login, the control surface, the org
    // Secret that provision-org renders, and the project.
    let parsed = url::Url::parse(&system_url)?;
    let provisioner = EmitProvisionerRequest {
        system_database_url: system_url.clone(),
        emit_secret: root.join("provisioner-secret.json"),
        emit_provisioner_sql: root.join("provisioner.sql"),
        db_host: parsed.host_str().context("the URL has a host")?.to_owned(),
        db_port: parsed.port().unwrap_or(5432),
    };
    emit_provisioner_credential(&provisioner)?;
    // CloudNativePG makes wamn_system the owner of the system database.
    admin
        .batch_execute("ALTER DATABASE wamn_system OWNER TO wamn_system")
        .await
        .context("make wamn_system the owner of the system database")?;
    admin
        .batch_execute(&std::fs::read_to_string(&provisioner.emit_provisioner_sql)?)
        .await
        .context("apply the provisioner statement")?;
    let worker_url = serde_json::from_slice::<Value>(&std::fs::read(&provisioner.emit_secret)?)?
        ["stringData"]["url"]
        .as_str()
        .context("the provisioner Secret has a url")?
        .to_owned();
    admin
        .batch_execute(&wamn_control_provision::sql::grant_control_surface_sql())
        .await
        .context("grant the control surface")?;
    admin
        .execute(
            wamn_control_registry::sql::upsert_project_sql(),
            &[&ORG, &PROJECT],
        )
        .await
        .context("create the project")?;

    let state = root.join("kubectl");
    std::fs::DirBuilder::new().mode(0o700).create(&state)?;
    std::fs::write(state.join("admin-url"), &system_url)?;
    store_object(
        &state,
        &wamn_control_provision::render_control_administration_secret_manifest(ORG, "hosts"),
    )?;
    let bin = root.join("bin");
    std::fs::DirBuilder::new().mode(0o700).create(&bin)?;
    write_fake_kubectl(&bin.join("kubectl"))?;

    // The event broker, with the users of the saga environment.
    let scope = Triple::new(ORG, PROJECT, ENVIRONMENT);
    let manifest = wamn_schema_generator::PackageManifest::from_slice(&std::fs::read(
        wamn_schema_generator::package_manifest_path(&wms_package()),
    )?)?;
    let broker = start_broker(root, &scope, &manifest)?;

    // SAFETY: the test sets these before it starts any thread that reads the
    // environment, and the worker and the fake `kubectl` read them.
    unsafe {
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                bin.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        );
        std::env::set_var(STATE_ENV, &state);
        std::env::set_var("AWS_ENDPOINT", format!("http://{minio_address}"));
        std::env::set_var("AWS_ALLOW_HTTP", "true");
        std::env::set_var("AWS_ACCESS_KEY_ID", MINIO_USER);
        std::env::set_var("AWS_SECRET_ACCESS_KEY", MINIO_PASSWORD);
        std::env::set_var("AWS_REGION", "us-east-1");
    }

    // The saga, as the environment.create route writes it.
    let (package_id, version) = pushed
        .tag
        .rsplit_once('-')
        .context("the package tag is <id>-<version>")?;
    let request = EnvironmentRequest {
        project: PROJECT.to_owned(),
        env: ENVIRONMENT.to_owned(),
        route_host: "wms.example.test".to_owned(),
        packages: vec![PackageReference {
            package_id: package_id.to_owned(),
            version: version.to_owned(),
        }],
        connections: vec![ConnectionRequest {
            instance_id: "labels".to_owned(),
            requirement_type: wamn_catalog::RequirementType::Blobstore,
            alias: "labels".to_owned(),
            // A gcs definition takes no credential handle, and the bind step
            // does not reach the store.
            definition: json!({"provider": "gcs", "container": "labels", "prefix": "wms/"}),
        }],
    };
    let mut control = connect(&system_url).await?;
    let transaction = control.transaction().await?;
    transaction
        .batch_execute("SET LOCAL ROLE wamn_control")
        .await?;
    let saga: String = transaction
        .query_one(
            create_environment_saga_sql(),
            &[
                &environment_target(ORG, PROJECT, ENVIRONMENT),
                &i32::try_from(STEPS.len())?,
                &ORG,
                &serde_json::to_string(&request)?,
                &STEPS.as_slice(),
            ],
        )
        .await
        .context("write the saga")?
        .get(0);
    transaction.commit().await?;

    let config = ServeConfig {
        system_database_url: worker_url.clone(),
        db_host: provisioner.db_host.clone(),
        db_port: provisioner.db_port,
        nats_url: broker.url.clone(),
        nats_username: broker.username.clone(),
        nats_password_file: broker.password_file.clone(),
        stream_replicas: 1,
        dup_window_secs: 120,
        ui_bucket: format!("s3://{UI_BUCKET}"),
        package_artifact_base: package_registry.artifact_base.clone(),
        component_artifact_base: component_base,
        release_artifact_base: format!("{registry_address}/wamn/releases"),
        registry_auth_file: auth_file,
        insecure_registry: true,
        oci_ca_paths: Vec::new(),
        pat_issuer: author.issuer.args.clone(),
        scenario_worker: PathBuf::from(std::env::var("WAMN_SCENARIO_WORKER_BIN")?),
    };
    let worker = connect(&worker_url).await?;
    let open = wamn_control::environment_saga::next_open_saga(&worker)
        .await?
        .context("the worker finds the saga")?;
    ensure!(
        open.saga_id == saga,
        "the worker opens the saga it was given"
    );
    run_saga(&config, &worker, open).await?;

    let row = admin
        .query_one(
            "SELECT status, coalesce(last_error, '') FROM provisioning.sagas WHERE saga_id = $1",
            &[&saga],
        )
        .await?;
    let (status, last_error): (String, String) = (row.get(0), row.get(1));
    let steps: Vec<(i32, String)> = admin
        .query(
            "SELECT step, status FROM provisioning.saga_steps WHERE saga_id = $1 ORDER BY step",
            &[&saga],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect();
    let expected: Vec<(i32, String)> = (1..=i32::try_from(STEPS.len())?)
        .map(|step| (step, "completed".to_owned()))
        .collect();
    assert_eq!(
        steps, expected,
        "every step completes; last error: {last_error}"
    );
    assert_eq!(status, "awaiting-operator", "last error: {last_error}");

    // The applied documents, against the fixture.
    let instance: String = admin
        .query_one(
            "SELECT instance_suffix FROM registry.project_envs \
              WHERE org = $1 AND project = $2 AND env = $3",
            &[&ORG, &PROJECT, &ENVIRONMENT],
        )
        .await?
        .get(0);
    let tenant_key = wamn_control_provision::tenant_key::tenant_key(
        TENANT,
        &wamn_control_provision::project_env_database_name(ORG, PROJECT, ENVIRONMENT, &instance),
    );
    let recorded = recorded_documents(&state, &instance, &tenant_key)?;
    let expected = std::fs::read_to_string(fixture()).unwrap_or_default();
    if recorded != expected {
        let actual = root.join("manifests.json");
        std::fs::write(&actual, &recorded)?;
        let kept = std::env::temp_dir().join(format!("wamn-serve-live-manifests-{id}.json"));
        std::fs::copy(&actual, &kept)?;
        bail!(
            "the applied documents differ from {}; the recorded documents are in {}",
            fixture().display(),
            kept.display()
        );
    }

    // The two steps of a copy saga that read a source (wamn-zua8.4), with
    // the environment this saga made as the source, as the worker login.
    // A role `clerk` selects one operation in the source first.
    let source_url = database_url(
        &worker_url,
        &wamn_control_provision::project_env_database_name(ORG, PROJECT, ENVIRONMENT, &instance),
    )?;
    let release: String = connect(&source_url)
        .await?
        .query_one(
            "SELECT manifest_digest FROM catalog.releases WHERE tenant_id = $1",
            &[&TENANT],
        )
        .await?
        .get(0);
    let snapshot = wamn_control::print_release_env::lookup_release_snapshot(
        &source_url,
        TENANT,
        &release,
        &config.release_artifact_base,
    )
    .await?;
    let closures = wamn_catalog::ReleaseClosures::from_manifest(&snapshot.manifest);
    let root = closures
        .roots()
        .next()
        .context("the published release serves an operation")?
        .to_owned();
    let mut source = connect(&source_url).await?;
    let transaction = source.transaction().await?;
    wamn_control::role_permissions::create_role(&transaction, TENANT, "clerk").await?;
    wamn_control::role_permissions::grant_permission(
        &transaction,
        TENANT,
        "clerk",
        &root,
        &closures,
    )
    .await?;
    transaction.commit().await?;
    let replacement = json!({"provider": "gcs", "container": "copied", "prefix": "wms/"});
    let read = wamn_control::environment_copy::read_source(
        &source_url,
        ENVIRONMENT,
        &[ConnectionReplacement {
            instance_id: "labels".to_owned(),
            definition: replacement.clone(),
        }],
    )
    .await?;
    assert_eq!(
        read,
        SourceRead {
            manifest_digest: release.clone(),
            packages: request.packages.clone(),
            connections: vec![ConnectionRequest {
                definition: replacement,
                ..request.connections[0].clone()
            }],
        }
    );
    // `copy-roles` into the author database, which serves the same package.
    let target_url = database_url(&worker_url, &author_database)?;
    wamn_control::environment_copy::copy_roles(
        &source_url,
        ENVIRONMENT,
        &target_url,
        author_tenant,
        &closures,
    )
    .await?;
    let permissions = async |url: &str, tenant: &str| -> anyhow::Result<Vec<(String, String)>> {
        Ok(connect(url)
            .await?
            .query(
                "SELECT permission, required_by FROM app_system.permissions \
                  WHERE tenant_id = $1 AND role_name = 'clerk' ORDER BY 1, 2",
                &[&tenant],
            )
            .await?
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect())
    };
    let copied = permissions(&target_url, author_tenant).await?;
    assert!(
        copied.contains(&(root.clone(), root.clone())),
        "the copy selects {root}: {copied:?}"
    );
    assert_eq!(copied, permissions(&source_url, TENANT).await?);
    drop(broker);
    drop(author);
    Ok(())
}

/// Push each component that the pushed package lists, as an author pushes it
/// with `push-component`: the package's own component with its authored
/// declaration, a wiring component with its platform declaration.
async fn push_components(
    tag: &str,
    registry: &PackageRegistry,
    artifact_base: &str,
    auth_file: &Path,
    tenant: &str,
    project_url: &str,
    control_url: &str,
) -> anyhow::Result<()> {
    use wamn_control::component_declaration::{
        authored_base_digests, declared_platform_packages, render_declaration_document,
        render_palette_declaration,
    };
    use wamn_control::release_composition::{
        PackageInput, SelectedComponentArtifact, load_wirings, wiring_store_alias,
    };
    let opened = open_package_source(PackageSource::Artifact {
        tag: tag.to_owned(),
        registry: registry.clone(),
    })
    .await?;
    let package = opened.root();
    let manifest = wamn_schema_generator::PackageManifest::from_slice(&std::fs::read(
        wamn_schema_generator::package_manifest_path(package),
    )?)?;
    let wirings = load_wirings(&[PackageInput {
        root: package.to_path_buf(),
        manifest: manifest.clone(),
    }])?;
    let listed: Vec<ListedComponent> =
        serde_json::from_slice(&std::fs::read(package.join(COMPONENT_LIST))?)?;
    let index: Vec<Value> = serde_json::from_slice(&std::fs::read(
        repository().join("apps/target/components.json"),
    )?)?;
    let declarations = tempfile_directory(&std::env::temp_dir(), "wamn-serve-live-declarations")?;
    for component in listed {
        let file = index
            .iter()
            .find(|entry| entry["name"] == component.name.as_str())
            .and_then(|entry| entry["file"].as_str())
            .with_context(|| format!("the build index has no {}", component.name))?;
        let bytes = std::fs::read(file)?;
        ensure!(
            hex::encode(ring::digest::digest(&ring::digest::SHA256, &bytes)) == component.sha256,
            "the built {} differs from the listed digest",
            component.name
        );
        let template = package
            .join("publication/components")
            .join(format!("{}.json.in", component.name));
        let (document, admitted) = if manifest.components.contains_key(&component.name) {
            // The author renders the own component from the source tree.
            let template = wms_package()
                .join("publication/components")
                .join(format!("{}.json.in", component.name));
            let document = render_declaration_document(
                &template,
                tenant,
                &authored_base_digests(&wms_package())?,
            )?;
            (
                document,
                vec!["wamn:node".to_owned(), "wamn:postgres".to_owned()],
            )
        } else {
            let artifact = SelectedComponentArtifact {
                package_id: manifest.package.id.as_str().into(),
                package_version: manifest.package.version.as_str().into(),
                component: component.name.as_str().into(),
                path: PathBuf::from(file),
                digest: format!("sha256:{}", component.sha256).into(),
            };
            let scope = wamn_catalog::ComponentPackageScope {
                tenant_id: tenant.to_owned(),
                package_id: manifest.package.id.clone(),
                package_version: manifest.package.version.clone(),
            };
            let document = render_palette_declaration(
                &template,
                &scope,
                wiring_store_alias(&wirings, &artifact)?.as_deref(),
            )?;
            let admitted = declared_platform_packages(&template, &document)?;
            (document, admitted)
        };
        let declaration = declarations.path().join(format!("{}.json", component.name));
        std::fs::write(&declaration, serde_json::to_vec(&document)?)?;
        wamn_control::push_component::push_component(
            wamn_control::push_component::AdmitComponentRequest {
                package: wms_package(),
                component_bytes: PathBuf::from(file),
                declaration,
                admitted_platform_packages: admitted,
            },
            wamn_control::push_component::PublishAdmittedComponentRequest {
                artifact_base: artifact_base.to_owned(),
                registry_auth_file: auth_file.to_path_buf(),
                insecure_registry: true,
                oci_ca_paths: Vec::new(),
                project_database_url: project_url.to_owned(),
                control_database_url: control_url.to_owned(),
            },
        )
        .await
        .with_context(|| format!("push the component {}", component.name))?;
    }
    Ok(())
}

fn tempfile_directory(parent: &Path, stem: &str) -> anyhow::Result<ScratchRoot> {
    let path = parent.join(format!("{stem}-{}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&path)?;
    Ok(ScratchRoot(path))
}

/// The local `nats-server`, killed on drop, and the provisioning user of the
/// saga environment.
struct Broker {
    url: String,
    username: String,
    password_file: PathBuf,
    child: std::process::Child,
}

impl Drop for Broker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_broker(
    root: &Path,
    scope: &Triple,
    manifest: &wamn_schema_generator::PackageManifest,
) -> anyhow::Result<Broker> {
    use wamn_control_provision::events::{
        advisory_stream_config, registration_consumers, source_stream_config,
    };
    let work = root.join("nats");
    std::fs::DirBuilder::new().mode(0o700).create(&work)?;
    let broker = wamn_test_infrastructure::event_broker::prepare(
        &work,
        scope,
        TENANT,
        &source_stream_config(scope, 1, Duration::from_secs(120)),
        &advisory_stream_config(scope, 1),
        &registration_consumers(scope, TENANT, std::slice::from_ref(manifest)),
    )?;
    // Only the users: the prepared file also names a fixed monitoring port.
    let prepared: Value = serde_json::from_slice(&std::fs::read(&broker.configuration)?)?;
    let configuration = work.join("nats.conf");
    write_private(
        &configuration,
        &serde_json::to_vec(&json!({ "authorization": prepared["authorization"] }))?,
    )?;
    let reserved = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = reserved.local_addr()?.port();
    drop(reserved);
    let log = std::fs::File::create(work.join("nats.log"))?;
    let child = Command::new(std::env::var("WAMN_NATIVE_C_NATS_BIN")?)
        .args(["--jetstream", "--addr", "127.0.0.1", "--port"])
        .arg(port.to_string())
        .arg("--store_dir")
        .arg(work.join("data"))
        .arg("--config")
        .arg(&configuration)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("start nats-server")?;
    let broker_handle = Broker {
        url: format!("nats://127.0.0.1:{port}"),
        username: broker.provisioning.username.clone(),
        password_file: broker.provisioning.password_file.clone(),
        child,
    };
    for _ in 0..200 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Ok(broker_handle);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("nats-server did not listen")
}

/// The fake `kubectl`: a script that runs this test executable at
/// [`fake_kubectl`] and prints what it wrote.
fn write_fake_kubectl(path: &Path) -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let script = format!(
        "#!/bin/sh\n\
         work=$(mktemp -d)\n\
         printf '%s\\n' \"$@\" > \"$work/args\"\n\
         {ARGS_ENV}=\"$work/args\" {OUT_ENV}=\"$work/out\" '{}' --exact fake_kubectl --ignored \
         --test-threads=1 > \"$work/log\" 2>&1\n\
         status=$?\n\
         if [ $status -ne 0 ]; then cat \"$work/log\" >&2; fi\n\
         if [ -f \"$work/out\" ]; then cat \"$work/out\"; fi\n\
         rm -rf \"$work\"\n\
         exit $status\n",
        executable.display()
    );
    std::fs::write(path, script)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// The fake `kubectl` itself. Without the variables of the script it does
/// nothing, so a plain `--ignored` run passes it.
#[tokio::test]
#[ignore = "the fake kubectl of a_create_environment_saga_runs_all_fifteen_steps"]
async fn fake_kubectl() -> anyhow::Result<()> {
    let (Some(state), Some(args), Some(out)) = (
        std::env::var_os(STATE_ENV),
        std::env::var_os(ARGS_ENV),
        std::env::var_os(OUT_ENV),
    ) else {
        return Ok(());
    };
    let state = PathBuf::from(state);
    let args: Vec<String> = std::fs::read_to_string(args)?
        .lines()
        .map(str::to_owned)
        .collect();
    let output = kubectl(&state, &args).await?;
    std::fs::write(out, output)?;
    Ok(())
}

async fn kubectl(state: &Path, args: &[String]) -> anyhow::Result<String> {
    let (namespace, rest) = match args {
        [flag, namespace, rest @ ..] if flag == "-n" => (Some(namespace.as_str()), rest),
        rest => (None, rest),
    };
    let rest: Vec<&str> = rest.iter().map(String::as_str).collect();
    match rest.as_slice() {
        ["apply", "-f", "-"] => {
            let document: Value = serde_json::from_slice(&read_stdin()?)?;
            record(state, &document)?;
            match document["kind"].as_str() {
                Some("Database") => create_database(state, &document).await?,
                Some("Publication") => create_publication(state, &document).await?,
                _ => {}
            }
            store_object(state, &document)?;
            Ok(String::new())
        }
        [
            "patch",
            "secret",
            name,
            "--type",
            "merge",
            "--patch-file",
            "/dev/stdin",
        ] => {
            let namespace = namespace.context("patch names no namespace")?;
            let patch: Value = serde_json::from_slice(&read_stdin()?)?;
            record(
                state,
                &json!({"patch": {"namespace": namespace, "secret": name}, "body": patch}),
            )?;
            let secret_path = object_path(state, namespace, "secret", name);
            let mut secret: Value = serde_json::from_slice(
                &std::fs::read(&secret_path).with_context(|| format!("secret {name} not found"))?,
            )?;
            merge_secret_values(&mut secret, &patch)?;
            std::fs::write(secret_path, serde_json::to_vec(&secret)?)?;
            Ok(String::new())
        }
        ["wait", object, condition, _timeout]
            if *condition == "--for=jsonpath={.status.applied}=true" =>
        {
            let (kind, name) = object.split_once('/').context("wait names kind/name")?;
            let namespace = namespace.context("wait names no namespace")?;
            ensure!(
                object_path(state, namespace, kind, name).is_file(),
                "{kind} {name} not found"
            );
            Ok(String::new())
        }
        ["get", "secret", name, "-o", path] => {
            let namespace = namespace.context("get names no namespace")?;
            let secret: Value = serde_json::from_slice(
                &std::fs::read(object_path(state, namespace, "secret", name))
                    .with_context(|| format!("secret {name} not found"))?,
            )?;
            let path = path
                .strip_prefix("jsonpath={.")
                .and_then(|path| path.strip_suffix('}'))
                .context("get takes one jsonpath")?;
            let mut value = &secret;
            for key in split_jsonpath(path) {
                value = &value[key.as_str()];
            }
            Ok(value.as_str().unwrap_or_default().to_owned())
        }
        other => bail!("the fake kubectl has no command {other:?}"),
    }
}

fn read_stdin() -> anyhow::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The keys of a jsonpath, with `\.` kept inside a key.
fn split_jsonpath(path: &str) -> Vec<String> {
    let mut keys = vec![String::new()];
    let mut characters = path.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => keys.last_mut().expect("one key").extend(characters.next()),
            '.' => keys.push(String::new()),
            other => keys.last_mut().expect("one key").push(other),
        }
    }
    keys
}

fn object_path(state: &Path, namespace: &str, kind: &str, name: &str) -> PathBuf {
    state
        .join("objects")
        .join(namespace)
        .join(kind.to_ascii_lowercase())
        .join(format!("{name}.json"))
}

/// Keep one applied object. A Secret keeps its values base64-encoded under
/// `data`, as the API server does.
fn store_object(state: &Path, document: &Value) -> anyhow::Result<()> {
    let metadata = &document["metadata"];
    let path = object_path(
        state,
        metadata["namespace"]
            .as_str()
            .context("the object names no namespace")?,
        document["kind"]
            .as_str()
            .context("the object has no kind")?,
        metadata["name"]
            .as_str()
            .context("the object has no name")?,
    );
    std::fs::create_dir_all(path.parent().context("the object path has a parent")?)?;
    let mut object = document.clone();
    if document["kind"] == "Secret" {
        let mut data = json!({});
        if let Some(values) = document["data"].as_object() {
            data = Value::Object(values.clone());
        }
        object["data"] = data;
        merge_secret_values(&mut object, document)?;
        if let Some(object) = object.as_object_mut() {
            object.remove("stringData");
        }
    }
    std::fs::write(path, serde_json::to_vec(&object)?)?;
    Ok(())
}

/// Apply the `stringData` and `data` of a merge patch to a stored Secret.
fn merge_secret_values(secret: &mut Value, patch: &Value) -> anyhow::Result<()> {
    let data = secret["data"]
        .as_object_mut()
        .context("the stored Secret has data")?;
    if let Some(values) = patch["stringData"].as_object() {
        for (key, value) in values {
            let text = value.as_str().context("a stringData value is text")?;
            data.insert(
                key.clone(),
                Value::String(base64::engine::general_purpose::STANDARD.encode(text)),
            );
        }
    }
    if let Some(values) = patch["data"].as_object() {
        for (key, value) in values {
            if value.is_null() {
                data.remove(key);
            } else {
                data.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(())
}

/// Append one applied document, in order, to the record.
fn record(state: &Path, document: &Value) -> anyhow::Result<()> {
    let directory = state.join("applied");
    std::fs::create_dir_all(&directory)?;
    let next = std::fs::read_dir(&directory)?.count();
    std::fs::write(
        directory.join(format!("{next:04}.json")),
        serde_json::to_vec(document)?,
    )?;
    Ok(())
}

async fn admin(state: &Path, database: Option<&str>) -> anyhow::Result<Client> {
    let url = std::fs::read_to_string(state.join("admin-url"))?;
    match database {
        Some(database) => connect(&database_url(&url, database)?).await,
        None => connect(&url).await,
    }
}

/// What CloudNativePG does for a `Database` CR.
async fn create_database(state: &Path, document: &Value) -> anyhow::Result<()> {
    let spec = &document["spec"];
    let name = spec["name"].as_str().context("the Database names a name")?;
    let owner = spec["owner"]
        .as_str()
        .context("the Database names an owner")?;
    admin(state, None)
        .await?
        .batch_execute(&format!(
            "CREATE DATABASE {} OWNER {}",
            quote(name),
            quote(owner)
        ))
        .await
        .context("create the database of the Database CR")
}

/// What CloudNativePG does for a `Publication` CR.
async fn create_publication(state: &Path, document: &Value) -> anyhow::Result<()> {
    let spec = &document["spec"];
    let database = spec["dbname"]
        .as_str()
        .context("the Publication names a dbname")?;
    let publication = spec["name"]
        .as_str()
        .context("the Publication names a name")?;
    let schema = spec["target"]["objects"][0]["tablesInSchema"]
        .as_str()
        .context("the Publication names a schema")?;
    admin(state, Some(database))
        .await?
        .batch_execute(&wamn_control_provision::sql::create_publication_sql(
            publication,
            schema,
        ))
        .await
        .context("create the publication of the Publication CR")
}

fn quote(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// The recorded documents as the fixture holds them: one JSON array, every
/// Secret value replaced with [`SECRET_MARKER`], each of [`RUN_ANNOTATIONS`]
/// with [`RUN_MARKER`], the tenant-key label with [`TENANT_KEY_MARKER`] after
/// it equals `tenant_key`, and the instance suffix with [`INSTANCE_MARKER`].
fn recorded_documents(state: &Path, instance: &str, tenant_key: &str) -> anyhow::Result<String> {
    let directory = state.join("applied");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    files.sort();
    let mut documents = Vec::new();
    for file in files {
        let mut document: Value = serde_json::from_slice(&std::fs::read(file)?)?;
        let values = if document.get("patch").is_some() {
            &mut document["body"]
        } else if document["kind"] == "Secret" {
            &mut document
        } else {
            documents.push(document);
            continue;
        };
        if let Some(label) = values.pointer_mut("/metadata/labels/wamn.tenant-key") {
            ensure!(
                *label == tenant_key,
                "the tenant-key label {label} is not the key of tenant {TENANT} in its database"
            );
            *label = Value::String(TENANT_KEY_MARKER.to_owned());
        }
        if let Some(annotations) = values["metadata"]["annotations"].as_object_mut() {
            for name in RUN_ANNOTATIONS {
                if let Some(value) = annotations.get_mut(name) {
                    *value = Value::String(RUN_MARKER.to_owned());
                }
            }
        }
        for field in ["data", "stringData"] {
            if let Some(entries) = values[field].as_object_mut() {
                for value in entries.values_mut() {
                    if !value.is_null() {
                        *value = Value::String(SECRET_MARKER.to_owned());
                    }
                }
            }
        }
        documents.push(document);
    }
    Ok(format!("{}\n", serde_json::to_string_pretty(&documents)?)
        .replace(instance, INSTANCE_MARKER))
}

//! Native file routes in the environment kind scenario.
use super::{NAMESPACE, ORG, PROJECT, Run};
use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::{Child, Command};
use wamn_control::environment::{
    self, Platform,
    document::{DeclaredRelease, EnvironmentDocument},
};
use wamn_control::package_artifact::{PackageRegistry, PushPackageRequest, push_package};

pub const COMPONENT_INPUT: &str = "WAMN_BLOB_PUT_COMPONENT";

/// Docker Hub no longer serves the pinned MinIO images. The scenario loads the
/// local copies into kind after their repository digests match these pins.
const IMAGES: [(&str, &str); 2] = [
    (
        "minio/minio:RELEASE.2025-09-07T16-13-09Z",
        "minio/minio@sha256:14cea493d9a34af32f524e538b8346cf79f3321eff8e708c1e2960462bd8936e",
    ),
    (
        "minio/mc:latest",
        "minio/mc@sha256:a7fe349ef4bd8521fb8497f55c6042871b2ae640607cf99d9bede5e9bdf11727",
    ),
];

struct Fixture {
    base_url: String,
    api_url: String,
    route_host: String,
    session: String,
    denied_session: String,
    project_reader_url: String,
    connection_reader_url: String,
    org: String,
    project: String,
    environment: String,
    minio_address: std::net::SocketAddr,
}

async fn assertions(run: &Run, fixture: &Fixture) -> anyhow::Result<()> {
    for token in [&fixture.session, &fixture.denied_session] {
        ensure!(
            token.starts_with("Bearer ") && !token.contains("REPLACE"),
            "supply a real fixture session"
        );
    }
    let deployment = run
        .json(
            "kubectl",
            &[
                "-n",
                NAMESPACE,
                "get",
                "deployment",
                "c4-files-minio",
                "-o",
                "json",
            ],
        )
        .await?;
    ensure!(
        deployment["spec"]["template"]["spec"]["containers"][0]["image"] == IMAGES[0].0,
        "the kind fixture uses the pinned MinIO image"
    );
    let (reader, reader_driver) =
        tokio_postgres::connect(&fixture.project_reader_url, tokio_postgres::NoTls).await?;
    let reader_driver = tokio::spawn(reader_driver);
    let (connections, connections_driver) =
        tokio_postgres::connect(&fixture.connection_reader_url, tokio_postgres::NoTls).await?;
    let connections_driver = tokio::spawn(connections_driver);
    let result = async {
        for database in [&reader,&connections] {
            let row = database.query_one("SELECT rolsuper,rolbypassrls FROM pg_roles WHERE rolname=current_user",&[]).await?;
            ensure!(!row.get::<_,bool>(0) && !row.get::<_,bool>(1),"the kind probe uses scoped database logins");
        }
        let insert = reader.batch_execute("INSERT INTO platform.file DEFAULT VALUES").await.unwrap_err();
        ensure!(insert.code()==Some(&tokio_postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
            "the deployed application login refuses direct file INSERT");
        let row = connections.query_one(
            "SELECT parameters FROM registry.lookup_file_connection($1,$2,$3,'files','files',NULL,NULL)",
            &[&fixture.org,&fixture.project,&fixture.environment],
        ).await?;
        let definition: Value = row.get(0);
        ensure!(definition["provider"]=="s3" && definition["endpoint"]=="http://c4-files-minio:9000",
            "the native host binds the S3-compatible MinIO fixture");
        let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .resolve("files-a.localhost",fixture.minio_address)
            .timeout(Duration::from_secs(30)).build()?;
        let base = fixture.base_url.trim_end_matches('/');
        let api = fixture.api_url.trim_end_matches('/');
        let upload = async |binding: &str,bytes: &'static [u8]| -> anyhow::Result<uuid::Uuid> {
            let response = client.post(format!("{base}/api/files/files?binding={binding}&name=kind.txt"))
                .header("host",&fixture.route_host).header("authorization",&fixture.session)
                .header("content-type","text/plain").body(bytes.to_vec()).send().await?;
            let status = response.status();
            let text = response.text().await?;
            ensure!(status.as_u16()==201,"native file upload {status}: {text}");
            Ok(serde_json::from_str::<Value>(&text)?["file_id"].as_str().context("file id")?.parse()?)
        };
        let mutate = async |id: uuid::Uuid,binding: &str,exposure: &str| -> anyhow::Result<Value> {
            let attachment = wamn_catalog::HostRouteSet::Application.attachments()
                .find(|(_,a)|a.reference=="wamn-control:file/expose").context("file.expose route")?.1;
            let path = attachment.definition["route"]["path"].as_str().context("file.expose path")?;
            let payload = json!([{"request_id":uuid::Uuid::new_v4().to_string(),"value":{
                "application":"files","binding":binding,"file_id":id.to_string(),"exposure":exposure}}]);
            let response = client.post(format!("{api}{path}"))
                .header("host",&fixture.route_host).header("authorization",&fixture.session)
                .header("content-type","application/json").body(payload.to_string()).send().await?;
            let status = response.status();
            let text = response.text().await?;
            ensure!(status.is_success(),"file.expose {status}: {text}");
            let [item]: [Value;1] = serde_json::from_str(&text)?;
            Ok(item)
        };
        let bytes = b"kind native file bytes";
        let id = upload("files",bytes).await?;
        let row = reader.query_one("SELECT application,size,sha256,connection_generation FROM platform.file WHERE id=$1",&[&id]).await?;
        ensure!(row.get::<_,String>(0)=="files" && row.get::<_,i64>(1)==i64::try_from(bytes.len())?,
            "the committed row owns the deployed upload and records its size");
        ensure!(row.get::<_,String>(2)==hex::encode(ring::digest::digest(&ring::digest::SHA256,bytes).as_ref()),"row hash matches the deployed bytes");
        ensure!(row.get::<_,i64>(3)>0,"the row pins its storage generation");
        let read_url = format!("{base}/api/files/files/{id}");
        let response = client.get(&read_url).header("host",&fixture.route_host)
            .header("authorization",&fixture.session).send().await?;
        ensure!(response.status().as_u16()==200 && response.bytes().await?.as_ref()==bytes,
            "the deployed host returns the uploaded bytes");
        ensure!(client.get(&read_url).header("host",&fixture.route_host).send().await?.status().as_u16()==401,"anonymous file reads refuse");
        ensure!(client.get(&read_url).header("host",&fixture.route_host).header("authorization",&fixture.denied_session)
            .send().await?.status().as_u16()==403,"sessions without file.get refuse");
        ensure!(mutate(id,"files","private").await?.get("value").is_some(),"private exposure succeeds");
        ensure!(client.get(&read_url).header("host",&fixture.route_host).header("authorization",&fixture.session)
            .send().await?.status().as_u16()==403,"private exposure refuses through the application URL");
        let public = mutate(id,"files","public").await?;
        let url = public["value"]["url"].as_str().context("the stored public URL")?;
        let origin = client.get(url).send().await?;
        ensure!(origin.status().as_u16()==200 && origin.headers().get("cache-control").is_some_and(|v|v=="max-age=3600"),
            "the public MinIO origin serves bytes with the required cache lifetime");
        ensure!(origin.bytes().await?.as_ref()==bytes,"public bytes match the upload");
        ensure!(mutate(id,"files","public").await?["value"]["url"]==url,"expose is idempotent");
        for _ in 0..2 {ensure!(mutate(id,"files","application").await?.get("value").is_some(),"unexpose is idempotent");}
        ensure!(client.get(url).send().await?.status().as_u16()==404,"unexpose removes the origin object at once");
        let denied = client.post(format!("{base}/api/files/files?binding=files_read&name=denied.txt"))
            .header("host",&fixture.route_host).header("authorization",&fixture.session)
            .header("content-type","text/plain").body("denied").send().await?;
        ensure!(denied.status().as_u16()==403,"a read binding refuses deployed uploads");
        let no_cdn = upload("files_no_cdn",b"no CDN").await?;
        ensure!(mutate(no_cdn,"files_no_cdn","public").await?["error"]["code"]=="cdn_host_required",
            "a store without cdn_host refuses public exposure");
        Ok::<_,anyhow::Error>(())
    }.await;
    reader_driver.abort();
    connections_driver.abort();
    result
}

async fn build_fixture(
    run: &Run,
    repository: &Path,
    database_url: &str,
) -> anyhow::Result<(
    PathBuf,
    Vec<wamn_control::component_admission::ComponentAdmission>,
)> {
    use wamn_schema_generator::{
        build::{BuildRequest, build_package},
        receipt::{RECEIPT_FILE, ReceiptComponent},
    };
    let source = repository.join("crates/control/lib/tests/support/file_package");
    let package = run.work.join("file-package/file_fixture");
    for directory in ["publication/components", "publication/wirings"] {
        std::fs::create_dir_all(package.join(directory))?;
    }
    std::fs::copy(source.join("kind.wamn.k.in"), package.join("wamn.k"))?;
    // This authored route starts the HTTP role for file.expose metadata calls.
    // Transfers use the separate native file listener.
    let definition = json!({
        "id":"fixture-run-http", "type":"http", "route":{"path":"/fixture/run"},
        "input-schema":{
            "type":"array", "minItems":1, "maxItems":100,
            "items":{"type":"object", "required":["request_id"],
                "additionalProperties":false,
                "properties":{"request_id":{"type":"string", "minLength":1}}}
        },
        "raw-body-bytes":{"maximum":1_048_576}
    });
    std::fs::write(
        package.join("publication/attachments.json"),
        serde_json::to_vec(&json!({"fixture-run-http":{
            "type":"http", "package-id":"file_fixture", "component":"fixture",
            "operation":"file-fixture:fixture/run",
            "definition-hash":wamn_execution_contract::canonical_json_sha256(&definition),
            "definition":definition, "auth-policy":{"modes":["session"]},
            "registered-operation":"file-fixture:fixture/run"
        }}))?,
    )?;
    std::fs::write(
        package.join("publication/components/fixture.json.in"),
        serde_json::to_vec(&json!({
            "scope":{"env_key":"__ENV_KEY__", "package-id":"file_fixture"},
            "component":"fixture", "interface-version":"0.1.0",
            "operations":{"file-fixture:fixture/run":{
                "registered-operation":"file-fixture:fixture/run",
                "input-ports":[], "output-ports":[], "parameters":[]}},
            "connections":[]
        }))?,
    )?;
    let palette = run.work.join("file-package/platform/execution/blob-put");
    std::fs::create_dir_all(&palette)?;
    std::fs::copy(
        source.join("publication/components/blob-put.json.in"),
        palette.join("declaration.json.in"),
    )?;
    std::fs::write(
        package.join("publication/wirings/blob-put.json"),
        serde_json::to_vec(&json!({
            "format-version":"0.1", "wiring-id":"blob-put", "version":1, "entry":"put",
            "nodes":{"put":{"component":"blob-put", "interface-version":"0.1.0",
                "operation":"wamn:node/async-handler@0.1.0",
                "params":{"store_alias":"files", "key_field":"/key", "body_field":"/body"}}},
            "edges":[]
        }))?,
    )?;
    let mut receipt = Box::pin(build_package(&BuildRequest::new(
        &package,
        &run.work.join("file-package/target/wamn"),
        database_url,
    )))
    .await?;
    let output = wamn_schema_generator::output_root(&package);
    let mut resolve = wit_parser::Resolve::new();
    resolve.push_str(
        "node.wit",
        include_str!("../../../../../crates/execution/workflow/router/wit/package.wit"),
    )?;
    let (wit_package, _) = resolve.push_dir(output.join("wit"))?;
    let world = resolve.select_world(&[wit_package], Some("generated"))?;
    let mut module =
        wit_component::dummy_module(&resolve, world, wit_parser::ManglingAndAbi::Standard32);
    wit_component::embed_component_metadata(
        &mut module,
        &resolve,
        world,
        wit_component::StringEncoding::UTF8,
    )?;
    let fixture = wit_component::ComponentEncoder::default()
        .module(&module)?
        .validate(true)
        .encode()?;
    // Match wamn build: remove the ambient WASI CLI imports of the raw std guest.
    let target =
        repository.join(std::env::var_os("CARGO_TARGET_DIR").unwrap_or_else(|| "target".into()));
    let blob = run.work.join("blob-put.wasm");
    run.run(
        &target
            .join("debug/wamn-component-virtualizer")
            .to_string_lossy(),
        &[
            "--input",
            &std::env::var(COMPONENT_INPUT).context(COMPONENT_INPUT)?,
            "--output",
            &blob.to_string_lossy(),
        ],
    )
    .await?;
    let components = [("fixture", fixture), ("blob-put", std::fs::read(blob)?)];
    std::fs::create_dir_all(output.join("components"))?;
    for (name, bytes) in components {
        let path = format!("components/{name}.wasm");
        std::fs::write(output.join(&path), &bytes)?;
        receipt.outputs.components.push(ReceiptComponent {
            name: name.into(),
            path,
            sha256: wamn_engine::component_admission::component_digest(&bytes),
        });
    }
    std::fs::write(output.join(RECEIPT_FILE), receipt.to_bytes())?;
    let scope = wamn_catalog::ComponentPackageScope::new(
        wamn_control_registry::project_env_key(ORG, PROJECT, "files"),
        "file_fixture",
        "0.1.0",
    )?;
    let mut admissions = Vec::new();
    for name in ["fixture", "blob-put"] {
        let (declaration, platform_packages) = if name == "fixture" {
            (
                wamn_control::component_declaration::render_declaration_document(
                    &package.join("publication/components/fixture.json.in"),
                    &scope.env_key,
                )?,
                vec!["wamn:node".into(), "wamn:postgres".into()],
            )
        } else {
            let template = palette.join("declaration.json.in");
            let declaration = wamn_control::component_declaration::render_palette_declaration(
                &template, &scope, None,
            )?;
            let packages = wamn_control::component_declaration::declared_platform_packages(
                &template,
                &declaration,
            )?;
            (declaration, packages)
        };
        let declaration_path = run.work.join(format!("file-{name}-declaration.json"));
        std::fs::write(&declaration_path, serde_json::to_vec(&declaration)?)?;
        admissions.push(wamn_control::component_admission::admit_component(
            wamn_control::component_admission::AdmitComponentRequest {
                package: package.clone(),
                component_bytes: output.join(format!("components/{name}.wasm")),
                declaration: declaration_path,
                admitted_platform_packages: platform_packages,
                base_digests: BTreeSet::new(),
            },
        )?);
    }
    Ok((package, admissions))
}

fn release_request() -> anyhow::Result<wamn_control::publish_release::PublishReleaseRequest> {
    Ok(wamn_control::publish_release::PublishReleaseRequest {
        applications: [(
            "files".into(),
            wamn_catalog::ApplicationDefinition {
                packages: ["file_fixture".into()].into(),
                connectors: BTreeMap::new(),
                connections: BTreeMap::new(),
            },
        )]
        .into(),
        application: None,
        database_url: String::new(),
        control_database_url: String::new(),
        org: ORG.into(),
        project: PROJECT.into(),
        environment: "files".into(),
        verified_publisher_principal: "kind-fixture".into(),
        run_schema: "wamn_run".into(),
        packages: vec![wamn_catalog::PackageCoordinate::new(
            "file_fixture",
            "0.1.0",
        )?],
    })
}

/// Build and push the authored file package and its palette component.
async fn publish(
    run: &Run,
    repository: &Path,
    registry: &PackageRegistry,
    database_url: &str,
) -> anyhow::Result<wamn_control::publish_release::PublishedRelease> {
    let (package, admissions) = Box::pin(build_fixture(run, repository, database_url)).await?;
    let component_base = format!(
        "{}/wamn/components",
        registry
            .artifact_base
            .strip_suffix("/wamn/packages")
            .context("fixture registry")?
    );
    Box::pin(push_package(&PushPackageRequest {
        package: package.clone(),
        registry: registry.clone(),
        component_artifact_base: component_base.clone(),
    }))
    .await?;
    let request = release_request()?;
    let mut published =
        wamn_control::publish_release::assemble_local_release(&request, &[package], &admissions)?
            .published;
    let route_path = std::env::var_os("WAMN_FLOW_HTTP_COMPONENT").map_or_else(
        || repository.join("apps/target/wasm32-wasip2/release/http_route.wasm"),
        std::path::PathBuf::from,
    );
    let entry = wamn_control::publish_release::compose_release_entry(
        &published,
        &admissions,
        &std::fs::read(route_path)?,
        &request.applications,
        None,
    )?;
    wash_runtime::oci::set_extra_ca_certificates(&registry.oci_ca_paths)?;
    let digest = wash_runtime::oci::push_component(
        &format!(
            "{component_base}/{}:{}",
            wamn_catalog::COMPOSED_ENTRY_REPOSITORY,
            entry.digest().trim_start_matches("sha256:")
        ),
        entry.bytes(),
        wash_runtime::oci::OciConfig::new_with_credentials(super::USER, super::PASSWORD),
        None,
    )
    .await
    .context("publish the file fixture HTTP entry")?;
    wamn_control::publish_release::record_composed_entry(
        &mut published,
        &entry,
        &format!("{}@{digest}", wamn_catalog::COMPOSED_ENTRY_REPOSITORY),
    )?;
    Ok(published)
}

pub async fn exercise(
    run: &Run,
    repository: &Path,
    platform: &Platform,
    registry: &PackageRegistry,
    images: &wamn_control::release_chart::ImageSet,
    admin_url: &str,
) -> anyhow::Result<()> {
    let source = repository.join("crates/control/lib/tests/support/file_package/deploy");
    for (image, pin) in IMAGES {
        let digests = run
            .run(
                "docker",
                &[
                    "image",
                    "inspect",
                    "--format",
                    "{{json .RepoDigests}}",
                    image,
                ],
            )
            .await?;
        ensure!(digests.contains(pin), "{image} is not the pinned {pin}");
        run.run(
            "kind",
            &["load", "docker-image", "--name", &run.cluster, image],
        )
        .await?;
    }
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "apply",
            "-f",
            &source.join("minio.yaml").to_string_lossy(),
        ],
    )
    .await?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "wait",
            "--for=condition=complete",
            "job/c4-files-minio-prepare",
            "--timeout=180s",
        ],
    )
    .await?;
    let (minio_address, _minio_forward) = forward(run, "svc/c4-files-minio", 9000).await?;
    let published = Box::pin(publish(run, repository, registry, admin_url)).await?;
    let pushed = wamn_control::publish_release::push_manifest_bytes(
        &published.canonical_bytes,
        &platform.release_artifact_base,
        false,
        &platform.oci_ca_paths,
        &platform.registry_auth_file,
    )
    .await?;
    let mut releases = registry.clone();
    releases
        .artifact_base
        .clone_from(&platform.release_artifact_base);
    super::qualification_artifact::push(
        &releases,
        &super::qualification_artifact::QualificationPayload {
            format: super::qualification_artifact::QUALIFICATION_PAYLOAD_FORMAT,
            release_digest: pushed.digest.as_str().into(),
            image_set: super::qualification_artifact::image_set_digests(images)?,
            cases: vec![],
        },
        published.canonical_bytes.len(),
    )
    .await?;
    let environment = run.work.join("files.k");
    std::fs::write(
        &environment,
        std::fs::read_to_string(source.join("environment.k"))?
            .replace("__RELEASE_DIGEST__", pushed.digest.as_str()),
    )?;
    let document = EnvironmentDocument::compile(&environment)?;
    let mut bootstrap = document.clone();
    bootstrap.release = DeclaredRelease::None;
    bootstrap
        .applications
        .get_mut("files")
        .context("files application")?
        .connections
        .clear();
    let bootstrap_path = run.work.join("files-bootstrap.k");
    std::fs::write(&bootstrap_path, bootstrap.to_kcl())?;
    environment::apply(platform, &bootstrap_path).await?;
    let mut connection: Value =
        serde_json::from_slice(&std::fs::read(source.join("connection.json"))?)?;
    connection["parameters"]["cdn_host"] =
        json!(format!("http://files-a.localhost:{}", minio_address.port()));
    for name in ["file-store", "files-no-cdn"] {
        connection["name"] = json!(name);
        if name == "files-no-cdn" {
            connection["parameters"]
                .as_object_mut()
                .context("store parameters")?
                .remove("cdn_host");
        }
        wamn_control::connection_configuration::write(
            &platform.system_database_url,
            ORG,
            "connection.write",
            &connection.to_string(),
            &wamn_platform_identity::auth_changes::AuthChanges::default(),
            None,
        )
        .await?;
        wamn_control::connection_configuration::rotate(
            &platform.system_database_url,
            ORG,
            name,
            &std::fs::read_to_string(source.join("credential.json"))?,
        )
        .await?;
    }
    let (mut system, system_task) =
        tokio_postgres::connect(&platform.system_database_url, tokio_postgres::NoTls).await?;
    let system_task = tokio::spawn(system_task);
    let tx = system.transaction().await?;
    tx.batch_execute("SET LOCAL ROLE wamn_control").await?;
    tx.batch_execute(&wamn_control_provision::bind_platform_principal_sql(
        wamn_control_provision::PlatformComponent::Provisioning,
    ))
    .await?;
    for name in ["file-store", "files-no-cdn"] {
        wamn_platform_identity::connections::enable_connection(&tx, ORG, PROJECT, name).await?;
    }
    for (scope, id) in [
        ("org", ORG.to_owned()),
        ("project", PROJECT.to_owned()),
        ("environment", format!("{PROJECT}/files")),
    ] {
        wamn_platform_identity::connections::allow_integration(&tx, ORG, scope, &id, "s3").await?;
    }
    tx.commit().await?;
    let suffix: String = system.query_one("SELECT instance_suffix FROM registry.project_envs WHERE org=$1 AND project=$2 AND env='files'", &[&ORG, &PROJECT]).await?.get(0);
    let mut group: Value = serde_yaml::from_slice(&std::fs::read(
        platform.host_group.as_ref().context("host group")?,
    )?)?;
    let event_server = &platform.events.as_ref().context("events")?.nats_url;
    let (with_events, secret) = super::event_overlay(&document, group, event_server)?;
    group = with_events;
    let secret_path = run.work.join("files-events.json");
    std::fs::write(&secret_path, serde_json::to_vec(&secret)?)?;
    run.run(
        "kubectl",
        &[
            "-n",
            NAMESPACE,
            "apply",
            "-f",
            &secret_path.to_string_lossy(),
        ],
    )
    .await?;
    let issuer = format!("https://integration-session.{NAMESPACE}.svc.cluster.local");
    group["env"]
        .as_array_mut()
        .context("host environment")?
        .extend([
            json!({"name":"WAMN_SESSION_ISSUER", "value":issuer}),
            json!({"name":"WAMN_SESSION_JWKS_CA", "value":"/integration-session/ca.crt"}),
            json!({"name":"WAMN_SESSION_INSTANCE_SUFFIX", "value":suffix}),
        ]);
    group["volumes"]
        .as_array_mut()
        .context("volumes")?
        .push(json!({"name":"integration-session", "configMap":{"name":"integration-session-ca"}}));
    group["volumeMounts"].as_array_mut().context("mounts")?.push(json!({"name":"integration-session", "mountPath":"/integration-session", "readOnly":true}));
    let group_path = run.work.join("files-host-group.yaml");
    std::fs::write(&group_path, serde_yaml::to_string(&group)?)?;
    let mut platform = platform.clone();
    platform.host_group = Some(group_path);
    super::apply_with_diagnostics(&run.work, &platform, &document, "files").await?;
    let database =
        wamn_control_provision::project_env_database_name(ORG, PROJECT, "files", &suffix);
    let mut project_url = url::Url::parse(admin_url)?;
    project_url.set_path(&database);
    let (mut project, project_task) =
        tokio_postgres::connect(project_url.as_str(), tokio_postgres::NoTls).await?;
    let project_task = tokio::spawn(project_task);
    let env_key = wamn_control_registry::project_env_key(ORG, PROJECT, "files");
    let sessions = sessions(&mut system, &mut project, &issuer, &suffix, &env_key).await?;
    system_task.abort();
    project_task.abort();
    let project_reader_url = secret_url(
        run,
        &platform,
        wamn_control_provision::WorkloadRoleFamily::App,
    )
    .await?;
    let connection_reader_url = secret_url(
        run,
        &platform,
        wamn_control_provision::WorkloadRoleFamily::ConnectionReader,
    )
    .await?;
    let host = wamn_control::release_chart::release_name(ORG, PROJECT, "files")?;
    // The Service has no selector, so forward to a running host pod of the
    // release host group.
    let pod = run
        .run(
            "kubectl",
            &[
                "-n",
                NAMESPACE,
                "get",
                "pods",
                "-l",
                &format!("wasmcloud.com/hostgroup={host}"),
                "--field-selector",
                "status.phase=Running",
                "-o",
                "name",
            ],
        )
        .await?;
    let pod = pod.lines().next().context("the files host pod")?;
    let (host_address, _host_forward) = forward(run, pod, 8082).await?;
    let (api_address, _api_forward) = forward(run, pod, 80).await?;
    assertions(
        run,
        &Fixture {
            base_url: format!("http://{host_address}"),
            api_url: format!("http://{api_address}"),
            route_host: document.route_host,
            session: sessions[0].clone(),
            denied_session: sessions[1].clone(),
            project_reader_url,
            connection_reader_url,
            org: ORG.into(),
            project: PROJECT.into(),
            environment: "files".into(),
            minio_address,
        },
    )
    .await
}

async fn secret_url(
    run: &Run,
    platform: &Platform,
    family: wamn_control_provision::WorkloadRoleFamily,
) -> anyhow::Result<String> {
    use base64::Engine as _;
    let name = wamn_control_provision::workload_secret_name(family, ORG, PROJECT, "files");
    let secret = run
        .json(
            "kubectl",
            &["-n", NAMESPACE, "get", "secret", &name, "-o", "json"],
        )
        .await?;
    let encoded = secret["data"]["url"].as_str().context("database URL")?;
    let bytes = base64::engine::general_purpose::STANDARD.decode(encoded)?;
    let mut url = url::Url::parse(std::str::from_utf8(&bytes)?)?;
    let local = url::Url::parse(&platform.system_database_url)?;
    url.set_host(local.host_str())?;
    url.set_port(local.port())
        .map_err(|()| anyhow::anyhow!("database port"))?;
    Ok(url.into())
}

async fn forward(
    run: &Run,
    resource: &str,
    target: u16,
) -> anyhow::Result<(std::net::SocketAddr, Child)> {
    let address = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let child = Command::new("kubectl")
        .env("KUBECONFIG", run.work.join("kubeconfig"))
        .args([
            "-n",
            NAMESPACE,
            "port-forward",
            resource,
            &format!("{}:{target}", address.port()),
        ])
        .kill_on_drop(true)
        .spawn()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if tokio::net::TcpStream::connect(address).await.is_ok() {
            return Ok((address, child));
        }
        ensure!(
            tokio::time::Instant::now() < deadline,
            "port forward for {resource} did not start"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn sessions(
    system: &mut tokio_postgres::Client,
    project: &mut tokio_postgres::Client,
    issuer: &str,
    suffix: &str,
    env_key: &str,
) -> anyhow::Result<[String; 2]> {
    let actor = wamn_control_provision::PlatformComponent::Provisioning
        .principal_id()
        .to_string();
    for db in [&*system, &*project] {
        db.execute("SELECT set_config('app.user_id',$1,false),set_config('app.operation','admin:files-kind',false)", &[&actor]).await?;
    }
    let user = uuid::Uuid::new_v4().to_string();
    let email = "files-kind@example.invalid";
    system.execute("INSERT INTO identity.principals(id,type,subject,email,display_name) VALUES ($1::text::uuid,'user',$2,$2,'Kind files')", &[&user, &email]).await?;
    system.execute("INSERT INTO identity.project_env_memberships(principal_id,org,project,env) VALUES ($1::text::uuid,$2,$3,'files')", &[&user, &ORG, &PROJECT]).await?;
    project
        .execute(
            "INSERT INTO platform.\"user\"(id,type,email) VALUES ($1::text::uuid,'user',$2)",
            &[&user, &email],
        )
        .await?;
    project
        .batch_execute("INSERT INTO platform.role(name) VALUES ('file-access'),('file-denied')")
        .await?;
    for action in ["put", "get", "expose", "delete"] {
        project
            .execute(
                "INSERT INTO platform.permission(role_name,permission) VALUES ('file-access',$1)",
                &[&format!("wamn-control:file/{action}")],
            )
            .await?;
    }
    for role in ["file-access", "file-denied"] {
        wamn_control::user_role::grant_role_on(
            project,
            env_key,
            email,
            role,
            &wamn_platform_identity::auth_changes::AuthChanges::default(),
        )
        .await?;
    }
    let audience = wamn_control_provision::session_target::session_audience(
        &wamn_control_registry::Triple::new(ORG, PROJECT, "files"),
        suffix,
    )?;
    let tx = system.transaction().await?;
    let renewal = wamn_platform_identity::password_login::create_login(
        &tx,
        &user.parse()?,
        issuer,
        &audience,
    )
    .await?
    .context("create file fixture login")?;
    tx.commit().await?;
    let mut tokens = Vec::new();
    for role in ["file-access", "file-denied"] {
        let token = wamn_platform_identity::session_token::sign_session_token(
            system,
            wamn_session::token::SessionClaims {
                iss: issuer.into(),
                sub: user.clone(),
                org: ORG.into(),
                aud: audience.clone(),
                roles: vec![role.into()],
                iat: 0,
                exp: 0,
                jti: uuid::Uuid::new_v4().to_string(),
                authority: wamn_session::token::SessionAuthority::Login(renewal.login.id.clone()),
                csrf: None,
            },
            chrono::Utc::now().timestamp(),
        )
        .await?;
        tokens.push(format!("Bearer {}", token.token()));
    }
    tokens
        .try_into()
        .map_err(|_| anyhow::anyhow!("two file fixture sessions"))
}

/// The file fixture release must have an http role, so that file.expose has a
/// workload on port 80. This needs no kind cluster.
#[tokio::test]
#[ignore = "requires: PostgreSQL binaries, WAMN_BLOB_PUT_COMPONENT, debug component virtualizer"]
async fn file_fixture_assembles_an_http_role_without_kind() -> anyhow::Result<()> {
    let mut postgres = wamn_test_postgres::start(&[])?;
    let database = postgres.create_database("file_http_role")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let work = std::env::temp_dir().join(format!("file-http-role-proof-{}", std::process::id()));
    let run = Run {
        work: work.clone(),
        cluster: String::new(),
        registry: String::new(),
        report: Vec::new(),
        failures: super::Failures::default(),
    };
    let (package, admissions) = Box::pin(build_fixture(&run, &repository, database.url())).await?;
    let published = wamn_control::publish_release::assemble_local_release(
        &release_request()?,
        &[package],
        &admissions,
    )?
    .published;
    let roles = wamn_control::release_chart::roles(&published.manifest, ORG, PROJECT, "files");
    assert!(
        roles
            .iter()
            .any(|role| role.name == wamn_control::release_chart::RoleName::Http),
        "the file fixture release has an http role"
    );
    let (_, attachment) = published
        .manifest
        .every_attachment()
        .find(|(id, _)| *id == "fixture-run-http")
        .expect("authored fixture route");
    assert_eq!(
        attachment.attachment_type(),
        wamn_catalog::AttachmentType::Http
    );
    std::fs::remove_dir_all(work)?;
    Ok(())
}

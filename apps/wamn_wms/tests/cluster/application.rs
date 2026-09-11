//! WMS fixture setup and released application scenarios.

use std::path::Path;

use anyhow::Context as _;
use futures_util::TryStreamExt as _;
use object_store::{ObjectStore as _, ObjectStoreExt as _, aws::AmazonS3};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio_postgres::Client;
use wamn_gate_harness::journey::{JourneyDocument, RuntimePhase};

use crate::wms_runtime_live::{
    assert_committed_move_after_label_failure, assert_committed_rows, assert_contention_and_replay,
    assert_remaining_operations, assert_single_label, write_result,
};

const PRODUCT_ID: &str = "00000000-0000-0000-0000-000000000101";
const LOCATION_A_ID: &str = "00000000-0000-0000-0000-000000000201";
const LOCATION_B_ID: &str = "00000000-0000-0000-0000-000000000202";
pub(super) const PALLET_ID: &str = "00000000-0000-0000-0000-000000000301";

pub(super) async fn prepare_application(
    inputs: &JourneyDocument,
    work: &Path,
    admin_url: &str,
    scenario_worker: &Path,
    label_render: &Path,
    minio_endpoint: &str,
    evidence: &Path,
) -> anyhow::Result<(
    wamn_ctl::dev::environment::ProvisionedRoute,
    wamn_ctl::print_release_env::ReleaseCarrier,
)> {
    crate::environment::install_control(admin_url, &inputs.system_pg_url).await?;
    let issuer = wamn_ctl::dev::pat_issuer::start_for_issuer(
        &inputs.system_pg_url,
        work,
        "https://127.0.0.1",
        std::time::Duration::from_secs(86_400),
        0,
        "wamn-identity-db",
    )
    .await?;
    let issued =
        crate::environment::provision_project(inputs, work, admin_url, issuer.args.clone()).await;
    let stopped = issuer.stop().await;
    let route = match (issued, stopped) {
        (Ok(route), Ok(())) => route,
        (Err(error), Ok(())) => return Err(error),
        (Ok(_), Err(error)) => return Err(error),
        (Err(error), Err(cleanup)) => {
            return Err(error.context(format!("PAT shutdown also failed: {cleanup:#}")));
        }
    };
    let credentials = crate::environment::prepare_project(inputs, work, admin_url, &route).await?;
    let release = crate::environment::publish(
        inputs,
        &route,
        &credentials,
        scenario_worker,
        label_render,
        minio_endpoint,
        evidence,
    )
    .await?;
    let (project, task) = wamn_ctl::dev::environment::connect(&route.database_url).await?;
    let seeded = seed_fixture(project.as_ref()).await;
    drop(project);
    task.abort();
    seeded?;
    Ok((route, release))
}

pub(super) fn host_secrets(
    inputs: &JourneyDocument,
    database_host: &str,
) -> anyhow::Result<Vec<wamn_test_infrastructure::secrets::HostSecret>> {
    use wamn_control_provision::WorkloadRoleFamily;
    use wamn_test_infrastructure::secrets::{HostSecretsInput, derive_host_secrets};
    derive_host_secrets(
        &inputs.host_secret_directory,
        &HostSecretsInput {
            role_families: vec![
                WorkloadRoleFamily::ExecutorPlatform,
                WorkloadRoleFamily::IdentityReader,
                WorkloadRoleFamily::HttpAdmitter,
                WorkloadRoleFamily::EventMaterializer,
            ],
            guest_secret_file: "guest-sql.json".into(),
            namespace: inputs.host_secret_namespace.clone(),
            database_host: database_host.to_owned(),
        },
    )
}

pub(super) fn render_host(
    inputs: &JourneyDocument,
    host_tag: &str,
    nats_url: &str,
    database_host: &str,
    manifest_digest: &str,
    source: &async_nats::jetstream::stream::Config,
    replicas: u32,
    work: &Path,
) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf)> {
    use wamn_control_provision::WorkloadRoleFamily;
    use wamn_test_infrastructure::rendering::{
        EventIdentity, HostIdentity, HostRoleSecret, HostValuesInput, assert_rendered_identity,
        render_host_values,
    };

    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let secrets = host_secrets(inputs, database_host)?;
    let guest = secrets
        .iter()
        .find(|secret| secret.family == WorkloadRoleFamily::App)
        .context("the WMS host requires its provisioned guest Secret")?;
    let role_secrets = secrets
        .iter()
        .filter(|secret| secret.family != WorkloadRoleFamily::App)
        .map(|secret| HostRoleSecret {
            family: secret.family,
            name: secret.name.clone(),
        })
        .collect();
    let rendered = render_host_values(
        &std::fs::read_to_string(repository.join("deploy/platform/values-host-default.yaml"))?,
        &std::fs::read_to_string(repository.join("deploy/platform/values-host-wms-pat.yaml"))?,
        &HostValuesInput {
            namespace: inputs.host_secret_namespace.clone(),
            host_tag: host_tag.to_owned(),
            replicas,
            stream_replicas: source.num_replicas,
            dup_window_secs: source.duplicate_window.as_secs(),
            component_artifact_base: inputs.component_artifact_base.clone(),
            release_artifact_base: inputs.release_artifact_base.clone(),
            manifest_digest: manifest_digest.to_owned(),
            nats_url: nats_url.to_owned(),
            event: EventIdentity {
                org: crate::environment::ORG.to_owned(),
                project: crate::environment::PROJECT.to_owned(),
                environment: crate::environment::ENVIRONMENT.to_owned(),
            },
            guest_secret_name: guest.name.clone(),
            role_secrets,
            object_store_secret_name: Some(format!(
                "wamn-object-store-credentials-{}--{}--{}",
                crate::environment::ORG,
                crate::environment::PROJECT,
                crate::environment::ENVIRONMENT,
            )),
        },
    )?;
    assert_rendered_identity(
        &rendered.overlay,
        &HostIdentity {
            org: crate::environment::ORG.to_owned(),
            project: crate::environment::PROJECT.to_owned(),
            schema: crate::environment::SCHEMA.to_owned(),
        },
    )?;
    let base = work.join("host-base.yaml");
    let overlay = work.join("host-overlay.yaml");
    std::fs::write(&base, rendered.base)?;
    std::fs::write(&overlay, rendered.overlay)?;
    Ok((base, overlay))
}

pub(super) fn render_workload(
    inputs: &JourneyDocument,
    image: &str,
    work: &Path,
) -> anyhow::Result<std::path::PathBuf> {
    use wamn_test_infrastructure::rendering::{
        HttpClaims, HttpWorkloadInput, render_http_workload,
    };
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = render_http_workload(
        &std::fs::read_to_string(
            repository.join("deploy/platform/http-route-workload.example.yaml"),
        )?,
        &HttpWorkloadInput {
            namespace: inputs.host_secret_namespace.clone(),
            image: image.to_owned(),
            route_host: inputs.route_host.clone(),
            claims: HttpClaims {
                tenant: crate::environment::TENANT.to_owned(),
                catalog: "default".to_owned(),
                environment: crate::environment::ENVIRONMENT.to_owned(),
                project: crate::environment::PROJECT.to_owned(),
                schema: crate::environment::SCHEMA.to_owned(),
            },
        },
    )?;
    let path = work.join("flow-http.yaml");
    std::fs::write(&path, output)?;
    Ok(path)
}

pub(super) async fn seed_fixture(project: &Client) -> anyhow::Result<()> {
    project.batch_execute(&format!(
        "INSERT INTO wms.product (id, product_code) VALUES ('{PRODUCT_ID}', 'PROD-101');\n\
         INSERT INTO wms.location (id, location_code) VALUES ('{LOCATION_A_ID}', 'LOC-A'), ('{LOCATION_B_ID}', 'LOC-B');\n\
         INSERT INTO wms.pallet (id, pallet_code, location_id, status) VALUES ('{PALLET_ID}', 'PAL-301', '{LOCATION_A_ID}', 'available');\n\
         INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status) VALUES ('{PALLET_ID}', '{PRODUCT_ID}', 10, 'available');"
    )).await.context("seed the existing WMS application fixture")
}

pub(super) fn runtime_phase(route_endpoint: String) -> RuntimePhase {
    RuntimePhase {
        route_endpoint,
        pallet_id: PALLET_ID.to_owned(),
        to_location_id: LOCATION_B_ID.to_owned(),
    }
}

pub(super) async fn released_routes(
    document: &JourneyDocument,
    store: &AmazonS3,
    evidence: &Path,
) -> anyhow::Result<()> {
    let contention = assert_contention_and_replay(document).await?;
    write_result(evidence, "wms-contention-result.json", &contention)?;
    let operations = assert_remaining_operations(document).await?;
    write_result(evidence, "wms-operations-result.json", &operations)?;

    let prefix = object_store::path::Path::from("wms");
    let objects = store
        .list(Some(&prefix))
        .try_collect::<Vec<_>>()
        .await
        .context("list labels written by the composed WMS route")?;
    let objects = objects
        .iter()
        .map(|object| {
            json!({
                "key": object.location.filename().unwrap_or(""),
                "size": object.size,
            })
        })
        .collect::<Vec<_>>();
    let movement = contention["movement_id"]
        .as_str()
        .context("the contention result has a movement id")?;
    assert_single_label(&objects, movement)?;
    write_result(evidence, "labels-objects.json", &Value::Array(objects))?;
    Ok(())
}

pub(super) async fn partial_completion(
    document: &mut JourneyDocument,
    project: &Client,
    evidence: &Path,
) -> anyhow::Result<()> {
    anyhow::ensure!(document.runtime.is_some(), "the WMS route is ready");
    let previous = document.runtime.take().context("the WMS route is ready")?;
    document.runtime = Some(RuntimePhase {
        route_endpoint: previous.route_endpoint.clone(),
        pallet_id: previous.pallet_id.clone(),
        to_location_id: LOCATION_A_ID.to_owned(),
    });
    let partial = assert_committed_move_after_label_failure(document).await;
    document.runtime = Some(previous);
    let (http, result) = partial?;
    write_result(evidence, "wms-partial-http.json", &http)?;
    write_result(evidence, "wms-partial-result.json", &result)?;
    anyhow::ensure!(
        http["status"] == 500,
        "the failed label write must return HTTP 500"
    );
    anyhow::ensure!(
        result["command_requests"] == 1 && result["effect_outcome"] == "responded",
        "the partial result must retain one command request and the responded outcome"
    );
    let rows = assert_committed_rows(project, &result).await?;
    write_result(evidence, "wms-partial-database.json", &rows)
}

pub(super) async fn generated_terminal(
    inputs: &JourneyDocument,
    repository: &Path,
    target: &Path,
    project_url: &str,
    target_instance: &str,
    mode: &str,
    work: &Path,
    evidence: &Path,
    store: &AmazonS3,
) -> anyhow::Result<()> {
    use sha2::{Digest as _, Sha256};
    let token =
        wamn_ctl::dev::environment::secret_value(&inputs.route_caller_secret_output, "token")?;
    let token_path = work.join(format!("generated-tui-{mode}-pat"));
    let database_path = work.join(format!("generated-tui-{mode}-database-url"));
    super::deployment::write_private(&token_path, token.as_bytes())?;
    super::deployment::write_private(&database_path, project_url.as_bytes())?;
    let runtime = inputs
        .runtime
        .as_ref()
        .context("the generated terminal requires the released route")?;
    let output = evidence.join(format!("generated-tui-{mode}"));
    super::deployment::checked(
        Command::new("python3")
            .arg(repository.join("docs/perf/2026.09/generated-tui-wms/tools/wms_pty.py"))
            .arg("--binary")
            .arg(target.join("debug/examples/wms_move"))
            .arg("--operator-pat-file")
            .arg(&token_path)
            .arg("--target-postgres-url-file")
            .arg(&database_path)
            .arg("--endpoint")
            .arg(&runtime.route_endpoint)
            .arg("--host")
            .arg(&inputs.route_host)
            .arg("--target-instance")
            .arg(target_instance)
            .arg("--mode")
            .arg(mode)
            .arg("--evidence-dir")
            .arg(&output),
    )
    .await?;
    let result: Value = serde_json::from_slice(&std::fs::read(output.join("result.json"))?)?;
    anyhow::ensure!(
        result["passed"] == true && result["cleanup"] == true,
        "the generated terminal did not complete its scenario and cleanup"
    );
    if mode == "success" {
        let key = result["stored_key"]
            .as_str()
            .context("the generated terminal returns its label key")?;
        let suffix = key
            .strip_prefix("wms/")
            .context("the generated terminal label belongs to WMS")?;
        anyhow::ensure!(
            suffix.len() == 36
                && suffix.bytes().all(|byte| byte.is_ascii_digit()
                    || (b'a'..=b'f').contains(&byte)
                    || byte == b'-'),
            "the generated terminal returned an invalid label key"
        );
        let label = store
            .get(&object_store::path::Path::from(key))
            .await?
            .bytes()
            .await?;
        std::fs::write(output.join("label.zpl"), &label)?;
        let digest = hex::encode(Sha256::digest(&label));
        anyhow::ensure!(
            result["label_sha256"] == digest,
            "the stored label differs from the generated terminal response"
        );
    }
    Ok(())
}

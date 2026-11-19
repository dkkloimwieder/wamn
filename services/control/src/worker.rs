//! Configure operation Job credentials and execute project operations.

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use std::path::Path;
use wamn_environment::environment::{Platform, capture_gap, document::EnvironmentFile, operation};
use wamn_identity_protocol::PrincipalId;
use wamn_kubernetes::job;
use wamn_registry::{Triple, name::REGISTRY_PULL};

use crate::{
    Configuration,
    control_route::operations::{Operation, Request},
};

/// Mount the installed OCI client configuration for the selected credential source.
pub(crate) fn mount_registry(job: &mut Value, metadata: bool) {
    let spec = &mut job["spec"]["template"]["spec"];
    let volume = if metadata {
        json!({"name":"registry","configMap":{"name":"wamn-registry-helper"}})
    } else {
        spec["imagePullSecrets"] = json!([{"name":REGISTRY_PULL}]);
        json!({"name":"registry","secret":{"secretName":REGISTRY_PULL,"defaultMode":256,
            "items":[{"key":".dockerconfigjson","path":"config.json"}]}})
    };
    spec["volumes"].as_array_mut().unwrap().push(volume);
    job::container(job)["volumeMounts"]
        .as_array_mut()
        .unwrap()
        .push(json!({"name":"registry","mountPath":"/credentials/registry","readOnly":true}));
    job::environment(job, &[("DOCKER_CONFIG", "/credentials/registry")]);
}

/// Mount the CA used to verify the broker's TLS certificate.
pub(crate) fn mount_broker_ca(job: &mut Value) {
    job["spec"]["template"]["spec"]["volumes"].as_array_mut().expect("volumes").push(
        json!({"name":"nats-ca","configMap":{"name":"nats-ca","items":[{"key":"ca.crt","path":"ca.crt"}]}}),
    );
    wamn_kubernetes::job::container(job)["volumeMounts"]
        .as_array_mut()
        .expect("mounts")
        .push(json!({"name":"nats-ca","mountPath":"/etc/wamn/nats-ca","readOnly":true}));
}

pub(crate) fn mount_identity_worker_token(job: &mut Value) {
    let token_file = Path::new(wamn_identity_client::TOKEN_PATH);
    job["spec"]["template"]["spec"]["volumes"].as_array_mut().expect("volumes").extend([
        json!({"name":"identity-ca","configMap":{"name":"identity-ca"}}),
        json!({"name":"identity-token","projected":{"sources":[{"serviceAccountToken":{
            "audience":wamn_identity_client::TOKEN_AUDIENCE,"path":token_file.file_name().and_then(std::ffi::OsStr::to_str).expect("token file name"),"expirationSeconds":3600
        }}]}}),
    ]);
    wamn_kubernetes::job::container(job)["volumeMounts"].as_array_mut().expect("mounts").extend([
        json!({"name":"identity-ca","mountPath":"/etc/identity-ca","readOnly":true}),
        json!({"name":"identity-token","mountPath":token_file.parent().expect("token directory"),"readOnly":true}),
    ]);
}

/// Mount the worker's credential staging directory.
pub(crate) fn mount_database_files(job: &mut Value) {
    job["spec"]["template"]["spec"]["volumes"]
        .as_array_mut()
        .expect("worker volumes")
        .push(json!({"name":"database-files","emptyDir":{"medium":"Memory"}}));
    job::container(job)["volumeMounts"]
        .as_array_mut()
        .expect("worker volume mounts")
        .push(json!({"name":"database-files","mountPath":wamn_postgres::DATABASE_DIR}));
}

pub(crate) async fn execute(
    config: &Configuration,
    platform: Platform,
    kind: Operation,
    request: Request,
    actor: &PrincipalId,
) -> anyhow::Result<Value> {
    let file = EnvironmentFile::compile(Path::new("/etc/wamn/environment/environment.yaml"))?;
    ensure!(
        file.org == config.org && file.env == request.env,
        "mounted environment does not match the Job target"
    );
    let triple = Triple::new(file.org.clone(), request.env, request.project);
    let chart = wamn_platform_install::release_chart::bundle::select(&platform.chart)?;
    let platform = Platform {
        chart: chart.directory().to_owned(),
        ..platform
    };
    match kind {
        Operation::Recover => {
            let owner = wamn_platform_install::environment::connection(
                &platform.system_database_url,
                &triple,
            )
            .await?;
            let capture = wamn_platform_install::environment::capture_configuration(owner)?;
            platform.prepare_database(&capture).await?;
            let recovered = capture_gap::recover(
                &platform,
                &capture_gap::RecoverCaptureGapRequest {
                    admin_database_url: capture.to_json()?,
                    org: triple.org,
                    project: triple.project,
                    env: triple.env,
                    system_database_url: platform.system_database_url.clone(),
                    nats: wamn_broker::nats::PlatformNats {
                        nats_url: platform.nats_url.clone(),
                        nats_tls_ca: wamn_broker::nats::CA_FILE.into(),
                        creds: "/etc/wamn/recovery-nats/user.creds".into(),
                    },
                },
            )
            .await?;
            Ok(
                json!({"slot":recovered.slot,"reason":recovered.reason,"start_lsn":recovered.start_lsn,
                "start_at":recovered.start_at.to_rfc3339(),"end_lsn":recovered.end_lsn,
                "created_at":recovered.created_at.to_rfc3339()}),
            )
        }
        Operation::Close => {
            let closed = capture_gap::close(
                &platform,
                &capture_gap::CloseCaptureGapRequest {
                    org: triple.org,
                    project: triple.project,
                    env: triple.env,
                    system_database_url: platform.system_database_url.clone(),
                },
            )
            .await?;
            Ok(
                json!({"slot":closed.slot,"created_at":closed.created_at.to_rfc3339(),
                "resync_at":closed.resync_at.to_rfc3339()}),
            )
        }
        Operation::Reset => operation::reset(&platform, &file.into_project(&triple.project)?).await,
        _ => {
            let application = request.application.context("application is required")?;
            let document = file.into_project(&triple.project)?;
            let integration_base;
            let component_base;
            operation::application(
                &platform,
                document,
                &application,
                match kind {
                    Operation::Deploy => {
                        integration_base = format!("{}/wamn/integrations", config.registry);
                        component_base = format!("{}/wamn/components", config.registry);
                        operation::Application::Deploy {
                            digest: request.release.context("package digest is required")?,
                            integration_base: &integration_base,
                            component_base: &component_base,
                            actor,
                        }
                    }
                    Operation::Apply => operation::Application::Apply(
                        request.release.context("package digest is required")?,
                    ),
                    Operation::Diff => operation::Application::Diff(request.release),
                    Operation::Rollback => operation::Application::Rollback(request.revision),
                    Operation::Stop => operation::Application::Stop,
                    Operation::Reset | Operation::Recover | Operation::Close => {
                        unreachable!("handled project operation")
                    }
                },
            )
            .await
        }
    }
}

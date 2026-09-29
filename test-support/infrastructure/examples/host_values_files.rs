//! Write the Google Cloud host values of docs/plan/gcp-deployment.md step 3.
//!
//! The values come from `rendering::render_host_values`, the derivation of the
//! Receiving cluster tests, over `deploy/platform/values-host-default.yaml` and
//! `deploy/platform/values-host-receiving-pat.yaml`. The program then makes the
//! Google Cloud changes that docs/operations/gcp.md section 3 lists as
//! differences from kind, and writes `values-host-base.yaml` and
//! `values-host.yaml` into the output directory.
//!
//! The WMS host group of docs/operations/gcp.md section 5 comes from
//! `deploy/platform/values-host-wms-pat.yaml` in the same way, joins the
//! Receiving group in `values-host.yaml`, and names the WMS Secrets. Its label
//! store signs with the pod's service account, so it mounts no credentials.
//!
//! cargo run -p wamn-test-infrastructure --example host_values_files -- \
//!   <output directory> <release artifact base> <Receiving manifest digest> \
//!   <WMS manifest digest>

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use serde_yaml::{Mapping, Value};
use wamn_control_provision::{WorkloadRoleFamily, workload_secret_name};
use wamn_test_infrastructure::rendering::{
    EventIdentity, HostRoleSecret, HostValuesInput, host_values, render_host_values,
};

const ORG: &str = "dkk";
const ENVIRONMENT: &str = "dev";
/// The org that the checked-in Receiving overlay names.
const OVERLAY_ORG: &str = "acme";
const NAMESPACE: &str = "hosts";
const HOST_IMAGE: &str = "us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-host:src-91f318b6c6fe387c@sha256:a403fef9f1e7bca336c3640851cd3a99eee2202885e6b59f5261490b39647d76";
/// The Google service account of the label store (docs/operations/gcp.md 5.1).
const BLOB_ACCOUNT: &str = "wamn-blob@wamn-dev.iam.gserviceaccount.com";
const COMPONENT_BASE: &str = "us-central1-docker.pkg.dev/wamn-dev/wamn/components";
const EVENT_NATS: &str = "nats://evt-nats.platform.svc.cluster.local:4222";
const CONTROL_NATS: &str = "nats://nats.platform.svc.cluster.local:4222";
const ISSUER: &str = "https://identity.identity.svc.cluster.local";

/// One application's host group.
struct Application {
    group: &'static str,
    project: &'static str,
    overlay: &'static str,
    /// The instance suffix of its project environment in the registry.
    instance_suffix: &'static str,
    guest_secret: &'static str,
    /// The event NATS Secrets, which carry the application's own users.
    event_secret: &'static str,
    materializer_secret: &'static str,
}

const RECEIVING: Application = Application {
    group: "default",
    project: "receiving",
    overlay: "deploy/platform/values-host-receiving-pat.yaml",
    instance_suffix: "4pqjfmli",
    guest_secret: "wamn-host-db",
    event_secret: "wamn-event-nats",
    materializer_secret: "wamn-materializer-nats",
};

const WMS: Application = Application {
    group: "wms",
    project: "wms",
    overlay: "deploy/platform/values-host-wms-pat.yaml",
    instance_suffix: "0nk1lrpr",
    guest_secret: "wamn-host-db-wms",
    event_secret: "wamn-event-nats-wms",
    materializer_secret: "wamn-materializer-nats-wms",
};
const ROLE_FAMILIES: [WorkloadRoleFamily; 4] = [
    WorkloadRoleFamily::ExecutorPlatform,
    WorkloadRoleFamily::IdentityReader,
    WorkloadRoleFamily::HttpAdmitter,
    WorkloadRoleFamily::EventMaterializer,
];

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 4,
        "usage: <output directory> <release artifact base> <Receiving manifest digest> <WMS manifest digest>"
    );
    let output = PathBuf::from(&arguments[0]);
    let (base, receiving) = render(&RECEIVING, &arguments[1], &arguments[2])?;
    let (_, wms) = render(&WMS, &arguments[1], &arguments[3])?;
    let mut overlay: Value = serde_yaml::from_str(&receiving)?;
    let wms: Value = serde_yaml::from_str(&wms)?;
    let group = wms["runtime"]["hostGroups"][0].clone();
    overlay["runtime"]["hostGroups"]
        .as_sequence_mut()
        .context("the Receiving values have runtime.hostGroups")?
        .push(group);
    fs::write(
        output.join("values-host-base.yaml"),
        google_cloud_base(&host_values(&base, HOST_IMAGE)?)?,
    )?;
    fs::write(
        output.join("values-host.yaml"),
        serde_yaml::to_string(&overlay)?,
    )?;
    println!("{}", output.display());
    Ok(())
}

/// The base values and the Google Cloud overlay of one application.
fn render(
    application: &Application,
    release_artifact_base: &str,
    manifest_digest: &str,
) -> anyhow::Result<(String, String)> {
    let repository = repository_root();
    let base = fs::read_to_string(repository.join("deploy/platform/values-host-default.yaml"))?;
    let overlay = overlay_for_org(
        &fs::read_to_string(repository.join(application.overlay))?,
        application.project,
    )?;
    let overlay = if application.project == WMS.project {
        without_object_store_credentials(&overlay)?
    } else {
        overlay
    };
    let rendered = render_host_values(
        &base,
        &overlay,
        &HostValuesInput {
            namespace: NAMESPACE.to_owned(),
            host_tag: "unused".to_owned(),
            replicas: 1,
            component_artifact_base: COMPONENT_BASE.to_owned(),
            release_artifact_base: release_artifact_base.to_owned(),
            manifest_digest: manifest_digest.to_owned(),
            nats_url: EVENT_NATS.to_owned(),
            stream_replicas: 1,
            dup_window_secs: 120,
            event: EventIdentity {
                org: ORG.to_owned(),
                project: application.project.to_owned(),
                environment: ENVIRONMENT.to_owned(),
            },
            guest_secret_name: application.guest_secret.to_owned(),
            role_secrets: ROLE_FAMILIES
                .into_iter()
                .map(|family| HostRoleSecret {
                    family,
                    name: workload_secret_name(family, ORG, application.project, ENVIRONMENT),
                })
                .collect(),
            object_store_secret_name: None,
        },
    )?;
    Ok((
        rendered.base,
        google_cloud_overlay(&rendered.overlay, application)?,
    ))
}

/// The WMS label store is a `gcs` binding, which signs with the pod's service
/// account: no credentials file, volume or mount.
fn without_object_store_credentials(overlay: &str) -> anyhow::Result<String> {
    let mut document: Value = serde_yaml::from_str(overlay)?;
    let group = group_mut(&mut document)?;
    for (list, key, name) in [
        ("env", "name", "WAMN_CREDENTIALS_FILE"),
        ("volumes", "name", "connection-credentials"),
        ("volumeMounts", "name", "connection-credentials"),
    ] {
        let entries = group[list]
            .as_sequence_mut()
            .with_context(|| format!("the overlay host group has {list}"))?;
        let before = entries.len();
        entries.retain(|entry| entry[key] != name);
        ensure!(
            entries.len() + 1 == before,
            "the overlay has one {name} in {list}"
        );
    }
    Ok(serde_yaml::to_string(&document)?)
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Name org `dkk` where the checked-in overlay names `acme`: the `WAMN_ORG`
/// value and the four role Secret names.
fn overlay_for_org(overlay: &str, project: &str) -> anyhow::Result<String> {
    let mut document: Value = serde_yaml::from_str(overlay)?;
    let env = group_mut(&mut document)?
        .get_mut("env")
        .and_then(Value::as_sequence_mut)
        .context("the overlay host group has env")?;
    let mut renamed = 0;
    for entry in env.iter_mut() {
        if entry["name"] == "WAMN_ORG" {
            ensure!(
                entry["value"] == OVERLAY_ORG,
                "WAMN_ORG is not {OVERLAY_ORG}"
            );
            entry["value"] = ORG.into();
        }
        if let Some(name) = entry
            .get_mut("valueFrom")
            .and_then(|from| from.get_mut("secretKeyRef"))
            .and_then(|reference| reference.get_mut("name"))
        {
            for family in ROLE_FAMILIES {
                if name.as_str()
                    == Some(
                        workload_secret_name(family, OVERLAY_ORG, project, ENVIRONMENT).as_str(),
                    )
                {
                    *name = workload_secret_name(family, ORG, project, ENVIRONMENT).into();
                    renamed += 1;
                }
            }
        }
    }
    ensure!(
        renamed == ROLE_FAMILIES.len(),
        "the overlay names {renamed} of the four role Secrets"
    );
    Ok(serde_yaml::to_string(&document)?)
}

/// The control NATS runs in namespace `platform`, and the host service
/// account acts as the label store account through Workload Identity.
fn google_cloud_base(base: &str) -> anyhow::Result<String> {
    let mut document: Value = serde_yaml::from_str(base)?;
    document["runtime"]["serviceAccount"]["annotations"]["iam.gke.io/gcp-service-account"] =
        BLOB_ACCOUNT.into();
    let nats = &mut document["global"]["nats"];
    for key in ["schedulerUrl", "dataUrl"] {
        ensure!(
            nats.get(key).is_some(),
            "the base values have no global.nats.{key}"
        );
        nats[key] = CONTROL_NATS.into();
    }
    Ok(serde_yaml::to_string(&document)?)
}

/// Remove what exists only for kind, set the resources of one small node, add
/// the session entries of the identity issuer, and name the group and its
/// event NATS Secrets.
fn google_cloud_overlay(overlay: &str, application: &Application) -> anyhow::Result<String> {
    let mut document: Value = serde_yaml::from_str(overlay)?;
    let group = group_mut(&mut document)?;
    group["name"] = application.group.into();
    rename_secret(group, "wamn-event-nats", application.event_secret)?;
    rename_secret(
        group,
        "wamn-materializer-nats",
        application.materializer_secret,
    )?;
    let args = group["extraArgs"]
        .as_sequence_mut()
        .context("the overlay host group has extraArgs")?;
    let before = args.len();
    args.retain(|arg| arg != "--allow-insecure-registries");
    ensure!(
        args.len() + 1 == before,
        "the overlay has one --allow-insecure-registries"
    );
    let env = group["env"]
        .as_sequence_mut()
        .context("the overlay host group has env")?;
    let before = env.len();
    env.retain(|entry| {
        !entry["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("OTEL_"))
    });
    ensure!(
        env.len() + 3 == before,
        "the overlay has three OTEL_ entries"
    );
    for (name, value) in [
        ("WAMN_SESSION_ISSUER", ISSUER),
        ("WAMN_SESSION_INSTANCE_SUFFIX", application.instance_suffix),
        ("WAMN_SESSION_JWKS_CA", "/etc/identity-ca/ca.crt"),
    ] {
        ensure!(
            env.iter().all(|entry| entry["name"] != name),
            "the overlay already declares {name}"
        );
        env.push(entry(&[("name", name), ("value", value)]));
    }
    group["volumes"]
        .as_sequence_mut()
        .context("the overlay host group has volumes")?
        .push(serde_yaml::from_str(
            "{name: identity-ca, configMap: {name: identity-ca}}",
        )?);
    group["volumeMounts"]
        .as_sequence_mut()
        .context("the overlay host group has volumeMounts")?
        .push(serde_yaml::from_str(
            "{name: identity-ca, mountPath: /etc/identity-ca, readOnly: true}",
        )?);
    let resources = &mut group["resources"];
    resources["requests"]["cpu"] = "500m".into();
    resources["requests"]["memory"] = "256Mi".into();
    resources["limits"]["cpu"] = "2".into();
    resources["limits"]["memory"] = "4Gi".into();
    Ok(serde_yaml::to_string(&document)?)
}

/// Point every env reference and volume of Secret `from` at Secret `to`.
fn rename_secret(group: &mut Value, from: &str, to: &str) -> anyhow::Result<()> {
    let mut renamed = 0;
    for entry in group["env"].as_sequence_mut().into_iter().flatten() {
        if let Some(name) = entry
            .get_mut("valueFrom")
            .and_then(|from| from.get_mut("secretKeyRef"))
            .and_then(|reference| reference.get_mut("name"))
            && name.as_str() == Some(from)
        {
            *name = to.into();
            renamed += 1;
        }
    }
    for volume in group["volumes"].as_sequence_mut().into_iter().flatten() {
        if let Some(name) = volume
            .get_mut("secret")
            .and_then(|secret| secret.get_mut("secretName"))
            && name.as_str() == Some(from)
        {
            *name = to.into();
            renamed += 1;
        }
    }
    ensure!(renamed > 0, "the overlay names no Secret {from}");
    Ok(())
}

fn group_mut(document: &mut Value) -> anyhow::Result<&mut Value> {
    let groups = document["runtime"]["hostGroups"]
        .as_sequence_mut()
        .context("the values have runtime.hostGroups")?;
    ensure!(groups.len() == 1, "the values have one host group");
    Ok(&mut groups[0])
}

fn entry(fields: &[(&str, &str)]) -> Value {
    let mut mapping = Mapping::new();
    for (key, value) in fields {
        mapping.insert((*key).into(), (*value).into());
    }
    Value::Mapping(mapping)
}

//! Write the Google Cloud host values of docs/plan/gcp-deployment.md step 3.
//!
//! The values come from `rendering::render_host_values`, the derivation of the
//! Receiving cluster tests, over `deploy/platform/values-host-default.yaml` and
//! `deploy/platform/values-host-receiving-pat.yaml`. The program then makes the
//! Google Cloud changes that docs/operations/gcp.md section 3 lists as
//! differences from kind, and writes `values-host-base.yaml` and
//! `values-host.yaml` into the output directory.
//!
//! cargo run -p wamn-test-infrastructure --example host_values_files -- \
//!   <output directory> <release artifact base> <release manifest digest>

use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, ensure};
use serde_yaml::{Mapping, Value};
use wamn_control_provision::{WorkloadRoleFamily, workload_secret_name};
use wamn_test_infrastructure::rendering::{
    EventIdentity, HostRoleSecret, HostValuesInput, host_values, render_host_values,
};

const ORG: &str = "dkk";
const PROJECT: &str = "receiving";
const ENVIRONMENT: &str = "dev";
/// The org that the checked-in Receiving overlay names.
const OVERLAY_ORG: &str = "acme";
const NAMESPACE: &str = "hosts";
const HOST_IMAGE: &str = "us-central1-docker.pkg.dev/wamn-dev/wamn/wamn-host:src-490a0d098a176e39@sha256:b44a6f944a410ca42dccf378c0948226946c54fefa17c4bf3cc246e25dcd9dd2";
const COMPONENT_BASE: &str = "us-central1-docker.pkg.dev/wamn-dev/wamn/components";
const EVENT_NATS: &str = "nats://evt-nats.platform.svc.cluster.local:4222";
const CONTROL_NATS: &str = "nats://nats.platform.svc.cluster.local:4222";
const ISSUER: &str = "https://identity.identity.svc.cluster.local";
const INSTANCE_SUFFIX: &str = "zf7o454t";
const ROLE_FAMILIES: [WorkloadRoleFamily; 4] = [
    WorkloadRoleFamily::ExecutorPlatform,
    WorkloadRoleFamily::IdentityReader,
    WorkloadRoleFamily::HttpAdmitter,
    WorkloadRoleFamily::EventMaterializer,
];

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() == 3,
        "usage: <output directory> <release artifact base> <release manifest digest>"
    );
    let output = PathBuf::from(&arguments[0]);
    let repository = repository_root();
    let base = fs::read_to_string(repository.join("deploy/platform/values-host-default.yaml"))?;
    let overlay = overlay_for_org(&fs::read_to_string(
        repository.join("deploy/platform/values-host-receiving-pat.yaml"),
    )?)?;
    let rendered = render_host_values(
        &base,
        &overlay,
        &HostValuesInput {
            namespace: NAMESPACE.to_owned(),
            host_tag: "unused".to_owned(),
            replicas: 1,
            component_artifact_base: COMPONENT_BASE.to_owned(),
            release_artifact_base: arguments[1].clone(),
            manifest_digest: arguments[2].clone(),
            nats_url: EVENT_NATS.to_owned(),
            stream_replicas: 1,
            dup_window_secs: 120,
            event: EventIdentity {
                org: ORG.to_owned(),
                project: PROJECT.to_owned(),
                environment: ENVIRONMENT.to_owned(),
            },
            guest_secret_name: "wamn-host-db".to_owned(),
            role_secrets: ROLE_FAMILIES
                .into_iter()
                .map(|family| HostRoleSecret {
                    family,
                    name: workload_secret_name(family, ORG, PROJECT, ENVIRONMENT),
                })
                .collect(),
            object_store_secret_name: None,
        },
    )?;
    let base = google_cloud_base(&host_values(&rendered.base, HOST_IMAGE)?)?;
    let overlay = google_cloud_overlay(&rendered.overlay)?;
    fs::write(output.join("values-host-base.yaml"), base)?;
    fs::write(output.join("values-host.yaml"), overlay)?;
    println!("{}", output.display());
    Ok(())
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Name org `dkk` where the checked-in overlay names `acme`: the `WAMN_ORG`
/// value and the four role Secret names.
fn overlay_for_org(overlay: &str) -> anyhow::Result<String> {
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
                        workload_secret_name(family, OVERLAY_ORG, PROJECT, ENVIRONMENT).as_str(),
                    )
                {
                    *name = workload_secret_name(family, ORG, PROJECT, ENVIRONMENT).into();
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

/// The control NATS runs in namespace `platform`.
fn google_cloud_base(base: &str) -> anyhow::Result<String> {
    let mut document: Value = serde_yaml::from_str(base)?;
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

/// Remove what exists only for kind, set the resources of one small node, and
/// add the session entries of the identity issuer.
fn google_cloud_overlay(overlay: &str) -> anyhow::Result<String> {
    let mut document: Value = serde_yaml::from_str(overlay)?;
    let group = group_mut(&mut document)?;
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
        ("WAMN_SESSION_INSTANCE_SUFFIX", INSTANCE_SUFFIX),
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

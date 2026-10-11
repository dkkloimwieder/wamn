//! The private native credentials of a Receiving environment release.

use std::fs;
use std::path::PathBuf;

use serde_json::json;
use wamn_gate_harness::case_release::NAMESPACE;

use super::super::identity;
use super::{ReceivingCluster, write_private};

pub(super) fn native_secrets(cluster: &ReceivingCluster) -> anyhow::Result<Vec<PathBuf>> {
    let event_path = cluster.resources.work.join("wamn-event-nats.json");
    let materializer_path = cluster.resources.work.join("wamn-materializer-nats.json");
    write_private(
        &event_path,
        &serde_json::to_vec(&json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"wamn-event-nats","namespace":NAMESPACE},
            "stringData":{
                "username":cluster.broker.runtime.username,
                "password":fs::read_to_string(&cluster.broker.runtime.password_file)?,
                "org":identity().org.as_str(),"project":identity().project.as_str(),"environment":identity().environment.as_str(),
                "stream_replicas":cluster.source.num_replicas.to_string(),
                "dup_window_secs":cluster.source.duplicate_window.as_secs().to_string(),
            },
        }))?,
    )?;
    write_private(
        &materializer_path,
        &serde_json::to_vec(&json!({
            "apiVersion":"v1","kind":"Secret","type":"Opaque",
            "metadata":{"name":"wamn-materializer-nats","namespace":NAMESPACE},
            "stringData":{"binding.json":fs::read_to_string(&cluster.broker.binding)?},
        }))?,
    )?;
    Ok(vec![event_path, materializer_path])
}

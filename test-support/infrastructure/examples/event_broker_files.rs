//! Write the event NATS files of one deployed environment.
//!
//! The users come from `event_broker::prepare`, the same derivation the
//! cluster tests use. docs/operations/gcp.md section 3 runs this program and
//! turns its files into the Secrets `evt-nats-authorization`,
//! `wamn-event-nats` and `wamn-materializer-nats`.
//!
//! cargo run -p wamn-test-infrastructure --example event_broker_files -- \
//!   <work> <server> <org> <project> <env> <tenant> <replicas> [<wamn.json>...]

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use async_nats::jetstream::consumer::pull;
use serde_json::Value;
use wamn_control_provision::events::{
    advisory_stream_config, registration_consumers, source_stream_config,
};
use wamn_control_registry::Triple;
use wamn_test_infrastructure::event_broker;

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() >= 7,
        "usage: <work> <server> <org> <project> <env> <tenant> <replicas> [<wamn.json>...]"
    );
    let work = PathBuf::from(&arguments[0]);
    let server = &arguments[1];
    let scope = Triple::new(
        arguments[2].as_str(),
        arguments[3].as_str(),
        arguments[4].as_str(),
    );
    let tenant = &arguments[5];
    let replicas: usize = arguments[6].parse().context("parse the stream replicas")?;
    let source = source_stream_config(&scope, replicas, Duration::from_secs(120));
    let advisory = advisory_stream_config(&scope, replicas);
    let consumers = declared_consumers(&scope, tenant, &arguments[7..])?;
    let broker = event_broker::prepare(&work, &scope, tenant, &source, &advisory, &consumers)?;
    // One --consumer-config argument of enable-cdc-project-env per line.
    fs::write(
        work.join("consumers.jsonl"),
        consumers
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?
            .join("\n"),
    )?;
    event_broker::write_binding(&broker, server, &source)?;

    let directory = work.join("event-nats");
    let configuration: Value = serde_json::from_slice(&fs::read(&broker.configuration)?)?;
    let authorization = configuration
        .get("authorization")
        .context("the broker configuration lacks its authorization block")?;
    write_private(
        &directory.join("authorization.conf"),
        format!("authorization: {}\n", serde_json::to_string(authorization)?).as_bytes(),
    )?;
    println!("{}", directory.display());
    Ok(())
}

/// The materializer consumers of the event registrations that the given
/// package manifests declare, by the rule the development loop also uses.
fn declared_consumers(
    scope: &Triple,
    tenant: &str,
    manifests: &[String],
) -> anyhow::Result<Vec<pull::Config>> {
    let manifests = manifests
        .iter()
        .map(|path| {
            Ok(wamn_schema_generator::PackageManifest::from_slice(
                &fs::read(path).with_context(|| format!("read {path}"))?,
            )?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut consumers = registration_consumers(scope, tenant, &manifests);
    consumers.extend(wamn_control_provision::events::entity_consumers(
        scope, &manifests,
    ));
    Ok(consumers)
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create private file {}", path.display()))?;
    file.write_all(bytes).context("write private broker file")
}

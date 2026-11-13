//! Write the event NATS files of one deployed environment.
//!
//! The users come from `event_broker::prepare`, the same derivation the
//! cluster tests use. The bench runs this program and starts a local NATS
//! from its files.
//!
//! cargo run -p wamn-test-infrastructure --example event_broker_files -- \
//!   <work> <org> <project> <env> <project_env_key> <replicas> [<wamn.json>...]

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use async_nats::jetstream::consumer::pull;
use serde_json::Value;
use wamn_broker::events::{advisory_stream_config, source_stream_config};
use wamn_catalog::manifest::PackageManifest;
use wamn_control_registry::Triple;
use wamn_test_infrastructure::event_broker;

fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        arguments.len() >= 6,
        "usage: <work> <org> <project> <env> <project_env_key> <replicas> [<wamn.json>...]"
    );
    let work = PathBuf::from(&arguments[0]);
    let scope = Triple::new(
        arguments[1].as_str(),
        arguments[2].as_str(),
        arguments[3].as_str(),
    );
    let project_env_key = &arguments[4];
    let replicas: usize = arguments[5].parse().context("parse the stream replicas")?;
    let source = source_stream_config(&scope, replicas, Duration::from_secs(120));
    let advisory = advisory_stream_config(&scope, replicas);
    let consumers = declared_consumers(&scope, &arguments[6..])?;
    let broker = event_broker::prepare(
        &work,
        &scope,
        project_env_key,
        &source,
        &advisory,
        &consumers,
    )?;
    // One --consumer-config argument of enable-cdc-project-env per line.
    fs::write(
        work.join("consumers.jsonl"),
        consumers
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()?
            .join("\n"),
    )?;

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

/// The materializer consumers of captured entities in the supplied packages.
fn declared_consumers(scope: &Triple, manifests: &[String]) -> anyhow::Result<Vec<pull::Config>> {
    let manifests = manifests
        .iter()
        .map(|path| {
            if let Some(root) = wamn_schema_generator::manifest_package_root(Path::new(path)) {
                wamn_schema_generator::build_record::check_current(root)?;
            }
            Ok(wamn_catalog::manifest::PackageManifest::from_slice(
                &fs::read(path).with_context(|| format!("read {path}"))?,
            )?)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let entities = manifests.iter().flat_map(PackageManifest::entity_pairs);
    let consumers = wamn_broker::events::entity_consumers(scope, entities);
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

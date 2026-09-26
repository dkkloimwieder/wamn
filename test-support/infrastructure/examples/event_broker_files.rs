//! Write the event NATS files of one deployed environment.
//!
//! The users come from `event_broker::prepare`, the same derivation the
//! cluster tests use, plus the `tap-admin` user that creates `WAMN_TAP`.
//! docs/operations/gcp.md section 3 runs this program and turns its files
//! into the Secrets `evt-nats-authorization`, `evt-nats-bootstrap`,
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
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Value, json};
use wamn_control_provision::events::{
    advisory_stream_config, materializer_consumer_config, source_stream_config,
};
use wamn_control_registry::Triple;
use wamn_test_infrastructure::event_broker;

const TAP_ADMIN: &str = "tap-admin";
const TAP_STREAM: &str = "WAMN_TAP";

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
    event_broker::write_binding(&broker, server, &source)?;

    let directory = work.join("event-nats");
    let configuration: Value = serde_json::from_slice(&fs::read(&broker.configuration)?)?;
    let mut authorization = configuration
        .get("authorization")
        .context("the broker configuration lacks its authorization block")?
        .clone();
    let password = random_password()?;
    let inbox = format!("_INBOX_{TAP_ADMIN}");
    authorization["users"]
        .as_array_mut()
        .context("the authorization block lacks its users")?
        .push(json!({
            "user": TAP_ADMIN,
            "password": password,
            "permissions": {
                "publish": {"allow": [
                    format!("$JS.API.STREAM.INFO.{TAP_STREAM}"),
                    format!("$JS.API.STREAM.CREATE.{TAP_STREAM}"),
                    format!("$JS.API.STREAM.UPDATE.{TAP_STREAM}"),
                    format!("{inbox}.>"),
                ]},
                "subscribe": {"allow": [format!("{inbox}.>")]},
            },
        }));
    write_private(
        &directory.join("authorization.conf"),
        format!(
            "authorization: {}\n",
            serde_json::to_string(&authorization)?
        )
        .as_bytes(),
    )?;
    write_private(
        &directory.join("context.json"),
        &serde_json::to_vec(&json!({
            "url": server,
            "user": TAP_ADMIN,
            "password": password,
            "inbox_prefix": inbox,
        }))?,
    )?;
    println!("{}", directory.display());
    Ok(())
}

/// The materializer consumers of the event registrations that the given
/// package manifests declare, named as the cluster tests name them.
fn declared_consumers(
    scope: &Triple,
    tenant: &str,
    manifests: &[String],
) -> anyhow::Result<Vec<pull::Config>> {
    let sanitize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    let mut consumers = Vec::new();
    for path in manifests {
        let manifest = wamn_schema_generator::PackageManifest::from_slice(
            &fs::read(path).with_context(|| format!("read {path}"))?,
        )?;
        for (name, operation) in &manifest.custom_operations {
            if let Some(registration) = &operation.registration {
                let durable = format!(
                    "mat_{}_{}_{}",
                    sanitize(tenant),
                    sanitize(&manifest.package.id),
                    sanitize(name)
                );
                let filter = format!(
                    "evt.{}.{}.{}.{}.>",
                    scope.org,
                    scope.project,
                    scope.env.as_str(),
                    wamn_event_wire::subject_token(&registration.entity)
                );
                consumers.push(materializer_consumer_config(
                    &durable,
                    &filter,
                    Duration::from_secs(30),
                    5,
                ));
            }
        }
    }
    Ok(consumers)
}

fn random_password() -> anyhow::Result<String> {
    let mut random = [0u8; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| anyhow::anyhow!("generate broker password"))?;
    Ok(hex::encode(random))
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

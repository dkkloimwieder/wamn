//! Install an org and invite its first owner.

use std::net::IpAddr;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_registry::OrgId;

use crate::provision_org::{CONTROL_SECRET, OAUTH_ENCRYPTION_INPUT};
use crate::release_chart::POD_REGISTRY;
use crate::{org_install::OrgInstaller, platform_registry::docker_configuration};
use wamn_postgres::database::ConnectionConfig;

pub use crate::platform_registry::registry_configuration;

/// The ConfigMap with the public system database CA in each org and env namespace.
pub const SYSTEM_DATABASE_CA: &str = "wamn-pg-ca";

pub(crate) const LOCAL_PROVIDER: &str = "kind";

/// Public platform inputs for org and env provisioning.
#[derive(Debug)]
pub struct OrgInputs {
    pub values: Value,
    pub database: ConnectionConfig,
    pub api_server: Vec<Value>,
}

/// Install the org release, provision its logins, wait for kind TLS, then invite its owner.
pub async fn create(org: &OrgId, owner: &str, installation: &OrgInstaller) -> anyhow::Result<()> {
    let OrgInputs {
        values: inputs,
        database,
        api_server,
    } = installation.inputs().await?;
    let inputs = &inputs["data"];
    let image = control_image(inputs)?;
    let values = org_values(inputs, &database, org, &api_server)?;
    let on_kind = values["provider"] == LOCAL_PROVIDER;
    let credentials = if on_kind {
        Some(docker_configuration(
            field(inputs, "registry")?,
            field(&values, "registry")?,
        )?)
    } else {
        None
    };
    installation.install_release(org, &values).await?;
    if let Some(credentials) = credentials {
        installation
            .write_registry_credentials(org, &credentials)
            .await?;
    }
    let job_uid = installation.provision_logins(org, image, &database).await?;
    installation.wait_for_trust(org).await?;
    installation.reload_control(org, &job_uid).await?;
    if on_kind {
        installation.wait_for_public_certificate(org).await?;
    }
    installation.invite_owner(org, image, owner).await
}

pub(crate) fn control_image(inputs: &Value) -> anyhow::Result<&str> {
    let image = field(inputs, "controlImage")?;
    let (_, digest) = image.rsplit_once('@').context(
        "pin systemSql.image in the platform release so org control and its Jobs use an immutable image",
    )?;
    digest.parse::<wamn_digest::Digest>()?;
    Ok(image)
}

/// The org chart values from the platform's public org inputs.
pub fn org_values(
    inputs: &Value,
    database: &ConnectionConfig,
    org: &OrgId,
    api_server: &[Value],
) -> anyhow::Result<Value> {
    let image = field(inputs, "controlImage")?;
    let app_chart = field(inputs, "appChart")?;
    let (_, digest) = app_chart
        .strip_prefix("oci://")
        .and_then(|reference| reference.rsplit_once('@'))
        .filter(|(repository, _)| !repository.is_empty())
        .context("pin appChart in the platform release to an OCI chart digest")?;
    digest
        .parse::<wamn_digest::Digest>()
        .context("invalid appChart digest in the platform release")?;
    let registry = field(inputs, "registry")?;
    let provider = field(inputs, "provider")?;
    let registry_egress = registry_destinations(inputs)?;
    let oauth_encryption: Value = serde_json::from_str(field(inputs, OAUTH_ENCRYPTION_INPUT)?)
        .context("decode the platform OAuth encryption configuration")?;
    let internal_registry = internal_registry(registry);
    let connection = |host: &str, username: &str, secret: &str| {
        json!({
            "host": host, "port": database.port, "database": database.database,
            "username": username, "credentials": {"namespace": org, "name": secret},
            "server_ca": {"namespace": org, "name": SYSTEM_DATABASE_CA},
        })
    };
    let control = crate::provision_org::control_login(org, &database.database);
    Ok(json!({
        "org": org, "base": field(inputs, "base")?, "registry": internal_registry,
        "provider": provider,
        "networkPolicy": {"apiServer": api_server, "registry": registry_egress},
        "control": {
            "image": image,
            "configuration": {
                "org": org,
                "database": connection(field(inputs, "controlDatabaseHost")?, control.as_str(), CONTROL_SECRET),
                "identity_issuer": field(inputs, "identityIssuer")?,
                "nats_url": field(inputs, "natsUrl")?,
                "registry": internal_registry,
                "platform_registry": internal_registry,
                "worker_image": image,
                "chart": app_chart,
                "registry_token_metadata": provider != LOCAL_PROVIDER,
                "events": {"stream_replicas": 1, "dup_window_secs": 120},
                "oauth_encryption": oauth_encryption,
            },
        },
    }))
}

pub(crate) fn registry_destinations(inputs: &Value) -> anyhow::Result<Vec<Value>> {
    let registry_egress: Vec<Value> = serde_json::from_str(field(inputs, "registryEgress")?)
        .context("decode registryEgress from wamn-org-inputs")?;
    ensure!(
        !registry_egress.is_empty(),
        "registryEgress in wamn-org-inputs must be a nonempty list"
    );
    for (index, destination) in registry_egress.iter().enumerate() {
        let valid_shape = destination
            .as_object()
            .is_some_and(|fields| fields.len() == 2);
        let cidr = destination["cidr"]
            .as_str()
            .and_then(|cidr| cidr.split_once('/'));
        let valid_cidr = cidr.is_some_and(|(address, prefix)| {
            let (Ok(address), Ok(bits)) = (address.parse::<IpAddr>(), prefix.parse::<u8>()) else {
                return false;
            };
            prefix.bytes().all(|byte| byte.is_ascii_digit())
                && bits <= if address.is_ipv4() { 32 } else { 128 }
        });
        let valid_port = destination["port"]
            .as_u64()
            .is_some_and(|port| (1..=u64::from(u16::MAX)).contains(&port));
        ensure!(
            valid_shape && valid_cidr && valid_port,
            "registryEgress entry {index} in wamn-org-inputs must contain a valid CIDR and TCP port"
        );
    }
    Ok(registry_egress)
}

pub(crate) fn internal_registry(registry: &str) -> String {
    registry
        .strip_prefix("localhost:5001")
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .map_or_else(
            || registry.to_owned(),
            |path| format!("{POD_REGISTRY}{path}"),
        )
}

/// Read a required string from the platform inputs.
pub fn field<'a>(object: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    object[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("platform configuration has no {key}"))
}

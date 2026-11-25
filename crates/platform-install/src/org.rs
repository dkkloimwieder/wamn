//! Install an org and invite its first owner.

use std::net::IpAddr;

use anyhow::{Context as _, ensure};
use serde_json::{Value, json};
use wamn_registry::OrgId;

use crate::provision_org::{APPLY_SECRET, CONTROL_SECRET, OAUTH_ENCRYPTION_INPUT};
use crate::release_chart::POD_REGISTRY;
use crate::{org_install::OrgInstaller, platform_registry::docker_configuration};
use wamn_postgres::database::ConnectionConfig;

pub use crate::platform_registry::registry_configuration;

const LOCAL_PROVIDER: &str = "kind";

/// Public data used to prepare an org release.
#[derive(Debug)]
pub struct OrgInputs {
    pub values: Value,
    pub database: ConnectionConfig,
    pub identity_ca: Value,
    pub nats_ca: Vec<u8>,
    pub postgres_ca: Vec<u8>,
    pub api_server: Vec<Value>,
}

/// Install the org release, provision its logins, then invite its owner.
pub async fn create(org: &OrgId, owner: &str, installation: &OrgInstaller) -> anyhow::Result<()> {
    let OrgInputs {
        values: inputs,
        database,
        identity_ca,
        nats_ca,
        postgres_ca,
        api_server,
    } = installation.inputs().await?;
    ensure!(!nats_ca.is_empty(), "public NATS CA is absent");
    let inputs = &inputs["data"];
    let image = field(inputs, "controlImage")?;
    let (_, digest) = image.rsplit_once('@').context(
        "pin systemSql.image in the platform release so org control and its worker use an immutable image",
    )?;
    digest.parse::<wamn_digest::Digest>()?;
    let values = org_values(
        inputs,
        &database,
        org,
        field(&identity_ca["data"], "ca.crt")?,
        std::str::from_utf8(&nats_ca).context("read the public NATS CA as PEM")?,
        std::str::from_utf8(&postgres_ca).context("read the public system database CA as PEM")?,
        &api_server,
    )?;
    let credentials = if registry_token_metadata(inputs)? {
        None
    } else {
        Some(docker_configuration(
            field(inputs, "registry")?,
            field(&values, "registry")?,
        )?)
    };
    installation.install_release(org, &values).await?;
    if let Some(credentials) = credentials {
        installation
            .write_registry_credentials(org, &credentials)
            .await?;
    }
    let job_uid = installation.provision_logins(org, image, &database).await?;
    installation.reload_control(org, &job_uid).await?;
    installation.invite_owner(org, image, owner).await
}

fn registry_token_metadata(inputs: &Value) -> anyhow::Result<bool> {
    Ok(field(inputs, "provider")? != LOCAL_PROVIDER)
}

/// The org chart values from the platform's public org inputs.
pub fn org_values(
    inputs: &Value,
    database: &ConnectionConfig,
    org: &OrgId,
    identity_ca: &str,
    nats_ca: &str,
    postgres_ca: &str,
    api_server: &[Value],
) -> anyhow::Result<Value> {
    let image = field(inputs, "controlImage")?;
    let application_chart = field(inputs, "applicationChart")?;
    let (_, digest) = application_chart
        .strip_prefix("oci://")
        .and_then(|reference| reference.rsplit_once('@'))
        .filter(|(repository, _)| !repository.is_empty())
        .context("pin applicationChart in the platform release to an OCI chart digest")?;
    digest
        .parse::<wamn_digest::Digest>()
        .context("invalid applicationChart digest in the platform release")?;
    let registry = field(inputs, "registry")?;
    let registry_egress: Value = serde_json::from_str(field(inputs, "registryEgress")?)
        .context("decode registryEgress from wamn-org-inputs")?;
    let registry_egress = registry_egress
        .as_array()
        .filter(|destinations| !destinations.is_empty())
        .context("registryEgress in wamn-org-inputs must be a nonempty list")?;
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
    let oauth_encryption: Value = serde_json::from_str(field(inputs, OAUTH_ENCRYPTION_INPUT)?)
        .context("decode the platform OAuth encryption configuration")?;
    let internal_registry = registry
        .strip_prefix("localhost:5001")
        .filter(|rest| rest.is_empty() || rest.starts_with('/'))
        .map_or_else(
            || registry.to_owned(),
            |path| format!("{POD_REGISTRY}{path}"),
        );
    let connection = |host: &str, username: &str, secret: &str| {
        json!({
            "host": host, "port": database.port, "database": database.database,
            "username": username, "credentials": {"namespace": org, "name": secret},
            "server_ca": {"namespace": org, "name": "wamn-pg-ca"},
        })
    };
    let control = crate::provision_org::control_login(org, &database.database);
    let apply = crate::provision_org::apply_login(org, &database.database);
    Ok(json!({
        "org": org, "base": field(inputs, "base")?, "registry": internal_registry, "identityCa": identity_ca, "natsCa": nats_ca, "postgresCa": postgres_ca,
        "postgres": {"image": field(inputs, "postgresImage")?},
        "networkPolicy": {"apiServer": api_server, "registry": registry_egress},
        "control": {
            "image": image,
            "configuration": {
                "org": org,
                "database": connection(field(inputs, "controlDatabaseHost")?, control.as_str(), CONTROL_SECRET),
                "apply_database": connection(field(inputs, "applyDatabaseHost")?, apply.as_str(), APPLY_SECRET),
                "identity_issuer": field(inputs, "identityIssuer")?,
                "nats_url": field(inputs, "natsUrl")?,
                "registry": internal_registry,
                "worker_image": image,
                "chart": application_chart,
                "registry_token_metadata": registry_token_metadata(inputs)?,
                "events": {"stream_replicas": 1, "dup_window_secs": 120},
                "oauth_encryption": oauth_encryption,
            },
        },
    }))
}

/// Read a required string from the platform inputs.
pub fn field<'a>(object: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    object[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("platform configuration has no {key}"))
}

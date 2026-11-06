//! System bootstrap requests and redacted Kubernetes credential decoding.

use anyhow::Context as _;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;

use super::document::ClusterDocument;
use super::job::{JobRequest, JobSecret};

pub(super) const DATABASE_HOST: &str = "wamn-pg-rw.platform.svc.cluster.local";

/// Encoded Secret data. Debug output omits every value.
#[derive(Deserialize)]
pub struct SecretData {
    #[serde(default)]
    data: std::collections::BTreeMap<String, String>,
}

impl std::fmt::Debug for SecretData {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretData")
            .field("data", &"[REDACTED]")
            .finish()
    }
}

pub(super) fn secret_text(secret: &SecretData, key: &str) -> anyhow::Result<String> {
    let encoded = secret
        .data
        .get(key)
        .with_context(|| format!("the Secret has no {key}"))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| anyhow::anyhow!("the Secret key {key} is not base64"))?;
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("the Secret key {key} is not UTF-8"))
}

/// Construct the system administrator credential from the CNPG Secret.
pub fn administrator(secret: &SecretData) -> anyhow::Result<JobSecret> {
    let mut url = url::Url::parse(&format!(
        "postgresql://postgres@{DATABASE_HOST}/wamn_system"
    ))?;
    url.set_password(Some(&secret_text(secret, "password")?.replace('%', "%25")))
        .map_err(|()| anyhow::anyhow!("encode the system administrator password"))?;
    Ok(JobSecret {
        name: "wamn-system-admin".into(),
        key: "url".into(),
        value: url.into(),
        environment: "WAMN_SYSTEM_ADMIN_URL".into(),
    })
}

/// Construct the system, initial organization, and issuer bootstrap requests.
pub fn provisioning_jobs(document: &ClusterDocument, image: &str) -> [JobRequest; 3] {
    let domain = document
        .route_hosts
        .control
        .split_once('.')
        .map_or(document.route_hosts.control.as_str(), |(_, domain)| domain);
    [
        (
            "platform-system",
            vec![
                "provision-system",
                "--platform-domain",
                domain,
                "--emit-apply-secret",
                "/out/secret.json",
                "--db-host",
                DATABASE_HOST,
            ],
            true,
        ),
        (
            "platform-org",
            vec![
                "provision-org",
                "--org",
                &document.org,
                "--template",
                "trials",
                "--pool",
                "wamn-pg",
                "--admin",
                &document.owner_email,
            ],
            false,
        ),
        (
            "platform-identity-issuer",
            vec![
                "provision-identity-issuer",
                "--issuer",
                "https://identity.identity.svc.cluster.local",
                "--prepare-generation",
                "a",
                "--namespace",
                "identity",
                "--emit-secret",
                "/out/secret.json",
                "--db-host",
                DATABASE_HOST,
            ],
            true,
        ),
    ]
    .map(|(name, arguments, emits_secret)| JobRequest {
        name: name.into(),
        image: image.into(),
        arguments: arguments.into_iter().map(str::to_owned).collect(),
        calls_kubernetes: false,
        emits_secret,
        once: true,
        ..JobRequest::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn administrator_encodes_the_password() {
        let secret = SecretData {
            data: [("password".into(), STANDARD.encode("p@ss:/?#%"))].into(),
        };
        let admin = administrator(&secret).unwrap();
        assert_eq!(
            admin.value,
            format!("postgresql://postgres:p%40ss%3A%2F%3F%23%25@{DATABASE_HOST}/wamn_system")
        );
        assert!(!format!("{admin:?}").contains("p%40ss"));
    }

    #[test]
    fn secret_decode_errors_do_not_expose_values() {
        for value in ["private!".to_owned(), STANDARD.encode([0xff])] {
            let secret = SecretData {
                data: [("password".into(), value.clone())].into(),
            };
            let error = secret_text(&secret, "password").unwrap_err();
            assert!(!format!("{error:#}").contains(&value));
        }
    }
}

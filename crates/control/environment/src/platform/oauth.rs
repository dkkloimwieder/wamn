//! OAuth envelope configuration for deployed and local hosts.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use base64::Engine as _;
use ring::rand::SecureRandom as _;
use serde_json::{Value, json};

use super::document::{ClusterDocument, Provider};

const KEY_PATH: &str = "/etc/wamn/oauth/key";
const SECRET: &str = "wamn-oauth";

pub(super) fn configuration(document: &ClusterDocument) -> Value {
    if document.provider == Provider::Gcp {
        json!({"provider":"gcp", "project":document.project_id,
            "region":document.zone.rsplit_once('-').expect("validated zone").0})
    } else {
        json!({"provider":"local"})
    }
}

/// Add the envelope backend and, locally, the shared private key mount.
pub(crate) fn host_group(group: &mut Value, configuration: &Value) -> anyhow::Result<()> {
    let env = group
        .as_object_mut()
        .context("host group object")?
        .entry("env")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("host environment list")?;
    env.retain(|value| {
        !matches!(
            value["name"].as_str(),
            Some("WAMN_OAUTH_ENCRYPTION" | "WAMN_OAUTH_KEY_FILE")
        )
    });
    env.push(json!({"name":"WAMN_OAUTH_ENCRYPTION", "value":configuration.to_string()}));
    if configuration["provider"] == "local" {
        env.push(json!({"name":"WAMN_OAUTH_KEY_FILE", "value":KEY_PATH}));
    }
    for field in ["volumes", "volumeMounts"] {
        if let Some(entries) = group.get_mut(field).and_then(Value::as_array_mut) {
            entries.retain(|value| value["name"] != "oauth-key");
        }
    }
    if configuration["provider"] == "local" {
        for (field, value) in [
            (
                "volumes",
                json!({"name":"oauth-key", "secret":{"secretName":SECRET,"defaultMode":384}}),
            ),
            (
                "volumeMounts",
                json!({"name":"oauth-key", "mountPath":"/etc/wamn/oauth","readOnly":true}),
            ),
        ] {
            let entries = group
                .as_object_mut()
                .expect("host group object")
                .entry(field)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .context("host volume list")?;
            entries.retain(|value| value["name"] != "oauth-key");
            entries.push(value);
        }
    }
    Ok(())
}

/// A new random key for a local OAuth provider.
pub(super) fn new_key() -> anyhow::Result<[u8; 32]> {
    let mut key = [0_u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| anyhow::anyhow!("OAuth key entropy unavailable"))?;
    Ok(key)
}

/// Create a target's key once, without changing an existing key.
pub fn local_key(path: &Path) -> anyhow::Result<()> {
    let mut key = [0_u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| anyhow::anyhow!("OAuth key entropy unavailable"))?;
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => file.write_all(&key).context("write local OAuth key"),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error).context("create local OAuth key"),
    }
}

pub fn dev_key(artifacts: &Path) -> PathBuf {
    artifacts
        .parent()
        .expect("local artifacts parent")
        .join("oauth.key")
}

pub(super) async fn ensure_key(
    target: &wamn_release_control::release_chart::Target,
    document: &ClusterDocument,
) -> anyhow::Result<bool> {
    if document.provider == Provider::Gcp {
        return Ok(false);
    }
    let existing: Option<Value> = crate::kubectl::get(
        &target.kubeconfig,
        &target.context,
        "hosts",
        "secret",
        SECRET,
    )
    .await?;
    if let Some(existing) = existing {
        validate_key(&existing)?;
        return Ok(false);
    }
    let key = new_key()?;
    // A concurrent create keeps the winner's key.
    for object in key_objects(&key, "hosts") {
        if let Err(error) = crate::kubectl::kubectl(
            &target.kubeconfig,
            &target.context,
            "hosts",
            &["create", "-f", "-"],
            Some(&serde_json::to_vec(&object)?),
        )
        .await
        {
            let winner: Option<Value> = crate::kubectl::get(
                &target.kubeconfig,
                &target.context,
                "hosts",
                "secret",
                SECRET,
            )
            .await?;
            if let Some(winner) = winner {
                validate_key(&winner)?;
                return Ok(false);
            }
            return Err(error).context("create the OAuth key Secret");
        }
    }

    Ok(true)
}

fn validate_key(secret: &Value) -> anyhow::Result<()> {
    let key = secret["data"]["key"]
        .as_str()
        .and_then(|key| base64::engine::general_purpose::STANDARD.decode(key).ok());
    anyhow::ensure!(
        key.is_some_and(|key| key.len() == 32),
        "hosts/wamn-oauth must contain a 32-byte key"
    );
    Ok(())
}

/// The Secret that the hosts of a local OAuth provider mount (host_group).
pub(super) fn key_objects(key: &[u8; 32], namespace: &str) -> Vec<Value> {
    vec![
        json!({"apiVersion":"v1", "kind":"Secret", "metadata":{"name":SECRET,"namespace":namespace},
        "data":{"key":base64::engine::general_purpose::STANDARD.encode(key)}}),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn secret_key_validation_never_exposes_key_material() {
        let key = new_key().unwrap();
        assert!(validate_key(&key_objects(&key, "hosts")[0]).is_ok());
        for private in [
            "private-invalid-base64".to_owned(),
            base64::engine::general_purpose::STANDARD.encode(b"private-short-key"),
        ] {
            let secret = json!({"data":{"key":private}});
            let error = validate_key(&secret).unwrap_err();
            assert!(!format!("{error:#}").contains(&private));
            assert!(!format!("{error:?}").contains("private-short-key"));
        }
    }

    #[test]
    fn local_key_is_private_and_survives_a_second_host() {
        let directory =
            std::env::temp_dir().join(format!("wamn-oauth-key-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("oauth.key");
        local_key(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        local_key(&path).unwrap();
        assert_eq!(original.len(), 32);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}

//! OAuth envelope configuration for deployed and local hosts.

use anyhow::Context as _;
use serde_json::{Value, json};

const KEY_PATH: &str = "/etc/wamn/oauth/key";
const SECRET: &str = "wamn-oauth";

/// Add the envelope backend and, locally, the shared private key mount.
pub fn host_group(group: &mut Value, configuration: &Value) -> anyhow::Result<()> {
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

//! The edge configuration: one TOML file, with one environment override for
//! each key.
//!
//! `wamn-edge --config <path>` or `WAMN_EDGE_CONFIG` names the file. Each key
//! has one `WAMN_EDGE_*` variable ([`OVERRIDES`]), which replaces the file's
//! value or supplies a key that the file leaves out. With no file, every key
//! comes from its variable. An unknown key is refused.
//! `services/edge/edge.example.toml` shows every key.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::Deserialize;

/// The variable that names the configuration file.
pub const CONFIG_VARIABLE: &str = "WAMN_EDGE_CONFIG";

/// Whether an override is text or an integer in the file.
#[derive(Debug, Clone, Copy)]
enum Shape {
    Text,
    Integer,
}

/// Each key's override variable, its path in the file, and its shape.
const OVERRIDES: &[(&str, &[&str], Shape)] = &[
    ("WAMN_EDGE_BUNDLE_DIR", &["release", "dir"], Shape::Text),
    (
        "WAMN_EDGE_BUNDLE_DIGEST",
        &["release", "digest"],
        Shape::Text,
    ),
    ("WAMN_EDGE_SESSION_KEYS", &["session", "keys"], Shape::Text),
    (
        "WAMN_EDGE_SESSION_ISSUER",
        &["session", "issuer"],
        Shape::Text,
    ),
    ("WAMN_EDGE_SESSION_ORG", &["session", "org"], Shape::Text),
    (
        "WAMN_EDGE_SESSION_AUDIENCE",
        &["session", "audience"],
        Shape::Text,
    ),
    ("WAMN_EDGE_DB", &["store", "db"], Shape::Text),
    ("WAMN_EDGE_ROUTE_HOST", &["http", "route_host"], Shape::Text),
    ("WAMN_EDGE_LISTEN", &["http", "listen"], Shape::Text),
    (
        "WAMN_EDGE_DEVICE_ATTACHMENT",
        &["device", "attachment"],
        Shape::Text,
    ),
    (
        "WAMN_EDGE_DEVICE_PRINCIPAL",
        &["device", "principal"],
        Shape::Text,
    ),
    ("WAMN_EDGE_DEVICE_ROLE", &["device", "role"], Shape::Text),
    (
        "WAMN_EDGE_DEVICE_SERIAL_PATH",
        &["device", "serial", "path"],
        Shape::Text,
    ),
    (
        "WAMN_EDGE_DEVICE_SERIAL_BAUD",
        &["device", "serial", "baud"],
        Shape::Integer,
    ),
    (
        "WAMN_EDGE_DEVICE_SERIAL_MAX_FRAME",
        &["device", "serial", "max_frame"],
        Shape::Integer,
    ),
    ("WAMN_EDGE_FORWARD_URL", &["forward", "url"], Shape::Text),
    (
        "WAMN_EDGE_FORWARD_TOKEN_FILE",
        &["forward", "token_file"],
        Shape::Text,
    ),
    (
        "WAMN_EDGE_FORWARD_CA_FILE",
        &["forward", "ca_file"],
        Shape::Text,
    ),
    (
        "WAMN_EDGE_FORWARD_KEY_FIELD",
        &["forward", "key_field"],
        Shape::Text,
    ),
];

/// Everything the box needs to serve.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EdgeConfig {
    pub release: ReleaseConfig,
    pub session: SessionConfig,
    pub store: StoreConfig,
    pub http: HttpConfig,
    /// The device loop. Without it, the box serves routes only.
    #[serde(default)]
    pub device: Option<DeviceConfig>,
    /// The forward of samples to the platform. Without it, samples stay in
    /// the store.
    #[serde(default)]
    pub forward: Option<ForwardConfig>,
}

/// The release bundle that the box serves.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseConfig {
    /// The release bundle directory.
    pub dir: PathBuf,
    /// The pinned digest of `edge-release.json`.
    pub digest: String,
}

/// The sessions that local routes accept.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionConfig {
    /// The session key file that the release installs.
    pub keys: PathBuf,
    /// The session issuer that signs the keys.
    pub issuer: String,
    /// The organization a session must name. It is also the tenant of every
    /// intent.
    pub org: String,
    /// The project-environment identity a session must name.
    pub audience: String,
}

/// The run-state file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    /// The SQLite file of intents and samples.
    pub db: PathBuf,
}

/// The local ingress.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    /// The route host that the ingress answers.
    pub route_host: String,
    /// The address the ingress listens on.
    pub listen: SocketAddr,
}

/// The device loop: one attachment called for each frame of one device.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceConfig {
    /// The attachment whose operation the loop calls. Its route must change
    /// records and must name no idempotency key.
    pub attachment: String,
    /// The fixed local principal of every device call.
    pub principal: String,
    /// The role in `grants.json` whose permissions the principal holds.
    pub role: String,
    pub serial: SerialConfig,
}

/// A serial device that sends one frame per line.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SerialConfig {
    /// The tty of the device.
    pub path: PathBuf,
    pub baud: u32,
    /// The most bytes in one frame before its newline. The loop drops a
    /// longer frame.
    pub max_frame: usize,
}

/// The platform route that receives each sample.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForwardConfig {
    /// The `https` URL of the platform route.
    pub url: String,
    /// The file of the device PAT. Only its owner may read it.
    pub token_file: PathBuf,
    /// PEM certificates that the forward trusts beside the system roots.
    #[serde(default)]
    pub ca_file: Option<PathBuf>,
    /// The item field of the platform route's idempotency key, such as
    /// `value.idempotency_key`. The forward writes the sample key into it.
    #[serde(default)]
    pub key_field: Option<String>,
}

impl EdgeConfig {
    /// Read the file at `path`, when there is one, and apply the overrides
    /// from the environment.
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let text = match path {
            Some(path) => std::fs::read_to_string(path)
                .with_context(|| format!("read the configuration {}", path.display()))?,
            None => String::new(),
        };
        Self::parse(&text, |name| std::env::var(name).ok())
    }

    /// Parse `text` and apply the override that `variable` returns for each
    /// key.
    pub fn parse(text: &str, variable: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let mut table: toml::Table = text.parse().context("parse the configuration")?;
        for (name, path, shape) in OVERRIDES {
            let Some(value) = variable(name) else {
                continue;
            };
            let value = match shape {
                Shape::Text => toml::Value::String(value),
                Shape::Integer => toml::Value::Integer(
                    value
                        .parse()
                        .with_context(|| format!("{name} is not an integer"))?,
                ),
            };
            let (key, sections) = path.split_last().expect("an override path names a key");
            let mut section = &mut table;
            for name in sections {
                section = section
                    .entry(*name)
                    .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                    .as_table_mut()
                    .with_context(|| format!("{name} is not a section"))?;
            }
            section.insert((*key).to_owned(), value);
        }
        table.try_into().context("read the configuration")
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::EdgeConfig;

    const EXAMPLE: &str = include_str!("../edge.example.toml");

    fn parse(text: &str, variables: &[(&str, &str)]) -> anyhow::Result<EdgeConfig> {
        let variables: HashMap<&str, &str> = variables.iter().copied().collect();
        EdgeConfig::parse(text, |name| {
            variables.get(name).map(|value| (*value).to_owned())
        })
    }

    #[test]
    fn the_example_file_is_a_configuration_with_a_serial_device() {
        let config = parse(EXAMPLE, &[]).expect("the example parses");
        let device = config.device.expect("the example has a device");
        assert_eq!(device.serial.baud, 9600);
        let forward = config.forward.expect("the example has a forward");
        assert_eq!(forward.key_field.as_deref(), Some("value.idempotency_key"));
        assert_eq!(config.http.listen.port(), 8080);
    }

    #[test]
    fn a_variable_replaces_a_key_or_supplies_a_missing_one() {
        let config = parse(
            EXAMPLE,
            &[
                ("WAMN_EDGE_DB", "/tmp/other.db"),
                ("WAMN_EDGE_DEVICE_SERIAL_MAX_FRAME", "64"),
            ],
        )
        .expect("the overrides apply");
        assert_eq!(config.store.db.to_str(), Some("/tmp/other.db"));
        assert_eq!(config.device.expect("a device").serial.max_frame, 64);

        let from_variables = parse(
            "",
            &[
                ("WAMN_EDGE_BUNDLE_DIR", "/bundle"),
                ("WAMN_EDGE_BUNDLE_DIGEST", "sha256:00"),
                ("WAMN_EDGE_SESSION_KEYS", "/keys.json"),
                ("WAMN_EDGE_SESSION_ISSUER", "https://issuer"),
                ("WAMN_EDGE_SESSION_ORG", "org-a"),
                ("WAMN_EDGE_SESSION_AUDIENCE", "tenant-a/edge"),
                ("WAMN_EDGE_DB", "/edge.db"),
                ("WAMN_EDGE_ROUTE_HOST", "edge.localhost"),
                ("WAMN_EDGE_LISTEN", "127.0.0.1:0"),
            ],
        )
        .expect("the variables alone are a configuration");
        assert!(from_variables.device.is_none(), "no device, no loop");
    }

    #[test]
    fn an_unknown_key_or_a_bad_integer_is_refused() {
        let unknown = EXAMPLE.replace("[store]", "[store]\nretention = 3");
        assert!(parse(&unknown, &[]).is_err());
        assert!(parse(EXAMPLE, &[("WAMN_EDGE_DEVICE_SERIAL_BAUD", "fast")]).is_err());
    }
}

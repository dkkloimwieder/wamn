use super::{BTreeMap, Deserialize, EnvApps, Serialize, Triple, ensure, fmt};
use anyhow::Context as _;
use wamn_registry::configuration::EnvSettings;

/// One environment document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvDocument {
    #[serde(flatten)]
    pub settings: EnvSettings,
    /// Time to wait for the application workload to become ready.
    #[serde(default = "default_readiness_budget_seconds")]
    pub readiness_budget_seconds: i32,
    /// Time for the host to drain before shutdown.
    #[serde(default = "default_drain_bound_seconds")]
    pub drain_bound_seconds: i32,
    /// Lifetime of cached positive project facts in seconds. Zero disables the cache.
    pub revocation_seconds: u32,
    /// Application name to its selection and membership.
    #[serde(deserialize_with = "read_apps")]
    pub apps: BTreeMap<wamn_registry::AppName, AppSelection>,
}

pub use wamn_registry::configuration::{
    Client, ConnectionReference, Integration, Schema, SchemaMode,
};

/// An application's deployment selection and declared membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AppSelection {
    /// Desired release, read from the registry or supplied by the deploy request.
    #[serde(skip)]
    pub current: DeclaredRelease,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    /// Transient membership from the verified release manifest.
    #[serde(skip)]
    pub packages: std::collections::BTreeSet<wamn_package::PackageName>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, ConnectionReference>,
}

impl From<AppSelection> for wamn_registry::AppDefinition {
    fn from(app: AppSelection) -> Self {
        Self {
            packages: app.packages,
            connections: app
                .connections
                .into_iter()
                .map(|(alias, connection)| (alias, connection.to_string()))
                .collect(),
        }
    }
}

fn read_apps<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<wamn_registry::AppName, AppSelection>, D::Error> {
    let values = BTreeMap::<wamn_registry::AppName, serde_json::Value>::deserialize(deserializer)?;
    values
        .into_iter()
        .map(|(name, value)| {
            let selection = serde_json::from_value(value).map_err(|error| {
                serde::de::Error::custom(format!("application {name}: {error}"))
            })?;
            Ok((name, selection))
        })
        .collect()
}

/// A current release: a full digest or `none`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeclaredRelease {
    /// Installed but not serving: `apply` uninstalls the release chart.
    #[default]
    None,
    /// `sha256:` and 64 lowercase hex characters.
    Digest(wamn_digest::Digest),
}

impl DeclaredRelease {
    pub(super) fn parse(text: &str) -> anyhow::Result<Self> {
        if text == "none" {
            return Ok(Self::None);
        }
        Ok(Self::Digest(text.parse()?))
    }
}

impl fmt::Display for DeclaredRelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("none"),
            Self::Digest(digest) => digest.fmt(formatter),
        }
    }
}

impl Serialize for DeclaredRelease {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for DeclaredRelease {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl EnvDocument {
    /// Resolve an omitted mode from the stored designation, never from a name or version.
    /// An absent environment record preserves data.
    pub fn schema_mode(&self, disposable: Option<bool>) -> SchemaMode {
        self.settings
            .schema
            .mode
            .unwrap_or(if disposable == Some(true) {
                SchemaMode::Reset
            } else {
                SchemaMode::Migrate
            })
    }

    /// Read a document from its JSON form and require_valid every field.
    ///
    /// # Errors
    ///
    /// When a field is unknown or missing, or a value is refused.
    pub fn from_json(bytes: &[u8]) -> anyhow::Result<Self> {
        let document: Self =
            serde_json::from_slice(bytes).context("decode the environment JSON")?;
        document.require_valid()?;
        Ok(document)
    }

    pub(super) fn require_valid(&self) -> anyhow::Result<()> {
        wamn_platform_install::env::validate_project_env(
            &self.settings.org,
            &self.settings.project,
            &self.settings.env,
        )
        .context("the environment coordinate is not valid")?;
        require_settings(
            &self.settings.target,
            self.readiness_budget_seconds,
            self.drain_bound_seconds,
            self.revocation_seconds,
            &self.settings.integrations,
        )?;
        require_apps(&self.apps, &self.settings.integrations)
    }

    /// Runtime membership excludes deployment selections.
    pub fn membership(&self) -> EnvApps {
        self.apps
            .iter()
            .map(|(name, app)| {
                (
                    name.clone(),
                    wamn_registry::AppDefinition {
                        packages: app.packages.clone(),
                        connections: app
                            .connections
                            .iter()
                            .map(|(alias, connection)| (alias.clone(), connection.to_string()))
                            .collect(),
                    },
                )
            })
            .collect()
    }

    /// The coordinate.
    pub fn triple(&self) -> Triple {
        Triple::new(
            self.settings.org.clone(),
            self.settings.env.clone(),
            self.settings.project.clone(),
        )
    }
}

pub(super) fn default_readiness_budget_seconds() -> i32 {
    600
}

pub(super) fn default_drain_bound_seconds() -> i32 {
    300
}

/// Validate fields shared across the environment.
pub(super) fn require_settings(
    target: &str,
    readiness_budget_seconds: i32,
    drain_bound_seconds: i32,
    revocation_seconds: u32,
    integrations: &BTreeMap<String, Integration>,
) -> anyhow::Result<()> {
    ensure!(!target.trim().is_empty(), "target is empty");
    ensure!(
        readiness_budget_seconds > 0,
        "readiness_budget_seconds must be positive"
    );
    ensure!(
        drain_bound_seconds > 0,
        "drain_bound_seconds must be positive"
    );
    ensure!(
        revocation_seconds <= 3600,
        "revocation_seconds must be between 0 and 3600"
    );
    for (name, integration) in integrations {
        ensure!(
            wamn_registry::configuration::valid_connection_name(name) && integration.version > 0,
            "integration {name} requires a valid connection name and positive version"
        );
    }
    Ok(())
}

pub(super) fn require_apps(
    apps: &BTreeMap<wamn_registry::AppName, AppSelection>,
    integrations: &BTreeMap<String, Integration>,
) -> anyhow::Result<()> {
    for (name, app) in apps {
        for (alias, connection) in &app.connections {
            ensure!(
                !alias.is_empty(),
                "application {name} has an empty connection alias"
            );
            ensure!(
                integrations.contains_key(connection.as_str()),
                "application {name} connection {alias} has no environment pin for {connection}"
            );
        }
    }
    Ok(())
}

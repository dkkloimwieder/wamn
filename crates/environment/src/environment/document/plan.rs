use super::{BTreeMap, Deserialize, EnvironmentApplications, Serialize, Triple, ensure, fmt};
use anyhow::Context as _;

/// One environment document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentDocument {
    pub org: wamn_registry::OrgId,
    pub project: wamn_registry::ProjectId,
    pub env: wamn_registry::EnvName,
    /// The immutable deployment target name.
    pub target: String,
    /// A policy of `registry.env_policies`, by name.
    pub policy: wamn_registry::EnvName,
    /// Grant cache lifetime from deploy input; absent in applied registry settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_seconds: Option<u32>,
    #[serde(default)]
    pub schema: Schema,
    /// Application name to its selection and membership.
    #[serde(deserialize_with = "read_applications")]
    pub applications: BTreeMap<wamn_catalog::ApplicationName, ApplicationSelection>,
    #[serde(default)]
    pub integrations: BTreeMap<String, Integration>,
}

pub use wamn_catalog::configuration::{
    Client, ConnectionReference, Integration, Schema, SchemaMode,
};

/// An application's deployment selection and declared membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ApplicationSelection {
    /// Desired release, read from the registry or supplied by the deploy request.
    #[serde(skip)]
    pub current: DeclaredRelease,
    #[schemars(
        length(min = 1, max = 253),
        regex(pattern = "^[a-z0-9]([a-z0-9.-]*[a-z0-9])?$")
    )]
    pub hostname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    /// Transient membership from the verified release manifest.
    #[serde(skip)]
    pub packages: std::collections::BTreeSet<wamn_catalog::PackageName>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, ConnectionReference>,
}

impl From<ApplicationSelection> for wamn_catalog::ApplicationDefinition {
    fn from(application: ApplicationSelection) -> Self {
        Self {
            packages: application.packages,
            connections: application
                .connections
                .into_iter()
                .map(|(alias, connection)| (alias, connection.to_string()))
                .collect(),
        }
    }
}

fn read_applications<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<wamn_catalog::ApplicationName, ApplicationSelection>, D::Error> {
    let values =
        BTreeMap::<wamn_catalog::ApplicationName, serde_json::Value>::deserialize(deserializer)?;
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

impl EnvironmentDocument {
    /// Resolve an omitted mode from the stored designation, never from a name or version.
    /// An absent environment record preserves data.
    pub fn schema_mode(&self, disposable: Option<bool>) -> SchemaMode {
        self.schema.mode.unwrap_or(if disposable == Some(true) {
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
        let document: Self = serde_json::from_slice(bytes).context("decode the environment JSON")?;
        ensure!(
            document.revocation_seconds.is_some(),
            "revocation_seconds is required in environment input"
        );
        document.require_valid()?;
        Ok(document)
    }

    pub(super) fn require_valid(&self) -> anyhow::Result<()> {
        wamn_provision::validate_project_env(&self.org, &self.project, &self.env)
            .context("the environment coordinate is not valid")?;
        require_settings(&self.target, self.revocation_seconds, &self.integrations)?;
        require_applications(&self.applications, &self.integrations)
    }

    /// Find the sole application that owns a package.
    pub fn application_for_package(
        &self,
        package: &wamn_catalog::PackageName,
    ) -> Option<&wamn_catalog::ApplicationName> {
        let mut owners = self
            .applications
            .iter()
            .filter(|(_, application)| application.packages.contains(package))
            .map(|(name, _)| name);
        let owner = owners.next()?;
        owners.next().is_none().then_some(owner)
    }

    /// Runtime membership excludes deployment selections.
    pub fn membership(&self) -> EnvironmentApplications {
        self.applications
            .iter()
            .map(|(name, application)| {
                (
                    name.clone(),
                    wamn_catalog::ApplicationDefinition {
                        packages: application.packages.clone(),
                        connections: application
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
        Triple::new(self.org.clone(), self.env.clone(), self.project.clone())
    }
}

/// Validate fields shared across the environment.
pub(super) fn require_settings(
    target: &str,
    revocation_seconds: Option<u32>,
    integrations: &BTreeMap<String, Integration>,
) -> anyhow::Result<()> {
    ensure!(!target.trim().is_empty(), "target is empty");
    ensure!(
        revocation_seconds.is_none_or(|seconds| seconds <= 3600),
        "revocation_seconds must be between 0 and 3600"
    );
    for (name, integration) in integrations {
        ensure!(
            wamn_catalog::configuration::valid_connection_name(name) && integration.version > 0,
            "integration {name} requires a valid connection name and positive version"
        );
    }
    Ok(())
}

pub(super) fn require_applications(
    applications: &BTreeMap<wamn_catalog::ApplicationName, ApplicationSelection>,
    integrations: &BTreeMap<String, Integration>,
) -> anyhow::Result<()> {
    for (name, application) in applications {
        let hostname = &application.hostname;
        ensure!(
            !hostname.is_empty()
                && hostname.len() <= 253
                && hostname.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'.'
                    || byte == b'-')
                && !hostname.starts_with(['.', '-'])
                && !hostname.ends_with(['.', '-']),
            "application {name}: hostname {hostname:?} is not a lowercase DNS name"
        );
        for (alias, connection) in &application.connections {
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

use super::{BTreeMap, Deserialize, EnvironmentApplications, Serialize, Triple, ensure, fmt};
use anyhow::Context as _;

/// One environment document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentDocument {
    pub org: String,
    pub project: String,
    pub env: String,
    /// The immutable deployment target name.
    pub target: String,
    /// A policy of `registry.env_policies`, by name.
    pub policy: String,
    /// Maximum project credential revocation delay, in seconds.
    pub revocation_seconds: u64,
    #[serde(default)]
    pub schema: Schema,
    /// Application name to its selection and membership.
    #[serde(deserialize_with = "read_applications")]
    pub applications: BTreeMap<String, ApplicationSelection>,
    #[serde(default)]
    pub integrations: BTreeMap<String, Integration>,
}

pub use wamn_registry::configuration::{Client, Integration, Schema, SchemaMode};

/// An application's deployment selection and declared membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationSelection {
    /// Desired release, read from the registry or supplied by the deploy request.
    #[serde(skip)]
    pub current: DeclaredRelease,
    pub hostname: String,
    /// Maximum application credential revocation delay, in seconds.
    pub revocation_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    /// Transient membership from the verified release manifest.
    #[serde(skip)]
    pub packages: std::collections::BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, String>,
}

impl From<ApplicationSelection> for wamn_catalog::ApplicationDefinition {
    fn from(application: ApplicationSelection) -> Self {
        Self {
            packages: application.packages,
            connections: application.connections,
        }
    }
}

fn read_applications<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, ApplicationSelection>, D::Error> {
    let values = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
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
    Digest(String),
}

impl DeclaredRelease {
    pub(super) fn parse(text: &str) -> anyhow::Result<Self> {
        if text == "none" {
            return Ok(Self::None);
        }
        let hex = text
            .strip_prefix("sha256:")
            .with_context(|| format!("release {text:?} is neither none nor a sha256: digest"))?;
        ensure!(
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "release {text:?} is not sha256: and 64 lowercase hex characters"
        );
        Ok(Self::Digest(text.to_owned()))
    }
}

impl fmt::Display for DeclaredRelease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("none"),
            Self::Digest(digest) => formatter.write_str(digest),
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
        let configuration: wamn_registry::configuration::EnvironmentConfiguration =
            serde_json::from_slice(bytes).context("decode the environment JSON")?;
        let document = Self::from_configuration(configuration);
        document.require_valid()?;
        Ok(document)
    }

    pub(super) fn require_valid(&self) -> anyhow::Result<()> {
        wamn_control_provision::validate_project_env(&self.org, &self.project, &self.env)
            .context("the environment coordinate is not valid")?;
        ensure!(!self.policy.is_empty(), "policy is empty");
        for (name, application) in &self.applications {
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
            if let Some(client) = &application.client {
                wamn_package_artifact::ensure_digest(&client.web)
                    .with_context(|| format!("application {name}: client.web"))?;
            }
            ensure!(!name.is_empty(), "an application has an empty name");
        }
        ensure!(!self.target.trim().is_empty(), "target is empty");
        for (application, definition) in &self.applications {
            for (alias, name) in &definition.connections {
                ensure!(
                    !alias.is_empty(),
                    "application {application} has an empty connection alias"
                );
                wamn_catalog::org_credential_handle(&self.org, name).map_err(anyhow::Error::msg)?;
            }
        }
        for (name, integration) in &self.integrations {
            ensure!(
                !name.is_empty() && integration.version > 0,
                "integration {name} requires a positive version"
            );
        }
        Ok(())
    }

    /// Find the sole application that owns a package.
    pub fn application_for_package(&self, package: &str) -> Option<&str> {
        let mut owners = self
            .applications
            .iter()
            .filter(|(_, application)| application.packages.contains(package))
            .map(|(name, _)| name.as_str());
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
                        connections: application.connections.clone(),
                    },
                )
            })
            .collect()
    }

    /// The release used by the existing environment-wide consumer.
    pub fn legacy_release(&self) -> &DeclaredRelease {
        // DEP2-03 replaces the single release consumer with application publication.
        self.applications
            .values()
            .next()
            .map_or(&DeclaredRelease::None, |app| &app.current)
    }

    /// The coordinate.
    pub fn triple(&self) -> Triple {
        Triple::new(self.org.as_str(), self.env.as_str(), self.project.as_str())
    }

    /// The canonical bytes: RFC 8785 JSON.
    ///
    /// # Errors
    ///
    /// Never for a valid document.
    pub fn canonical_json(&self) -> anyhow::Result<Vec<u8>> {
        let value = serde_json::to_value(self).context("encode the environment")?;
        Ok(wamn_digest::canonical_json_bytes(&value))
    }

    /// The document as an `environment.k` file that compiles to this document.
    pub fn to_kcl(&self) -> String {
        use std::fmt::Write as _;
        let quote = |text: &str| serde_json::to_string(text).expect("a string encodes");
        let mut lines = vec!["import environment".to_owned(), String::new()];
        lines.push("environment.Environment {".to_owned());
        for (key, value) in [
            ("org", self.org.as_str()),
            ("project", self.project.as_str()),
            ("env", self.env.as_str()),
            ("target", self.target.as_str()),
            ("policy", self.policy.as_str()),
        ] {
            lines.push(format!("    {key} = {}", quote(value)));
        }
        lines.push(format!(
            "    revocation_seconds = {}",
            self.revocation_seconds
        ));
        if let Some(mode) = self.schema.mode {
            let mode = serde_json::to_string(&mode).expect("a schema mode encodes");
            lines.push(format!("    schema = {{mode = {mode}}}"));
        }
        lines.push("    integrations = {".to_owned());
        for (name, integration) in &self.integrations {
            lines.push(format!(
                "        {} = {{version = {}}}",
                quote(name),
                integration.version
            ));
        }
        lines.push("    }".to_owned());
        lines.push("    applications = {".to_owned());
        for (name, application) in &self.applications {
            let bindings = application
                .connections
                .iter()
                .map(|(alias, name)| format!("{} = {}", quote(alias), quote(name)))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "        {} = {{\n            hostname = {}\n            revocation_seconds = {}\n            connections = {{{bindings}}}",
                quote(name),
                quote(&application.hostname),
                application.revocation_seconds
            ));
            if let Some(client) = &application.client {
                lines.push(format!(
                    "            client = {{package = {}, web = {}}}",
                    quote(&client.package),
                    quote(&client.web)
                ));
            }
            lines.push("        }".to_owned());
        }
        lines.push("    }".to_owned());
        lines.push("}".to_owned());
        let mut out = String::new();
        for line in lines {
            writeln!(out, "{line}").expect("writing to a String succeeds");
        }
        out
    }
}

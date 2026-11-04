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
    /// `none`, or a full `sha256:` digest.
    pub release: DeclaredRelease,
    pub route_host: String,
    /// The package and pinned web artifact this route host serves. Absent means no client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<Client>,
    /// A policy of `registry.env_policies`, by name.
    pub policy: String,
    /// Application name to the packages of the release that belong to it.
    pub applications: EnvironmentApplications,
    #[serde(default)]
    pub integrations: BTreeMap<String, Integration>,
}

/// The package id and immutable web artifact of an environment's client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    pub package: String,
    pub web: String,
}

/// The published version of a project integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    pub version: i32,
}

/// The `release` field: a full digest or `none`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredRelease {
    /// Installed but not serving: `apply` uninstalls the release chart.
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
    /// Read a document from its JSON form and check every field.
    ///
    /// # Errors
    ///
    /// When a field is unknown or missing, or a value is refused.
    pub fn from_json(bytes: &[u8]) -> anyhow::Result<Self> {
        let document: Self =
            serde_json::from_slice(bytes).context("decode the environment JSON")?;
        document.check()?;
        Ok(document)
    }

    fn check(&self) -> anyhow::Result<()> {
        wamn_control_provision::validate_project_env(&self.org, &self.project, &self.env)
            .context("the environment coordinate is not valid")?;
        ensure!(
            !self.route_host.is_empty()
                && self.route_host.len() <= 253
                && self.route_host.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || byte == b'.'
                        || byte == b'-'
                })
                && !self.route_host.starts_with(['.', '-'])
                && !self.route_host.ends_with(['.', '-']),
            "route_host {:?} is not a lowercase DNS name",
            self.route_host
        );
        ensure!(!self.policy.is_empty(), "policy is empty");
        if let Some(client) = &self.client {
            wamn_package_artifact::ensure_digest(&client.web).context("client.web")?;
        }
        for (name, application) in &self.applications {
            ensure!(!name.is_empty(), "an application has an empty name");
            ensure!(
                !application.packages.is_empty(),
                "application {name} has no packages"
            );
            ensure!(
                !application.packages.contains(""),
                "application {name} has an empty package id"
            );
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

    /// The coordinate.
    pub fn triple(&self) -> Triple {
        Triple::new(self.org.as_str(), self.project.as_str(), self.env.as_str())
    }

    /// The canonical bytes: RFC 8785 JSON.
    ///
    /// # Errors
    ///
    /// Never for a valid document.
    pub fn canonical_json(&self) -> anyhow::Result<Vec<u8>> {
        let value = serde_json::to_value(self).context("encode the environment")?;
        Ok(wamn_execution_contract::canonical_json_bytes(&value))
    }

    /// The document as an `environment.k` file that compiles to this document.
    pub fn to_kcl(&self) -> String {
        use std::fmt::Write as _;
        let quote = |text: &str| serde_json::to_string(text).expect("a string encodes");
        let mut lines = vec!["import environment".to_owned(), String::new()];
        lines.push("environment.Environment {".to_owned());
        for (key, value) in [
            ("org", self.org.clone()),
            ("project", self.project.clone()),
            ("env", self.env.clone()),
            ("target", self.target.clone()),
            ("release", self.release.to_string()),
            ("route_host", self.route_host.clone()),
            ("policy", self.policy.clone()),
        ] {
            lines.push(format!("    {key} = {}", quote(&value)));
        }
        if let Some(client) = &self.client {
            lines.push(format!(
                "    client = {{package = {}, web = {}}}",
                quote(&client.package),
                quote(&client.web)
            ));
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
            let packages: Vec<String> = application.packages.iter().map(|id| quote(id)).collect();
            let bindings = application
                .connections
                .iter()
                .map(|(alias, name)| format!("{} = {}", quote(alias), quote(name)))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "        {} = {{packages = [{}], connections = {{{bindings}}}}}",
                quote(name),
                packages.join(", ")
            ));
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

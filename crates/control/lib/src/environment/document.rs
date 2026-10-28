//! The environment document, `environment.k`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The document names the coordinate, the release digest or `none`, the route
//! host, the policy by name, the applications and their packages
//! (docs/architecture/execution.md#workflows), the target, connection names and
//! floors, and nothing else. It compiles with the `environment` KCL schema
//! module to JSON, which this module reads into [`EnvironmentDocument`]. Its
//! canonical form is RFC 8785 JSON, as `wamn.json` is.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_catalog::EnvironmentApplications;
use wamn_control_registry::Triple;

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
    /// The package whose web client this route host serves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_package: Option<String>,
    /// A policy of `registry.env_policies`, by name.
    pub policy: String,
    /// Application name to the packages of the release that belong to it.
    pub applications: EnvironmentApplications,
    /// Package id to the version to contract to (R13).
    #[serde(default)]
    pub floors: BTreeMap<String, String>,
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
    fn parse(text: &str) -> anyhow::Result<Self> {
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
    /// Compile an `environment.k` file and read it.
    ///
    /// # Errors
    ///
    /// When KCL refuses the file, or when the JSON is not a valid document.
    pub fn compile(file: &Path) -> anyhow::Result<Self> {
        let bytes = wamn_schema_generator::compile_environment(file)?;
        Self::from_json(&bytes)
            .with_context(|| format!("{} is not a valid environment", file.display()))
    }

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
            for (integration, pin) in &definition.connectors {
                ensure!(
                    !integration.is_empty(),
                    "application {application} has an empty connector code"
                );
                if pin != "host" {
                    wamn_package_artifact::ensure_digest(pin)?;
                }
            }
            for (alias, name) in &definition.connections {
                ensure!(
                    !alias.is_empty(),
                    "application {application} has an empty connection alias"
                );
                wamn_catalog::org_credential_handle(&self.org, name).map_err(anyhow::Error::msg)?;
            }
        }
        for (package, version) in &self.floors {
            ensure!(
                !package.is_empty() && !version.is_empty(),
                "a floor has an empty package id or version"
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
        if let Some(package) = &self.client_package {
            lines.push(format!("    client_package = {}", quote(package)));
        }
        lines.push("    applications = {".to_owned());
        for (name, application) in &self.applications {
            let packages: Vec<String> = application.packages.iter().map(|id| quote(id)).collect();
            let bindings = application
                .connections
                .iter()
                .map(|(alias, name)| format!("{} = {}", quote(alias), quote(name)))
                .collect::<Vec<_>>()
                .join(", ");
            let connectors = application
                .connectors
                .iter()
                .map(|(code, pin)| format!("{} = {}", quote(code), quote(pin)))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "        {} = {{packages = [{}], connections = {{{bindings}}}, connectors = {{{connectors}}}}}",
                quote(name),
                packages.join(", ")
            ));
        }
        lines.push("    }".to_owned());
        lines.push("    floors = {".to_owned());
        for (package, version) in &self.floors {
            lines.push(format!("        {} = {}", quote(package), quote(version)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn document() -> Value {
        json!({
            "org": "acme",
            "project": "wms",
            "env": "prod",
            "release": format!("sha256:{}", "a".repeat(64)),
            "route_host": "wms.acme.example",
            "policy": "prod",
            "target": "cloud",
            "applications": {"wms": {"packages": ["wamn_wms"], "connections": {"erp": "shared_erp", "files": "files"}, "connectors": {"http":"host", "fixture":format!("sha256:{}", "c".repeat(64))}}},
            "floors": {"wamn_wms": "2.4.0"}
        })
    }

    #[test]
    fn connector_pins_refuse_partial_digests_and_values() {
        for pin in [
            json!("latest"),
            json!("sha256:abcd"),
            json!({"digest":"host"}),
            json!(42),
        ] {
            let mut input = document();
            input["applications"]["wms"]["connectors"]["http"] = pin;
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        }
        let parsed = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        assert_eq!(parsed.applications["wms"].connectors["http"], "host");
    }

    #[test]
    fn refuses_inline_connection_definitions() {
        let mut input = document();
        input["connections"] = json!({"erp": {"type": "http", "definition": {}}});
        assert_eq!(
            serde_json::from_value::<EnvironmentDocument>(input)
                .unwrap_err()
                .to_string(),
            "unknown field `connections`, expected one of `org`, `project`, `env`, `target`, `release`, `route_host`, `client_package`, `policy`, `applications`, `floors`"
        );
    }

    #[test]
    fn names_round_trip_and_values_are_refused() {
        let parsed = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        assert_eq!(parsed.target, "cloud");
        assert_eq!(parsed.applications["wms"].connections["erp"], "shared_erp");
        for value in [
            json!({"org":"shared_erp"}),
            json!({"url":"https://example.com"}),
            json!({"credential":"secret"}),
            json!({"parameters":{}}),
        ] {
            let mut input = document();
            input["applications"]["wms"]["connections"]["erp"] = value;
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        }
        for field in ["integration", "mode"] {
            let mut input = document();
            input["applications"]["wms"][field] = json!("http");
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        }
    }

    #[test]
    fn target_is_required_and_nonempty() {
        let mut input = document();
        input.as_object_mut().unwrap().remove("target");
        assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        for target in [json!(""), json!("  "), json!(42)] {
            input["target"] = target;
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        }
    }

    #[test]
    fn org_references_round_trip_and_refuse_mixed_forms() {
        for binding in [
            json!({"org":"shared_erp","type":"http"}),
            json!({"org":"shared_erp","definition":{}}),
            json!({"org":"shared_erp","secret":"forbidden"}),
        ] {
            let mut input = document();
            let parsed = EnvironmentDocument::from_json(input.to_string().as_bytes()).unwrap();
            assert_eq!(parsed.applications["wms"].connections["erp"], "shared_erp");
            input["applications"]["wms"]["connections"]["erp"] = binding;
            assert_eq!(
                serde_json::from_value::<EnvironmentDocument>(input)
                    .unwrap_err()
                    .to_string(),
                "invalid type: map, expected a string"
            );
        }
    }

    #[test]
    fn the_kcl_schema_accepts_names_and_refuses_connection_values() {
        let parsed = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        let directory =
            std::env::temp_dir().join(format!("wamn-binding-kcl-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("environment.k");
        let kcl = parsed.to_kcl();
        std::fs::write(&file, &kcl).unwrap();
        assert_eq!(EnvironmentDocument::compile(&file).unwrap(), parsed);
        for replacement in [
            "{type = \"ftp\"}",
            "{type = \"http\", definition = {}}",
            "{org = \"shared_erp\"}",
        ] {
            std::fs::write(
                &file,
                kcl.replace(
                    "\"erp\" = \"shared_erp\"",
                    &format!("\"erp\" = {replacement}"),
                ),
            )
            .unwrap();
            assert!(EnvironmentDocument::compile(&file).is_err());
        }
        std::fs::write(&file, kcl.replace("    target = \"cloud\"\n", "")).unwrap();
        assert!(EnvironmentDocument::compile(&file).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reads_a_document_and_refuses_an_unknown_field() {
        let parsed = EnvironmentDocument::from_json(document().to_string().as_bytes())
            .expect("a valid document");
        assert_eq!(
            parsed.release,
            DeclaredRelease::Digest(format!("sha256:{}", "a".repeat(64)))
        );
        assert_eq!(parsed.triple().to_string(), "acme/wms/prod");
        let mut unknown = document();
        unknown["image"] = json!("host@sha256:00");
        let error = EnvironmentDocument::from_json(unknown.to_string().as_bytes())
            .expect_err("an unknown field is refused");
        assert!(
            format!("{error:#}").contains("unknown field `image`"),
            "{error:#}"
        );
    }

    #[test]
    fn applications_are_required_named_and_hold_packages() {
        let mut missing = document();
        missing.as_object_mut().unwrap().remove("applications");
        let error = EnvironmentDocument::from_json(missing.to_string().as_bytes()).unwrap_err();
        assert!(
            format!("{error:#}").contains("missing field `applications`"),
            "{error:#}"
        );
        for (applications, reason) in [
            (
                json!({"": {"packages": ["wamn_wms"]}}),
                "an application has an empty name",
            ),
            (
                json!({"wms": {"packages": []}}),
                "application wms has no packages",
            ),
            (
                json!({"wms": {"packages": [""]}}),
                "application wms has an empty package id",
            ),
        ] {
            let mut value = document();
            value["applications"] = applications;
            let error = EnvironmentDocument::from_json(value.to_string().as_bytes()).unwrap_err();
            assert_eq!(error.to_string(), reason);
        }
        let directory =
            std::env::temp_dir().join(format!("wamn-environment-apps-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the directory");
        let file = directory.join("environment.k");
        let kcl = EnvironmentDocument::from_json(document().to_string().as_bytes())
            .unwrap()
            .to_kcl();
        let start = kcl.find("    applications = {").unwrap();
        let end = start + kcl[start..].find("\n    }\n").unwrap() + "\n    }\n".len();
        std::fs::write(&file, format!("{}{}", &kcl[..start], &kcl[end..])).unwrap();
        let refused = EnvironmentDocument::compile(&file);
        std::fs::remove_dir_all(&directory).expect("remove the directory");
        assert!(
            refused.is_err(),
            "KCL refuses a document without applications"
        );
    }

    #[test]
    fn reads_none_and_a_full_digest_and_refuses_another_value() {
        assert_eq!(
            DeclaredRelease::parse("none").unwrap(),
            DeclaredRelease::None
        );
        let digest = format!("sha256:{}", "3f9c".repeat(16));
        assert_eq!(
            DeclaredRelease::parse(&digest).unwrap(),
            DeclaredRelease::Digest(digest.clone())
        );
        for refused in [
            "latest",
            "sha256:",
            "sha256:3f9c",
            &digest[..digest.len() - 1],
            "sha256:3F9C",
            "sha512:00",
        ] {
            assert!(DeclaredRelease::parse(refused).is_err(), "{refused}");
        }
    }

    #[test]
    fn canonical_bytes_do_not_depend_on_key_order() {
        let parsed = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        let reordered: Value = serde_json::from_slice(&parsed.canonical_json().unwrap()).unwrap();
        assert_eq!(
            EnvironmentDocument::from_json(reordered.to_string().as_bytes()).unwrap(),
            parsed
        );
    }

    #[test]
    fn the_kcl_form_compiles_back_to_the_same_document() {
        let parsed = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        let directory =
            std::env::temp_dir().join(format!("wamn-environment-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the directory");
        // The name R21 gives the document, which the schema module shares.
        let file = directory.join("environment.k");
        std::fs::write(&file, parsed.to_kcl()).expect("write the document");
        let compiled = EnvironmentDocument::compile(&file);
        std::fs::remove_dir_all(&directory).expect("remove the directory");
        assert_eq!(compiled.expect("the document compiles"), parsed);
    }

    #[test]
    fn kcl_refuses_a_field_the_schema_lacks() {
        let directory =
            std::env::temp_dir().join(format!("wamn-environment-bad-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the directory");
        let file = directory.join("bad.k");
        std::fs::write(
            &file,
            "import environment\n\nenvironment.Environment {\n    org = \"acme\"\n    project = \"wms\"\n    env = \"prod\"\n    release = \"none\"\n    route_host = \"wms.acme.example\"\n    policy = \"prod\"\n    applications = {}\n    image = \"host\"\n}\n",
        )
        .expect("write the document");
        let refused = EnvironmentDocument::compile(&file);
        std::fs::remove_dir_all(&directory).expect("remove the directory");
        assert!(refused.is_err(), "a field outside the schema is refused");
    }
}

//! The environment document, `environment.k` (docs/plan/platform-deploy.md
//! §10.1, R21).
//!
//! The document names the coordinate, the release digest or `none`, the route
//! host, the policy by name, the connection definitions and the floors, and
//! nothing else. It compiles with the `environment` KCL schema module to JSON,
//! which this module reads into [`EnvironmentDocument`]. Its canonical form is
//! RFC 8785 JSON, as `wamn.json` is.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

use anyhow::{Context as _, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wamn_catalog::RequirementType;
use wamn_control_registry::Triple;

/// One environment document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentDocument {
    pub org: String,
    pub project: String,
    pub env: String,
    /// `none`, or a full `sha256:` digest.
    pub release: DeclaredRelease,
    pub route_host: String,
    /// A policy of `registry.env_policies`, by name.
    pub policy: String,
    /// Connection definitions by instance id.
    #[serde(default)]
    pub connections: BTreeMap<String, ConnectionDefinition>,
    /// Package id to the version to contract to (R13).
    #[serde(default)]
    pub floors: BTreeMap<String, String>,
}

/// A connection definition. Credentials are provisioned, never declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ConnectionDefinition {
    Inline {
        #[serde(rename = "type")]
        requirement_type: RequirementType,
        definition: Value,
    },
    Org {
        org: String,
    },
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
        let mut org_names = BTreeSet::new();
        for (instance, connection) in &self.connections {
            ensure!(
                !instance.is_empty(),
                "a connection has an empty instance id"
            );
            match connection {
                ConnectionDefinition::Inline {
                    requirement_type,
                    definition,
                } => {
                    if let Err(error) = requirement_type.check_definition(definition) {
                        bail!("connection {instance}: {error}");
                    }
                }
                ConnectionDefinition::Org { org } => {
                    wamn_catalog::org_credential_handle(&self.org, org)
                        .map_err(anyhow::Error::msg)?;
                    ensure!(
                        org_names.insert(org),
                        "org connection {org} is referenced by two aliases"
                    );
                }
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
            ("release", self.release.to_string()),
            ("route_host", self.route_host.clone()),
            ("policy", self.policy.clone()),
        ] {
            lines.push(format!("    {key} = {}", quote(&value)));
        }
        lines.push("    connections = {".to_owned());
        for (instance, connection) in &self.connections {
            match connection {
                ConnectionDefinition::Inline {
                    requirement_type,
                    definition,
                } => {
                    let kind =
                        serde_json::to_value(requirement_type).expect("a requirement type encodes");
                    lines.push(format!(
                        "        {} = {{type = {kind}, definition = {definition}}}",
                        quote(instance)
                    ));
                }
                ConnectionDefinition::Org { org } => {
                    lines.push(format!(
                        "        {} = {{org = {}}}",
                        quote(instance),
                        quote(org)
                    ));
                }
            }
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
    use serde_json::json;

    fn document() -> Value {
        json!({
            "org": "acme",
            "project": "wms",
            "env": "prod",
            "release": format!("sha256:{}", "a".repeat(64)),
            "route_host": "wms.acme.example",
            "policy": "prod",
            "connections": {
                "files": {"type": "blobstore", "definition": {"endpoint": "https://s3.example", "container": "files", "prefix": "wms/"}}
            },
            "floors": {"wamn_wms": "2.4.0"}
        })
    }

    fn http_document() -> Value {
        let mut value = document();
        value["connections"] = json!({
            "erp": {"type": "http", "definition": {
                "primary-authority": "http://http-fixture:8080/", "tls-verification": "disabled"
            }}
        });
        value
    }

    #[test]
    fn org_references_round_trip_and_refuse_mixed_or_duplicate_forms() {
        let mut value = http_document();
        value["connections"]["erp"] = json!({"org": "shared_erp"});
        let parsed = EnvironmentDocument::from_json(value.to_string().as_bytes()).unwrap();
        assert_eq!(
            parsed.connections["erp"],
            ConnectionDefinition::Org {
                org: "shared_erp".to_owned()
            }
        );
        let directory =
            std::env::temp_dir().join(format!("wamn-org-connection-kcl-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("environment.k");
        let kcl = parsed.to_kcl();
        std::fs::write(&file, &kcl).unwrap();
        assert_eq!(EnvironmentDocument::compile(&file).unwrap(), parsed);
        std::fs::write(
            &file,
            kcl.replace(
                "org = \"shared_erp\"",
                "org = \"shared_erp\", type = \"http\", definition = {}",
            ),
        )
        .unwrap();
        assert!(EnvironmentDocument::compile(&file).is_err());
        std::fs::remove_dir_all(&directory).unwrap();
        for extra in [
            json!({"type": "http"}),
            json!({"definition": {}}),
            json!({"secret": "forbidden"}),
        ] {
            let mut mixed = value.clone();
            mixed["connections"]["erp"]
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert!(EnvironmentDocument::from_json(mixed.to_string().as_bytes()).is_err());
        }
        value["connections"]["second"] = json!({"org": "shared_erp"});
        assert!(
            EnvironmentDocument::from_json(value.to_string().as_bytes())
                .unwrap_err()
                .to_string()
                .contains("two aliases")
        );
    }

    #[test]
    fn reads_an_http_connection_and_refuses_an_unknown_type() {
        let mut value = http_document();
        let parsed = EnvironmentDocument::from_json(value.to_string().as_bytes()).unwrap();
        assert_eq!(
            parsed.connections["erp"],
            ConnectionDefinition::Inline {
                requirement_type: RequirementType::Http,
                definition: value["connections"]["erp"]["definition"].clone(),
            }
        );
        value["connections"]["erp"]["type"] = json!("ftp");
        let error = EnvironmentDocument::from_json(value.to_string().as_bytes()).unwrap_err();
        assert!(
            error
                .root_cause()
                .to_string()
                .contains("did not match any variant"),
            "{error:#}"
        );
    }

    #[test]
    fn refuses_invalid_http_coordinates_by_name() {
        for coordinate in ["primary-authority", "tls-verification"] {
            for (replacement, reason) in [
                (
                    None,
                    format!(" lacks {coordinate}; Http reads it at resolve time"),
                ),
                (
                    Some(json!("")),
                    format!("'s {coordinate} is empty; Http needs it"),
                ),
                (
                    Some(json!(42)),
                    format!("'s {coordinate} must be a string; Http reads it as one"),
                ),
            ] {
                let mut value = http_document();
                let definition = value["connections"]["erp"]["definition"]
                    .as_object_mut()
                    .unwrap();
                if let Some(replacement) = replacement {
                    definition.insert(coordinate.to_owned(), replacement);
                } else {
                    definition.remove(coordinate);
                }
                let error =
                    EnvironmentDocument::from_json(value.to_string().as_bytes()).unwrap_err();
                assert_eq!(
                    error.to_string(),
                    format!("connection erp: the generation definition{reason}")
                );
            }
        }
    }

    #[test]
    fn refuses_unread_http_keys_and_a_nonobject_definition() {
        for key in ["provider", "endpoint", "headers"] {
            let mut value = http_document();
            value["connections"]["erp"]["definition"][key] = json!("unused");
            let error = EnvironmentDocument::from_json(value.to_string().as_bytes()).unwrap_err();
            assert_eq!(
                error.to_string(),
                format!(
                    "connection erp: the generation definition carries {key}, which Http never reads; a coordinate nobody reads is a coordinate nobody validates"
                )
            );
        }
        let mut value = http_document();
        value["connections"]["erp"]["definition"] = json!("not an object");
        let error = EnvironmentDocument::from_json(value.to_string().as_bytes()).unwrap_err();
        assert_eq!(
            error.to_string(),
            "connection erp: the generation definition must be a JSON object"
        );
    }

    #[test]
    fn the_kcl_schema_accepts_http_and_refuses_an_unknown_connection_type() {
        let parsed =
            EnvironmentDocument::from_json(http_document().to_string().as_bytes()).unwrap();
        let directory =
            std::env::temp_dir().join(format!("wamn-environment-http-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create the directory");
        let file = directory.join("environment.k");
        let kcl = parsed.to_kcl();
        std::fs::write(&file, &kcl).expect("write the document");
        let compiled = EnvironmentDocument::compile(&file);
        std::fs::write(&file, kcl.replace("type = \"http\"", "type = \"ftp\""))
            .expect("write the unknown type");
        let refused = EnvironmentDocument::compile(&file);
        std::fs::remove_dir_all(&directory).expect("remove the directory");
        assert_eq!(compiled.expect("HTTP compiles"), parsed);
        assert!(
            refused.is_err(),
            "the schema refuses an unknown connection type"
        );
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
    fn refuses_a_connection_definition_its_type_refuses() {
        let mut bad = document();
        bad["connections"]["files"]["definition"] = json!("not an object");
        let error = EnvironmentDocument::from_json(bad.to_string().as_bytes())
            .expect_err("the definition is refused");
        assert!(
            format!("{error:#}").contains("connection files"),
            "{error:#}"
        );
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
            "import environment\n\nenvironment.Environment {\n    org = \"acme\"\n    project = \"wms\"\n    env = \"prod\"\n    release = \"none\"\n    route_host = \"wms.acme.example\"\n    policy = \"prod\"\n    image = \"host\"\n}\n",
        )
        .expect("write the document");
        let refused = EnvironmentDocument::compile(&file);
        std::fs::remove_dir_all(&directory).expect("remove the directory");
        assert!(refused.is_err(), "a field outside the schema is refused");
    }
}

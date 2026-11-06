//! The environment document, `environment.k`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The document keeps shared configuration and each application's selection.
//! Runtime hosts receive only the declared packages and connections.
//! KCL compiles the document to JSON. Its canonical form is RFC 8785 JSON.

pub(super) mod plan;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_catalog::EnvironmentApplications;
use wamn_control_registry::Triple;

pub use plan::{ApplicationSelection, EnvironmentDocument};

pub use plan::Client;

pub use plan::Integration;

pub use plan::DeclaredRelease;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn document() -> Value {
        json!({
            "org": "acme", "project": "wms", "env": "prod",
            "policy": "prod", "target": "cloud",
            "applications": {
                "wms": {
                    "current": format!("sha256:{}", "a".repeat(64)),
                    "hostname": "wms.acme.example",
                    "packages": ["wamn_wms"],
                    "connections": {"erp": "shared_erp", "files": "files"},
                    "client": {"package": "wamn_wms", "web": format!("sha256:{}", "b".repeat(64))}
                },
                "receiving": {
                    "current": format!("sha256:{}", "c".repeat(64)),
                    "previous": format!("sha256:{}", "d".repeat(64)),
                    "hostname": "receiving.acme.example",
                    "packages": ["wamn_receiving"]
                }
            },
            "integrations": {"shared_erp": {"version": 1}, "files": {"version": 1}}
        })
    }

    #[test]
    fn application_selections_round_trip_through_kcl_and_the_product_reader() {
        let input = document();
        let parsed = EnvironmentDocument::from_json(input.to_string().as_bytes()).unwrap();
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let file = scratch.path().join("environment.k");
        std::fs::write(&file, parsed.to_kcl()).unwrap();
        let compiled = EnvironmentDocument::compile(&file).unwrap();
        assert_eq!(compiled, parsed);
        let canonical: Value = serde_json::from_slice(&compiled.canonical_json().unwrap()).unwrap();
        assert!(canonical["applications"]["wms"].get("previous").is_none());
        assert_eq!(
            canonical["applications"]["receiving"]["previous"],
            input["applications"]["receiving"]["previous"]
        );
        assert_ne!(
            compiled.applications["wms"].hostname,
            compiled.applications["receiving"].hostname
        );
        assert_eq!(compiled.triple().to_string(), "acme/wms/prod");
        assert_eq!(compiled.target, input["target"]);
        assert_eq!(compiled.policy, input["policy"]);
        assert_eq!(canonical["integrations"], input["integrations"]);
        let membership = serde_json::to_value(compiled.membership()).unwrap();
        assert_eq!(
            membership["wms"],
            json!({"packages": ["wamn_wms"], "connections": {"erp": "shared_erp", "files": "files"}})
        );
    }

    #[test]
    fn selection_digests_are_validated_in_json_and_kcl_with_the_application_name() {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let file = scratch.path().join("environment.k");
        for field in ["current", "previous", "web"] {
            for invalid in [
                String::new(),
                "latest".to_owned(),
                "sha256:".to_owned(),
                format!("sha256:{}", "A".repeat(64)),
                format!("sha256:{}", "a".repeat(63)),
                format!("sha256:{}", "a".repeat(65)),
                format!("sha256:{}\n", "a".repeat(64)),
            ] {
                let mut input = document();
                let slot = if field == "web" {
                    &mut input["applications"]["wms"]["client"]["web"]
                } else {
                    &mut input["applications"]["wms"][field]
                };
                *slot = json!(invalid);
                let error =
                    EnvironmentDocument::from_json(input.to_string().as_bytes()).unwrap_err();
                assert!(format!("{error:#}").contains("wms"));
                let parsed =
                    EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
                let valid = format!(
                    "sha256:{}",
                    match field {
                        "web" => "b",
                        "previous" => "d",
                        _ => "a",
                    }
                    .repeat(64)
                );
                let kcl = parsed.to_kcl().replace(
                    &serde_json::to_string(&valid).unwrap(),
                    &serde_json::to_string(&invalid).unwrap(),
                );
                std::fs::write(&file, kcl).unwrap();
                let error = EnvironmentDocument::compile(&file).unwrap_err();
                let application = if field == "previous" {
                    "receiving"
                } else {
                    "wms"
                };
                assert!(format!("{error:#}").contains(application));
            }
        }
        let mut input = document();
        input["applications"]["wms"]["current"] = json!("none");
        assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_ok());
        input["applications"]["wms"]["previous"] = json!("none");
        assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
    }

    #[test]
    fn refuses_old_ownership_and_invalid_membership_or_shared_fields() {
        for (field, value) in [
            ("release", json!("none")),
            ("route_host", json!("wms.example")),
            ("client", json!({})),
            ("connections", json!({})),
            ("image", json!("host")),
            ("target", json!("  ")),
        ] {
            let mut input = document();
            input[field] = value;
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        }
        for field in ["target", "applications"] {
            let mut input = document();
            input.as_object_mut().unwrap().remove(field);
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        }
        for (field, value) in [
            ("packages", json!([])),
            ("packages", json!([""])),
            ("hostname", json!("Invalid Host")),
            ("mode", json!("http")),
            ("connections", json!({"erp": "org:other:erp"})),
            (
                "connections",
                json!({"erp": {"type": "http", "definition": {}}}),
            ),
        ] {
            let mut input = document();
            input["applications"]["wms"][field] = value;
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        }
        let mut unnamed = document();
        let application = unnamed["applications"]
            .as_object_mut()
            .unwrap()
            .remove("wms")
            .unwrap();
        unnamed["applications"][""] = application;
        assert!(EnvironmentDocument::from_json(unnamed.to_string().as_bytes()).is_err());
        for version in [
            json!(0),
            json!(-1),
            json!("latest"),
            json!({"digest": "host"}),
        ] {
            let mut input = document();
            input["integrations"]["shared_erp"]["version"] = version;
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        }
    }

    #[test]
    fn kcl_refuses_old_fields_missing_applications_and_connection_values() {
        let parsed = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let file = scratch.path().join("environment.k");
        let kcl = parsed.to_kcl();
        for old in [
            "release = \"none\"",
            "route_host = \"wms.example\"",
            "client = {package = \"wms\", web = \"invalid\"}",
        ] {
            std::fs::write(
                &file,
                kcl.replace("    policy", &format!("    {old}\n    policy")),
            )
            .unwrap();
            assert!(EnvironmentDocument::compile(&file).is_err());
        }
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
        let start = kcl.find("    applications = {").unwrap();
        std::fs::write(&file, format!("{}}}\n", &kcl[..start])).unwrap();
        assert!(EnvironmentDocument::compile(&file).is_err());
    }
}

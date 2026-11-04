//! The environment document, `environment.k`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The document names the coordinate, the release digest or `none`, the route
//! host, the policy by name, the applications and their packages
//! (docs/architecture/execution.md#workflows), the target, and connection names.
//! It compiles with the `environment` KCL schema
//! module to JSON, which this module reads into [`EnvironmentDocument`]. Its
//! canonical form is RFC 8785 JSON, as `wamn.json` is.

pub(super) mod plan;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use wamn_catalog::EnvironmentApplications;
use wamn_control_registry::Triple;

pub use plan::EnvironmentDocument;

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
            "org": "acme",
            "project": "wms",
            "env": "prod",
            "release": format!("sha256:{}", "a".repeat(64)),
            "route_host": "wms.acme.example",
            "policy": "prod",
            "target": "cloud",
            "applications": {"wms": {"packages": ["wamn_wms"], "connections": {"erp": "shared_erp", "files": "files"}}},
            "integrations": {"shared_erp":{"version":1}, "files":{"version":1}}
        })
    }

    #[test]
    fn integration_versions_require_positive_integers() {
        for version in [
            json!(0),
            json!(-1),
            json!("latest"),
            json!({"digest":"host"}),
        ] {
            let mut input = document();
            input["integrations"]["shared_erp"]["version"] = version;
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
        }
        let parsed = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        assert_eq!(parsed.integrations["shared_erp"].version, 1);
    }

    #[test]
    fn refuses_inline_connection_definitions() {
        let mut input = document();
        input["connections"] = json!({"erp": {"type": "http", "definition": {}}});
        assert_eq!(
            serde_json::from_value::<EnvironmentDocument>(input)
                .unwrap_err()
                .to_string(),
            "unknown field `connections`, expected one of `org`, `project`, `env`, `target`, `release`, `route_host`, `client`, `policy`, `applications`, `integrations`"
        );
    }

    #[test]
    fn binding_refuses_a_handle_from_another_org_before_connecting() {
        let mut input = document();
        input["applications"]["wms"]["connections"]["erp"] = json!("org:other:erp");
        assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
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
        let mut input = document();
        input["client"] = json!({
            "package": "wamn_wms",
            "web": wamn_execution_contract::bytes_sha256(b"web artifact")
        });
        let parsed = EnvironmentDocument::from_json(input.to_string().as_bytes()).unwrap();
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
    fn client_web_requires_a_full_lowercase_digest_in_json_and_kcl() {
        let directory =
            std::env::temp_dir().join(format!("wamn-environment-web-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let file = directory.join("environment.k");
        for web in [
            "sha256:".to_owned(),
            format!("sha256:{}", "a".repeat(63)),
            format!("sha256:{}", "a".repeat(65)),
            format!("sha256:{}", "A".repeat(64)),
            format!("sha512:{}", "a".repeat(64)),
            format!("sha256:{}\n", "a".repeat(64)),
        ] {
            let mut input = document();
            input["client"] = json!({"package": "wamn_wms", "web": web});
            assert!(EnvironmentDocument::from_json(input.to_string().as_bytes()).is_err());
            let parsed: EnvironmentDocument = serde_json::from_value(input).unwrap();
            std::fs::write(&file, parsed.to_kcl()).unwrap();
            assert!(
                wamn_schema_generator::compile_environment(&file).is_err(),
                "{web:?}"
            );
        }
        std::fs::remove_dir_all(&directory).unwrap();
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

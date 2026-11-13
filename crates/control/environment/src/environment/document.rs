//! The environment document, `environment.k`
//! (docs/architecture/overview.md#environment-lifecycle).
//!
//! The document keeps shared configuration and each application's selection.
//! Runtime hosts receive only the declared packages and connections.
//! KCL compiles the document to JSON. Its canonical form is RFC 8785 JSON.

pub(super) mod plan;
mod store;
pub(super) use store::JOB_DOCUMENT;

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
pub use plan::{Schema, SchemaMode};

impl EnvironmentDocument {
    /// Read compiled JSON or compile an `environment.k` file.
    ///
    /// # Errors
    ///
    /// When KCL refuses the file, or when the JSON is not a valid document.
    pub fn compile(file: &Path) -> anyhow::Result<Self> {
        let bytes = if file
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            std::fs::read(file)?
        } else {
            wamn_compiler::compile_environment(file)?
        };
        Self::from_json(&bytes)
            .with_context(|| format!("{} is not a valid environment", file.display()))
    }
}

/// A requested change to one application's canonical selection.
#[derive(Debug, Clone, Copy)]
pub(super) enum SelectionChange<'a> {
    Deploy(&'a str),
    Reapply,
    Rollback(Option<&'a str>),
}

/// Distinct release selections from successful operations in application Helm history.
#[derive(Debug, Default)]
pub(super) struct AppliedReleases {
    pub current: Option<String>,
    pub previous: Option<String>,
}

impl AppliedReleases {
    /// Consume successful revisions newest first, stopping at the preceding distinct release.
    pub fn record(&mut self, digest: &str) -> anyhow::Result<bool> {
        wamn_package_artifact::ensure_digest(digest)?;
        match &self.current {
            None => self.current = Some(digest.to_owned()),
            Some(current) if current != digest => {
                self.previous = Some(digest.to_owned());
                return Ok(true);
            }
            Some(_) => {}
        }
        Ok(false)
    }
}

pub(super) fn selection_warning(
    application: &str,
    current: &DeclaredRelease,
    applied: Option<&str>,
) -> Option<String> {
    let requested = match current {
        DeclaredRelease::None => None,
        DeclaredRelease::Digest(digest) => Some(digest.as_str()),
    };
    (requested != applied).then(|| {
        format!(
            "application {application}: ConfigMap selects {}, last successful Helm apply is {}; inspect wamn status --env <environment.k>",
            requested.unwrap_or("none"),
            applied.unwrap_or("none"),
        )
    })
}

impl EnvironmentDocument {
    /// Change one release pair without changing shared configuration or other applications.
    pub(super) fn select_release(
        &mut self,
        application: &str,
        change: SelectionChange<'_>,
        applied: &AppliedReleases,
    ) -> anyhow::Result<()> {
        let selection = self
            .applications
            .get_mut(application)
            .with_context(|| format!("application {application} is not declared"))?;
        let next = match change {
            SelectionChange::Reapply => match &selection.current {
                DeclaredRelease::None => return Ok(()),
                DeclaredRelease::Digest(current) => current.as_str(),
            },
            SelectionChange::Deploy(digest) | SelectionChange::Rollback(Some(digest)) => digest,
            SelectionChange::Rollback(None) => {
                ensure!(
                    matches!(&selection.current, DeclaredRelease::Digest(current) if Some(current) == applied.current.as_ref())
                        && selection.previous == applied.previous
                        && applied.previous.is_some(),
                    "application {application}: rollback target is ambiguous; use wamn rollback {application} --env <environment.k> --release <digest>"
                );
                applied.previous.as_deref().expect("prior release exists")
            }
        };
        wamn_package_artifact::ensure_digest(next)?;
        if applied.current.as_deref() == Some(next)
            && matches!(&selection.current, DeclaredRelease::Digest(current) if current == next)
        {
            return Ok(());
        }
        let previous = if applied.current.as_deref() == Some(next) {
            &applied.previous
        } else {
            &applied.current
        };
        let next = next.to_owned();
        selection.previous.clone_from(previous);
        selection.current = DeclaredRelease::Digest(next);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    pub(super) fn document() -> Value {
        json!({
            "org": "acme", "project": "wms", "env": "prod",
            "policy": "prod", "target": "cloud",
            "schema": {"mode": "reset"},
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
    fn stale_checkout_and_failed_deploy_retries_use_applied_releases() {
        let mut document =
            EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        let a0 = document.applications["wms"].current.to_string();
        let a1 = format!("sha256:{}", "e".repeat(64));
        let a2 = format!("sha256:{}", "f".repeat(64));
        let mut applied = AppliedReleases::default();
        // A successful reapply of A1 does not replace A0 as the prior release.
        for digest in [&a1, &a1, &a0] {
            applied.record(digest).unwrap();
        }
        let before = document.clone();
        let warning = selection_warning(
            "wms",
            &document.applications["wms"].current,
            applied.current.as_deref(),
        )
        .unwrap();
        assert!(warning.contains(&document.applications["wms"].current.to_string()));
        assert!(warning.contains(&a1));
        document
            .select_release("wms", SelectionChange::Deploy(&a2), &applied)
            .unwrap();
        let mut expected = before;
        let target = expected.applications.get_mut("wms").unwrap();
        target.current = DeclaredRelease::Digest(a2.clone());
        target.previous = Some(a1.clone());
        assert_eq!(document, expected);
        for change in [SelectionChange::Reapply, SelectionChange::Deploy(&a2)] {
            document.select_release("wms", change, &applied).unwrap();
            assert_eq!(document, expected);
        }
        let applied = AppliedReleases {
            current: Some(a2),
            previous: Some(a1),
        };
        document
            .select_release("wms", SelectionChange::Reapply, &applied)
            .unwrap();
        assert_eq!(document, expected);
        assert!(
            selection_warning(
                "wms",
                &document.applications["wms"].current,
                applied.current.as_deref()
            )
            .is_none()
        );
        document
            .select_release("wms", SelectionChange::Rollback(None), &applied)
            .unwrap();
        let target = expected.applications.get_mut("wms").unwrap();
        target.current = DeclaredRelease::Digest(applied.previous.unwrap());
        target.previous = applied.current;
        assert_eq!(document, expected);
    }

    #[test]
    fn ambiguous_rollback_requires_an_explicit_valid_release() {
        let mut initial =
            EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        let a0 = initial.applications["wms"].current.to_string();
        let a1 = format!("sha256:{}", "e".repeat(64));
        let a2 = format!("sha256:{}", "f".repeat(64));
        initial.applications.get_mut("wms").unwrap().previous = Some(a1.clone());
        for applied in [
            AppliedReleases {
                current: Some(a0),
                previous: Some(a2.clone()),
            },
            AppliedReleases {
                current: Some(a1),
                previous: Some(a2.clone()),
            },
            AppliedReleases::default(),
        ] {
            let mut document = initial.clone();
            assert!(
                document
                    .select_release("wms", SelectionChange::Rollback(None), &applied)
                    .is_err()
            );
            assert_eq!(document, initial);
            assert!(
                document
                    .select_release("wms", SelectionChange::Rollback(Some("latest")), &applied)
                    .is_err()
            );
            assert_eq!(document, initial);
            document
                .select_release("wms", SelectionChange::Rollback(Some(&a2)), &applied)
                .unwrap();
            let mut expected = initial.clone();
            let target = expected.applications.get_mut("wms").unwrap();
            target.current = DeclaredRelease::Digest(a2.clone());
            target.previous = applied.current;
            assert_eq!(document, expected);
        }
    }

    #[test]
    fn first_install_and_reselecting_the_applied_release_do_not_rotate_the_file() {
        let mut document =
            EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        let a0 = document.applications["wms"].current.to_string();
        let a1 = format!("sha256:{}", "e".repeat(64));
        document
            .select_release(
                "wms",
                SelectionChange::Deploy(&a1),
                &AppliedReleases::default(),
            )
            .unwrap();
        assert!(document.applications["wms"].previous.is_none());
        let applied = AppliedReleases {
            current: Some(a0),
            previous: Some(a1),
        };
        document
            .select_release(
                "wms",
                SelectionChange::Deploy(applied.current.as_deref().unwrap()),
                &applied,
            )
            .unwrap();
        assert_eq!(
            document.applications["wms"].current.to_string(),
            applied.current.unwrap()
        );
        assert_eq!(document.applications["wms"].previous, applied.previous);
        let before = document.clone();
        assert!(
            document
                .select_release(
                    "absent",
                    SelectionChange::Reapply,
                    &AppliedReleases::default()
                )
                .is_err()
        );
        assert_eq!(document, before);
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
        assert_eq!(canonical["schema"], input["schema"]);
        let selected = super::super::analyse::select_application(&compiled, Some("wms")).unwrap();
        assert_eq!(selected.schema, compiled.schema);
        let membership = serde_json::to_value(compiled.membership()).unwrap();
        assert_eq!(
            membership["wms"],
            json!({"packages": ["wamn_wms"], "connections": {"erp": "shared_erp", "files": "files"}})
        );
    }

    #[test]
    fn schema_modes_match_in_json_and_kcl_and_default_only_from_the_record() {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let file = scratch.path().join("environment.k");
        for mode in [None, Some(SchemaMode::Reset), Some(SchemaMode::Migrate)] {
            let mut input = document();
            if let Some(mode) = mode {
                input["schema"]["mode"] = serde_json::to_value(mode).unwrap();
            } else {
                input.as_object_mut().unwrap().remove("schema");
            }
            let parsed = EnvironmentDocument::from_json(input.to_string().as_bytes()).unwrap();
            std::fs::write(&file, parsed.to_kcl()).unwrap();
            assert_eq!(EnvironmentDocument::compile(&file).unwrap(), parsed);
            for (disposable, default) in [
                (Some(true), SchemaMode::Reset),
                (Some(false), SchemaMode::Migrate),
                (None, SchemaMode::Migrate),
            ] {
                assert_eq!(parsed.schema_mode(disposable), mode.unwrap_or(default));
            }
            // Mode edits use only the document owner and retain all application facts.
            let applications = parsed.applications;
            let mut edited = EnvironmentDocument::compile(&file).unwrap();
            for mode in [SchemaMode::Reset, SchemaMode::Migrate, SchemaMode::Reset] {
                edited.schema.mode = Some(mode);
                std::fs::write(&file, edited.to_kcl()).unwrap();
                edited = EnvironmentDocument::compile(&file).unwrap();
                assert_eq!(edited.applications, applications);
                assert_eq!(edited.schema.mode, Some(mode));
            }
        }
        let parsed = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        for invalid in ["", "unknown", "RESET"] {
            let mut input = document();
            input["schema"]["mode"] = json!(invalid);
            let error = EnvironmentDocument::from_json(input.to_string().as_bytes()).unwrap_err();
            assert!(format!("{error:#}").contains(invalid));
            std::fs::write(
                &file,
                parsed
                    .to_kcl()
                    .replace("mode = \"reset\"", &format!("mode = {invalid:?}")),
            )
            .unwrap();
            let error = EnvironmentDocument::compile(&file).unwrap_err();
            assert!(format!("{error:#}").contains("mode"));
        }
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

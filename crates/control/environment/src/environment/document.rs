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

/// A requested change to one application's authored selection.
#[derive(Debug, Clone, Copy)]
pub enum SelectionChange<'a> {
    Deploy(&'a str),
    Reapply,
    Rollback,
}

impl EnvironmentDocument {
    /// Change one release pair without changing shared configuration or other applications.
    pub fn select_release(
        &mut self,
        application: &str,
        change: SelectionChange<'_>,
    ) -> anyhow::Result<()> {
        let selection = self
            .applications
            .get_mut(application)
            .with_context(|| format!("application {application} is not declared"))?;
        let next = match change {
            SelectionChange::Reapply => return Ok(()),
            SelectionChange::Deploy(digest) => {
                wamn_package_artifact::ensure_digest(digest)?;
                if matches!(&selection.current, DeclaredRelease::Digest(current) if current == digest)
                {
                    return Ok(());
                }
                digest.to_owned()
            }
            SelectionChange::Rollback => selection.previous.clone().with_context(|| {
                format!("application {application}: no retained prior release; nothing was written")
            })?,
        };
        selection.previous =
            match std::mem::replace(&mut selection.current, DeclaredRelease::Digest(next)) {
                DeclaredRelease::None => None,
                DeclaredRelease::Digest(current) => Some(current),
            };
        Ok(())
    }
}

/// Merge a prepared selection into the latest file under a short file lock.
/// A concurrent edit to the target or shared configuration refuses before Helm.
pub(super) fn persist_selection(
    file: &Path,
    before: &EnvironmentDocument,
    prepared: &EnvironmentDocument,
    application: &str,
) -> anyhow::Result<EnvironmentDocument> {
    use std::io::Write as _;
    let file = std::fs::canonicalize(file)?;
    let _lock = super::lock::document_lock(&file)?;
    let mut latest = EnvironmentDocument::compile(&file)?;
    ensure!(
        latest.org == before.org
            && latest.project == before.project
            && latest.env == before.env
            && latest.target == before.target
            && latest.policy == before.policy
            && latest.integrations == before.integrations
            && latest.applications.get(application) == before.applications.get(application),
        "environment configuration or application {application} changed during preparation; retry"
    );
    let selection = latest
        .applications
        .get_mut(application)
        .context("application disappeared")?;
    let intended = &prepared.applications[application];
    if selection.current == intended.current && selection.previous == intended.previous {
        return Ok(latest);
    }
    selection.current.clone_from(&intended.current);
    selection.previous.clone_from(&intended.previous);
    let temporary = file.with_extension("k.pending");
    let result = (|| -> anyhow::Result<()> {
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        output.set_permissions(std::fs::metadata(&file)?.permissions())?;
        output.write_all(latest.to_kcl().as_bytes())?;
        output.sync_all()?;
        std::fs::rename(&temporary, &file)?;
        std::fs::File::open(file.parent().context("environment parent")?)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.with_context(|| format!("persist application {application} in {}", file.display()))?;
    Ok(latest)
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
    fn replacement_retry_and_rollback_change_only_the_target_selection() {
        let digest = |byte: char| format!("sha256:{}", byte.to_string().repeat(64));
        let (a0, a1, b0, b1) = (digest('a'), digest('b'), digest('c'), digest('d'));
        let mut document = EnvironmentDocument::from_json(
            &serde_json::to_vec(&serde_json::json!({
                "org": "acme", "project": "apps", "env": "dev",
                "target": "cloud", "policy": "dev",
                "applications": {
                    "a": {"current": a0, "hostname": "a.example", "packages": ["a"]},
                    "b": {"current": b0, "hostname": "b.example", "packages": ["b"]}
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let mut expected = document.clone();
        for (application, change, current, previous) in [
            ("a", SelectionChange::Deploy(&a1), &a1, Some(&a0)),
            // Retry a pending selection after a failed apply without rotating its predecessor.
            ("a", SelectionChange::Reapply, &a1, Some(&a0)),
            ("a", SelectionChange::Deploy(&a1), &a1, Some(&a0)),
            ("b", SelectionChange::Deploy(&b1), &b1, Some(&b0)),
            ("a", SelectionChange::Rollback, &a0, Some(&a1)),
        ] {
            document.select_release(application, change).unwrap();
            let target = expected.applications.get_mut(application).unwrap();
            target.current = DeclaredRelease::Digest(current.to_owned());
            target.previous = previous.cloned();
            assert_eq!(document, expected);
        }
        for (application, change) in [
            ("a", SelectionChange::Deploy("latest")),
            ("absent", SelectionChange::Deploy(&a1)),
        ] {
            assert!(document.select_release(application, change).is_err());
            assert_eq!(document, expected);
        }
        document.applications.get_mut("a").unwrap().current = DeclaredRelease::None;
        document.applications.get_mut("a").unwrap().previous = None;
        assert!(
            document
                .select_release("a", SelectionChange::Rollback)
                .is_err()
        );
        document
            .select_release("a", SelectionChange::Deploy(&a0))
            .unwrap();
        assert_eq!(
            document.applications["a"].current,
            DeclaredRelease::Digest(a0)
        );
        assert!(document.applications["a"].previous.is_none());
    }

    #[test]
    fn persistence_keeps_pending_selection_and_refuses_stale_configuration() {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let file = scratch.path().join("environment.k");
        let initial = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        std::fs::write(&file, initial.to_kcl()).unwrap();
        let mut prepared = initial.clone();
        let selection = prepared.applications.get_mut("wms").unwrap();
        selection.previous = Some(selection.current.to_string());
        selection.current = DeclaredRelease::Digest(format!("sha256:{}", "e".repeat(64)));
        persist_selection(&file, &initial, &prepared, "wms").unwrap();
        // A failed rollout leaves the prepared selection on disk for the next invocation.
        let pending = EnvironmentDocument::compile(&file).unwrap();
        assert_eq!(pending, prepared);
        let bytes = std::fs::read(&file).unwrap();
        persist_selection(&file, &pending, &pending, "wms").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), bytes);
        let mut changed = pending.clone();
        changed.policy = "other".into();
        std::fs::write(&file, changed.to_kcl()).unwrap();
        assert!(persist_selection(&file, &pending, &initial, "wms").is_err());
        assert_eq!(EnvironmentDocument::compile(&file).unwrap(), changed);
    }

    #[test]
    fn concurrent_application_writes_merge_without_losing_either_selection() {
        let scratch = wamn_test_infrastructure::scratch::ScratchRoot::create().unwrap();
        let file = scratch.path().join("environment.k");
        let before = EnvironmentDocument::from_json(document().to_string().as_bytes()).unwrap();
        std::fs::write(&file, before.to_kcl()).unwrap();
        let a1 = format!("sha256:{}", "e".repeat(64));
        let b1 = format!("sha256:{}", "f".repeat(64));
        let mut a = before.clone();
        a.select_release("wms", SelectionChange::Deploy(&a1))
            .unwrap();
        let mut b = before.clone();
        b.select_release("receiving", SelectionChange::Deploy(&b1))
            .unwrap();
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                persist_selection(&file, &before, &a, "wms").unwrap();
            });
            let second = scope.spawn(|| {
                barrier.wait();
                persist_selection(&file, &before, &b, "receiving").unwrap();
            });
            first.join().unwrap();
            second.join().unwrap();
        });
        a.applications.insert(
            "receiving".into(),
            b.applications.remove("receiving").unwrap(),
        );
        assert_eq!(EnvironmentDocument::compile(&file).unwrap(), a);
        assert!(persist_selection(&file, &before, &before, "wms").is_err());
        assert_eq!(EnvironmentDocument::compile(&file).unwrap(), a);
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

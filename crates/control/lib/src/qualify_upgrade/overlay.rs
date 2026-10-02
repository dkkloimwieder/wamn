//! Validate immutable overlay re-pins and unchanged consumed base contracts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use anyhow::{Context as _, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wamn_catalog::ServingManifest;
use wamn_schema_generator::{BaseDependencyRequirement, PackageManifest};

use super::{PackageIdentity, PresentedPackage};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OverlayEvidence {
    pub(crate) component_digest: String,
    pub(crate) overlays: Vec<OverlayPinEvidence>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OverlayPinEvidence {
    pub(crate) predecessor: PackageIdentity,
    pub(crate) candidate: PackageIdentity,
    pub(crate) base_alias: String,
    pub(crate) predecessor_pin: BaseDependencyRequirement,
    pub(crate) candidate_pin: BaseDependencyRequirement,
    pub(crate) consumed_contracts: BTreeMap<String, String>,
}

/// Check every installed overlay before the coordinated application starts.
pub(super) fn validate_transition(
    predecessor_base: &PresentedPackage,
    candidate_base: &PresentedPackage,
    predecessors: &[PresentedPackage],
    candidates: &[PresentedPackage],
    serving: &ServingManifest,
    component_path: &Path,
) -> anyhow::Result<OverlayEvidence> {
    let component = fs::read(component_path).context("read the exact candidate base component")?;
    let digest = wamn_engine::component_admission::component_digest(&component);
    let base = &predecessor_base.manifest.package;
    ensure!(
        candidate_base.manifest.package.id == base.id,
        "overlay upgrade changes the base package identity"
    );
    let mut evidence = Vec::new();
    for predecessor in predecessors {
        for (alias, pin) in &predecessor.manifest.base_dependencies {
            if pin.package != base.id {
                continue;
            }
            ensure!(
                pin.version == base.version,
                "installed overlay {} does not pin the installed base predecessor",
                predecessor.identity.package_id
            );
            ensure!(
                serving
                    .release
                    .packages
                    .iter()
                    .any(
                        |package| package.package_id() == predecessor.identity.package_id
                            && package.package_version() == predecessor.identity.package_version
                    ),
                "affected overlay {} is not the selected serving predecessor coordinate",
                predecessor.identity.package_id
            );
            let candidate = candidates
                .iter()
                .find(|package| package.identity.package_id == predecessor.identity.package_id)
                .with_context(|| {
                    format!(
                        "successor roots omit affected installed overlay {}",
                        predecessor.identity.package_id
                    )
                })?;
            let candidate_pin = candidate
                .manifest
                .base_dependencies
                .get(alias)
                .context("overlay successor removes its base alias")?;
            validate_repin(
                &predecessor.manifest,
                &candidate.manifest,
                alias,
                &candidate_base.manifest.package.version,
                &digest,
            )?;
            ensure!(
                predecessor.identity.migrations == candidate.identity.migrations,
                "overlay successor changes its immutable migration stream"
            );
            let mut contracts = BTreeMap::new();
            for operation in &pin.operations {
                let sealed = wamn_schema_generator::canonical_operation_identity(base, operation)?;
                ensure!(
                    serving.components.iter().any(|component| {
                        component.package_id == base.id
                            && component.digest.as_str() == pin.digest
                            && component.operations.values().any(|export| {
                                export.registered_operation.as_deref() == Some(sealed.as_str())
                            })
                    }),
                    "overlay predecessor pin or consumed operation is absent from the serving release"
                );
                let original = consumed_contract(
                    &predecessor_base.root,
                    &predecessor_base.manifest,
                    operation,
                )?;
                let successor =
                    consumed_contract(&candidate_base.root, &candidate_base.manifest, operation)?;
                ensure!(
                    original == successor,
                    "consumed base operation {operation} changes its contract; incompatible transitions belong to Epic 3"
                );
                contracts.insert(
                    operation.clone(),
                    wamn_execution_contract::canonical_json_sha256(&original),
                );
            }
            evidence.push(OverlayPinEvidence {
                predecessor: predecessor.identity.clone(),
                candidate: candidate.identity.clone(),
                base_alias: alias.clone(),
                predecessor_pin: pin.clone(),
                candidate_pin: candidate_pin.clone(),
                consumed_contracts: contracts,
            });
        }
    }
    ensure!(
        !evidence.is_empty(),
        "coordinated upgrade has no installed overlays of the base"
    );
    evidence.sort_by(|left, right| {
        left.predecessor
            .package_id
            .cmp(&right.predecessor.package_id)
    });
    let affected = evidence
        .iter()
        .map(|overlay| overlay.predecessor.package_id.as_str())
        .collect::<BTreeSet<_>>();
    for candidate in candidates {
        if candidate.identity.package_id != base.id
            && candidate
                .manifest
                .base_dependencies
                .values()
                .any(|pin| pin.package == base.id)
        {
            ensure!(
                affected.contains(candidate.identity.package_id.as_str()),
                "successor roots introduce a new overlay during a base upgrade"
            );
        }
    }
    let evidence = OverlayEvidence {
        component_digest: digest,
        overlays: evidence,
    };
    validate_evidence(
        &evidence,
        &predecessor_base.identity,
        &candidate_base.identity,
    )?;
    Ok(evidence)
}

/// Refuse inconsistent pin metadata when accepted evidence is loaded again.
pub(crate) fn validate_evidence(
    evidence: &OverlayEvidence,
    base_predecessor: &PackageIdentity,
    base_candidate: &PackageIdentity,
) -> anyhow::Result<()> {
    let digest = evidence
        .component_digest
        .strip_prefix("sha256:")
        .context("candidate base component digest is not SHA-256")?;
    ensure!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "candidate base component digest is not canonical SHA-256"
    );
    ensure!(
        !evidence.overlays.is_empty(),
        "coordinated upgrade evidence omits all affected overlays"
    );
    let mut previous: Option<&str> = None;
    for overlay in &evidence.overlays {
        let predecessor = &overlay.predecessor;
        let candidate = &overlay.candidate;
        ensure!(
            previous.is_none_or(|id| id < predecessor.package_id.as_str()),
            "overlay evidence is not uniquely sorted"
        );
        previous = Some(&predecessor.package_id);
        ensure!(
            predecessor.package_id != base_predecessor.package_id
                && candidate.package_id == predecessor.package_id
                && candidate.package_version != predecessor.package_version
                && candidate.predecessor_version.as_deref()
                    == Some(predecessor.package_version.as_str())
                && candidate.migrations == predecessor.migrations,
            "overlay evidence changes migrations or lacks its direct successor"
        );
        let original = &overlay.predecessor_pin;
        let successor = &overlay.candidate_pin;
        ensure!(
            original.package == base_predecessor.package_id
                && original.version == base_predecessor.package_version
                && successor.package == base_candidate.package_id
                && successor.version == base_candidate.package_version
                && successor.digest == evidence.component_digest
                && original.operations == successor.operations,
            "overlay evidence does not bind the exact base transition"
        );
        let operations = original
            .operations
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        ensure!(
            !operations.is_empty()
                && operations.len() == original.operations.len()
                && operations
                    == overlay
                        .consumed_contracts
                        .keys()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>(),
            "overlay evidence omits or repeats consumed operation contracts"
        );
        for digest in overlay.consumed_contracts.values() {
            let digest = digest
                .strip_prefix("sha256:")
                .context("consumed contract digest is not SHA-256")?;
            ensure!(
                digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "consumed contract digest is not canonical SHA-256"
            );
        }
    }
    Ok(())
}

fn validate_repin(
    predecessor: &PackageManifest,
    candidate: &PackageManifest,
    alias: &str,
    base_version: &str,
    component_digest: &str,
) -> anyhow::Result<()> {
    ensure!(
        candidate.package.id == predecessor.package.id
            && candidate.package.version != predecessor.package.version
            && candidate.package.predecessor_version.as_deref()
                == Some(predecessor.package.version.as_str()),
        "overlay successor must name its installed immediate predecessor"
    );
    let mut expected = predecessor.clone();
    expected.package = candidate.package.clone();
    let pin = expected
        .base_dependencies
        .get_mut(alias)
        .context("predecessor overlay has no affected base alias")?;
    base_version.clone_into(&mut pin.version);
    component_digest.clone_into(&mut pin.digest);
    ensure!(
        expected == *candidate,
        "overlay successor must change only its package coordinate and affected base version/digest"
    );
    Ok(())
}

fn consumed_contract(
    root: &Path,
    manifest: &PackageManifest,
    operation: &str,
) -> anyhow::Result<Value> {
    let (module, name) = operation
        .split_once('.')
        .context("consumed operation has no module")?;
    ensure!(
        manifest.custom_operations.contains_key(operation)
            || manifest.models.get(module).is_some_and(|model| model
                .operations
                .keys()
                .any(|action| action.as_str() == name)),
        "consumed operation {operation} is removed; this incompatible transition belongs to Epic 3"
    );
    let mut contract = serde_json::Map::new();
    for kind in ["operation", "input", "result", "errors"] {
        let path = root.join(format!("generated/contracts/{module}/{name}.{kind}.json"));
        // An operation without a result declaration has no result contract.
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if kind == "result" && error.kind() == std::io::ErrorKind::NotFound => {
                continue;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "read consumed contract {}; removed or changed consumed contracts belong to Epic 3",
                        path.display()
                    )
                });
            }
        };
        let mut value: Value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse consumed contract {}", path.display()))?;
        if kind == "operation" {
            normalize_operation_contract(&mut value, &manifest.package, operation)?;
        }
        contract.insert(kind.to_owned(), value);
    }
    // The typed callback input belongs to the authored base declaration rather
    // than the generated operation contract. It must remain unchanged too.
    if let Some(custom) = manifest.custom_operations.get(operation) {
        contract.insert(
            "pre_commit_input".to_owned(),
            serde_json::to_value(&custom.pre_commit)?,
        );
    }
    Ok(Value::Object(contract))
}

fn normalize_operation_contract(
    contract: &mut Value,
    package: &wamn_schema_generator::PackageIdentity,
    operation: &str,
) -> anyhow::Result<()> {
    let sealed = wamn_schema_generator::canonical_operation_identity(package, operation)?;
    let (unsealed, version) = wamn_catalog::split_sealed_operation(&sealed)
        .context("canonical consumed operation has no version")?;
    let object = contract
        .as_object_mut()
        .context("operation contract must be an object")?;
    ensure!(
        object.get("operation").and_then(Value::as_str) == Some(sealed.as_str()),
        "consumed operation contract names another package coordinate"
    );
    for field in ["operation", "grant"] {
        if let Some(value) = object.get_mut(field) {
            // Private operation contracts explicitly carry a null grant.
            if field == "grant" && value.is_null() {
                continue;
            }
            ensure!(
                value.as_str() == Some(sealed.as_str()),
                "consumed operation {field} has another identity"
            );
            *value = json!(unsealed);
        }
    }
    if let Some(value) = object.get_mut("pre_commit") {
        ensure!(
            value.as_str() == Some(format!("{unsealed}-pre-commit@{version}").as_str()),
            "consumed callback has another identity"
        );
        *value = json!(format!("{unsealed}-pre-commit"));
    }
    if let Some(statements) = object.get_mut("statements") {
        for statement in statements
            .as_array_mut()
            .context("consumed statement contracts must be an array")?
        {
            let statement = statement
                .as_object_mut()
                .context("statement contract must be an object")?;
            // SQL bytes and their storage paths are implementation identities.
            // Bind/result shapes, statement names and transaction facts remain.
            statement.remove("digest");
            statement.remove("path");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(version: &str) -> wamn_schema_generator::PackageIdentity {
        wamn_schema_generator::PackageIdentity {
            id: "wamn_receiving".to_owned(),
            version: version.to_owned(),
            predecessor_version: None,
        }
    }

    fn contract(version: &str) -> Value {
        json!({"operation": format!("wamn-receiving:receiving/record-receipt@{version}"), "grant": format!("wamn-receiving:receiving/record-receipt@{version}"), "pre_commit": format!("wamn-receiving:receiving/record-receipt-pre-commit@{version}"), "idempotency": {"log": {"operation": "wamn-receiving:receiving/record-receipt"}}, "statements": [{"name":"write", "path":"old.sql", "digest":"old", "binds":[{"type":"uuid"}], "columns":[], "transactional":true}]})
    }

    fn identity(id: &str, version: &str, predecessor: Option<&str>) -> PackageIdentity {
        PackageIdentity {
            package_id: id.to_owned(),
            package_version: version.to_owned(),
            predecessor_version: predecessor.map(str::to_owned),
            manifest_sha256: format!("sha256:{}", "a".repeat(64)),
            migrations: vec![],
        }
    }

    fn evidence() -> OverlayEvidence {
        let digest = format!("sha256:{}", "b".repeat(64));
        let original = BaseDependencyRequirement {
            package: "base".to_owned(),
            version: "2.1.0".to_owned(),
            digest: format!("sha256:{}", "a".repeat(64)),
            operations: vec!["widget.archive".to_owned()],
        };
        let successor = BaseDependencyRequirement {
            version: "2.2.0".to_owned(),
            digest: digest.clone(),
            ..original.clone()
        };
        OverlayEvidence {
            component_digest: digest,
            overlays: vec![OverlayPinEvidence {
                predecessor: identity("overlay", "2.1.0", None),
                candidate: identity("overlay", "2.2.0", Some("2.1.0")),
                base_alias: "base_fixture".to_owned(),
                predecessor_pin: original,
                candidate_pin: successor,
                consumed_contracts: BTreeMap::from([(
                    "widget.archive".to_owned(),
                    format!("sha256:{}", "c".repeat(64)),
                )]),
            }],
        }
    }

    #[test]
    fn persisted_pin_evidence_requires_the_exact_complete_transition() {
        let old = identity("base", "2.1.0", None);
        let new = identity("base", "2.2.0", Some("2.1.0"));
        let accepted = evidence();
        validate_evidence(&accepted, &old, &new).unwrap();
        let mut missing = accepted.clone();
        missing.overlays[0].consumed_contracts.clear();
        assert!(validate_evidence(&missing, &old, &new).is_err());
        let mut wrong_pin = accepted.clone();
        wrong_pin.overlays[0].candidate_pin.digest = format!("sha256:{}", "d".repeat(64));
        assert!(validate_evidence(&wrong_pin, &old, &new).is_err());
        let mut duplicate = accepted.clone();
        duplicate.overlays.push(accepted.overlays[0].clone());
        assert!(validate_evidence(&duplicate, &old, &new).is_err());
        let mut migration = accepted;
        migration.overlays[0]
            .candidate
            .migrations
            .push(super::super::MigrationIdentity {
                ordinal: 1,
                relative_path: "migrations/new.sql".to_owned(),
                sha256: "changed".to_owned(),
            });
        assert!(validate_evidence(&migration, &old, &new).is_err());
    }

    #[test]
    fn coordinate_and_sql_implementation_changes_preserve_consumed_contract() {
        let mut old = contract("2.1.0");
        let mut new = contract("2.2.0");
        new["statements"][0]["digest"] = json!("new");
        new["statements"][0]["path"] = json!("new.sql");
        normalize_operation_contract(&mut old, &package("2.1.0"), "receiving.record_receipt")
            .unwrap();
        normalize_operation_contract(&mut new, &package("2.2.0"), "receiving.record_receipt")
            .unwrap();
        assert_eq!(old, new);
        assert_eq!(
            old["idempotency"]["log"]["operation"],
            "wamn-receiving:receiving/record-receipt"
        );
    }

    #[test]
    fn changed_bind_shape_remains_a_contract_change() {
        let mut old = contract("2.1.0");
        let mut new = contract("2.2.0");
        new["statements"][0]["binds"][0]["type"] = json!("text");
        normalize_operation_contract(&mut old, &package("2.1.0"), "receiving.record_receipt")
            .unwrap();
        normalize_operation_contract(&mut new, &package("2.2.0"), "receiving.record_receipt")
            .unwrap();
        assert_ne!(old, new);
    }

    #[test]
    fn refuses_wrong_identity_and_preserves_statement_classification() {
        let mut wrong = contract("2.1.0");
        assert!(
            normalize_operation_contract(&mut wrong, &package("2.2.0"), "receiving.record_receipt")
                .is_err()
        );
        let mut nontransactional = contract("2.1.0");
        nontransactional["statements"][0]["transactional"] = json!(false);
        normalize_operation_contract(
            &mut nontransactional,
            &package("2.1.0"),
            "receiving.record_receipt",
        )
        .unwrap();
        let mut same = contract("2.2.0");
        same["statements"][0]["transactional"] = json!(false);
        normalize_operation_contract(&mut same, &package("2.2.0"), "receiving.record_receipt")
            .unwrap();
        assert_eq!(nontransactional, same);
        let mut transactional = contract("2.2.0");
        normalize_operation_contract(
            &mut transactional,
            &package("2.2.0"),
            "receiving.record_receipt",
        )
        .unwrap();
        assert_ne!(nontransactional, transactional);
    }

    #[test]
    fn overlay_repin_refuses_authoring_and_dependency_changes() {
        let original = PackageManifest::from_slice(include_bytes!(
            "../../../../../apps/platform_fixture_overlay/generated/wamn.json"
        ))
        .unwrap();
        let mut candidate = original.clone();
        candidate.package.version = "2.2.0".to_owned();
        candidate.package.predecessor_version = Some(original.package.version.clone());
        candidate
            .base_dependencies
            .get_mut("base_fixture")
            .unwrap()
            .version = "2.2.0".to_owned();
        candidate
            .base_dependencies
            .get_mut("base_fixture")
            .unwrap()
            .digest = "sha256:new".to_owned();
        validate_repin(&original, &candidate, "base_fixture", "2.2.0", "sha256:new").unwrap();
        candidate
            .base_dependencies
            .get_mut("base_fixture")
            .unwrap()
            .operations
            .push("widget.get".to_owned());
        assert!(
            validate_repin(&original, &candidate, "base_fixture", "2.2.0", "sha256:new").is_err()
        );
        candidate
            .base_dependencies
            .get_mut("base_fixture")
            .unwrap()
            .operations
            .pop();
        candidate.routes.path_prefix = Some("/changed".to_owned());
        assert!(
            validate_repin(&original, &candidate, "base_fixture", "2.2.0", "sha256:new").is_err()
        );
    }
}

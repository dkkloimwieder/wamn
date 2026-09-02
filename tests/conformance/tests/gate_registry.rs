//! Canonical semantic gate-registry conformance.
//!
//! Commands, artifact inputs, and dependencies remain authoritative in the live
//! Job manifests and recipe directives. This test derives them instead of
//! accepting a second hand-maintained copy in the registry.

use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const REGISTRY_PATH: &str = "architecture/gate-registry.json";
const GATE_DIRECTORY: &str = "deploy/gates";
const EVIDENCE_FOLLOW_UP: &str = "bd:wamn-2jdm.8";
const SCHEDULING_FOLLOW_UP: &str = "bd:wamn-2jdm.8";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema_version: String,
    // Prose about where gate governance is written down. Retained so the
    // registry keeps parsing under deny_unknown_fields; the assertion that
    // compared it to a copy of that prose was deleted with the doc greps.
    #[serde(rename = "authority")]
    _authority: String,
    registry_owner: String,
    machine_evidence_policy: String,
    decisions: Vec<Decision>,
    excluded_decisions: Vec<ExcludedDecision>,
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    id: String,
    status: String,
    // Retained so the registry keeps parsing under `deny_unknown_fields`. The
    // assertion that resolved it against docs/archive/PLAN/PLAN.md was deleted
    // with that document: a test that greps prose cannot fail when the code
    // breaks, and does fail when someone edits a sentence.
    #[serde(rename = "plan_anchor")]
    _plan_anchor: String,
    primary_source: SourceIdentity,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExcludedDecision {
    id: String,
    status: String,
    #[serde(rename = "plan_anchor")]
    _plan_anchor: String,
    reason: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceIdentity {
    source_kind: SourceKind,
    source: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    source_kind: SourceKind,
    source: String,
    classification: Classification,
    decision_ids: Vec<String>,
    bead_owner: String,
    proof_tier: String,
    expected_outcome: String,
    can_fail: bool,
    allowed_skips: Vec<String>,
    coverage_exclusions: Vec<String>,
    cadence: String,
    latest_evidence: LatestEvidence,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum SourceKind {
    Manifest,
    Recipe,
    Retired,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum Classification {
    RequiredGate,
    Measurement,
    Drill,
    PartialProof,
    // A gate whose subject still exists but whose production input path is
    // dormant, so passing proves nothing about production. Distinct from
    // `Retired` (the subject is gone) and from `PartialProof` (the gate really
    // does prove part of its claim). The gate stays registered, keeps its
    // decision mapping, and must record why in `coverage_exclusions`.
    ParkedGate,
    Setup,
    NonGate,
    Retired,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LatestEvidence {
    issue: String,
    git: String,
    machine_evidence: String,
    evidence_follow_up: String,
    scheduling_follow_up: String,
}

#[derive(Debug)]
struct DerivedJob {
    images: BTreeSet<String>,
    invocations: Vec<String>,
    dependencies: BTreeSet<String>,
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance package must live at tests/conformance")
        .to_path_buf()
}

fn load_registry(root: &Path) -> Registry {
    let path = root.join(REGISTRY_PATH);
    let bytes = fs::read(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let raw: Value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("invalid {} JSON: {error}", path.display()));
    reject_derivable_registry_fields(&raw, "$");
    serde_json::from_value(raw)
        .unwrap_or_else(|error| panic!("invalid {} schema: {error}", path.display()))
}

fn reject_derivable_registry_fields(value: &Value, location: &str) {
    match value {
        Value::Object(fields) => {
            for (name, nested) in fields {
                assert!(
                    !matches!(
                        name.as_str(),
                        "command"
                            | "commands"
                            | "job"
                            | "image"
                            | "images"
                            | "artifact_input"
                            | "artifact_inputs"
                            | "dependencies"
                            | "required_dependencies"
                    ),
                    "{location}.{name} duplicates data derivable from a manifest or recipe"
                );
                reject_derivable_registry_fields(nested, &format!("{location}.{name}"));
            }
        }
        Value::Array(values) => {
            for (index, nested) in values.iter().enumerate() {
                reject_derivable_registry_fields(nested, &format!("{location}[{index}]"));
            }
        }
        _ => {}
    }
}

fn discover_manifests(root: &Path) -> BTreeSet<String> {
    let directory = root.join(GATE_DIRECTORY);
    fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        .map(|entry| {
            let entry = entry.expect("gate directory entry must be readable");
            let name = entry.file_name().to_string_lossy().into_owned();
            (entry.path(), name)
        })
        .filter(|(path, name)| path.is_file() && name.ends_with("-job.yaml"))
        .map(|(_, name)| format!("{GATE_DIRECTORY}/{name}"))
        .collect()
}

fn derive_job(path: &Path) -> Result<DerivedJob, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let job_document = source
        .split("\n---")
        .find(|document| document.lines().any(|line| line.trim() == "kind: Job"))
        .ok_or_else(|| format!("{} has no Job document", path.display()))?;
    if !job_document
        .lines()
        .any(|line| line.trim() == "apiVersion: batch/v1")
    {
        return Err(format!("{} Job is not batch/v1", path.display()));
    }
    if !job_document.lines().any(|line| line.trim() == "metadata:") {
        return Err(format!("{} Job has no metadata", path.display()));
    }

    let images: BTreeSet<_> = job_document
        .lines()
        .filter_map(|line| line.trim().strip_prefix("image:"))
        .map(str::trim)
        .filter(|image| !image.is_empty())
        .map(ToOwned::to_owned)
        .collect();
    if images.is_empty() {
        return Err(format!("{} Job has no container image", path.display()));
    }

    let invocations: Vec<_> = job_document
        .lines()
        .filter(|line| {
            let line = line.trim();
            line.starts_with("command:") || line.starts_with("args:")
        })
        .map(str::trim)
        .map(ToOwned::to_owned)
        .collect();
    if invocations.is_empty() {
        return Err(format!("{} Job has no command or args", path.display()));
    }

    let dependencies = job_document
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            line.strip_prefix("secretName:")
                .or_else(|| line.strip_prefix("configMapName:"))
                .or_else(|| line.strip_prefix("serviceAccountName:"))
        })
        .map(str::trim)
        .filter(|dependency| !dependency.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    Ok(DerivedJob {
        images,
        invocations,
        dependencies,
    })
}

fn validate_registry(
    registry: &Registry,
    manifests: &BTreeSet<String>,
    root: &Path,
) -> Result<(), String> {
    if registry.schema_version != "0.1" {
        return Err(format!(
            "unsupported registry schema version {}",
            registry.schema_version
        ));
    }
    if registry.registry_owner != "bd:wamn-2jdm.2"
        || !registry
            .machine_evidence_policy
            .contains("tools/kubernetes-gate-run")
        || !registry
            .machine_evidence_policy
            .contains("wamn-kubernetes-gate-verdict/v0.1")
        || !registry.machine_evidence_policy.contains("wamn-2jdm.8")
        || !registry.machine_evidence_policy.contains("wamn-2jkm.98")
        || !registry.machine_evidence_policy.contains("wamn-4tob.6.25")
    {
        return Err("registry authority and pending evidence policy must be explicit".to_string());
    }

    let expected_decisions: BTreeSet<_> = (1..=24).map(|number| format!("D{number}")).collect();
    let mut inventoried_decisions = BTreeSet::new();
    let mut owned_decisions = BTreeMap::new();
    for decision in &registry.decisions {
        if !inventoried_decisions.insert(decision.id.clone()) {
            return Err(format!(
                "duplicate canonical decision owner for {}",
                decision.id
            ));
        }
        if !matches!(decision.status.as_str(), "shipped" | "retired") {
            return Err(format!(
                "{} has invalid owned status {}",
                decision.id, decision.status
            ));
        }
        owned_decisions.insert(decision.id.clone(), decision);
    }
    for decision in &registry.excluded_decisions {
        if !inventoried_decisions.insert(decision.id.clone()) {
            return Err(format!(
                "duplicate canonical decision owner for {}",
                decision.id
            ));
        }
        if !matches!(decision.status.as_str(), "open" | "planned-not-shipped")
            || decision.reason.trim().is_empty()
        {
            return Err(format!("{} has an invalid decision exclusion", decision.id));
        }
    }
    if inventoried_decisions != expected_decisions {
        let missing: Vec<_> = expected_decisions
            .difference(&inventoried_decisions)
            .collect();
        let unknown: Vec<_> = inventoried_decisions
            .difference(&expected_decisions)
            .collect();
        return Err(format!(
            "decision inventory drift; missing={missing:?}, unknown={unknown:?}"
        ));
    }

    let mut sources = BTreeSet::new();
    let mut decision_supporters = BTreeMap::<String, BTreeSet<String>>::new();
    let mut registered_manifests = BTreeSet::new();

    for entry in &registry.entries {
        let source_key = format!("{:?}:{}", entry.source_kind, entry.source);
        if !sources.insert(source_key.clone()) {
            return Err(format!("duplicate source ownership for {}", entry.source));
        }
        if entry.bead_owner.strip_prefix("bd:").is_none() {
            return Err(format!(
                "{} has a mutable or malformed bead owner",
                entry.source
            ));
        }
        if entry.proof_tier.is_empty()
            || entry.expected_outcome.is_empty()
            || entry.cadence.is_empty()
        {
            return Err(format!("{} has incomplete semantics", entry.source));
        }
        if entry
            .allowed_skips
            .iter()
            .chain(entry.coverage_exclusions.iter())
            .any(|item| item.trim().is_empty())
        {
            return Err(format!("{} has an empty skip or exclusion", entry.source));
        }

        let backs_live_decision = !matches!(
            entry.classification,
            Classification::Setup | Classification::NonGate | Classification::Retired
        );
        if backs_live_decision && entry.decision_ids.is_empty() {
            return Err(format!("{} has no shipped-decision mapping", entry.source));
        }
        if matches!(
            entry.classification,
            Classification::Setup | Classification::NonGate
        ) && !entry.decision_ids.is_empty()
        {
            return Err(format!(
                "{} is {:?} but claims a shipped decision",
                entry.source, entry.classification
            ));
        }

        let mut entry_decisions = BTreeSet::new();
        for decision_id in &entry.decision_ids {
            if !entry_decisions.insert(decision_id) {
                return Err(format!(
                    "{} duplicates decision mapping {decision_id}",
                    entry.source
                ));
            }
            let decision = owned_decisions.get(decision_id).ok_or_else(|| {
                format!(
                    "{} maps excluded or unknown decision {decision_id}",
                    entry.source
                )
            })?;
            let historical_primary = entry.classification == Classification::Retired
                && decision.status == "shipped"
                && decision.primary_source.source_kind == SourceKind::Retired
                && decision.primary_source.source == entry.source;
            let compatible = historical_primary
                || matches!(
                    (entry.classification, decision.status.as_str()),
                    (Classification::Retired, "retired")
                )
                || (entry.classification != Classification::Retired
                    && decision.status == "shipped");
            if !compatible {
                return Err(format!(
                    "{} classification and {} status disagree",
                    entry.source, decision.id
                ));
            }
            decision_supporters
                .entry(decision_id.clone())
                .or_default()
                .insert(source_key.clone());
        }

        if entry.classification == Classification::RequiredGate && !entry.can_fail {
            return Err(format!("{} is a decorative required gate", entry.source));
        }
        // Parking withdraws a coverage claim. Withdrawing it silently is the
        // failure being parked was supposed to expose, so the note is required.
        if entry.classification == Classification::ParkedGate
            && entry.coverage_exclusions.is_empty()
        {
            return Err(format!(
                "{} is parked without a recorded coverage exclusion",
                entry.source
            ));
        }
        if matches!(
            entry.classification,
            Classification::Setup | Classification::NonGate | Classification::Retired
        ) && entry.can_fail
        {
            return Err(format!(
                "{} cannot carry a verdict as {:?}",
                entry.source, entry.classification
            ));
        }

        if entry.latest_evidence.issue.strip_prefix("bd:").is_none() {
            return Err(format!("{} lacks immutable bead evidence", entry.source));
        }
        let git = entry
            .latest_evidence
            .git
            .strip_prefix("git:")
            .ok_or_else(|| format!("{} lacks immutable git evidence", entry.source))?;
        if git.len() != 40 || !git.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("{} has malformed git evidence", entry.source));
        }
        if entry.latest_evidence.machine_evidence != "missing"
            || entry.latest_evidence.evidence_follow_up != EVIDENCE_FOLLOW_UP
            || entry.latest_evidence.scheduling_follow_up != SCHEDULING_FOLLOW_UP
        {
            return Err(format!(
                "{} must explicitly defer machine evidence and scheduling",
                entry.source
            ));
        }

        if entry.classification == Classification::Retired {
            if entry.source_kind != SourceKind::Retired || root.join(&entry.source).exists() {
                return Err(format!("retired gate {} is still live", entry.source));
            }
        } else {
            if entry.source_kind == SourceKind::Retired {
                return Err(format!(
                    "live entry {} uses retired source kind",
                    entry.source
                ));
            }
        }

        match entry.source_kind {
            SourceKind::Manifest => {
                registered_manifests.insert(entry.source.clone());
                let derived = derive_job(&root.join(&entry.source))?;
                if derived.images.is_empty() || derived.invocations.is_empty() {
                    return Err(format!("{} has no derivable execution", entry.source));
                }
                let _ = derived.dependencies;
            }
            // A recipe entry named a `# recipe-test:` directive in
            // docs/archive/build-and-test.md. That document is deleted and its
            // selectors were prose, so nothing real remains to resolve them
            // against; the entry's classification, evidence, and decision
            // mapping above are still checked.
            SourceKind::Recipe => {}
            SourceKind::Retired => {}
        }
    }

    if &registered_manifests != manifests {
        let missing: Vec<_> = manifests.difference(&registered_manifests).collect();
        let stale: Vec<_> = registered_manifests.difference(manifests).collect();
        return Err(format!(
            "manifest registry drift; unregistered={missing:?}, stale={stale:?}"
        ));
    }
    for decision in &registry.decisions {
        if decision.status == "shipped"
            && decision.primary_source.source_kind == SourceKind::Retired
            && !registry.entries.iter().any(|entry| {
                entry.classification != Classification::Retired
                    && entry.decision_ids.contains(&decision.id)
            })
        {
            return Err(format!(
                "{} has a retired historical primary but no live supporting gate mapping",
                decision.id
            ));
        }
        // The guard above stops retirement from silently unproving a shipped
        // decision. Parking is retirement's softer form and needs the same
        // guard, or it becomes the loophole around it.
        if decision.status == "shipped"
            && !registry.entries.iter().any(|entry| {
                !matches!(
                    entry.classification,
                    Classification::ParkedGate | Classification::Retired
                ) && entry.decision_ids.contains(&decision.id)
            })
        {
            return Err(format!(
                "{} is supported only by parked or retired gates",
                decision.id
            ));
        }
        let primary_key = format!(
            "{:?}:{}",
            decision.primary_source.source_kind, decision.primary_source.source
        );
        let supporters = decision_supporters
            .get(&decision.id)
            .ok_or_else(|| format!("{} has no supporting gate mapping", decision.id))?;
        if !supporters.contains(&primary_key) {
            return Err(format!(
                "{} primary owner {} is not a supporting gate",
                decision.id, decision.primary_source.source
            ));
        }
    }

    Ok(())
}

fn fixtures() -> (PathBuf, Registry, BTreeSet<String>) {
    let root = repository_root();
    let registry = load_registry(&root);
    let manifests = discover_manifests(&root);
    (root, registry, manifests)
}

#[test]
fn canonical_registry_covers_every_live_gate_source() {
    let (root, registry, manifests) = fixtures();
    // m1-gate-job.yaml, socketguard-job.yaml, traceproof-job.yaml. The literal
    // is a tripwire on the inventory's size; `validate_registry` separately
    // proves the registry and the directory name the same Jobs.
    assert_eq!(manifests.len(), 3, "the retained Job inventory changed");
    validate_registry(&registry, &manifests, &root)
    .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn d24_is_retained_by_the_api_publish_recipe() {
    let (_, registry, _) = fixtures();
    let decision = registry
        .decisions
        .iter()
        .find(|decision| decision.id == "D24")
        .expect("D24 remains inventoried");
    assert_eq!(decision.status, "shipped");
    assert_eq!(decision.primary_source.source_kind, SourceKind::Recipe);
    assert_eq!(decision.primary_source.source, "H5-API-PUBLISH");

    let recipe = registry
        .entries
        .iter()
        .find(|entry| entry.source == "H5-API-PUBLISH")
        .expect("H5-API-PUBLISH remains registered");
    assert!(recipe.decision_ids.iter().any(|id| id == "D24"));
}

#[test]
fn m1_gate_claims_completed_checks_9_and_10() {
    let (root, registry, _) = fixtures();
    let entry = registry
        .entries
        .iter()
        .find(|entry| entry.source == "deploy/gates/m1-gate-job.yaml")
        .expect("M1 gate must be registered");
    assert_eq!(entry.bead_owner, "bd:wamn-0h0g.11.10");
    assert_eq!(entry.expected_outcome, "pass-checks-9-and-10");
    assert!(entry.coverage_exclusions.is_empty());

    let manifest = fs::read_to_string(root.join(&entry.source)).expect("read M1 manifest");
    let sidecar = fs::read_to_string(root.join("deploy/gates/m1-postgres.Dockerfile"))
        .expect("read derived M1 PostgreSQL recipe");
    assert!(manifest.contains("wamn.dev/m1-checks: \"9,10\""));
    assert!(!manifest.contains("m1-pending-checks"));
    assert!(manifest.contains("M1 PASS — checks 9 and 10 passed"));
    assert!(manifest.contains("wamn-gates --log-level error m1 --timeout-secs 120"));
    assert!(manifest.contains("restartPolicy: Always"));
    assert!(manifest.contains("image: wamn-postgres:m1-pg18-720c455e"));
    assert!(!manifest.contains("m1-pg18-720c455e@sha256:"));
    assert!(manifest.contains("imagePullPolicy: Never"));
    assert!(manifest.contains("--sidecar-preflight-record"));
    assert!(!manifest.contains("ctr -n k8s.io images tag"));
    assert!(manifest.contains("emptyDir: {}"));
    assert!(manifest.contains("postgres://postgres@127.0.0.1:5432/postgres"));
    assert!(manifest.contains("sha256:POST_BUILD_MAIN_IMAGE_ID"));
    assert!(manifest.contains("claim_log_prefix\":\"M1_MAIN_IMAGE_ID="));
    assert!(sidecar.contains("FROM --platform=linux/amd64 postgres:18.6-trixie@sha256:ae6c78831cbc35fa3a4aaf4d763ddacf6183d6004774cc2dc28b3920410d1d1a"));
    assert!(sidecar.contains("wamn.dev/upstream-index=\"sha256:ae6c78831cbc35fa3a4aaf4d763ddacf6183d6004774cc2dc28b3920410d1d1a\""));
    assert!(sidecar.contains("wamn.dev/upstream-child=\"sha256:cd78ca58eb75f929698e117a589488ccb2bd45107247fe02400b50ff6c418324\""));
    assert!(!manifest.contains("secretKeyRef"));
    assert!(!manifest.contains("wamn-pg"));
    assert!(!manifest.contains("wamn-sysdb"));
    assert!(!manifest.contains("error causation-e2e --timeout-secs"));
}

#[test]
fn rejects_unregistered_manifest_mutant() {
    let (root, mut registry, manifests) = fixtures();
    let index = registry
        .entries
        .iter()
        .position(|entry| entry.source_kind == SourceKind::Manifest)
        .expect("registry must contain a manifest");
    registry.entries.remove(index);
    let error = validate_registry(&registry, &manifests, &root)
    .expect_err("an unregistered manifest must fail");
    assert!(error.contains("manifest registry drift"), "{error}");
}

#[test]
fn rejects_removed_decision_mapping_mutant() {
    let (root, mut registry, manifests) = fixtures();
    let entry = registry
        .entries
        .iter_mut()
        .find(|entry| !entry.decision_ids.is_empty())
        .expect("registry must contain a shipped decision");
    entry.decision_ids.clear();
    let error = validate_registry(&registry, &manifests, &root)
    .expect_err("a missing decision mapping must fail");
    assert!(error.contains("no shipped-decision mapping"), "{error}");
}

#[test]
fn rejects_duplicate_decision_ownership_mutant() {
    let (root, mut registry, manifests) = fixtures();
    let duplicate = registry.decisions[0].clone();
    registry.decisions.push(duplicate);
    let error = validate_registry(&registry, &manifests, &root)
    .expect_err("duplicate decision ownership must fail");
    assert!(
        error.contains("duplicate canonical decision owner"),
        "{error}"
    );
}

#[test]
fn rejects_retired_primary_without_live_support_mutant() {
    let (root, mut registry, manifests) = fixtures();
    for entry in &mut registry.entries {
        if entry.classification != Classification::Retired
            && entry.decision_ids.iter().any(|decision| decision == "D1")
        {
            entry.decision_ids.retain(|decision| decision != "D1");
            if entry.decision_ids.is_empty() {
                entry.decision_ids.push("D14".to_string());
            }
        }
    }
    let error = validate_registry(&registry, &manifests, &root)
    .expect_err("a retired primary without live support must fail");
    assert!(error.contains("no live supporting gate mapping"), "{error}");
}

#[test]
fn rejects_decorative_required_gate_mutant() {
    let (root, mut registry, manifests) = fixtures();
    let entry = registry
        .entries
        .iter_mut()
        .find(|entry| entry.classification == Classification::RequiredGate)
        .expect("registry must contain a required gate");
    entry.can_fail = false;
    let error = validate_registry(&registry, &manifests, &root)
    .expect_err("a decorative required gate must fail");
    assert!(error.contains("decorative required gate"), "{error}");
}

#[test]
fn causation_gates_are_required_with_complete_coverage() {
    let (_, registry, _) = fixtures();
    for source in ["H5-CAUSATION", "H5-CAUSATION-E2E"] {
        let entry = registry
            .entries
            .iter()
            .find(|entry| entry.source == source)
            .unwrap_or_else(|| panic!("{source} remains registered"));
        assert_eq!(
            entry.classification,
            Classification::RequiredGate,
            "{source} has production-fed causation and must remain required"
        );
        assert!(entry.can_fail);
        assert_eq!(
            entry
                .decision_ids
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["D3", "D19"])
        );
        assert!(entry.coverage_exclusions.is_empty());
    }
}

#[test]
fn rejects_parked_gate_without_coverage_note_mutant() {
    let (root, mut registry, manifests) = fixtures();
    let entry = registry
        .entries
        .iter_mut()
        .find(|entry| entry.classification == Classification::RequiredGate)
        .expect("registry must contain a required gate");
    entry.classification = Classification::ParkedGate;
    entry.coverage_exclusions.clear();
    let error = validate_registry(&registry, &manifests, &root)
    .expect_err("a parked gate with no coverage note must fail");
    assert!(
        error.contains("parked without a recorded coverage exclusion"),
        "{error}"
    );
}

#[test]
fn rejects_solely_parked_decision_support_mutant() {
    let (root, mut registry, manifests) = fixtures();
    for entry in &mut registry.entries {
        if entry.classification != Classification::Retired
            && entry.decision_ids.iter().any(|decision| decision == "D19")
        {
            entry.classification = Classification::ParkedGate;
            entry
                .coverage_exclusions
                .push("mutant parking note".to_string());
        }
    }
    let error = validate_registry(&registry, &manifests, &root)
    .expect_err("a decision left with only parked support must fail");
    assert!(
        error.contains("supported only by parked or retired gates"),
        "{error}"
    );
}

#[test]
fn gate_evidence_terminology_has_no_legacy_aliases() {
    let root = repository_root();
    let governed_files = [
        "architecture/gate-registry.json",
        "architecture/package-roles.json",
        "architecture/workspace-tiers.json",
        "tests/conformance/src/lib.rs",
        "tests/conformance/src/kubernetes_gate_verdict.rs",
        "tests/conformance/tests/gate_registry.rs",
        "tests/conformance/tests/kubernetes_gate_runner.rs",
        "tests/conformance/tests/workspace_tiers.rs",
        "tools/kubernetes-gate-run",
    ];
    let forbidden = [
        concat!("architecture/", "receipts"),
        concat!("machine_", "receipt"),
        concat!("receipt", "_follow_up"),
        concat!("kubernetes_gate_", "receipt"),
        concat!("gate_mutation_", "receipts"),
        concat!("--", "receipt"),
        concat!("gate ", "receipt"),
        concat!("gate-of-record ", "receipt"),
        concat!("mutation ", "receipt"),
        concat!("proof ", "receipt"),
    ];

    for relative in governed_files {
        let text = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|error| panic!("failed to read {relative}: {error}"));
        for term in forbidden {
            assert!(
                !text.to_ascii_lowercase().contains(term),
                "{relative} retains legacy gate-evidence term {term:?}"
            );
        }
    }
}

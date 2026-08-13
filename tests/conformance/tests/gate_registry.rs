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
const BUILD_AND_TEST_DOC: &str = "docs/archive/build-and-test.md";
const LIVE_GATE_AUTHORITY_DOCUMENT: &str = "docs/scope-reduction-mvp.md";
const LIVE_GATE_AUTHORITY_ANCHOR: &str = "## D · Gate-manifest disposition";
const HISTORICAL_PLAN_DOCUMENT: &str = "docs/archive/PLAN/PLAN.md";
const EXPECTED_GATE_AUTHORITY: &str = "MVP gate-manifest disposition derives from docs/scope-reduction-mvp.md Appendix D. Retained D-number and recipe metadata is historical registry compatibility only. Commands, artifact inputs, and dependencies derive from the referenced Job manifests and recipe-test directives and are intentionally absent.";
const EVIDENCE_FOLLOW_UP: &str = "bd:wamn-2jdm.8";
const SCHEDULING_FOLLOW_UP: &str = "bd:wamn-2jdm.8";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema_version: String,
    authority: String,
    registry_owner: String,
    machine_evidence_policy: String,
    mutation_campaigns: Vec<MutationCampaign>,
    decisions: Vec<Decision>,
    excluded_decisions: Vec<ExcludedDecision>,
    entries: Vec<Entry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MutationCampaign {
    bead: String,
    scope: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    id: String,
    status: String,
    plan_anchor: String,
    primary_source: SourceIdentity,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExcludedDecision {
    id: String,
    status: String,
    plan_anchor: String,
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
    mutation_evidence: MutationEvidence,
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
    Setup,
    NonGate,
    Retired,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MutationEvidence {
    status: String,
    follow_up: Option<String>,
    evidence: Option<String>,
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
struct RecipeSelector {
    proof_tier: String,
    package: String,
    target_kind: String,
    target: String,
    filter: String,
    minimum_tests: usize,
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

fn parse_recipe_selectors(document: &str) -> Result<BTreeMap<String, RecipeSelector>, String> {
    let mut selectors = BTreeMap::new();
    for (index, line) in document.lines().enumerate() {
        let Some(directive) = line.trim().strip_prefix("# recipe-test:") else {
            continue;
        };
        let fields: Vec<_> = directive.split('|').map(str::trim).collect();
        if fields.len() != 8 {
            return Err(format!(
                "recipe directive at line {} has {} fields, expected 8",
                index + 1,
                fields.len()
            ));
        }
        let id = fields[0].to_string();
        if id.is_empty() {
            return Err(format!(
                "recipe directive at line {} has an empty id",
                index + 1
            ));
        }
        let minimum_tests = fields[6].parse::<usize>().map_err(|error| {
            format!(
                "recipe {id} at line {} has invalid minimum test count: {error}",
                index + 1
            )
        })?;
        if minimum_tests == 0 {
            return Err(format!("recipe {id} declares no tests"));
        }
        let selector = RecipeSelector {
            proof_tier: fields[1].to_string(),
            package: fields[2].to_string(),
            target_kind: fields[3].to_string(),
            target: fields[4].to_string(),
            filter: fields[5].to_string(),
            minimum_tests,
        };
        if selectors.insert(id.clone(), selector).is_some() {
            return Err(format!("duplicate recipe selector {id}"));
        }
    }
    Ok(selectors)
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
    recipes: &BTreeMap<String, RecipeSelector>,
    historical_plan: &str,
    root: &Path,
) -> Result<(), String> {
    if registry.schema_version != "0.1" {
        return Err(format!(
            "unsupported registry schema version {}",
            registry.schema_version
        ));
    }
    let live_authority = fs::read_to_string(root.join(LIVE_GATE_AUTHORITY_DOCUMENT))
        .map_err(|error| format!("failed to read live gate authority: {error}"))?;
    if registry.authority != EXPECTED_GATE_AUTHORITY
        || !live_authority.contains(LIVE_GATE_AUTHORITY_ANCHOR)
        || registry.registry_owner != "bd:wamn-2jdm.2"
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
    let mutation_campaigns: BTreeSet<_> = registry
        .mutation_campaigns
        .iter()
        .map(|campaign| {
            if campaign.scope.trim().is_empty() {
                return Err(format!("{} has an empty mutation scope", campaign.bead));
            }
            Ok(campaign.bead.as_str())
        })
        .collect::<Result<_, String>>()?;
    if mutation_campaigns != BTreeSet::from(["bd:wamn-2jdm.4", "bd:wamn-2jdm.5", "bd:wamn-2jdm.6"])
    {
        return Err("mutation campaign ownership is incomplete".to_string());
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
        if !historical_plan.contains(&decision.plan_anchor) {
            return Err(format!(
                "{} no longer resolves in historical PLAN compatibility anchor {:?}",
                decision.id, decision.plan_anchor
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
            || !historical_plan.contains(&decision.plan_anchor)
        {
            return Err(format!(
                "{} has an invalid historical PLAN compatibility exclusion",
                decision.id
            ));
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
            "historical PLAN decision inventory drift; missing={missing:?}, unknown={unknown:?}"
        ));
    }

    let mut sources = BTreeSet::new();
    let mut decision_supporters = BTreeMap::<String, BTreeSet<String>>::new();
    let mut registered_manifests = BTreeSet::new();
    let mut registered_recipes = BTreeSet::new();

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
            if entry.mutation_evidence.status != "not-applicable"
                || entry.mutation_evidence.follow_up.is_some()
            {
                return Err(format!(
                    "retired gate {} has live mutation work",
                    entry.source
                ));
            }
        } else {
            if entry.source_kind == SourceKind::Retired {
                return Err(format!(
                    "live entry {} uses retired source kind",
                    entry.source
                ));
            }
            let valid_mutation_evidence = match entry.mutation_evidence.status.as_str() {
                "pending" => {
                    entry.mutation_evidence.follow_up.is_some()
                        && entry.mutation_evidence.evidence.is_none()
                }
                "proven" => {
                    entry.mutation_evidence.follow_up.is_none()
                        && entry
                            .mutation_evidence
                            .evidence
                            .as_deref()
                            .is_some_and(|evidence| root.join(evidence).is_file())
                }
                _ => false,
            };
            if !valid_mutation_evidence {
                return Err(format!(
                    "{} must carry proven mutation evidence or point to its follow-up",
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
            SourceKind::Recipe => {
                registered_recipes.insert(entry.source.clone());
                let selector = recipes
                    .get(&entry.source)
                    .ok_or_else(|| format!("broken recipe selector {}", entry.source))?;
                if selector.proof_tier.is_empty()
                    || selector.package.is_empty()
                    || selector.target_kind.is_empty()
                    || selector.target.is_empty()
                    || selector.filter.is_empty()
                    || selector.minimum_tests == 0
                {
                    return Err(format!(
                        "recipe {} has an incomplete selector",
                        entry.source
                    ));
                }
            }
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
    let documented_recipes: BTreeSet<_> = recipes.keys().cloned().collect();
    if registered_recipes != documented_recipes {
        let missing: Vec<_> = documented_recipes.difference(&registered_recipes).collect();
        let stale: Vec<_> = registered_recipes.difference(&documented_recipes).collect();
        return Err(format!(
            "recipe registry drift; unregistered={missing:?}, stale={stale:?}"
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

fn fixtures() -> (
    PathBuf,
    Registry,
    BTreeSet<String>,
    BTreeMap<String, RecipeSelector>,
    String,
) {
    let root = repository_root();
    let registry = load_registry(&root);
    let manifests = discover_manifests(&root);
    let recipe_document = fs::read_to_string(root.join(BUILD_AND_TEST_DOC))
        .expect("build-and-test document must be readable");
    let recipes =
        parse_recipe_selectors(&recipe_document).expect("recipe directives must be valid");
    let historical_plan = fs::read_to_string(root.join(HISTORICAL_PLAN_DOCUMENT))
        .expect("historical PLAN compatibility document must be readable");
    (root, registry, manifests, recipes, historical_plan)
}

#[test]
fn canonical_registry_covers_every_live_gate_source() {
    let (root, registry, manifests, recipes, historical_plan) = fixtures();
    assert_eq!(manifests.len(), 7, "the retained Job inventory changed");
    assert_eq!(recipes.len(), 40, "the documented recipe inventory changed");
    validate_registry(&registry, &manifests, &recipes, &historical_plan, &root)
        .unwrap_or_else(|error| panic!("{error}"));
}

#[test]
fn rejects_gate_authority_drift_mutant() {
    let (root, mut registry, manifests, recipes, historical_plan) = fixtures();
    registry.authority.push_str(" drift");
    let error = validate_registry(&registry, &manifests, &recipes, &historical_plan, &root)
        .expect_err("gate authority drift must fail");
    assert!(error.contains("registry authority"), "{error}");
}

#[test]
fn rejects_unregistered_manifest_mutant() {
    let (root, mut registry, manifests, recipes, historical_plan) = fixtures();
    let index = registry
        .entries
        .iter()
        .position(|entry| entry.source_kind == SourceKind::Manifest)
        .expect("registry must contain a manifest");
    registry.entries.remove(index);
    let error = validate_registry(&registry, &manifests, &recipes, &historical_plan, &root)
        .expect_err("an unregistered manifest must fail");
    assert!(error.contains("manifest registry drift"), "{error}");
}

#[test]
fn rejects_broken_recipe_selector_mutant() {
    let (root, mut registry, manifests, recipes, historical_plan) = fixtures();
    let entry = registry
        .entries
        .iter_mut()
        .find(|entry| entry.source_kind == SourceKind::Recipe)
        .expect("registry must contain a recipe");
    entry.source.push_str("-BROKEN");
    let error = validate_registry(&registry, &manifests, &recipes, &historical_plan, &root)
        .expect_err("a broken selector must fail");
    assert!(error.contains("broken recipe selector"), "{error}");
}

#[test]
fn rejects_removed_decision_mapping_mutant() {
    let (root, mut registry, manifests, recipes, historical_plan) = fixtures();
    let entry = registry
        .entries
        .iter_mut()
        .find(|entry| !entry.decision_ids.is_empty())
        .expect("registry must contain a shipped decision");
    entry.decision_ids.clear();
    let error = validate_registry(&registry, &manifests, &recipes, &historical_plan, &root)
        .expect_err("a missing decision mapping must fail");
    assert!(error.contains("no shipped-decision mapping"), "{error}");
}

#[test]
fn rejects_duplicate_decision_ownership_mutant() {
    let (root, mut registry, manifests, recipes, historical_plan) = fixtures();
    let duplicate = registry.decisions[0].clone();
    registry.decisions.push(duplicate);
    let error = validate_registry(&registry, &manifests, &recipes, &historical_plan, &root)
        .expect_err("duplicate decision ownership must fail");
    assert!(
        error.contains("duplicate canonical decision owner"),
        "{error}"
    );
}

#[test]
fn rejects_retired_primary_without_live_support_mutant() {
    let (root, mut registry, manifests, recipes, historical_plan) = fixtures();
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
    let error = validate_registry(&registry, &manifests, &recipes, &historical_plan, &root)
        .expect_err("a retired primary without live support must fail");
    assert!(error.contains("no live supporting gate mapping"), "{error}");
}

#[test]
fn rejects_decorative_required_gate_mutant() {
    let (root, mut registry, manifests, recipes, historical_plan) = fixtures();
    let entry = registry
        .entries
        .iter_mut()
        .find(|entry| entry.classification == Classification::RequiredGate)
        .expect("registry must contain a required gate");
    entry.can_fail = false;
    let error = validate_registry(&registry, &manifests, &recipes, &historical_plan, &root)
        .expect_err("a decorative required gate must fail");
    assert!(error.contains("decorative required gate"), "{error}");
}

#[test]
fn rejects_proven_mutation_without_checked_in_evidence() {
    let (root, mut registry, manifests, recipes, historical_plan) = fixtures();
    let entry = registry
        .entries
        .iter_mut()
        .find(|entry| entry.source_kind != SourceKind::Retired)
        .expect("registry must contain a live gate");
    entry.mutation_evidence.status = "proven".to_string();
    entry.mutation_evidence.follow_up = None;
    entry.mutation_evidence.evidence = None;
    let error = validate_registry(&registry, &manifests, &recipes, &historical_plan, &root)
        .expect_err("proven mutation evidence without an evidence record must fail");
    assert!(error.contains("mutation evidence"), "{error}");
}

#[test]
fn gate_evidence_terminology_has_no_legacy_aliases() {
    let root = repository_root();
    let governed_files = [
        "architecture/gate-registry.json",
        "architecture/package-roles.json",
        "architecture/workspace-tiers.json",
        "docs/archive/build-and-test.md",
        "docs/archive/findings.md",
        "tests/conformance/src/lib.rs",
        "tests/conformance/src/kubernetes_gate_verdict.rs",
        "tests/conformance/tests/gate_mutation_evidence.rs",
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

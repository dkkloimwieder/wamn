//! Conformance guard for production PostgreSQL state and static writer ownership.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

const MANIFEST_PATH: &str = "architecture/state-owners.json";
const EXTERNAL_AUTHORITIES: [&str; 6] = [
    "backup-objects-and-native-metadata",
    "jetstream-transport-state",
    "kubernetes-desired-and-observed-state",
    "oci-and-signing-artifacts",
    "secret-bytes-and-native-metadata",
    "telemetry",
];

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    scope: Scope,
    principals: BTreeMap<String, Principal>,
    canonical_sources: Vec<CanonicalSource>,
    excluded_sources: Vec<ExcludedSource>,
    scan_policy: ScanPolicy,
    objects: Vec<StateObject>,
    families: Vec<StateFamily>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    included_authority: String,
    excluded_external_authorities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Principal {
    path: String,
    role: String,
    #[serde(default)]
    temporary: Option<TemporaryWriter>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TemporaryWriter {
    expiry_bead: String,
    may_gain_writes: bool,
    writer_families: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalSource {
    path: String,
    kind: String,
    scope: String,
    #[serde(default)]
    marker: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExcludedSource {
    path: String,
    source_class: String,
    reason: String,
    expected_objects: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanPolicy {
    roots: Vec<String>,
    extensions: Vec<String>,
    rust_cfg_test_modules: String,
    exclusions: Vec<ScanExclusion>,
    unqualified_contexts: Vec<UnqualifiedContext>,
    dynamic_writers: Vec<DynamicWriter>,
    ignored_literals: Vec<IgnoredLiteral>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScanExclusion {
    kind: String,
    value: String,
    source_class: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnqualifiedContext {
    path_prefix: String,
    default_schema: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DynamicWriter {
    path: String,
    principal: String,
    family: String,
    marker: String,
    operations: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IgnoredLiteral {
    path: String,
    literal: String,
    reason: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateObject {
    id: String,
    #[serde(flatten)]
    ownership: Ownership,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StateFamily {
    id: String,
    plane: String,
    pattern: String,
    semantic_owner: String,
    migration_owners: Vec<String>,
    schema_source: String,
    writers: Vec<String>,
    readers: Vec<String>,
    compatibility_horizon: String,
    drift_gate: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ownership {
    plane: String,
    semantic_owner: String,
    migration_owners: Vec<String>,
    schema_source: String,
    writers: Vec<String>,
    readers: Vec<String>,
    compatibility_horizon: String,
    drift_gate: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Discovery {
    path: String,
    line: usize,
    operation: String,
    target: String,
}

#[derive(Clone, Debug)]
enum StateTarget<'a> {
    Object(&'a StateObject),
    Family(&'a StateFamily),
}

impl StateTarget<'_> {
    fn id(&self) -> &str {
        match self {
            Self::Object(object) => &object.id,
            Self::Family(family) => &family.id,
        }
    }

    fn writers(&self) -> &[String] {
        match self {
            Self::Object(object) => &object.ownership.writers,
            Self::Family(family) => &family.writers,
        }
    }
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("tests/conformance lives two levels below the repository root")
        .to_path_buf()
}

fn read_manifest(repository: &Path) -> Manifest {
    let path = repository.join(MANIFEST_PATH);
    let source = std::fs::read_to_string(&path).expect("read state ownership manifest");
    serde_json::from_str(&source).expect("parse state ownership manifest")
}

fn validate_manifest(repository: &Path, manifest: &Manifest) -> Result<(), String> {
    if manifest.schema_version != 1 {
        return Err(format!(
            "unsupported state ownership schema version {}",
            manifest.schema_version
        ));
    }
    require_nonempty(
        "scope.included_authority",
        &manifest.scope.included_authority,
    )?;
    validate_external_authorities(manifest)?;
    validate_principals(repository, manifest)?;
    validate_sources(repository, manifest)?;

    let mut ids = BTreeSet::new();
    for object in &manifest.objects {
        if !ids.insert(object.id.as_str()) {
            return Err(format!("duplicate PostgreSQL state id `{}`", object.id));
        }
        validate_owned_state(
            manifest,
            &object.id,
            &object.ownership.plane,
            &object.ownership.semantic_owner,
            &object.ownership.migration_owners,
            &object.ownership.schema_source,
            &object.ownership.writers,
            &object.ownership.readers,
            &object.ownership.compatibility_horizon,
            &object.ownership.drift_gate,
        )?;
    }
    for family in &manifest.families {
        if !ids.insert(family.id.as_str()) {
            return Err(format!("duplicate PostgreSQL state id `{}`", family.id));
        }
        require_nonempty(&format!("{}.pattern", family.id), &family.pattern)?;
        validate_owned_state(
            manifest,
            &family.id,
            &family.plane,
            &family.semantic_owner,
            &family.migration_owners,
            &family.schema_source,
            &family.writers,
            &family.readers,
            &family.compatibility_horizon,
            &family.drift_gate,
        )?;
    }

    validate_temporary_writers(manifest)?;
    validate_dynamic_writers(repository, manifest)?;
    validate_canonical_inventory(repository, manifest)?;
    validate_scan_policy(repository, manifest)?;
    Ok(())
}

fn require_nonempty(field: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        Err(format!("`{field}` must not be empty"))
    } else {
        Ok(())
    }
}

fn validate_external_authorities(manifest: &Manifest) -> Result<(), String> {
    let actual: BTreeSet<&str> = manifest
        .scope
        .excluded_external_authorities
        .iter()
        .map(String::as_str)
        .collect();
    let expected: BTreeSet<&str> = EXTERNAL_AUTHORITIES.into_iter().collect();
    if actual != expected {
        return Err(format!(
            "external authority exclusions differ: expected {expected:?}, got {actual:?}"
        ));
    }
    Ok(())
}

fn validate_principals(repository: &Path, manifest: &Manifest) -> Result<(), String> {
    for (id, principal) in &manifest.principals {
        require_nonempty(&format!("principals.{id}.role"), &principal.role)?;
        validate_repository_path(repository, &principal.path, &format!("principal `{id}`"))?;
        if !matches!(
            first_component(&principal.path),
            Some("crates" | "services" | "components" | "deploy")
        ) {
            return Err(format!(
                "principal `{id}` path `{}` is outside production ownership roots",
                principal.path
            ));
        }
    }
    Ok(())
}

fn validate_sources(repository: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut paths = BTreeSet::new();
    for source in &manifest.canonical_sources {
        if !paths.insert(source.path.as_str()) {
            return Err(format!("duplicate canonical source `{}`", source.path));
        }
        validate_repository_path(repository, &source.path, "canonical source")?;
        require_nonempty("canonical source scope", &source.scope)?;
        match source.kind.as_str() {
            "ddl" => {
                if source.marker.is_some() {
                    return Err(format!(
                        "DDL source `{}` must not carry a marker",
                        source.path
                    ));
                }
            }
            "inline-family" | "generated-family" => {
                let marker = source
                    .marker
                    .as_deref()
                    .ok_or_else(|| format!("source `{}` needs a marker", source.path))?;
                let text = std::fs::read_to_string(repository.join(&source.path))
                    .map_err(|error| format!("read {}: {error}", source.path))?;
                if !text.contains(marker) {
                    return Err(format!(
                        "canonical source `{}` is missing marker `{marker}`",
                        source.path
                    ));
                }
            }
            other => return Err(format!("unknown canonical source kind `{other}`")),
        }
    }

    for excluded in &manifest.excluded_sources {
        validate_repository_path(repository, &excluded.path, "excluded source")?;
        require_nonempty("excluded source class", &excluded.source_class)?;
        require_nonempty("excluded source reason", &excluded.reason)?;
        if paths.contains(excluded.path.as_str()) {
            return Err(format!(
                "`{}` cannot be canonical and excluded",
                excluded.path
            ));
        }
    }

    let deploy_sql = repository.join("deploy/sql");
    let classified: BTreeSet<&str> = manifest
        .canonical_sources
        .iter()
        .map(|source| source.path.as_str())
        .chain(
            manifest
                .excluded_sources
                .iter()
                .map(|source| source.path.as_str()),
        )
        .collect();
    for entry in std::fs::read_dir(&deploy_sql).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("sql") {
            continue;
        }
        let relative = relative_path(repository, &entry.path())?;
        if !classified.contains(relative.as_str()) {
            return Err(format!(
                "deploy SQL source `{relative}` is neither canonical production DDL nor explicitly excluded"
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_owned_state(
    manifest: &Manifest,
    id: &str,
    plane: &str,
    semantic_owner: &str,
    migration_owners: &[String],
    schema_source: &str,
    writers: &[String],
    readers: &[String],
    compatibility_horizon: &str,
    drift_gate: &str,
) -> Result<(), String> {
    for (field, value) in [
        ("plane", plane),
        ("semantic_owner", semantic_owner),
        ("schema_source", schema_source),
        ("compatibility_horizon", compatibility_horizon),
        ("drift_gate", drift_gate),
    ] {
        require_nonempty(&format!("{id}.{field}"), value)?;
    }
    if migration_owners.len() != 1 {
        return Err(format!(
            "`{id}` must name exactly one migration owner, found {:?}",
            migration_owners
        ));
    }
    if writers.is_empty() || readers.is_empty() {
        return Err(format!("`{id}` must document both writers and readers"));
    }
    let source_exists = manifest
        .canonical_sources
        .iter()
        .any(|source| source.path == schema_source);
    if !source_exists {
        return Err(format!(
            "`{id}` schema source `{schema_source}` is not a canonical source"
        ));
    }
    for principal in std::iter::once(semantic_owner)
        .chain(migration_owners.iter().map(String::as_str))
        .chain(writers.iter().map(String::as_str))
        .chain(readers.iter().map(String::as_str))
    {
        if !manifest.principals.contains_key(principal) {
            return Err(format!("`{id}` references unknown principal `{principal}`"));
        }
    }
    Ok(())
}

fn validate_temporary_writers(manifest: &Manifest) -> Result<(), String> {
    for (id, principal) in &manifest.principals {
        let Some(temporary) = &principal.temporary else {
            continue;
        };
        if principal.role != "compatibility-writer" {
            return Err(format!(
                "temporary principal `{id}` must be a compatibility-writer"
            ));
        }
        require_nonempty(
            &format!("principals.{id}.temporary.expiry_bead"),
            &temporary.expiry_bead,
        )?;
        if !temporary.expiry_bead.starts_with("wamn-") {
            return Err(format!(
                "temporary compatibility writer `{id}` expiry must be a Wamn Bead id"
            ));
        }
        if temporary.may_gain_writes {
            return Err(format!(
                "temporary compatibility writer `{id}` may not gain writes"
            ));
        }
        let migration_owner = manifest.objects.iter().any(|object| {
            object
                .ownership
                .migration_owners
                .iter()
                .any(|owner| owner == id)
        }) || manifest
            .families
            .iter()
            .any(|family| family.migration_owners.iter().any(|owner| owner == id));
        if migration_owner {
            return Err(format!(
                "temporary compatibility writer `{id}` cannot own migrations"
            ));
        }
        let locked_families: BTreeSet<&str> = temporary
            .writer_families
            .iter()
            .map(String::as_str)
            .collect();
        if locked_families.is_empty() || locked_families.len() != temporary.writer_families.len() {
            return Err(format!(
                "temporary compatibility writer `{id}` must have a nonempty, duplicate-free locked writer-family scope"
            ));
        }
        let dynamic_families: BTreeSet<&str> = manifest
            .scan_policy
            .dynamic_writers
            .iter()
            .filter(|writer| writer.principal == *id)
            .map(|writer| writer.family.as_str())
            .collect();
        let actual_objects: Vec<&str> = manifest
            .objects
            .iter()
            .filter(|object| object.ownership.writers.iter().any(|writer| writer == id))
            .map(|object| object.id.as_str())
            .collect();
        let actual_families: BTreeSet<&str> = manifest
            .families
            .iter()
            .filter(|family| family.writers.iter().any(|writer| writer == id))
            .map(|family| family.id.as_str())
            .collect();
        if !actual_objects.is_empty()
            || dynamic_families != locked_families
            || actual_families != locked_families
        {
            return Err(format!(
                "temporary compatibility writer `{id}` may appear only on its locked writer-family scope; \
                 objects={actual_objects:?}, locked_families={locked_families:?}, \
                 dynamic_families={dynamic_families:?}, actual_families={actual_families:?}"
            ));
        }
    }
    Ok(())
}

fn validate_dynamic_writers(repository: &Path, manifest: &Manifest) -> Result<(), String> {
    for writer in &manifest.scan_policy.dynamic_writers {
        validate_repository_path(repository, &writer.path, "dynamic writer")?;
        let principal = manifest.principals.get(&writer.principal).ok_or_else(|| {
            format!(
                "dynamic writer names unknown principal `{}`",
                writer.principal
            )
        })?;
        if !path_is_within(&writer.path, &principal.path) {
            return Err(format!(
                "dynamic writer `{}` is outside principal `{}` path `{}`",
                writer.path, writer.principal, principal.path
            ));
        }
        let source = std::fs::read_to_string(repository.join(&writer.path))
            .map_err(|error| format!("read {}: {error}", writer.path))?;
        if !source.contains(&writer.marker) {
            return Err(format!(
                "dynamic writer `{}` is missing marker `{}`",
                writer.path, writer.marker
            ));
        }
        if writer.operations.is_empty()
            || writer.operations.iter().any(|operation| {
                !matches!(
                    operation.as_str(),
                    "insert" | "update" | "delete" | "merge" | "truncate"
                )
            })
        {
            return Err(format!(
                "dynamic writer `{}` has invalid operations {:?}",
                writer.path, writer.operations
            ));
        }
        let state = manifest
            .families
            .iter()
            .find(|candidate| candidate.id == writer.family)
            .ok_or_else(|| format!("dynamic writer names unknown family `{}`", writer.family))?;
        if !state.writers.contains(&writer.principal) {
            return Err(format!(
                "dynamic principal `{}` is not authorized for family `{}`",
                writer.principal, writer.family
            ));
        }
    }
    Ok(())
}

fn validate_canonical_inventory(repository: &Path, manifest: &Manifest) -> Result<(), String> {
    let mut discovered = BTreeSet::new();
    for source in manifest
        .canonical_sources
        .iter()
        .filter(|source| source.kind == "ddl")
    {
        let text = std::fs::read_to_string(repository.join(&source.path))
            .map_err(|error| format!("read {}: {error}", source.path))?;
        for object in extract_create_tables(&text) {
            if !discovered.insert((object, source.path.as_str())) {
                return Err(format!("duplicate table declaration in `{}`", source.path));
            }
        }
    }

    let registered: BTreeSet<(&str, &str)> = manifest
        .objects
        .iter()
        .map(|object| (object.id.as_str(), object.ownership.schema_source.as_str()))
        .collect();
    compare_canonical_objects(&registered, &discovered)?;

    for excluded in &manifest.excluded_sources {
        let text = std::fs::read_to_string(repository.join(&excluded.path))
            .map_err(|error| format!("read {}: {error}", excluded.path))?;
        let actual = extract_create_tables(&text);
        let expected: BTreeSet<String> = excluded.expected_objects.iter().cloned().collect();
        if actual != expected {
            return Err(format!(
                "excluded source `{}` objects drifted: expected {expected:?}, got {actual:?}",
                excluded.path
            ));
        }
    }
    Ok(())
}

fn compare_canonical_objects(
    registered: &BTreeSet<(&str, &str)>,
    discovered: &BTreeSet<(String, &str)>,
) -> Result<(), String> {
    let normalized: BTreeSet<(&str, &str)> = discovered
        .iter()
        .map(|(object, source)| (object.as_str(), *source))
        .collect();
    if registered == &normalized {
        return Ok(());
    }
    let unregistered: Vec<_> = normalized.difference(registered).copied().collect();
    let stale: Vec<_> = registered.difference(&normalized).copied().collect();
    Err(format!(
        "canonical PostgreSQL inventory mismatch; unregistered={unregistered:?}; stale={stale:?}"
    ))
}

fn extract_create_tables(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line
                .strip_prefix("CREATE TABLE ")
                .or_else(|| line.strip_prefix("CREATE UNLOGGED TABLE "))?;
            let object = rest
                .strip_prefix("IF NOT EXISTS ")
                .unwrap_or(rest)
                .split(|character: char| character.is_whitespace() || character == '(')
                .next()?;
            Some(normalize_identifier(object))
        })
        .collect()
}

fn validate_scan_policy(repository: &Path, manifest: &Manifest) -> Result<(), String> {
    let expected_roots = ["components", "crates", "deploy/sql", "services"];
    let actual_roots: BTreeSet<&str> = manifest
        .scan_policy
        .roots
        .iter()
        .map(String::as_str)
        .collect();
    if actual_roots != expected_roots.into_iter().collect() {
        return Err(format!(
            "writer scan roots are incomplete: {actual_roots:?}"
        ));
    }
    let extensions: BTreeSet<&str> = manifest
        .scan_policy
        .extensions
        .iter()
        .map(String::as_str)
        .collect();
    if extensions != ["rs", "sql"].into_iter().collect() {
        return Err(format!(
            "writer scan extensions are incomplete: {extensions:?}"
        ));
    }
    if manifest.scan_policy.rust_cfg_test_modules != "exclude" {
        return Err("inline Rust test modules must be explicitly excluded".to_string());
    }
    for root in &manifest.scan_policy.roots {
        validate_repository_path(repository, root, "scan root")?;
    }
    for exclusion in &manifest.scan_policy.exclusions {
        require_nonempty("scan exclusion class", &exclusion.source_class)?;
        match exclusion.kind.as_str() {
            "path-prefix" | "exact-file" => {
                validate_repository_path(repository, &exclusion.value, "scan exclusion")?
            }
            "path-segment" => require_nonempty("scan path segment", &exclusion.value)?,
            other => return Err(format!("unknown scan exclusion kind `{other}`")),
        }
    }
    for context in &manifest.scan_policy.unqualified_contexts {
        validate_repository_path(repository, &context.path_prefix, "SQL source context")?;
        require_nonempty("default SQL schema", &context.default_schema)?;
    }
    for ignored in &manifest.scan_policy.ignored_literals {
        validate_repository_path(repository, &ignored.path, "ignored SQL literal")?;
        require_nonempty("ignored SQL literal", &ignored.literal)?;
        require_nonempty("ignored SQL literal reason", &ignored.reason)?;
        let source = std::fs::read_to_string(repository.join(&ignored.path))
            .map_err(|error| format!("read {}: {error}", ignored.path))?;
        if !source.contains(&ignored.literal) {
            return Err(format!(
                "ignored literal `{}` no longer exists in `{}`",
                ignored.literal, ignored.path
            ));
        }
    }
    Ok(())
}

fn scan_repository_writers(
    repository: &Path,
    manifest: &Manifest,
) -> Result<Vec<Discovery>, String> {
    let mut files = Vec::new();
    for root in &manifest.scan_policy.roots {
        collect_files(repository, &repository.join(root), manifest, &mut files)?;
    }
    files.sort();
    files.dedup();

    let mut discoveries = Vec::new();
    for path in files {
        let relative = relative_path(repository, &path)?;
        let source =
            std::fs::read_to_string(&path).map_err(|error| format!("read {relative}: {error}"))?;
        let extension = path.extension().and_then(|value| value.to_str());
        let literals = match extension {
            Some("rs") => rust_string_literals(production_rust_source(&source)),
            Some("sql") => vec![(1, strip_sql_comments(&source))],
            _ => continue,
        };
        for (line, literal) in literals {
            if manifest
                .scan_policy
                .ignored_literals
                .iter()
                .any(|ignored| ignored.path == relative && ignored.literal == literal)
            {
                continue;
            }
            discoveries.extend(discover_writes(&relative, line, &literal));
        }
    }
    Ok(discoveries)
}

fn collect_files(
    repository: &Path,
    current: &Path,
    manifest: &Manifest,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    if current.is_file() {
        let relative = relative_path(repository, current)?;
        if is_excluded(&relative, &manifest.scan_policy.exclusions)
            || !manifest.scan_policy.extensions.iter().any(|extension| {
                current.extension().and_then(|value| value.to_str()) == Some(extension)
            })
        {
            return Ok(());
        }
        files.push(current.to_path_buf());
        return Ok(());
    }
    let entries = std::fs::read_dir(current)
        .map_err(|error| format!("read directory {}: {error}", current.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let relative = relative_path(repository, &entry.path())?;
        if is_excluded(&relative, &manifest.scan_policy.exclusions) {
            continue;
        }
        collect_files(repository, &entry.path(), manifest, files)?;
    }
    Ok(())
}

fn is_excluded(path: &str, exclusions: &[ScanExclusion]) -> bool {
    exclusions
        .iter()
        .any(|exclusion| match exclusion.kind.as_str() {
            "exact-file" => path == exclusion.value,
            "path-prefix" => path_is_within(path, &exclusion.value),
            "path-segment" => Path::new(path)
                .components()
                .any(|component| component.as_os_str() == exclusion.value.as_str()),
            _ => false,
        })
}

fn production_rust_source(source: &str) -> &str {
    let mut offset = 0;
    while let Some(found) = source[offset..].find("#[cfg(test)]") {
        let start = offset + found;
        let tail = &source[start + "#[cfg(test)]".len()..];
        let following = tail.trim_start();
        if following.starts_with("mod tests") {
            return &source[..start];
        }
        offset = start + "#[cfg(test)]".len();
    }
    source
}

fn rust_string_literals(source: &str) -> Vec<(usize, String)> {
    let bytes = source.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0;
    let mut line = 1;
    while index < bytes.len() {
        if bytes.get(index..index + 2) == Some(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if bytes.get(index..index + 2) == Some(b"/*") {
            let mut depth = 1;
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes.get(index..index + 2) == Some(b"/*") {
                    depth += 1;
                    index += 2;
                } else if bytes.get(index..index + 2) == Some(b"*/") {
                    depth -= 1;
                    index += 2;
                } else {
                    if bytes[index] == b'\n' {
                        line += 1;
                    }
                    index += 1;
                }
            }
            continue;
        }
        if bytes[index] == b'\n' {
            line += 1;
            index += 1;
            continue;
        }
        if bytes[index] == b'r' {
            let mut hashes = 0;
            let mut cursor = index + 1;
            while cursor < bytes.len() && bytes[cursor] == b'#' {
                hashes += 1;
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'"' {
                let start_line = line;
                cursor += 1;
                let body_start = cursor;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'\n' {
                        line += 1;
                    }
                    if bytes[cursor] == b'"'
                        && bytes.get(cursor + 1..cursor + 1 + hashes) == Some(&vec![b'#'; hashes])
                    {
                        literals.push((start_line, source[body_start..cursor].to_string()));
                        index = cursor + 1 + hashes;
                        break;
                    }
                    cursor += 1;
                }
                if cursor >= bytes.len() {
                    break;
                }
                continue;
            }
        }
        if bytes[index] == b'"' {
            let start_line = line;
            index += 1;
            let mut literal = String::new();
            while index < bytes.len() {
                match bytes[index] {
                    b'"' => {
                        index += 1;
                        literals.push((start_line, literal));
                        break;
                    }
                    b'\\' if index + 1 < bytes.len() => {
                        index += 1;
                        match bytes[index] {
                            b'\n' => {
                                line += 1;
                                literal.push(' ');
                            }
                            b'n' | b'r' | b't' => literal.push(' '),
                            escaped => literal.push(char::from(escaped)),
                        }
                        index += 1;
                    }
                    b'\n' => {
                        line += 1;
                        literal.push('\n');
                        index += 1;
                    }
                    byte if byte.is_ascii() => {
                        literal.push(char::from(byte));
                        index += 1;
                    }
                    _ => {
                        let character = source[index..]
                            .chars()
                            .next()
                            .expect("index is on a UTF-8 boundary");
                        literal.push(character);
                        index += character.len_utf8();
                    }
                }
            }
            continue;
        }
        index += 1;
    }
    literals
}

fn strip_sql_comments(source: &str) -> String {
    source
        .lines()
        .map(|line| line.split_once("--").map_or(line, |(before, _)| before))
        .collect::<Vec<_>>()
        .join("\n")
}

fn discover_writes(path: &str, starting_line: usize, source: &str) -> Vec<Discovery> {
    let tokens = sql_tokens(source);
    let Some((first, _)) = tokens.first() else {
        return Vec::new();
    };
    if ![
        "ALTER", "CREATE", "DELETE", "DO", "DROP", "GRANT", "INSERT", "MERGE", "SELECT", "SET",
        "TRUNCATE", "UPDATE", "WITH",
    ]
    .iter()
    .any(|keyword| first.eq_ignore_ascii_case(keyword))
    {
        return Vec::new();
    }
    let privilege_list = privilege_list_positions(&tokens);
    let mut discoveries = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let (token, line) = &tokens[index];
        if privilege_list[index] {
            index += 1;
            continue;
        }
        let operation = match token.to_ascii_uppercase().as_str() {
            "INSERT" if token_is(&tokens, index + 1, "INTO") => {
                index += 2;
                "insert"
            }
            "UPDATE" if !names_trigger_event(&tokens, index) && !locks_rows(&tokens, index) => {
                index += 1;
                "update"
            }
            "DELETE" if token_is(&tokens, index + 1, "FROM") => {
                index += 2;
                "delete"
            }
            "MERGE" if token_is(&tokens, index + 1, "INTO") => {
                index += 2;
                "merge"
            }
            "TRUNCATE" => {
                index += 1;
                if token_is(&tokens, index, "TABLE") {
                    index += 1;
                }
                "truncate"
            }
            _ => {
                index += 1;
                continue;
            }
        };
        let Some((target, _)) = tokens.get(index) else {
            continue;
        };
        if [
            "SET", "SKIP", "OF", "INSERT", "UPDATE", "DELETE", "MERGE", "TRUNCATE",
        ]
        .iter()
        .any(|keyword| target.eq_ignore_ascii_case(keyword))
        {
            continue;
        }
        if operation == "update" && !has_set_clause(&tokens, index) {
            continue;
        }
        discoveries.push(Discovery {
            path: path.to_string(),
            line: starting_line + line.saturating_sub(1),
            operation: operation.to_string(),
            target: target.clone(),
        });
        index += 1;
    }
    discoveries
}

fn token_is(tokens: &[(String, usize)], index: usize, keyword: &str) -> bool {
    tokens
        .get(index)
        .is_some_and(|(token, _)| token.eq_ignore_ascii_case(keyword))
}

/// `CREATE TRIGGER ... BEFORE INSERT OR UPDATE ON t` names trigger events, so
/// the `UPDATE` there is DDL vocabulary rather than a statement.
fn names_trigger_event(tokens: &[(String, usize)], index: usize) -> bool {
    tokens[..index].iter().rev().take(3).any(|(previous, _)| {
        previous.eq_ignore_ascii_case("BEFORE") || previous.eq_ignore_ascii_case("AFTER")
    })
}

/// `SELECT ... FOR UPDATE` takes a row lock; it never writes a row itself.
fn locks_rows(tokens: &[(String, usize)], index: usize) -> bool {
    index > 0 && token_is(tokens, index - 1, "FOR")
}

/// Every PostgreSQL `UPDATE` statement carries a `SET` clause. Prose such as an
/// `update flow draft` error context, which the statement-keyword prefilter
/// cannot tell from SQL, never does. A `{...}` format placeholder counts: the
/// scan cannot see through it, and builders such as the run-queue batch claim
/// interpolate their whole `SET` clause that way.
fn has_set_clause(tokens: &[(String, usize)], target_index: usize) -> bool {
    tokens
        .iter()
        .skip(target_index + 1)
        .any(|(token, _)| token.eq_ignore_ascii_case("SET") || token.starts_with('{'))
}

/// Marks each token that sits in a `GRANT`/`REVOKE` privilege list, where
/// `INSERT`, `UPDATE`, `DELETE`, and `TRUNCATE` name privileges rather than
/// statements. The list runs from the `GRANT`/`REVOKE` keyword to the `ON` or
/// `TO` that closes it.
fn privilege_list_positions(tokens: &[(String, usize)]) -> Vec<bool> {
    let mut inside = false;
    tokens
        .iter()
        .map(|(token, _)| {
            if token.eq_ignore_ascii_case("GRANT") || token.eq_ignore_ascii_case("REVOKE") {
                inside = true;
            } else if token.eq_ignore_ascii_case("ON") || token.eq_ignore_ascii_case("TO") {
                inside = false;
            }
            inside
        })
        .collect()
}

/// Splits SQL into word tokens. Single-quoted SQL string literals are opaque:
/// their text is data — a `TG_OP` comparison, a hyphenated error message, a
/// privilege name — and must never be read as a statement keyword.
fn sql_tokens(source: &str) -> Vec<(String, usize)> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut line = 1;
    let mut token_line = 1;
    let mut quoted = false;
    for character in source.chars() {
        if character == '\'' {
            quoted = !quoted;
        }
        let allowed = !quoted
            && character != '\''
            && (character.is_ascii_alphanumeric()
                || matches!(character, '_' | '.' | '"' | '{' | '}' | '<' | '>'));
        if allowed {
            if current.is_empty() {
                token_line = line;
            }
            current.push(character);
        } else if !current.is_empty() {
            tokens.push((std::mem::take(&mut current), token_line));
        }
        if character == '\n' {
            line += 1;
        }
    }
    if !current.is_empty() {
        tokens.push((current, token_line));
    }
    tokens
}

fn validate_discovered_writers(
    manifest: &Manifest,
    discoveries: &[Discovery],
) -> Result<(), String> {
    for discovery in discoveries {
        let target = resolve_target(manifest, discovery)?;
        let authorized = target.writers().iter().any(|writer| {
            manifest
                .principals
                .get(writer)
                .is_some_and(|principal| path_is_within(&discovery.path, &principal.path))
        });
        if !authorized {
            return Err(format!(
                "{}:{}: undeclared {} writer `{}` for `{}`",
                discovery.path,
                discovery.line,
                discovery.operation,
                discovery.target,
                target.id()
            ));
        }
    }
    Ok(())
}

fn resolve_target<'a>(
    manifest: &'a Manifest,
    discovery: &Discovery,
) -> Result<StateTarget<'a>, String> {
    let normalized = normalize_identifier(&discovery.target);
    if let Some(object) = manifest
        .objects
        .iter()
        .find(|object| object.id == normalized)
    {
        return Ok(StateTarget::Object(object));
    }

    let basename = normalized.rsplit('.').next().unwrap_or(&normalized);
    if basename == "wamn_catalog" {
        return family(manifest, "app-schema-catalog-snapshot");
    }
    if basename == "wamn_entities" {
        return family(manifest, "app-schema-entity-map");
    }
    if let Some(dynamic) = manifest.scan_policy.dynamic_writers.iter().find(|writer| {
        writer.path == discovery.path
            && (normalized.contains('{')
                || writer
                    .marker
                    .to_ascii_lowercase()
                    .contains(&normalized.to_ascii_lowercase()))
    }) {
        return family(manifest, &dynamic.family);
    }

    let context = manifest
        .scan_policy
        .unqualified_contexts
        .iter()
        .filter(|context| path_is_within(&discovery.path, &context.path_prefix))
        .max_by_key(|context| context.path_prefix.len());
    if !normalized.contains('.') {
        let context = context.ok_or_else(|| {
            format!(
                "{}:{}: unqualified {} target `{}` has no declared default schema",
                discovery.path, discovery.line, discovery.operation, discovery.target
            )
        })?;
        let qualified = format!("{}.{}", context.default_schema, normalized);
        if let Some(object) = manifest
            .objects
            .iter()
            .find(|object| object.id == qualified)
        {
            return Ok(StateTarget::Object(object));
        }
    } else if normalized.starts_with('{')
        && let Some(context) = context
    {
        let qualified = format!("{}.{}", context.default_schema, basename);
        if let Some(object) = manifest
            .objects
            .iter()
            .find(|object| object.id == qualified)
        {
            return Ok(StateTarget::Object(object));
        }
    }

    Err(format!(
        "{}:{}: {} targets unregistered PostgreSQL object or family `{}`",
        discovery.path, discovery.line, discovery.operation, discovery.target
    ))
}

fn family<'a>(manifest: &'a Manifest, id: &str) -> Result<StateTarget<'a>, String> {
    manifest
        .families
        .iter()
        .find(|family| family.id == id)
        .map(StateTarget::Family)
        .ok_or_else(|| format!("registered family `{id}` is absent"))
}

fn normalize_identifier(identifier: &str) -> String {
    identifier
        .split('.')
        .map(|part| {
            let part = part.trim_matches(|character: char| {
                matches!(character, '"' | '\'' | ';' | ',' | '(' | ')')
            });
            if part.starts_with('{') || part.starts_with('<') {
                part.to_string()
            } else {
                part.to_ascii_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn validate_repository_path(repository: &Path, path: &str, label: &str) -> Result<(), String> {
    if path.is_empty() || Path::new(path).is_absolute() || path.contains("..") {
        return Err(format!("{label} has unsafe repository path `{path}`"));
    }
    if !repository.join(path).exists() {
        return Err(format!("{label} path `{path}` does not exist"));
    }
    Ok(())
}

fn first_component(path: &str) -> Option<&str> {
    Path::new(path)
        .components()
        .find_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
}

fn path_is_within(path: &str, prefix: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn relative_path(repository: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(repository)
        .map_err(|error| error.to_string())?
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("non-UTF-8 repository path {}", path.display()))
}

#[test]
fn repository_state_ownership_manifest_is_complete() {
    let repository = repository();
    let manifest = read_manifest(&repository);
    validate_manifest(&repository, &manifest).expect("validate state ownership manifest");
    let discoveries =
        scan_repository_writers(&repository, &manifest).expect("scan production writer sources");
    eprintln!(
        "SR28 inventoried {} static production PostgreSQL write operations",
        discoveries.len()
    );
    validate_discovered_writers(&manifest, &discoveries)
        .expect("all production PostgreSQL writers are declared");
    assert!(
        discoveries.iter().any(
            |writer| writer.path == "crates/execution/run-state/src/sql.rs"
                && writer.target == "runs"
        ),
        "the scan must cover unqualified run-store writes"
    );
}

#[test]
fn missing_scan_exclusion_path_is_rejected() {
    let repository = repository();
    let mut manifest = read_manifest(&repository);
    manifest.scan_policy.exclusions.push(ScanExclusion {
        kind: "path-prefix".to_string(),
        value: "components/does-not-exist".to_string(),
        source_class: "mutation".to_string(),
    });
    let error = validate_scan_policy(&repository, &manifest).unwrap_err();
    assert!(
        error.contains("scan exclusion path `components/does-not-exist` does not exist"),
        "{error}"
    );
}

#[test]
fn unregistered_canonical_table_is_rejected() {
    let registered = BTreeSet::from([("catalog.catalogs", "canonical.sql")]);
    let discovered = BTreeSet::from([
        ("catalog.catalogs".to_string(), "canonical.sql"),
        ("catalog.rogue".to_string(), "canonical.sql"),
    ]);
    let error = compare_canonical_objects(&registered, &discovered).unwrap_err();
    assert!(error.contains("catalog.rogue"), "{error}");
    assert!(error.contains("unregistered"), "{error}");
}

#[test]
fn conflicting_second_migration_owner_is_rejected() {
    let repository = repository();
    let mut manifest = read_manifest(&repository);
    manifest.objects[0]
        .ownership
        .migration_owners
        .push("provision-store".to_string());
    let error = validate_manifest(&repository, &manifest).unwrap_err();
    assert!(error.contains("exactly one migration owner"), "{error}");
}

#[test]
fn temporary_writer_cannot_expand_to_platform_object() {
    let repository = repository();
    let mut manifest = read_manifest(&repository);
    manifest.objects[0]
        .ownership
        .writers
        .push("standard-nodes-raw-sql".to_string());
    let error = validate_manifest(&repository, &manifest).unwrap_err();
    assert!(
        error.contains("may appear only on its locked writer-family scope"),
        "{error}"
    );
    assert!(error.contains("registry.meta"), "{error}");
}

#[test]
fn temporary_writer_cannot_expand_with_matching_dynamic_declaration() {
    let repository = repository();
    let mut manifest = read_manifest(&repository);
    manifest.families[0]
        .writers
        .push("standard-nodes-raw-sql".to_string());
    manifest.scan_policy.dynamic_writers.push(DynamicWriter {
        path: "crates/execution/standard-nodes/src/postgres.rs".to_string(),
        principal: "standard-nodes-raw-sql".to_string(),
        family: manifest.families[0].id.clone(),
        marker: "ctx.pg_execute(sql".to_string(),
        operations: vec!["insert".to_string()],
    });
    let error = validate_manifest(&repository, &manifest).unwrap_err();
    assert!(
        error.contains("may appear only on its locked writer-family scope"),
        "{error}"
    );
    assert!(error.contains("app-schema-catalog-snapshot"), "{error}");
}

#[test]
fn undeclared_writer_is_rejected() {
    let repository = repository();
    let manifest = read_manifest(&repository);
    let rogue = Discovery {
        path: "services/rogue/src/lib.rs".to_string(),
        line: 7,
        operation: "insert".to_string(),
        target: "registry.orgs".to_string(),
    };
    let error = validate_discovered_writers(&manifest, &[rogue]).unwrap_err();
    assert!(error.contains("undeclared insert writer"), "{error}");
    assert!(error.contains("registry.orgs"), "{error}");
}

#[test]
fn external_authority_classes_are_excluded() {
    let manifest = read_manifest(&repository());
    validate_external_authorities(&manifest).expect("external authority exclusions are exact");
    assert!(
        manifest
            .objects
            .iter()
            .all(|object| !EXTERNAL_AUTHORITIES.contains(&object.id.as_str()))
    );
}

#[test]
fn generated_tenant_family_is_covered() {
    let repository = repository();
    let manifest = read_manifest(&repository);
    let family = manifest
        .families
        .iter()
        .find(|family| family.id == "generated-tenant-entities")
        .expect("generated tenant family");
    assert_eq!(family.pattern, "<app_schema>.<catalog_entity_name>");
    let dynamic_paths: BTreeSet<&str> = manifest
        .scan_policy
        .dynamic_writers
        .iter()
        .filter(|writer| writer.family == family.id)
        .map(|writer| writer.path.as_str())
        .collect();
    assert_eq!(
        dynamic_paths,
        BTreeSet::from([
            "components/execution/flowrunner/src/lib.rs",
            "crates/data/entity-access/src/planner.rs",
            "crates/execution/standard-nodes/src/postgres.rs",
            "crates/schema/compiler/src/seed/emit.rs"
        ])
    );
    validate_dynamic_writers(&repository, &manifest).expect("dynamic writer markers are live");
}

#[test]
fn unqualified_writer_requires_declared_source_context() {
    let manifest = read_manifest(&repository());
    let covered = Discovery {
        path: "crates/execution/run-state/src/sql.rs".to_string(),
        line: 1,
        operation: "insert".to_string(),
        target: "runs".to_string(),
    };
    assert_eq!(
        resolve_target(&manifest, &covered)
            .expect("run-state context")
            .id(),
        "wamn_run.runs"
    );

    let uncovered = Discovery {
        path: "crates/unknown/src/lib.rs".to_string(),
        ..covered
    };
    let error = resolve_target(&manifest, &uncovered).unwrap_err();
    assert!(error.contains("no declared default schema"), "{error}");
}

#[test]
fn lowercase_undeclared_writer_is_rejected() {
    let manifest = read_manifest(&repository());
    let discoveries = discover_writes(
        "services/rogue/src/lib.rs",
        11,
        "insert into registry.orgs (id) values ('rogue')",
    );
    assert_eq!(discoveries.len(), 1, "lowercase SQL must be inventoried");
    let error = validate_discovered_writers(&manifest, &discoveries).unwrap_err();
    assert!(error.contains("undeclared insert writer"), "{error}");
    assert!(error.contains("registry.orgs"), "{error}");
}

#[test]
fn producer_side_direct_run_admission_is_rejected() {
    let manifest = read_manifest(&repository());
    for (path, statement) in [
        (
            "services/rogue-producer/src/lib.rs",
            "INSERT INTO wamn_run.runs (tenant_id, run_id) VALUES ('t1', 'bypass')",
        ),
        (
            "components/rogue-producer/src/lib.rs",
            "INSERT INTO wamn_run.run_queue (tenant_id, run_id) VALUES ('t1', 'bypass')",
        ),
        (
            "components/rogue-producer/src/lib.rs",
            "UPDATE wamn_run.run_queue SET partition_key='patched' WHERE run_id='bypass'",
        ),
    ] {
        let discoveries = discover_writes(path, 1, statement);
        assert_eq!(discoveries.len(), 1, "admission bypass must be inventoried");
        let error = validate_discovered_writers(&manifest, &discoveries).unwrap_err();
        assert!(error.contains("undeclared"), "{error}");
        assert!(error.contains("wamn_run.run"), "{error}");
    }
}

#[test]
fn trigger_event_declarations_are_not_inventoried_as_writes() {
    let discoveries = discover_writes(
        "deploy/sql/catalog-schema.sql",
        1,
        "CREATE TRIGGER immutable BEFORE INSERT OR UPDATE ON catalog.release_attachments \
         FOR EACH ROW EXECUTE FUNCTION catalog.reject_immutable_row_change()",
    );
    assert!(
        discoveries.is_empty(),
        "a trigger event is DDL, not an UPDATE statement: {discoveries:?}"
    );
}

/// Asserts a snippet yields exactly the one genuine write it contains. Each
/// false-positive regression case below pairs its artefact with a real
/// `UPDATE ... SET`, so the case fails if its own skip rule is removed rather
/// than passing because some coarser rule happened to swallow the artefact too.
fn assert_only_write(discoveries: &[Discovery], operation: &str, target: &str) {
    assert_eq!(
        discoveries.len(),
        1,
        "exactly the genuine write must be inventoried: {discoveries:?}"
    );
    assert_eq!(discoveries[0].operation, operation, "{discoveries:?}");
    assert_eq!(discoveries[0].target, target, "{discoveries:?}");
}

#[test]
fn plpgsql_trigger_operation_literal_is_not_an_update() {
    let discoveries = discover_writes(
        "crates/schema/control/src/run_plane.rs",
        741,
        "CREATE OR REPLACE FUNCTION wamn_run.guard_authoring_report_write() \
         RETURNS trigger LANGUAGE plpgsql AS $function$ \
         DECLARE \
             old_row jsonb := CASE WHEN TG_OP = 'UPDATE' THEN to_jsonb(OLD) END; \
         BEGIN \
             UPDATE wamn_run.authoring_report_reservations \
                SET state = 'observed' \
              WHERE report_id = old_row ->> 'report_id'; \
             RETURN NEW; \
         END $function$",
    );
    assert_only_write(
        &discoveries,
        "update",
        "wamn_run.authoring_report_reservations",
    );
}

#[test]
fn hyphenated_error_message_literal_is_not_an_update() {
    let discoveries = discover_writes(
        "deploy/sql/catalog-schema.sql",
        1,
        "CREATE FUNCTION catalog.guard_flow_draft_update() RETURNS trigger AS $function$ \
         BEGIN \
             IF NEW.revision <> OLD.revision + 1 THEN \
                 RAISE EXCEPTION USING ERRCODE = '55000', \
                     MESSAGE = 'flow-draft-uncontrolled-update'; \
             END IF; \
             UPDATE catalog.flow_drafts SET edited_at = now() WHERE draft_id = NEW.draft_id; \
             RETURN NEW; \
         END $function$ LANGUAGE plpgsql",
    );
    assert_only_write(&discoveries, "update", "catalog.flow_drafts");
}

#[test]
fn row_lock_clause_is_not_an_update() {
    let discoveries = discover_writes(
        "deploy/sql/flow-tests.sql",
        1,
        "SELECT to_jsonb(reservation) INTO reservation_command \
           FROM wamn_run.authoring_report_reservations AS reservation \
          WHERE reservation.state = 'pending' \
          FOR UPDATE; \
          IF reservation_command IS NULL THEN \
              RAISE EXCEPTION USING ERRCODE = '55000'; \
          END IF; \
          UPDATE wamn_run.authoring_report_reservations \
             SET state = 'claimed' \
           WHERE report_id = reservation_command ->> 'report_id';",
    );
    assert_only_write(
        &discoveries,
        "update",
        "wamn_run.authoring_report_reservations",
    );
}

#[test]
fn grant_privilege_list_is_not_a_write() {
    let discoveries = discover_writes(
        "deploy/sql/catalog-schema.sql",
        1,
        "GRANT SELECT, INSERT, UPDATE ON catalog.flow_drafts TO wamn_scenario_author; \
         UPDATE catalog.flow_drafts SET edited_at = now() WHERE draft_id = $1;",
    );
    assert_only_write(&discoveries, "update", "catalog.flow_drafts");
}

#[test]
fn quoted_privilege_name_probe_is_not_a_write() {
    for (path, statement) in [
        (
            "crates/schema/control/src/run_plane.rs",
            "SELECT namespace.nspname, relation.relname, actor.rolname, privilege.name \
               FROM pg_catalog.pg_roles AS actor \
               CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                                  ('DELETE'::text), ('TRUNCATE'::text)) AS privilege(name) \
              WHERE pg_catalog.has_table_privilege(actor.oid, relation.oid, privilege.name)",
        ),
        (
            "services/scenario-worker/src/authoring.rs",
            "WITH allowed_mutation(schema_name, table_name, privilege) AS ( \
                 VALUES ('catalog', 'flow_drafts', 'INSERT'), \
                        ('catalog', 'flow_drafts', 'UPDATE') \
             ) SELECT true",
        ),
    ] {
        let discoveries = discover_writes(path, 1, statement);
        assert!(
            discoveries.is_empty(),
            "a quoted privilege name is data, not a statement: {discoveries:?}"
        );
    }
}

#[test]
fn english_prose_context_is_not_an_update() {
    let discoveries = discover_writes(
        "services/scenario-worker/src/authoring.rs",
        660,
        "update flow draft",
    );
    assert!(
        discoveries.is_empty(),
        "an error context without a SET clause is prose, not an UPDATE statement: {discoveries:?}"
    );
}

#[test]
fn interpolated_set_clause_still_inventories_the_update() {
    let discoveries = discover_writes(
        "crates/execution/run-state/src/queue/sql.rs",
        154,
        "WITH claimed AS MATERIALIZED ( {cte} ) \
         UPDATE run_queue AS q \
             {set} \
            FROM claimed \
           WHERE q.tenant_id = claimed.tenant_id AND q.run_id = claimed.run_id",
    );
    assert_eq!(
        discoveries.len(),
        1,
        "a `SET` clause behind a format placeholder must still be inventoried: {discoveries:?}"
    );
    assert_eq!(discoveries[0].target, "run_queue");
}

#[test]
fn run_plane_effect_backfill_writes_resolve_to_schema_control() {
    let manifest = read_manifest(&repository());
    for (line, operation, target, id) in [
        (
            1606,
            "insert",
            "{schema}.effect_attempts",
            "wamn_run.effect_attempts",
        ),
        (
            1622,
            "insert",
            "{schema}.effect_attempt_dispatches",
            "wamn_run.effect_attempt_dispatches",
        ),
        (
            1632,
            "insert",
            "{schema}.effect_attempt_outcomes",
            "wamn_run.effect_attempt_outcomes",
        ),
        (1643, "update", "{schema}.node_runs", "wamn_run.node_runs"),
    ] {
        let discovery = Discovery {
            path: "crates/schema/control/src/run_plane.rs".to_string(),
            line,
            operation: operation.to_string(),
            target: target.to_string(),
        };
        assert_eq!(
            resolve_target(&manifest, &discovery)
                .expect("run-plane default schema")
                .id(),
            id
        );
        validate_discovered_writers(&manifest, std::slice::from_ref(&discovery))
            .expect("schema-control is a declared run-plane writer");
    }
}

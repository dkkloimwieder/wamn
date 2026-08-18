use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

const OWNERS_PATH: &str = "architecture/state-owners.json";
const TABLE_PATH: &str = "architecture/protected-writes.json";
const AUTHOR_SQL_EXPOSURE: &str = "author SQL, RLS-bounded";
const NODE_ERROR_CHECK: &str = "constraint:node_runs_error_kind_check;kind=check;deferrable=false;deferred=false;validated=true;definition=CHECK (error_kind = ANY (ARRAY['retryable'::text, 'rate-limited'::text, 'terminal'::text, 'invalid-input'::text]))";

#[derive(Debug, Deserialize)]
struct OwnershipManifest {
    schema_version: String,
    canonical_sources: Vec<CanonicalSource>,
    objects: Vec<OwnershipEntry>,
    families: Vec<OwnershipFamily>,
}

#[derive(Debug, Deserialize)]
struct CanonicalSource {
    path: String,
    scope: String,
}

#[derive(Debug, Deserialize)]
struct OwnershipEntry {
    id: String,
    semantic_owner: String,
    migration_owners: Vec<String>,
    schema_source: String,
    writers: Vec<String>,
    #[serde(default)]
    lifecycle: Lifecycle,
    #[serde(default)]
    superseded_by: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OwnershipFamily {
    pattern: String,
    semantic_owner: String,
    migration_owners: Vec<String>,
    schema_source: String,
    writers: Vec<String>,
    #[serde(default)]
    lifecycle: Lifecycle,
    #[serde(default)]
    superseded_by: Vec<String>,
}

/// Lifecycle state of a declared relation. `retired` names a relation whose
/// static writer was deleted rather than delegated; it carries a forwarding
/// address and must be fully revoked.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Lifecycle {
    #[default]
    Active,
    Retired,
}

/// The ownership facts the protected-relation table is checked against.
#[derive(Debug)]
struct DeclaredRelation {
    ops: bool,
    installer: String,
    owner: String,
    writers_empty: bool,
    lifecycle: Lifecycle,
    superseded_by: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtectedRelationTable {
    schema_version: String,
    rows: Vec<ProtectedRelationRow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtectedRelationRow {
    scope: String,
    relation: String,
    ops: bool,
    installer: String,
    owner: String,
    mechanisms: Vec<String>,
    roles: Vec<ExecutingRole>,
    #[serde(rename = "author-reachable")]
    author_reachable: String,
    guards: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutingRole {
    role: String,
    basis: String,
    operations: Vec<String>,
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("conformance crate is under tests/conformance")
        .to_path_buf()
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> T {
    serde_json::from_str(
        &std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
}

fn sole_installer(relation: &str, owners: &[String]) -> String {
    assert_eq!(
        owners.len(),
        1,
        "{relation} must declare exactly one installer"
    );
    owners[0].clone()
}

fn source_scopes(sources: Vec<CanonicalSource>) -> BTreeMap<String, String> {
    sources
        .into_iter()
        .map(|source| (source.path, source.scope))
        .collect()
}

fn is_ops_artifact(schema_source: &str, scopes: &BTreeMap<String, String>) -> bool {
    scopes
        .get(schema_source)
        .unwrap_or_else(|| panic!("undeclared canonical source {schema_source}"))
        == "production-control-database-ops"
}

#[test]
fn protected_relation_table_matches_declared_ownership() {
    let repository = repository();
    let owners: OwnershipManifest = read_json(&repository.join(OWNERS_PATH));
    let table: ProtectedRelationTable = read_json(&repository.join(TABLE_PATH));
    assert_eq!(owners.schema_version, "0.1");
    assert_eq!(table.schema_version, "0.1");
    let scopes = source_scopes(owners.canonical_sources);

    let mut declared = owners
        .objects
        .into_iter()
        .map(|entry| {
            let scope = scopes
                .get(&entry.schema_source)
                .unwrap_or_else(|| panic!("undeclared source {}", entry.schema_source))
                .clone();
            (
                (scope, entry.id.clone()),
                DeclaredRelation {
                    ops: is_ops_artifact(&entry.schema_source, &scopes),
                    installer: sole_installer(&entry.id, &entry.migration_owners),
                    owner: entry.semantic_owner,
                    writers_empty: entry.writers.is_empty(),
                    lifecycle: entry.lifecycle,
                    superseded_by: entry.superseded_by,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    declared.extend(owners.families.into_iter().map(|family| {
        let scope = scopes
            .get(&family.schema_source)
            .unwrap_or_else(|| panic!("undeclared source {}", family.schema_source))
            .clone();
        (
            (scope, family.pattern.clone()),
            DeclaredRelation {
                ops: is_ops_artifact(&family.schema_source, &scopes),
                installer: sole_installer(&family.pattern, &family.migration_owners),
                owner: family.semantic_owner,
                writers_empty: family.writers.is_empty(),
                lifecycle: family.lifecycle,
                superseded_by: family.superseded_by,
            },
        )
    }));

    let mut previous_relation: Option<(&str, &str)> = None;
    let mut actual_relations = BTreeSet::new();
    for row in &table.rows {
        if let Some(previous) = previous_relation {
            assert!(
                previous < (row.scope.as_str(), row.relation.as_str()),
                "rows must be sorted and unique"
            );
        }
        previous_relation = Some((&row.scope, &row.relation));
        let expected = declared
            .get(&(row.scope.clone(), row.relation.clone()))
            .unwrap_or_else(|| {
                panic!(
                    "undeclared protected relation {} in {}",
                    row.relation, row.scope
                )
            });
        assert_eq!(
            (row.ops, &row.installer, &row.owner),
            (expected.ops, &expected.installer, &expected.owner)
        );
        let author_writable = row.author_reachable == AUTHOR_SQL_EXPOSURE;
        let granted_roles = row
            .roles
            .iter()
            .filter(|role| role.basis == "grant")
            .map(|role| role.role.as_str())
            .collect::<Vec<_>>();
        // A retired relation is one whose static writer was deleted rather than
        // delegated. It owes a forwarding address and a full revoke; it is not
        // tenant business state.
        if expected.lifecycle == Lifecycle::Retired {
            assert!(
                expected.writers_empty,
                "retired {} must not declare a static writer",
                row.relation
            );
            assert!(
                !expected.superseded_by.is_empty(),
                "retired {} must name the relation that took over",
                row.relation
            );
            assert!(
                granted_roles.is_empty(),
                "retired {} must be fully revoked, still granted to {granted_roles:?}",
                row.relation
            );
        }
        // `writers: []` is evidence, not a definition: it licenses author-SQL
        // ownership or a completed retirement, and exactly one of the two.
        if expected.writers_empty {
            let retired = expected.lifecycle == Lifecycle::Retired
                && !expected.superseded_by.is_empty()
                && granted_roles.is_empty();
            assert!(
                author_writable ^ retired,
                "{} has no static writer and must be either author-SQL writable or retired with a superseding relation and zero grants",
                row.relation
            );
        }
        assert!(
            !row.mechanisms.is_empty(),
            "{} has no mechanism",
            row.relation
        );
        assert!(
            !row.roles.is_empty(),
            "{} has no executing role",
            row.relation
        );
        assert!(
            !row.guards.is_empty(),
            "{} has no database guard",
            row.relation
        );
        assert!(row.mechanisms.is_sorted());
        assert!(row.guards.is_sorted());
        assert!(
            matches!(row.author_reachable.as_str(), "no" | AUTHOR_SQL_EXPOSURE),
            "{} has unknown author exposure",
            row.relation
        );
        let author_role_present = row.roles.iter().any(|role| role.role == "wamn_app");
        assert_eq!(
            author_writable, author_role_present,
            "{} author exposure does not match its grant row",
            row.relation
        );
        let mut previous_role: Option<(&str, &str)> = None;
        for role in &row.roles {
            assert!(matches!(role.basis.as_str(), "grant" | "owner"));
            assert!(!role.operations.is_empty());
            assert!(role.operations.is_sorted());
            if let Some(previous) = previous_role {
                assert!(previous < (role.role.as_str(), role.basis.as_str()));
            }
            previous_role = Some((&role.role, &role.basis));
        }
        actual_relations.insert((row.scope.clone(), row.relation.clone()));
    }
    let node_runs = table
        .rows
        .iter()
        .find(|row| {
            row.scope == "production-project-database" && row.relation == "wamn_run.node_runs"
        })
        .expect("node_runs is protected");
    let node_error_checks = node_runs
        .guards
        .iter()
        .filter(|guard| guard.starts_with("constraint:node_runs_error_kind_check;"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(node_error_checks, [NODE_ERROR_CHECK]);
    assert_eq!(
        table
            .rows
            .iter()
            .filter(|row| row.ops)
            .map(|row| (row.scope.as_str(), row.relation.as_str()))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([
            ("production-control-database-ops", "provisioning.copy_sagas"),
            ("production-control-database-ops", "provisioning.dumps"),
            (
                "production-control-database-ops",
                "provisioning.migration_confirmations",
            ),
        ])
    );
    assert_eq!(actual_relations, declared.into_keys().collect());
}

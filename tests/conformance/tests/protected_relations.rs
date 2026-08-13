use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

const OWNERS_PATH: &str = "architecture/state-owners.json";
const TABLE_PATH: &str = "architecture/protected-writes.json";
const AUTHOR_SQL_EXPOSURE: &str = "author SQL, RLS-bounded";

#[derive(Debug, Deserialize)]
struct OwnershipManifest {
    schema_version: String,
    objects: Vec<OwnershipEntry>,
    families: Vec<OwnershipFamily>,
}

#[derive(Debug, Deserialize)]
struct OwnershipEntry {
    id: String,
    semantic_owner: String,
    migration_owners: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OwnershipFamily {
    pattern: String,
    semantic_owner: String,
    migration_owners: Vec<String>,
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
    relation: String,
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

#[test]
fn protected_relation_table_matches_declared_ownership() {
    let repository = repository();
    let owners: OwnershipManifest = read_json(&repository.join(OWNERS_PATH));
    let table: ProtectedRelationTable = read_json(&repository.join(TABLE_PATH));
    assert_eq!(owners.schema_version, "0.1");
    assert_eq!(table.schema_version, "0.1");

    let mut declared = owners
        .objects
        .into_iter()
        .map(|entry| {
            (
                entry.id.clone(),
                (
                    sole_installer(&entry.id, &entry.migration_owners),
                    entry.semantic_owner,
                ),
            )
        })
        .collect::<BTreeMap<_, _>>();
    declared.extend(owners.families.into_iter().map(|family| {
        (
            family.pattern.clone(),
            (
                sole_installer(&family.pattern, &family.migration_owners),
                family.semantic_owner,
            ),
        )
    }));

    let mut previous_relation: Option<&str> = None;
    let mut actual_relations = BTreeSet::new();
    for row in &table.rows {
        if let Some(previous) = previous_relation {
            assert!(
                previous < row.relation.as_str(),
                "rows must be sorted and unique"
            );
        }
        previous_relation = Some(&row.relation);
        let expected = declared
            .get(&row.relation)
            .unwrap_or_else(|| panic!("undeclared protected relation {}", row.relation));
        assert_eq!((&row.installer, &row.owner), (&expected.0, &expected.1));
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
            row.author_reachable == AUTHOR_SQL_EXPOSURE,
            author_role_present,
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
        actual_relations.insert(row.relation.clone());
    }
    assert_eq!(actual_relations, declared.into_keys().collect());
}

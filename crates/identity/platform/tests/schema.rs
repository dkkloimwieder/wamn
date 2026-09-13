//! Drift guards for the platform identity schema in the T1 system database.

use std::path::Path;

use wamn_control_provision::PlatformComponent;

fn system_schema_sql() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../deploy/sql/system-schema.sql"),
    )
    .expect("read deploy/sql/system-schema.sql")
}

fn code_only(sql: &str) -> String {
    sql.lines()
        .map(|line| line.find("--").map_or(line, |index| &line[..index]))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn system_schema_contains_the_platform_identity_core() {
    let sql = code_only(&system_schema_sql());
    assert!(sql.contains("CREATE SCHEMA identity AUTHORIZATION wamn_system"));
    for table in ["principals", "project_roles", "pats"] {
        assert!(
            sql.contains(&format!("CREATE TABLE identity.{table}")),
            "missing identity.{table}"
        );
    }
    for literal in ["'human'", "'service'", "'active'", "'disabled'"] {
        assert!(sql.contains(literal), "missing identity literal {literal}");
    }
    assert!(sql.contains("UNIQUE (kind, subject)"));
    assert!(sql.contains("REFERENCES registry.projects (org, id) ON DELETE CASCADE"));
    for removed in [
        "identity.local_credentials",
        "identity.sessions",
        "password_hash",
        "cookie_hash",
        "csrf_hash",
    ] {
        assert!(
            !sql.contains(removed),
            "removed local-auth schema {removed}"
        );
    }
}

#[test]
fn system_schema_stores_personal_access_tokens_as_expirable_digests() {
    let sql = code_only(&system_schema_sql());
    assert!(sql.contains("token_prefix   text NOT NULL UNIQUE"));
    assert!(sql.contains("token_hash     text NOT NULL"));
    assert!(sql.contains("expires_at     timestamptz NOT NULL"));
    assert!(sql.contains("revoked_at     timestamptz,"));
    assert!(sql.contains("CHECK (token_prefix ~ '^[0-9a-f]{16}$')"));
    assert!(sql.contains("CHECK (token_hash ~ '^[0-9a-f]{64}$')"));
    assert!(sql.contains("CHECK (expires_at > created_at)"));
    assert!(sql.contains(
        "FOREIGN KEY (principal_id, principal_kind)\n        \
         REFERENCES identity.principals (id, kind) ON DELETE RESTRICT"
    ));
    assert!(sql.contains("CHECK (principal_kind IN ('human', 'service'))"));
}

/// The platform CHECK and the seeded row carry the id that
/// `PlatformComponent::Provisioning` derives. PostgreSQL has no UUIDv5, so this
/// test compares the static literals with the derivation.
#[test]
fn system_schema_pins_the_provisioning_principal_id() {
    let sql = code_only(&system_schema_sql());
    let provisioning = PlatformComponent::Provisioning;
    let name = provisioning.principal_name();
    let id = provisioning.principal_id();
    assert!(sql.contains("CHECK (kind IN ('human', 'service', 'platform'))"));
    assert!(sql.contains(&format!("('{name}', '{name}', '{id}'::uuid))")));
    assert!(sql.contains(&format!(
        "PERFORM pg_catalog.set_config('app.user_id', '{id}', true);"
    )));
    assert!(sql.contains(&format!(
        "VALUES ('{id}', 'platform',\n          '{name}', '{name}');"
    )));
}

/// Each identity authority relation carries the four stamp columns with no
/// default and one static stamp trigger.
#[test]
fn system_schema_stamps_the_identity_authority_relations() {
    let sql = code_only(&system_schema_sql());
    for table in [
        "principals",
        "project_roles",
        "pats",
        "project_env_memberships",
    ] {
        let start = sql
            .find(&format!("CREATE TABLE identity.{table} ("))
            .unwrap_or_else(|| panic!("missing identity.{table}"));
        let body = &sql[start..start + sql[start..].find("\n);").expect("table end")];
        for (column, column_type) in [
            ("created_at", "timestamptz"),
            ("created_by", "uuid"),
            ("updated_at", "timestamptz"),
            ("updated_by", "uuid"),
        ] {
            assert!(
                body.lines().any(|line| {
                    line.split_whitespace().collect::<Vec<_>>()
                        == [column, column_type, "NOT", "NULL,"]
                }),
                "identity.{table}.{column} is not a NOT NULL {column_type} with no default"
            );
        }
        assert_eq!(
            sql.matches(&format!(
                "CREATE TRIGGER record_history_stamp\n    \
                 BEFORE INSERT OR UPDATE ON identity.{table}\n    \
                 FOR EACH ROW\n    \
                 EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');"
            ))
            .count(),
            1,
            "identity.{table} needs exactly one stamp trigger"
        );
    }
    assert!(!sql.contains("assigned_at"));
    assert!(!sql.contains("granted_at"));
}

#[test]
fn system_schema_has_no_plaintext_identity_credential_column() {
    let sql = code_only(&system_schema_sql());
    for forbidden in [
        "password text",
        "secret text",
        "credential text",
        "token text",
        "token_secret",
        "token_plaintext",
    ] {
        assert!(
            !sql.contains(forbidden),
            "plaintext credential column {forbidden:?}"
        );
    }
}

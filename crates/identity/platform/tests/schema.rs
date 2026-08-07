//! Drift guards for the platform identity schema in the T1 system database.

use std::path::Path;

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
    for table in ["principals", "local_credentials", "project_roles", "pats"] {
        assert!(
            sql.contains(&format!("CREATE TABLE identity.{table}")),
            "missing identity.{table}"
        );
    }
    for literal in ["'human'", "'service'", "'active'", "'disabled'"] {
        assert!(sql.contains(literal), "missing identity literal {literal}");
    }
    assert!(sql.contains("UNIQUE (kind, subject)"));
    assert!(sql.contains("UNIQUE (id, kind)"));
    assert!(sql.contains("principal_kind text NOT NULL DEFAULT 'human'"));
    assert!(sql.contains("CHECK (principal_kind = 'human')"));
    assert!(sql.contains("REFERENCES identity.principals (id, kind) ON DELETE CASCADE"));
    assert!(sql.contains("password_hash text NOT NULL"));
    assert!(sql.contains("password_hash LIKE '$argon2id$%'"));
    assert!(sql.contains("REFERENCES registry.projects (org, id) ON DELETE CASCADE"));
}

#[test]
fn system_schema_stores_personal_access_tokens_as_expirable_digests() {
    let sql = code_only(&system_schema_sql());
    assert!(sql.contains("token_prefix text NOT NULL UNIQUE"));
    assert!(sql.contains("token_hash   text NOT NULL"));
    assert!(sql.contains("expires_at   timestamptz NOT NULL"));
    assert!(sql.contains("revoked_at   timestamptz,"));
    assert!(sql.contains("CHECK (token_prefix ~ '^[0-9a-f]{16}$')"));
    assert!(sql.contains("CHECK (token_hash ~ '^[0-9a-f]{64}$')"));
    assert!(sql.contains("CHECK (expires_at > created_at)"));
    assert!(sql.contains("REFERENCES identity.principals (id) ON DELETE RESTRICT"));
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

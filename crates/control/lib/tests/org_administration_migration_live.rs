//! Live test of `system/0007_org_administration.sql` (wamn-a40n.3): on a
//! control database installed before it, the migration changes the principal
//! type `human` to `user`, creates the org tables as a fresh install has them,
//! and gives the control family and the identity issuer the surfaces that
//! provisioning grants. The test holds the process lock of its server, because
//! the installers create cluster-wide roles.

use tokio_postgres::{Client, NoTls};
use wamn_control::provision_system::{ProvisionSystemRequest, provision_system};
use wamn_control_provision::identity_issuer::grant_identity_issuer_surface_sql;
use wamn_control_provision::sql::{ensure_workload_acl_role_sql, grant_control_surface_sql};
use wamn_control_provision::workload_role::WorkloadRoleFamily;
use wamn_test_infrastructure::locked_database;

const MIGRATION: &str =
    include_str!("../../../../deploy/sql/migrations/system/0007_org_administration.sql");
const PROVISIONING: &str = "770df186-ac15-579e-b46b-c297cae2011b";

/// The relations whose principal type 0007 changes.
const TYPED_RELATIONS: &str = "'identity.principals'::regclass, 'identity.pats'::regclass, \
    'identity.password_credentials'::regclass, 'identity.password_tokens'::regclass, \
    'identity.password_logins'::regclass, 'identity.project_env_memberships'::regclass, \
    'catalog.authoring_command_audit'::regclass";

/// The principal type as 0006 left it: `human` in every check, default and
/// foreign key, and in the principal lock.
const HUMAN_TYPE: &str = r#"
ALTER TABLE identity.pats DROP CONSTRAINT pats_principal_id_principal_type_fkey,
  DROP CONSTRAINT pats_principal_type_check;
ALTER TABLE identity.password_credentials
  DROP CONSTRAINT password_credentials_principal_id_principal_type_fkey,
  DROP CONSTRAINT password_credentials_principal_type_check;
ALTER TABLE identity.password_tokens
  DROP CONSTRAINT password_tokens_principal_id_principal_type_fkey,
  DROP CONSTRAINT password_tokens_principal_type_check;
ALTER TABLE identity.password_logins
  DROP CONSTRAINT password_logins_principal_id_principal_type_fkey,
  DROP CONSTRAINT password_logins_principal_type_check;
ALTER TABLE identity.project_env_memberships
  DROP CONSTRAINT project_env_memberships_principal_id_principal_type_fkey,
  DROP CONSTRAINT project_env_memberships_user_check;
ALTER TABLE identity.principals DROP CONSTRAINT principals_type_check,
  DROP CONSTRAINT principals_email_check;
ALTER TABLE identity.principals
  ADD CONSTRAINT principals_type_check CHECK (type IN ('human', 'service', 'platform')),
  ADD CONSTRAINT principals_email_check CHECK ((type = 'human') = (email IS NOT NULL)
    AND (email IS NULL OR (email ~ '^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$'
    AND char_length(email) <= 254)));
ALTER TABLE identity.pats
  ADD CONSTRAINT pats_principal_id_principal_type_fkey FOREIGN KEY (principal_id, principal_type)
    REFERENCES identity.principals (id, type) ON DELETE RESTRICT,
  ADD CONSTRAINT pats_principal_type_check CHECK (principal_type IN ('human', 'service'));
ALTER TABLE identity.password_credentials ALTER COLUMN principal_type SET DEFAULT 'human',
  ADD CONSTRAINT password_credentials_principal_type_check CHECK (principal_type = 'human'),
  ADD CONSTRAINT password_credentials_principal_id_principal_type_fkey
    FOREIGN KEY (principal_id, principal_type) REFERENCES identity.principals (id, type) ON DELETE RESTRICT;
ALTER TABLE identity.password_tokens ALTER COLUMN principal_type SET DEFAULT 'human',
  ADD CONSTRAINT password_tokens_principal_type_check CHECK (principal_type = 'human'),
  ADD CONSTRAINT password_tokens_principal_id_principal_type_fkey
    FOREIGN KEY (principal_id, principal_type) REFERENCES identity.principals (id, type) ON DELETE RESTRICT;
ALTER TABLE identity.password_logins ALTER COLUMN principal_type SET DEFAULT 'human',
  ADD CONSTRAINT password_logins_principal_type_check CHECK (principal_type = 'human'),
  ADD CONSTRAINT password_logins_principal_id_principal_type_fkey
    FOREIGN KEY (principal_id, principal_type) REFERENCES identity.principals (id, type) ON DELETE RESTRICT;
ALTER TABLE identity.project_env_memberships ALTER COLUMN principal_type SET DEFAULT 'human',
  ADD CONSTRAINT project_env_memberships_principal_id_principal_type_fkey
    FOREIGN KEY (principal_id, principal_type) REFERENCES identity.principals (id, type) ON DELETE CASCADE,
  ADD CONSTRAINT project_env_memberships_human_check CHECK (principal_type = 'human');
CREATE OR REPLACE FUNCTION identity.lock_password_principal(principal uuid) RETURNS boolean
LANGUAGE plpgsql SECURITY DEFINER SET search_path = pg_catalog AS $$
DECLARE eligible boolean;
BEGIN
    SELECT status = 'active' AND type = 'human' INTO eligible
    FROM identity.principals WHERE id = principal FOR UPDATE;
    RETURN COALESCE(eligible, false);
END;
$$;
ALTER TABLE catalog.authoring_command_audit
  DROP CONSTRAINT authoring_command_audit_principal_type_check,
  ADD CONSTRAINT authoring_command_audit_principal_type_check
    CHECK (principal_type IN ('human', 'service'));
"#;

/// A user as 0006 stored one, with a password and a token, and one audit row.
const HUMAN_ROWS: &str = r#"
SELECT pg_catalog.set_config('app.user_id', '00000000-0000-4000-8000-0000000000aa', true);
INSERT INTO identity.principals (id, type, subject, email, display_name)
  VALUES ('00000000-0000-4000-8000-000000000001', 'human', 'owner', 'owner@example.test', 'Owner');
INSERT INTO identity.password_credentials (principal_id, password_hash)
  VALUES ('00000000-0000-4000-8000-000000000001', '$argon2id$v=19$fixture');
INSERT INTO identity.pats (principal_id, principal_type, token_prefix, token_hash, label, expires_at)
  VALUES ('00000000-0000-4000-8000-000000000001', 'human', '0123456789abcdef',
          repeat('0', 64), 'fixture', now() + interval '1 day');
INSERT INTO catalog.authoring_command_audit (tenant_id, command_id, command_type, principal_id,
    principal_type, principal_subject, effective_role, org, project, environment, target_ref,
    request_hash, outcome_bytes)
  VALUES ('tenant', 'command', 'gate', '00000000-0000-4000-8000-000000000001', 'human',
          'owner', 'project-admin', 'acme', 'shop', 'dev', 'ref', 'sha256:' || repeat('0', 64),
          '\x01');
"#;

async fn connect(url: &str) -> Client {
    let (client, connection) = tokio_postgres::connect(url, NoTls)
        .await
        .expect("connect to the disposable database");
    tokio::spawn(async move {
        let _ = connection.await;
    });
    client
}

async fn column(client: &Client, statement: &str) -> Vec<String> {
    client
        .query(statement, &[])
        .await
        .expect(statement)
        .iter()
        .map(|row| row.get(0))
        .collect()
}

/// The columns, constraints and triggers of `relations`, and the principal
/// lock.
async fn definitions(client: &Client, relations: &str) -> Vec<String> {
    column(
        client,
        &format!(
            "SELECT entry FROM ( \
           SELECT 'column ' || c.oid::regclass::text || '.' || a.attname || ' ' \
                  || format_type(a.atttypid, a.atttypmod) || ' ' || a.attnotnull::text \
                  || ' ' || coalesce(pg_get_expr(d.adbin, d.adrelid), '') AS entry \
             FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
            WHERE c.oid IN ({relations}) \
              AND a.attnum > 0 AND NOT a.attisdropped \
           UNION ALL \
           SELECT 'constraint ' || conrelid::regclass::text || ' ' || conname || ' ' \
                  || pg_get_constraintdef(oid) \
             FROM pg_constraint \
            WHERE conrelid IN ({relations}) \
           UNION ALL \
           SELECT 'trigger ' || tgrelid::regclass::text || ' ' || pg_get_triggerdef(oid) \
             FROM pg_trigger \
            WHERE tgrelid IN ({relations}) \
              AND NOT tgisinternal \
           UNION ALL \
           SELECT 'function ' || pg_get_functiondef('identity.lock_password_principal(uuid)'::regprocedure) \
         ) q ORDER BY entry COLLATE \"C\""
        ),
    )
    .await
}

/// The columns, constraints and triggers of the two org tables.
async fn org_tables(client: &Client) -> Vec<String> {
    definitions(
        client,
        "'identity.org_memberships'::regclass, 'identity.org_roles'::regclass",
    )
    .await
}

/// Every privilege that names `role` in this database: on schemas, relations
/// and columns.
async fn surface(client: &Client, role: &str) -> Vec<String> {
    column(
        client,
        &format!(
            "WITH grantee AS (SELECT oid FROM pg_roles WHERE rolname = '{role}') \
             SELECT entry FROM ( \
               SELECT 'schema ' || n.nspname || ' ' || x.privilege_type AS entry \
                 FROM pg_namespace n, aclexplode(n.nspacl) x, grantee g WHERE x.grantee = g.oid \
               UNION ALL \
               SELECT 'table ' || c.oid::regclass::text || ' ' || x.privilege_type \
                 FROM pg_class c, aclexplode(c.relacl) x, grantee g WHERE x.grantee = g.oid \
               UNION ALL \
               SELECT 'column ' || c.oid::regclass::text || '.' || a.attname || ' ' || x.privilege_type \
                 FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid, \
                      aclexplode(a.attacl) x, grantee g WHERE x.grantee = g.oid \
             ) q ORDER BY entry COLLATE \"C\""
        ),
    )
    .await
}

#[tokio::test]
async fn the_migration_creates_the_org_tables_and_grants_the_provisioned_surfaces() {
    let url = locked_database::database(wamn_test_postgres::database);
    let client = connect(&url).await;
    client
        .batch_execute(
            "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
               CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
                 NOREPLICATION NOBYPASSRLS; \
             END IF; END $$;",
        )
        .await
        .expect("create the wamn_system role");
    provision_system(&ProvisionSystemRequest {
        system_database_url: url.to_string(),
        platform_domain: "wamn.example.test".to_owned(),
    })
    .await
    .expect("install the control store");
    let fresh = org_tables(&client).await;
    let fresh_typed = definitions(&client, TYPED_RELATIONS).await;

    // The database as 0006 left it: no org tables, the control family's
    // reads of issue 2, and an issuer that neither reads org roles nor
    // inserts a principal.
    client
        .batch_execute(&format!(
            "{control} {issuer} \
             DROP TABLE identity.org_roles, identity.org_memberships; \
             REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA identity, provisioning, registry \
               FROM wamn_control; \
             REVOKE ALL PRIVILEGES ON SCHEMA identity, provisioning, registry FROM wamn_control; \
             GRANT USAGE ON SCHEMA identity TO wamn_control; \
             GRANT SELECT ON identity.principals, identity.project_roles, identity.password_logins \
               TO wamn_control; \
             REVOKE INSERT (type, subject, email, display_name) ON identity.principals \
               FROM wamn_identity_issuer;",
            control = ensure_workload_acl_role_sql(WorkloadRoleFamily::Control),
            issuer = grant_identity_issuer_surface_sql(),
        ))
        .await
        .expect("make the database as 0006 left it");
    client
        .batch_execute(&format!("{HUMAN_TYPE} BEGIN; {HUMAN_ROWS} COMMIT;"))
        .await
        .expect("store the principal type as 0006 did");

    client
        .batch_execute(&format!(
            "BEGIN; SET LOCAL ROLE wamn_system; {MIGRATION} COMMIT;"
        ))
        .await
        .expect("apply system/0007 as wamn_system");
    assert_eq!(
        org_tables(&client).await,
        fresh,
        "the migration creates the org tables as a fresh install has them"
    );
    assert_eq!(
        definitions(&client, TYPED_RELATIONS).await,
        fresh_typed,
        "the migration changes the principal type as a fresh install has it"
    );
    assert_eq!(
        column(
            &client,
            "SELECT 'principal ' || type || ' ' || updated_by FROM identity.principals \
               WHERE id = '00000000-0000-4000-8000-000000000001' \
             UNION ALL SELECT 'password ' || principal_type || ' ' || updated_by \
               FROM identity.password_credentials \
             UNION ALL SELECT 'pat ' || principal_type || ' ' || updated_by FROM identity.pats \
             UNION ALL SELECT 'audit ' || principal_type FROM catalog.authoring_command_audit \
             ORDER BY 1"
        )
        .await,
        [
            "audit human".to_owned(),
            format!("password user {PROVISIONING}"),
            format!("pat user {PROVISIONING}"),
            format!("principal user {PROVISIONING}"),
        ],
        "the user rows change and stamp wamn:provisioning, and the audit row keeps its value"
    );
    let control = surface(&client, "wamn_control").await;
    let issuer = surface(&client, "wamn_identity_issuer").await;
    for expected in [
        "table identity.org_roles INSERT",
        "table identity.project_env_memberships DELETE",
        "table registry.projects SELECT",
    ] {
        assert!(
            control.iter().any(|entry| entry == expected),
            "the migration grants the control family {expected}: {control:#?}"
        );
    }
    for expected in [
        "column identity.org_roles.role SELECT",
        "column identity.principals.email INSERT",
    ] {
        assert!(
            issuer.iter().any(|entry| entry == expected),
            "the migration grants the identity issuer {expected}: {issuer:#?}"
        );
    }

    client
        .batch_execute(&format!(
            "{} {}",
            grant_control_surface_sql(),
            grant_identity_issuer_surface_sql()
        ))
        .await
        .expect("apply the provisioned surfaces");
    assert_eq!(
        surface(&client, "wamn_control").await,
        control,
        "the migration and provisioning grant the control family one surface"
    );
    assert_eq!(
        surface(&client, "wamn_identity_issuer").await,
        issuer,
        "the migration and provisioning grant the identity issuer one surface"
    );
}

//! Read PostgreSQL catalog facts for run-plane reconciliation.

use super::{BareSchemaName, generation_role_contract_violation_sql};

/// Security attributes of the host-only scenario-author role (zero or one row).
pub fn select_scenario_author_role_sql() -> &'static str {
    "SELECT rolcanlogin, rolsuper, rolcreatedb, rolcreaterole, rolinherit, \
            rolreplication, rolbypassrls \
       FROM pg_catalog.pg_roles WHERE rolname = 'wamn_scenario_author'"
}

/// Provisioning-owned writer role attributes plus ownership/membership/CONNECT.
pub fn select_effect_writer_role_sql() -> String {
    format!(
        "SELECT role.rolcanlogin, role.rolsuper, role.rolcreatedb, role.rolcreaterole, \
            role.rolinherit, role.rolreplication, role.rolbypassrls, \
            pg_catalog.has_database_privilege(role.oid, current_database(), 'CONNECT'), \
            EXISTS (SELECT 1 FROM pg_catalog.pg_class WHERE relowner = role.oid) \
              OR EXISTS (SELECT 1 FROM pg_catalog.pg_namespace WHERE nspowner = role.oid) \
              OR EXISTS (SELECT 1 FROM pg_catalog.pg_proc WHERE proowner = role.oid) \
              OR EXISTS (SELECT 1 FROM pg_catalog.pg_database WHERE datdba = role.oid), \
            EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members WHERE member = role.oid) \
              OR EXISTS ( \
                   SELECT 1 FROM pg_catalog.pg_auth_members AS membership \
                   JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member \
                   WHERE membership.roleid = role.oid \
                     AND (member.rolname !~ '^wamn_effect_writer_[0-9a-f]{{40}}_[ab]$' \
                          OR NOT member.rolcanlogin OR member.rolsuper \
                          OR member.rolcreatedb OR member.rolcreaterole \
                          OR NOT member.rolinherit OR member.rolreplication \
                          OR member.rolbypassrls)) \
              OR {generation_contract} \
       FROM pg_catalog.pg_roles AS role \
      WHERE role.rolname = 'wamn_effect_writer'",
        generation_contract = generation_role_contract_violation_sql(),
    )
}

/// Exact direct writer USAGE/no-PUBLIC boundary plus effective CREATE.
pub fn select_effect_writer_schema_privileges_sql() -> &'static str {
    "SELECT COALESCE( \
              EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE( \
                        namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                       WHERE acl.grantee = role.oid AND acl.privilege_type = 'USAGE') \
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE( \
                        namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                       WHERE acl.grantee = 0 \
                         AND acl.privilege_type IN ('USAGE', 'CREATE')), false), \
            COALESCE(pg_catalog.has_schema_privilege(role.oid, namespace.oid, 'CREATE'), false) \
       FROM (SELECT 1) AS singleton \
       LEFT JOIN pg_catalog.pg_roles AS role \
         ON role.rolname = 'wamn_effect_writer' \
       LEFT JOIN pg_catalog.pg_namespace AS namespace ON namespace.nspname = $1"
}

/// Direct grants on the three immutable effect-writer tables.
pub fn select_effect_table_privileges_sql() -> &'static str {
    "SELECT table_name, grantee, privilege_type \
       FROM information_schema.table_privileges \
      WHERE table_schema = $1 \
        AND table_name IN ('effect_attempts', 'effect_attempt_dispatches', \
                           'effect_attempt_outcomes') \
        AND grantee IN ('PUBLIC', 'wamn_app', 'wamn_scenario_author', \
                        'wamn_effect_writer') \
      ORDER BY table_name, grantee, privilege_type"
}

/// Effective grants and owners on the effect-writer table boundary.
pub fn select_effect_table_effective_privileges_sql() -> &'static str {
    "SELECT relation.relname, actor.rolname, privilege.name, owner.rolname \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS owner ON owner.oid = relation.relowner \
       CROSS JOIN pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('DELETE'::text), ('TRUNCATE'::text), \
                          ('REFERENCES'::text), ('TRIGGER'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname IN ('effect_attempts', 'effect_attempt_dispatches', \
                                 'effect_attempt_outcomes') \
        AND actor.rolname IN ('wamn_app', 'wamn_scenario_author', 'wamn_effect_writer') \
        AND pg_catalog.has_table_privilege(actor.oid, relation.oid, privilege.name) \
      ORDER BY relation.relname, actor.rolname, privilege.name"
}

/// Effective column grants on the effect-writer table boundary.
pub fn select_effect_table_effective_column_privileges_sql() -> &'static str {
    "SELECT relation.relname, actor.rolname, privilege.name \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       CROSS JOIN pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('REFERENCES'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname IN ('effect_attempts', 'effect_attempt_dispatches', \
                                 'effect_attempt_outcomes') \
        AND actor.rolname IN ('wamn_app', 'wamn_scenario_author', 'wamn_effect_writer') \
        AND pg_catalog.has_any_column_privilege(actor.oid, relation.oid, privilege.name) \
      ORDER BY relation.relname, actor.rolname, privilege.name"
}

/// Effective table privileges of the private writer on run-authority tables.
pub fn select_effect_writer_run_table_privileges_sql() -> &'static str {
    "SELECT relation.relname, privilege.name \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS actor ON actor.rolname = 'wamn_effect_writer' \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('DELETE'::text), ('TRUNCATE'::text), \
                          ('REFERENCES'::text), ('TRIGGER'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname IN ('runs', 'run_queue') \
        AND pg_catalog.has_table_privilege(actor.oid, relation.oid, privilege.name) \
      ORDER BY relation.relname, privilege.name"
}

/// Effective column privileges of the private writer on run-authority tables.
pub fn select_effect_writer_run_column_privileges_sql() -> &'static str {
    "SELECT relation.relname, attribute.attname, privilege.name \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = relation.oid \
       JOIN pg_catalog.pg_roles AS actor ON actor.rolname = 'wamn_effect_writer' \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('REFERENCES'::text)) AS privilege(name) \
      WHERE namespace.nspname = $1 AND relation.relkind = 'r' \
        AND relation.relname IN ('runs', 'run_queue') \
        AND attribute.attnum > 0 AND NOT attribute.attisdropped \
        AND pg_catalog.has_column_privilege( \
              actor.oid, relation.oid, attribute.attname, privilege.name) \
      ORDER BY relation.relname, attribute.attnum, privilege.name"
}

// --- Dispatcher read-principal observation (wamn-0h0g.12.123) ----------------
//
// `$2` carries the ROLE NAME instead of the inline literal every neighbouring
// query uses. The name and the grant text both belong to
// `wamn_control_provision`, which this pure crate deliberately does not depend
// on; parameterizing keeps ONE encoding of the principal rather than a second
// copy that can drift from the builder that grants it.
//
// **Both queries read DIRECT `aclitem` entries, never `has_*_privilege`.** The
// repair is `REVOKE ALL … FROM <reader>` followed by narrow `GRANT`s, which can
// only move the reader's OWN acl entries. An effective-privilege observation
// also sees whatever the reader reaches through `PUBLIC` or through a group,
// which the repair cannot revoke — so it would encode a state the grant can
// never satisfy, drift would stay true forever, and the reconciler would plan
// the repair on every pass without converging. That is wamn-0h0g.12.40
// exactly, and it was only ever caught by a live gate.

/// Direct schema-level privileges held by the dispatcher read principal, plus
/// whether that cluster-global role exists at all. `$1` is the run-plane
/// schema, `$2` the role name.
pub fn select_dispatch_reader_schema_privileges_sql() -> &'static str {
    "SELECT reader.oid IS NOT NULL, \
            ARRAY( \
              SELECT acl.privilege_type \
                FROM pg_catalog.pg_namespace AS namespace \
                CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE( \
                  namespace.nspacl, \
                  pg_catalog.acldefault('n', namespace.nspowner))) AS acl \
               WHERE namespace.nspname = $1 \
                 AND acl.grantee = reader.oid \
               ORDER BY 1) \
       FROM (SELECT 1) AS singleton \
       LEFT JOIN pg_catalog.pg_roles AS reader ON reader.rolname = $2"
}

/// Direct table-level privileges held by the dispatcher read principal in the
/// run-plane schema. `$1` is the schema, `$2` the role name.
///
/// The `relkind` filter is the OTHER half of "observe only what the repair can
/// reach": `REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA` covers tables,
/// partitioned tables, views, materialized views and foreign tables — and NOT
/// sequences. Observing a sequence grant here would be drift no `GRANT`/`REVOKE`
/// in the repair could ever clear.
pub fn select_dispatch_reader_table_privileges_sql() -> &'static str {
    "SELECT relation.relname, acl.privilege_type \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS reader ON reader.rolname = $2 \
       CROSS JOIN LATERAL pg_catalog.aclexplode(relation.relacl) AS acl \
      WHERE namespace.nspname = $1 \
        AND relation.relkind IN ('r', 'p', 'v', 'm', 'f') \
        AND acl.grantee = reader.oid \
      ORDER BY 1, 2"
}

/// Whether guest-visible `wamn_app` inherits the host-only author role.
pub fn select_app_scenario_author_membership_sql() -> &'static str {
    "SELECT COALESCE(pg_catalog.pg_has_role( \
        (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app'), \
        (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'wamn_scenario_author'), \
        'MEMBER'), false)"
}

/// Direct grants on the managed authoring/release boundary. `$1` is the project
/// run-plane schema; PUBLIC is included so stale ambient writes are visible.
pub fn select_authoring_table_privileges_sql() -> &'static str {
    "SELECT table_schema, table_name, grantee, privilege_type \
       FROM information_schema.table_privileges \
      WHERE grantee IN ('PUBLIC', 'wamn_app', 'wamn_scenario_author', \
                        'wamn_effect_writer') \
        AND ((table_schema = 'catalog' AND table_name IN \
              ('packages', 'package_migrations', 'effective_releases', \
               'effective_release_packages', 'effective_release_heads', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings')) \
          OR (table_schema = $1 AND table_name IN ('environment_policies', 'runs'))) \
      ORDER BY table_schema, table_name, grantee, privilege_type"
}

/// Effective privileges for both boundary roles, including authority obtained
/// through inherited groups or table ownership. Direct ACL convergence cannot
/// safely revoke an arbitrary group or reassign an owner, so the planner uses
/// this observation to install a post-repair refusal instead of false-cleaning.
pub fn select_authoring_effective_table_privileges_sql() -> &'static str {
    "SELECT namespace.nspname, relation.relname, actor.rolname, privilege.name \
       FROM pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('DELETE'::text), ('TRUNCATE'::text), \
                          ('REFERENCES'::text), ('TRIGGER'::text)) \
                  AS privilege(name) \
       JOIN pg_catalog.pg_class AS relation ON relation.relkind = 'r' \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
      WHERE actor.rolname IN ('wamn_app', 'wamn_scenario_author', \
                              'wamn_effect_writer') \
        AND ((namespace.nspname = 'catalog' AND relation.relname IN \
              ('packages', 'package_migrations', 'effective_releases', \
               'effective_release_packages', 'effective_release_heads', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings')) \
          OR (namespace.nspname = $1 \
              AND relation.relname IN ('environment_policies', 'runs'))) \
        AND pg_catalog.has_table_privilege( \
              actor.oid, relation.oid, privilege.name) \
      ORDER BY namespace.nspname, relation.relname, actor.rolname, privilege.name"
}

/// Effective per-column read/mutation/reference authority. Table-level
/// privileges also appear here and are expected when present in the table
/// spec; the drift of interest is a surviving column grant not represented by
/// that spec.
pub fn select_authoring_effective_column_privileges_sql() -> &'static str {
    "SELECT namespace.nspname, relation.relname, actor.rolname, privilege.name \
       FROM pg_catalog.pg_roles AS actor \
       CROSS JOIN (VALUES ('SELECT'::text), ('INSERT'::text), ('UPDATE'::text), \
                          ('REFERENCES'::text)) AS privilege(name) \
       JOIN pg_catalog.pg_class AS relation ON relation.relkind = 'r' \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
      WHERE actor.rolname IN ('wamn_app', 'wamn_scenario_author', \
                              'wamn_effect_writer') \
        AND ((namespace.nspname = 'catalog' AND relation.relname IN \
              ('packages', 'package_migrations', 'effective_releases', \
               'effective_release_packages', 'effective_release_heads', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings')) \
          OR (namespace.nspname = $1 \
              AND relation.relname IN ('environment_policies', 'runs'))) \
        AND pg_catalog.has_any_column_privilege( \
              actor.oid, relation.oid, privilege.name) \
      ORDER BY namespace.nspname, relation.relname, actor.rolname, privilege.name"
}

/// Owners of the managed authoring/release boundary. PostgreSQL ownership is
/// stronger than an ACL and can remain after every direct privilege is
/// revoked, so it is observed independently of `has_table_privilege`.
pub fn select_authoring_table_owners_sql() -> &'static str {
    "SELECT namespace.nspname, relation.relname, owner.rolname \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
       JOIN pg_catalog.pg_roles AS owner ON owner.oid = relation.relowner \
      WHERE relation.relkind = 'r' \
        AND ((namespace.nspname = 'catalog' AND relation.relname IN \
              ('packages', 'package_migrations', 'effective_releases', \
               'effective_release_packages', 'effective_release_heads', \
               'connection_requirements', 'connection_instances', \
               'connection_generations', 'connection_bindings')) \
          OR (namespace.nspname = $1 \
              AND relation.relname IN ('environment_policies', 'runs'))) \
      ORDER BY namespace.nspname, relation.relname"
}

/// Effective guest authority on the retired run-write surface.
///
/// Table privileges, the historical `capture_mode` carrier, and all column
/// privileges are observed separately so removing a table grant cannot hide a
/// surviving column grant. The final value is true only when no column remains
/// writable by either boundary role or `PUBLIC`.
pub fn select_run_capture_privileges_sql() -> String {
    "WITH target AS ( \
           SELECT pg_catalog.to_regclass(pg_catalog.format('%I.runs', $1::text)) AS oid \
         ), boundary_roles AS ( \
           SELECT oid FROM pg_catalog.pg_roles \
            WHERE rolname IN ('wamn_app','wamn_scenario_author') \
         ), capture AS ( \
           SELECT attribute.attnum \
             FROM target \
             JOIN pg_catalog.pg_attribute AS attribute \
               ON attribute.attrelid = target.oid \
              AND attribute.attname = 'capture_mode' \
              AND attribute.attnum > 0 AND NOT attribute.attisdropped \
         ) \
         SELECT \
           (EXISTS ( \
             SELECT 1 \
               FROM boundary_roles actor \
               CROSS JOIN unnest(ARRAY['INSERT','UPDATE']) privilege \
              WHERE pg_catalog.has_table_privilege( \
                actor.oid, (SELECT oid FROM target), privilege)) \
            OR EXISTS ( \
              SELECT 1 \
                FROM target \
                JOIN pg_catalog.pg_class relation ON relation.oid = target.oid \
                CROSS JOIN LATERAL pg_catalog.aclexplode( \
                  COALESCE(relation.relacl, \
                           pg_catalog.acldefault('r', relation.relowner))) acl \
               WHERE acl.grantee = 0 \
                 AND acl.privilege_type IN ('INSERT','UPDATE'))), \
           (EXISTS ( \
              SELECT 1 \
                FROM boundary_roles actor \
                CROSS JOIN unnest(ARRAY['INSERT','UPDATE']) privilege \
               WHERE pg_catalog.has_column_privilege( \
                 actor.oid, (SELECT oid FROM target), \
                 (SELECT attnum FROM capture), privilege)) \
            OR EXISTS ( \
              SELECT 1 \
                FROM target \
                JOIN pg_catalog.pg_attribute attribute \
                  ON attribute.attrelid = target.oid \
                 AND attribute.attnum = (SELECT attnum FROM capture) \
                CROSS JOIN LATERAL \
                  pg_catalog.aclexplode(attribute.attacl) acl \
               WHERE acl.grantee = 0 \
                 AND acl.privilege_type IN ('INSERT','UPDATE'))), \
           (NOT EXISTS ( \
              SELECT 1 \
                FROM target \
                JOIN pg_catalog.pg_attribute AS attribute \
                  ON attribute.attrelid = target.oid \
                 AND attribute.attnum > 0 AND NOT attribute.attisdropped \
                CROSS JOIN boundary_roles AS actor \
                CROSS JOIN unnest(ARRAY['INSERT','UPDATE']) privilege \
               WHERE pg_catalog.has_column_privilege( \
                 actor.oid, attribute.attrelid, attribute.attnum, privilege)) \
            AND NOT EXISTS ( \
              SELECT 1 \
                FROM target \
                JOIN pg_catalog.pg_attribute AS attribute \
                  ON attribute.attrelid = target.oid \
                 AND attribute.attnum > 0 AND NOT attribute.attisdropped \
                CROSS JOIN LATERAL pg_catalog.aclexplode(attribute.attacl) acl \
               WHERE acl.grantee = 0 \
                 AND acl.privilege_type IN ('INSERT','UPDATE')))"
        .to_string()
}

/// Revocable guest-visible authority on `run_queue` from `PUBLIC` or a direct
/// `wamn_app` ACL. An absent role or relation yields false for from-zero plans.
pub fn select_app_run_queue_authority_sql() -> &'static str {
    "WITH target AS ( \
       SELECT pg_catalog.to_regclass( \
                pg_catalog.format('%I.run_queue', $1::text)) AS oid \
     ), app AS ( \
       SELECT oid FROM pg_catalog.pg_roles WHERE rolname = 'wamn_app' \
     ) \
     SELECT EXISTS ( \
       SELECT 1 \
         FROM target \
         JOIN pg_catalog.pg_class AS relation ON relation.oid = target.oid \
         CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE( \
           relation.relacl, pg_catalog.acldefault('r', relation.relowner))) AS acl \
        WHERE acl.grantee = 0 OR acl.grantee = (SELECT oid FROM app) \
     ) OR EXISTS ( \
       SELECT 1 \
         FROM target \
         JOIN pg_catalog.pg_attribute AS attribute ON attribute.attrelid = target.oid \
         CROSS JOIN LATERAL pg_catalog.aclexplode(attribute.attacl) AS acl \
        WHERE attribute.attnum > 0 AND NOT attribute.attisdropped \
          AND (acl.grantee = 0 OR acl.grantee = (SELECT oid FROM app)) \
     )"
}

/// Effective schema USAGE for the host-only author role on catalog and `$1`.
/// OID overloads make an absent role/schema a false row rather than an error,
/// preserving strictly read-only from-zero dry runs.
pub fn select_scenario_author_schema_usage_sql() -> &'static str {
    "SELECT target.schema_name, \
            COALESCE(pg_catalog.has_schema_privilege( \
                author.oid, namespace.oid, 'USAGE'), false) \
       FROM (VALUES ('catalog'::text), ($1::text)) AS target(schema_name) \
       LEFT JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.nspname = target.schema_name \
       LEFT JOIN pg_catalog.pg_roles AS author \
         ON author.rolname = 'wamn_scenario_author' \
      ORDER BY target.schema_name"
}

/// Every ordinary table + column in `$1`: `(relname, attname, not-null,
/// has-default, formatted-type)` in attnum order.
pub fn select_schema_columns_sql() -> &'static str {
    "SELECT c.relname, a.attname, a.attnotnull, ad.adbin IS NOT NULL, \
            pg_catalog.format_type(a.atttypid, a.atttypmod) FROM pg_class c \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     JOIN pg_attribute a ON a.attrelid = c.oid \
     LEFT JOIN pg_attrdef ad ON ad.adrelid = a.attrelid AND ad.adnum = a.attnum \
     WHERE n.nspname = $1 AND c.relkind = 'r' AND a.attnum > 0 AND NOT a.attisdropped \
     ORDER BY c.relname, a.attnum"
}

/// ENABLE/FORCE flags on the projected environment-policy relation.
pub fn select_environment_policy_row_security_sql() -> &'static str {
    "SELECT relation.relrowsecurity, relation.relforcerowsecurity \
       FROM pg_catalog.pg_class AS relation \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
      WHERE namespace.nspname = $1 \
        AND relation.relname = 'environment_policies' \
        AND relation.relkind = 'r'"
}

/// Every policy on the projected environment-policy relation, including the
/// fields whose widening can change the visible tenant set.
pub fn select_environment_policy_policies_sql() -> &'static str {
    "SELECT policy.polname, \
            CASE policy.polcmd WHEN 'r' THEN 'select' WHEN 'a' THEN 'insert' \
              WHEN 'w' THEN 'update' WHEN 'd' THEN 'delete' ELSE 'all' END, \
            policy.polpermissive, \
            ARRAY(SELECT CASE role_oid WHEN 0 THEN 'PUBLIC' \
                              ELSE pg_catalog.pg_get_userbyid(role_oid) END \
                    FROM unnest(policy.polroles) AS role_oid ORDER BY 1), \
            pg_catalog.pg_get_expr(policy.polqual, policy.polrelid, true), \
            pg_catalog.pg_get_expr(policy.polwithcheck, policy.polrelid, true) \
       FROM pg_catalog.pg_policy AS policy \
       JOIN pg_catalog.pg_class AS relation ON relation.oid = policy.polrelid \
       JOIN pg_catalog.pg_namespace AS namespace \
         ON namespace.oid = relation.relnamespace \
      WHERE namespace.nspname = $1 \
        AND relation.relname = 'environment_policies' \
        AND relation.relkind = 'r' \
      ORDER BY policy.polname"
}

/// Count persisted flow graphs that carry either retired top-level queue key.
pub fn count_retired_authored_ordering_rows_sql(schema: &BareSchemaName) -> String {
    format!(
        "SELECT count(*) FROM {}.flows \
          WHERE graph_json ? 'ordering' OR graph_json ? 'partition-policy'",
        schema.quoted()
    )
}

/// Every index in `$1`: `(indexname, indexdef)`.
pub fn select_schema_indexes_sql() -> &'static str {
    "SELECT indexname, indexdef FROM pg_indexes WHERE schemaname = $1"
}

/// Every CHECK on an ordinary table in `$1`: `(table, name, canonical def)`.
pub fn select_schema_checks_sql() -> &'static str {
    "SELECT c.relname, con.conname, pg_get_constraintdef(con.oid, true) \
     FROM pg_constraint con \
     JOIN pg_class c ON c.oid = con.conrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND c.relkind = 'r' AND con.contype = 'c' \
     ORDER BY c.relname, con.conname"
}

/// Every named foreign key on an ordinary table in `$1`.
pub fn select_schema_foreign_keys_sql() -> &'static str {
    "SELECT c.relname, con.conname, pg_get_constraintdef(con.oid, true) \
     FROM pg_constraint con \
     JOIN pg_class c ON c.oid = con.conrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND c.relkind = 'r' AND con.contype = 'f' \
     ORDER BY c.relname, con.conname"
}

/// Every non-internal trigger in `$1`: `(table, name, canonical def)`. A
/// non-origin enablement mode is suffixed so disabled/replica-only guards
/// cannot compare equal to the enabled schema of record.
pub fn select_schema_triggers_sql() -> &'static str {
    "SELECT c.relname, t.tgname, \
            CASE WHEN t.tgenabled = 'O' THEN pg_get_triggerdef(t.oid, true) \
                 ELSE pg_get_triggerdef(t.oid, true) || ' /* trigger-mode:' || t.tgenabled::text || ' */' \
            END \
     FROM pg_trigger t \
     JOIN pg_class c ON c.oid = t.tgrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND NOT t.tgisinternal \
     ORDER BY c.relname, t.tgname"
}

/// Tables in `$1` carrying the legacy `wamn_outbox_event` trigger.
pub fn select_outbox_trigger_tables_sql() -> &'static str {
    "SELECT c.relname FROM pg_trigger t \
     JOIN pg_class c ON c.oid = t.tgrelid \
     JOIN pg_namespace n ON n.oid = c.relnamespace \
     WHERE n.nspname = $1 AND t.tgname = 'wamn_outbox_event' AND NOT t.tgisinternal"
}

/// Whether the legacy `wamn_outbox_event()` function exists in `$1`.
pub fn select_outbox_function_present_sql() -> &'static str {
    "SELECT EXISTS ( SELECT FROM pg_proc p \
     JOIN pg_namespace n ON n.oid = p.pronamespace \
     WHERE n.nspname = $1 AND p.proname = 'wamn_outbox_event' )"
}

/// Whether the per-database `catalog` metadata schema exists.
pub fn catalog_schema_present_sql() -> &'static str {
    "SELECT EXISTS ( SELECT FROM pg_namespace WHERE nspname = 'catalog' )"
}

/// Rows in `catalog.event_registrations` carrying a retired declaration key.
///
/// The shell runs this only when the table was observed present.
pub fn count_stale_registration_keys_sql() -> &'static str {
    "SELECT count(*) FROM catalog.event_registrations \
     WHERE registration ?| ARRAY['state', 'partition-key']"
}

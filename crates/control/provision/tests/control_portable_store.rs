//! Control portable-store schema and PostgreSQL 18 proofs (wamn-0h0g.9.9), plus
//! the author-authority class the wamn-0h0g.8.18 residency move grants on it.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const CURRENT_DATABASE_PUBLIC_CONNECT_SQL: &str =
    include_str!("../../../../test-support/fixtures/sql/current-database-public-connect.sql");

fn ddl() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../deploy/sql/control-portable-store.sql"),
    )
    .expect("read control portable-store DDL")
}

#[test]
fn portable_store_record_is_exact_and_storage_only() {
    let sql = ddl();
    assert_eq!(wamn_control_provision::CONTROL_PORTABLE_STORE_SQL, sql);
    assert_eq!(
        wamn_control_provision::CONTROL_BOOTSTRAP_SQL,
        [
            wamn_control_provision::SYSTEM_SCHEMA_SQL,
            wamn_control_provision::CONTROL_PORTABLE_STORE_SQL
        ]
    );
    for relation in [
        "catalog.catalogs",
        "catalog.flow_artifacts",
        "catalog.releases",
        "catalog.release_flows",
        "catalog.catalog_heads",
        "catalog.flow_drafts",
        "catalog.release_exposure_manifests",
        "catalog.release_sources",
        "catalog.release_attachments",
        "catalog.connection_requirements",
        "catalog.authoring_command_audit",
        "wamn_run.authoring_test_run_reservations",
        "wamn_run.authoring_test_case_runs",
        "wamn_run.authoring_test_reports",
        "catalog.deployment_attestations",
        // wamn-0h0g.8.18: the owner-maintained login-identity-to-tenant mapping.
        "wamn_authority.author_login_tenants",
    ] {
        assert!(
            sql.contains(&format!("CREATE TABLE IF NOT EXISTS {relation}")),
            "missing control portable relation {relation}"
        );
    }

    for excluded in [
        // wamn-pm7k: the draft concept died with the pivot — the wiring
        // document IS the validated artifact and its hash IS the identity.
        "catalog.validated_flow_drafts",
        // wamn-0h0g.8.5.5: gate cases are effect-free by contract, so a gate
        // reaches no connection and this relation's concept is void. It never
        // had production DML in any plane.
        "catalog.draft_safe_connection_grants",
        // wamn-0h0g.26.16: flow-shaped release TEST EVIDENCE named a release
        // member by `flow_id` under a retired identity, and nothing in the
        // workspace ever executed its registrar.
        "catalog.release_flow_test_evidence",
        "catalog.attachment_activation",
        "catalog.attachment_tombstones",
        "catalog.connection_instances",
        "catalog.connection_generations",
        "catalog.connection_bindings",
        "catalog.connection_generation_retention",
        "catalog.schema_migrations",
        "catalog.entities",
        "catalog.event_registrations",
        "wamn_run.runs",
        "wamn_run.authoring_test_sets",
    ] {
        assert!(
            !sql.contains(&format!("CREATE TABLE IF NOT EXISTS {excluded}")),
            "project/runtime relation leaked into the control record: {excluded}"
        );
    }

    // wamn-0h0g.26.16: the evidence row and its registrar are gone, so neither
    // the tested-resolution carrier nor the report coordinate it resolved may
    // reappear anywhere in the artifact.
    assert!(!sql.contains("tested_resolution_map"));
    assert!(!sql.contains("register_release_flow_test_evidence("));
    assert!(!sql.contains("CREATE TABLE IF NOT EXISTS catalog.release_flow_test_evidence"));
    assert!(!sql.contains("REFERENCES catalog.validated_flow_drafts"));
    assert!(sql.contains("DROP TABLE catalog.validated_flow_drafts RESTRICT"));
    assert!(sql.contains("DROP TABLE catalog.draft_safe_connection_grants RESTRICT"));
    assert!(!sql.contains("CREATE TABLE IF NOT EXISTS catalog.execution_bundles"));
    assert!(sql.contains("DROP TABLE catalog.execution_bundles RESTRICT"));
    assert!(!sql.contains("REFERENCES registry."));
    assert!(!sql.contains("REFERENCES catalog.connection_generations"));
    // wamn-0h0g.8.18 ended the dormancy for exactly ONE principal. Everything the
    // store used to assert by "there are no grants at all" is now asserted as an
    // exact set, by `control_author_authority_is_the_exact_ratified_class` below.
    assert!(!sql.contains("GRANT DELETE"));
    assert!(!sql.contains("GRANT ALL"));
    assert!(!sql.contains("guard_authoring_test_orchestration_write"));
    // The test-set store deletes with its size cap, hash, and FKs
    // (wamn-0h0g.15.27); a draft's own `cases` are the only test source.
    assert!(!sql.contains("authoring_test_sets"));
    assert!(!sql.contains("REFERENCES wamn_run.authoring_test_sets"));
    // A hash that names nothing is not evidence (wamn-0h0g.13.56). The evidence
    // carrier itself left with wamn-0h0g.26.16; what remains is the retained
    // record tables' arm, which refuses an already-provisioned database still
    // carrying the column (wamn-0h0g.15.91). The name may therefore appear only
    // there — never in a declaration, a function signature, or a column fact.
    assert!(!sql.contains("p_test_set_hash"));
    assert!(!sql.contains("'test_set_hash:text:true'"));
    assert!(
        !sql.lines()
            .any(|line| line.trim_start().starts_with("test_set_hash ")),
        "no relation may declare the retired test_set_hash column"
    );
    // The retirement block is the control plane's only cutover path
    // (wamn-0h0g.15.91); a mutation that stops retiring is invisible to the live
    // gate, which applies to a fresh database and so never takes a probe's true
    // branch.
    for retirement in [
        "DROP FUNCTION IF EXISTS catalog.register_deployment_attestation(",
        "DROP TABLE catalog.release_flow_test_evidence RESTRICT;",
        "AND routine.proname = 'register_release_flow_test_evidence'",
        "DROP COLUMN deployed_resolution_map;",
        "'control-portable-retired-test-set-lineage-requires-reprovision'",
        "'control-portable-retired-audit-ledger-requires-reprovision'",
        "'control-portable-retired-release-members-requires-reprovision'",
    ] {
        assert!(sql.contains(retirement), "missing retirement: {retirement}");
    }
    // wamn-0h0g.15.159: membership is row-per-member in catalog.release_flows.
    // The snapshot column survives only where the retirement arm above names it
    // to refuse an already-provisioned database — never in a declaration, a
    // function signature, or an asserted column fact.
    assert!(!sql.contains("p_members_json"));
    assert!(!sql.contains("'members_json:jsonb:true'"));
    assert!(
        !sql.lines()
            .any(|line| line.trim_start().starts_with("members_json ")),
        "no relation may declare the retired members_json column"
    );
    // wamn-0h0g.15.32: the attestation fingerprint's ordering is now CONSTRUCTED
    // stable rather than accidentally so. Pinned here because no mutant can prove
    // it — the nine constraint definitions sort identically under C and
    // en_US.UTF-8, so REMOVING the collation leaves the fingerprint byte-identical
    // and the live gate green.
    assert!(sql.contains(
        r#"con.contype::text || ':' || pg_get_constraintdef(con.oid, true),
        E'\n' ORDER BY (con.contype::text || ':'
        || pg_get_constraintdef(con.oid, true)) COLLATE "C""#
    ));
    // wamn-0h0g.26.16: every generation of the release-evidence constraint
    // fingerprint leaves with the relation it hashed, the live one included.
    for retired_evidence_fingerprint in [
        "ca7165425f601f1ae9e6140b53aa49c2d83a24add8b2192c8474f9bfa5d75eaf",
        "96216cbdb364cd136ed8d1e925673cc8870beb1d15f9b016c7268b78066ac0a7",
        "ab4c8a54366eab426d72c31c81531e929a4b615d051f300be7c993c628699f78",
        "06bf7790877f52c2094511dc368d605f7de4b112383fc6b857d8886844160c85",
        "7e6f31e287802d22eea4a7320a072471a793b94fe3882e4e8bbc30fd981bd7ed",
    ] {
        assert!(!sql.contains(retired_evidence_fingerprint));
    }
}

#[test]
fn current_database_connect_posture_is_single_scoped_and_used_by_every_measured_gate() {
    let executable = without_comments(CURRENT_DATABASE_PUBLIC_CONNECT_SQL);
    let public_revoke = ["REVOKE CONNECT ON DATABASE %I ", "FROM PUBLIC"].concat();
    let asset_path = [
        "test-support/fixtures/sql/",
        "current-database-public-connect.sql",
    ]
    .concat();
    // Assembled, not spelled, for the same reason as the two above: this file is
    // itself one of the scanned gates, so a literal here would count toward its
    // own `>= 2` applications and let the roster answer for the file it is
    // supposed to be measuring (wamn-3o3a). This file is all test code — it has
    // no `#[cfg(test)]` module to split on — so assembly is the only immunity
    // available.
    let posture_const = ["CURRENT_DATABASE", "_PUBLIC_CONNECT_SQL"].concat();
    assert!(
        executable
            .trim_start()
            .starts_with("DO $current_database_public_connect$")
    );
    assert!(
        executable
            .trim_end()
            .ends_with("$current_database_public_connect$;")
    );
    assert_eq!(executable.matches(public_revoke.as_str()).count(), 1);
    assert!(executable.contains("pg_catalog.current_database()"));
    assert!(executable.contains("ASSERT NOT EXISTS"));
    assert!(executable.contains("acl.grantee = 0"));
    assert!(executable.contains("acl.privilege_type = 'CONNECT'"));
    for cluster_wide in [
        "WHERE datallowconn",
        "FOR database_name IN",
        "revoke_public_connect_floor_sql",
    ] {
        assert!(
            !executable.contains(cluster_wide),
            "the current-database posture reached the cluster through {cluster_wide}"
        );
    }

    for (path, source) in [
        (
            "crates/control/provision/tests/control_portable_store.rs",
            include_str!("control_portable_store.rs"),
        ),
        (
            "services/scenario-worker/tests/management_live.rs",
            include_str!("../../../../services/scenario-worker/tests/management_live.rs"),
        ),
        // wamn-0h0g.25.2 (e75e3ff4) took `services/ctl/src/publish_release.rs`
        // and `services/ctl/tests/release_manifest_mint_live.rs` off this roster
        // by taking the measurement off them: the verb is now pure production
        // code with no test module or database client at all, and the mint gate
        // no longer provisions the control store or touches CONNECT. A gate that
        // does not measure a live database cannot contaminate one.
        (
            "services/ctl/tests/protected_relations_live.rs",
            include_str!("../../../../services/ctl/tests/protected_relations_live.rs"),
        ),
        (
            "services/ctl/tests/run_plane_live.rs",
            include_str!("../../../../services/ctl/tests/run_plane_live.rs"),
        ),
        (
            "services/ctl/tests/terminalize_effect_uncertain_live.rs",
            include_str!("../../../../services/ctl/tests/terminalize_effect_uncertain_live.rs"),
        ),
    ] {
        assert_eq!(
            source.matches(asset_path.as_str()).count(),
            1,
            "{path} does not include the shared posture exactly once"
        );
        assert!(
            source.matches(posture_const.as_str()).count() >= 2,
            "{path} includes the posture but never applies it"
        );
        assert!(
            !source.contains(public_revoke.as_str()),
            "{path} retained a private copy of the shared posture"
        );
    }

    // The contaminate-then-converge proof that used to be read out of
    // `release_manifest_mint_live.rs` here now belongs entirely to
    // `current_database_connect_posture_kills_remove_and_sibling_scope_mutants_on_postgres`
    // below, which contaminates the current database inside one explicit
    // transaction, applies the posture TWICE, and additionally proves the
    // sibling database keeps its own PUBLIC CONNECT.
}

/// The artifact text with comment lines removed, so a statement scan cannot be
/// fooled by prose that names a grant.
fn without_comments(sql: &str) -> String {
    sql.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `(target, privilege)` pair the artifact grants the control author,
/// parsed out of the statements themselves so line wrapping cannot change the
/// asserted set.
fn author_grants(sql: &str) -> BTreeSet<(String, String)> {
    let mut granted = BTreeSet::new();
    for statement in without_comments(sql).split(';') {
        let statement = statement.split_whitespace().collect::<Vec<_>>().join(" ");
        let Some(body) = statement.strip_prefix("GRANT ") else {
            continue;
        };
        let Some(body) = body.strip_suffix(" TO wamn_control_author") else {
            continue;
        };
        let (privileges, targets) = body
            .split_once(" ON ")
            .unwrap_or_else(|| panic!("a GRANT names its target: {body}"));
        let targets = targets
            .strip_prefix("SCHEMA ")
            .or_else(|| targets.strip_prefix("FUNCTION "))
            .or_else(|| targets.strip_prefix("TABLE "))
            .unwrap_or(targets);
        // A routine signature carries its own comma-separated argument list, so
        // only relation and schema lists may be split.
        let items: Vec<&str> = if targets.contains('(') {
            vec![targets]
        } else {
            targets.split(", ").collect()
        };
        for target in items {
            for privilege in privileges.split(", ") {
                granted.insert((target.to_owned(), privilege.to_owned()));
            }
        }
    }
    granted
}

/// wamn-0h0g.8.18: the store grants exactly the ratified authority class to
/// exactly one principal, and nothing that class does not name.
///
/// This replaces the "there are no grants at all" dormancy assertions. An extra
/// GRANT, a widened privilege, a second grantee, or an author-accessed relation
/// that lost its restrictive policy all fail here.
#[test]
fn control_author_authority_is_the_exact_ratified_class() {
    let sql = ddl();
    assert_eq!(
        wamn_control_provision::CONTROL_AUTHOR_ROLE,
        "wamn_control_author"
    );

    // Read-only portable catalog and draft-base facts.
    let read_only = [
        "catalog.flow_artifacts",
        "catalog.releases",
        "catalog.release_flows",
        "catalog.catalog_heads",
        "catalog.connection_requirements",
    ];
    // Append-only facts: immutable after append.
    let append_only = [
        "catalog.authoring_command_audit",
        "wamn_run.authoring_test_reports",
    ];
    // Landed state machines: exactly the transitions they shipped with.
    let state_machines = [
        "catalog.flow_drafts",
        "wamn_run.authoring_test_run_reservations",
        "wamn_run.authoring_test_case_runs",
    ];

    let mut expected = BTreeSet::new();
    for schema in ["catalog", "wamn_run", "wamn_authority"] {
        expected.insert((schema.to_owned(), "USAGE".to_owned()));
    }
    // The one definer routine the author may call. It gives it no privilege on
    // the relation behind the routine — that is the point of the pattern, and
    // the withheld list below is what proves it.
    expected.insert((
        "wamn_authority.session_author_tenant()".to_owned(),
        "EXECUTE".to_owned(),
    ));
    for relation in read_only {
        expected.insert((relation.to_owned(), "SELECT".to_owned()));
    }
    for relation in append_only {
        for privilege in ["SELECT", "INSERT"] {
            expected.insert((relation.to_owned(), privilege.to_owned()));
        }
    }
    for relation in state_machines {
        for privilege in ["SELECT", "INSERT", "UPDATE"] {
            expected.insert((relation.to_owned(), privilege.to_owned()));
        }
    }
    assert_eq!(author_grants(&sql), expected);

    // Nothing else in the store is reachable: no attestation publication, no
    // project run or binding authority, no artifact-reader or effect-writer
    // relation, no unrelated control relation, and no authority over the mapping
    // that decides the author's tenant.
    let granted: BTreeSet<String> = author_grants(&sql)
        .into_iter()
        .map(|(target, _)| target)
        .collect();
    for withheld in [
        "catalog.deployment_attestations",
        "catalog.release_exposure_manifests",
        "catalog.release_sources",
        "catalog.release_attachments",
        "catalog.catalogs",
        "wamn_authority.author_login_tenants",
    ] {
        assert!(
            !granted.contains(withheld),
            "the control author reached {withheld}"
        );
    }
    for routine in [
        "catalog.register_deployment_attestation",
        "catalog.reject_immutable_row_change",
        "catalog.guard_flow_draft_update",
    ] {
        assert!(
            !granted.iter().any(|target| target.starts_with(routine)),
            "the control author may execute {routine}"
        );
    }

    // Exactly one grantee, and no other plane's principal is even reachable.
    let stripped = without_comments(&sql);
    assert!(!stripped.contains("wamn_effect_writer"));
    assert!(!stripped.contains("wamn_run_projection_writer"));
    assert!(!stripped.contains("wamn_app"));
    for statement in stripped.split(';') {
        let statement = statement.split_whitespace().collect::<Vec<_>>().join(" ");
        if statement.starts_with("GRANT ") {
            assert!(
                statement.contains("TO wamn_control_author"),
                "a second grantee appeared: {statement}"
            );
        }
    }
    // The project plane's author role appears exactly once, and only as the
    // subject of the apply-time assertion that it holds no CONNECT here.
    assert_eq!(stripped.matches("wamn_scenario_author").count(), 1);
    assert!(stripped.contains("WHERE rolname = 'wamn_scenario_author'"));

    // Tenant authority is the mapping, not the caller-set GUC. Every
    // author-accessed relation — exactly the granted relations — carries a
    // RESTRICTIVE policy resolving through `session_user`, so rewriting
    // `app.tenant` can only narrow a session, never widen it.
    let tenant_policies = stripped
        .split("DO $policies$")
        .nth(1)
        .expect("the tenant policy block exists")
        .split("$policies$;")
        .next()
        .expect("the tenant policy block closes");
    let author_policies = stripped
        .split("DO $author_policies$")
        .nth(1)
        .expect("the author policy block exists")
        .split("$author_policies$;")
        .next()
        .expect("the author policy block closes");
    for (name, policy_block) in [("tenant", tenant_policies), ("author", author_policies)] {
        let drop = policy_block
            .find("DROP POLICY IF EXISTS %I ON %s")
            .unwrap_or_else(|| panic!("the {name} policy replay does not replace drift"));
        let create = policy_block
            .find("CREATE POLICY %I ON %s")
            .unwrap_or_else(|| panic!("the {name} policy replay does not create its policy"));
        assert!(
            drop < create,
            "the {name} policy is not replaced atomically"
        );
        assert!(
            !policy_block.contains("pg_policy"),
            "the {name} policy replay reimplemented catalog equivalence"
        );
    }
    assert!(author_policies.contains("AS RESTRICTIVE TO wamn_control_author"));
    assert!(author_policies.contains("USING (tenant_id = wamn_authority.session_author_tenant())"));
    assert!(
        author_policies.contains("WITH CHECK (tenant_id = wamn_authority.session_author_tenant())")
    );
    assert!(
        !author_policies.contains("app.tenant"),
        "the author policy derived tenant authority from app.tenant"
    );
    let mut policied = 0;
    for relation in read_only.iter().chain(&append_only).chain(&state_machines) {
        assert!(
            author_policies.contains(&format!("'{relation}'")),
            "{relation} has no applicable restrictive author policy"
        );
        policied += 1;
    }
    let listed = author_policies.matches("'catalog.").count()
        + author_policies.matches("'wamn_run.").count();
    assert_eq!(
        listed, policied,
        "the restrictive policy set is not exactly the granted relation set"
    );

    // The resolver is an owner routine with a fixed search path.
    let resolver = stripped
        .split("CREATE OR REPLACE FUNCTION wamn_authority.session_author_tenant()")
        .nth(1)
        .expect("the resolver exists")
        .split("REVOKE ALL ON FUNCTION")
        .next()
        .expect("the resolver is revoked from PUBLIC");
    assert!(resolver.contains("SECURITY DEFINER"));
    assert!(resolver.contains("SET search_path = pg_catalog, wamn_authority"));
    assert!(resolver.contains("mapping.login_identity = session_user"));
    assert!(
        !resolver.contains("app.tenant"),
        "the tenant resolver consulted the caller-set GUC"
    );
    assert!(!stripped.contains("wamn_run.lock_catalog_head"));
    // wamn-0h0g.7.5's "no second SECURITY DEFINER path" is NOT asserted here any
    // more. It used to be a newline-exact `contains` over the DDL text, which is
    // brittle in both directions: reformatting breaks it, and it cannot tell a
    // real second definer from a comment mentioning one. This file is STATIC
    // checked-in DDL, not builder output, so a text pin over it mostly asserts
    // that a file contains what the file contains — the drift-guard rationale
    // applies to generated SQL, where a text pin catches a Rust builder moving.
    // The invariant is now asserted where it is decidable: against pg_proc on a
    // real server, in
    // control_portable_store_applies_twice_and_enforces_contract_on_postgres.

    // The mapping relation itself is owner-only and carries no policy, because
    // FORCE ROW LEVEL SECURITY applies to the owner the resolver runs as.
    assert!(
        stripped.contains("REVOKE ALL ON TABLE wamn_authority.author_login_tenants FROM PUBLIC")
    );
    assert!(!stripped.contains("'wamn_authority.author_login_tenants'"));

    // The bounded set is asserted at apply time, not merely intended.
    for refusal in [
        "'control-author-effective-privilege-out-of-bounds'",
        "'control-author-authority-boundary-violated'",
    ] {
        assert!(sql.contains(refusal), "missing apply-time guard {refusal}");
    }
}

/// Rewrite one connection URL's identity, keeping its host, port, and database.
///
/// The tenant mapping resolves on `session_user`, which `SET ROLE` does not
/// change, so an author proof has to actually authenticate as the author.
fn as_role(url: &str, role: &str, password: &str) -> String {
    let mut parsed = url::Url::parse(url).expect("the control PG URL parses");
    parsed
        .set_username(role)
        .expect("a postgres URL carries a username");
    parsed
        .set_password(Some(password))
        .expect("a postgres URL carries a password");
    parsed.to_string()
}

/// Run one script through `psql` with `ON_ERROR_STOP`, returning its output.
fn psql(url: &str, script: &str) -> std::process::Output {
    let mut child = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    child.wait_with_output().expect("wait for psql")
}

fn psql_ok(url: &str, stage: &str, script: &str) {
    let output = psql(url, script);
    assert!(
        output.status.success(),
        "{stage} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires disposable PostgreSQL 18 URL in WAMN_CONTROL_PORTABLE_PG_URL"]
fn current_database_connect_posture_kills_remove_and_sibling_scope_mutants_on_postgres() {
    let url = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL")
        .expect("WAMN_CONTROL_PORTABLE_PG_URL names a disposable PostgreSQL 18 database");
    let sibling = format!("wamn_1143_sibling_{}", std::process::id());
    psql_ok(
        &url,
        "create the sibling-scope witness",
        &format!(
            "DROP DATABASE IF EXISTS {sibling} WITH (FORCE); \
             CREATE DATABASE {sibling}; \
             GRANT CONNECT ON DATABASE {sibling} TO PUBLIC;"
        ),
    );

    let proof = psql(
        &url,
        &format!(
            "BEGIN; \
             DO $contaminate$ BEGIN \
               EXECUTE pg_catalog.format( \
                 'GRANT CONNECT ON DATABASE %I TO PUBLIC', \
                 pg_catalog.current_database()); \
             END $contaminate$; \
             {CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             {CURRENT_DATABASE_PUBLIC_CONNECT_SQL} \
             DO $scope$ BEGIN \
               ASSERT NOT EXISTS ( \
                 SELECT FROM pg_catalog.pg_database AS database \
                 CROSS JOIN LATERAL pg_catalog.aclexplode( \
                   COALESCE(database.datacl, \
                            pg_catalog.acldefault('d', database.datdba))) AS acl \
                 WHERE database.datname = pg_catalog.current_database() \
                   AND acl.grantee = 0 AND acl.privilege_type = 'CONNECT'), \
                 'removing the posture left PUBLIC CONNECT on the current database'; \
               ASSERT EXISTS ( \
                 SELECT FROM pg_catalog.pg_database AS database \
                 CROSS JOIN LATERAL pg_catalog.aclexplode( \
                   COALESCE(database.datacl, \
                            pg_catalog.acldefault('d', database.datdba))) AS acl \
                 WHERE database.datname = '{sibling}' \
                   AND acl.grantee = 0 AND acl.privilege_type = 'CONNECT'), \
                 'the current-database posture revoked the sibling database'; \
             END $scope$; \
             COMMIT;"
        ),
    );
    let cleanup = psql(
        &url,
        &format!("DROP DATABASE IF EXISTS {sibling} WITH (FORCE);"),
    );
    assert!(
        cleanup.status.success(),
        "clean up sibling-scope witness failed:\n{}",
        String::from_utf8_lossy(&cleanup.stderr)
    );
    assert!(
        proof.status.success(),
        "current-database PUBLIC CONNECT proof failed:\n{}",
        String::from_utf8_lossy(&proof.stderr)
    );
}

#[test]
fn control_portable_store_applies_twice_and_enforces_contract_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL") else {
        eprintln!(
            "skipping control_portable_store_applies_twice_and_enforces_contract_on_postgres \
             (set WAMN_CONTROL_PORTABLE_PG_URL)"
        );
        return;
    };

    let mut script = String::from(CURRENT_DATABASE_PUBLIC_CONNECT_SQL);
    script.push_str(
        "CREATE EXTENSION IF NOT EXISTS pgcrypto;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DO $$ BEGIN\n\
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN\n\
             CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
           END IF;\n\
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_portable_probe') THEN\n\
             CREATE ROLE wamn_portable_probe NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
           END IF;\n\
         END $$;\n\
         DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', current_database()); END $$;\n\
         SET ROLE wamn_system;\n",
    );
    script.push_str(&ddl());
    script.push('\n');
    // The additive artifact itself is the bootstrap reconciliation path.
    script.push_str(&ddl());
    script.push_str(&format!(
        r#"
-- wamn-0h0g.7.5's holding, asserted semantically rather than as a text pin over
-- the DDL: the installed store has EXACTLY ONE SECURITY DEFINER routine and no
-- second. It moved here from the evidence-registrar proof wamn-0h0g.26.16
-- deleted; the invariant it carries is about there being no SECOND definer path,
-- which outlives the registrar that used to be the first. pg_proc.prosecdef is
-- the server's own answer, so formatting, comments and whitespace cannot fool it.
DO $$ DECLARE found text[]; BEGIN
  SELECT array_agg(namespace.nspname || '.' || routine.proname ORDER BY
                   namespace.nspname, routine.proname) INTO found
    FROM pg_catalog.pg_proc AS routine
    JOIN pg_catalog.pg_namespace AS namespace
      ON namespace.oid = routine.pronamespace
   WHERE routine.prosecdef
     AND namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority');
  IF found IS DISTINCT FROM ARRAY['wamn_authority.session_author_tenant'] THEN
    RAISE EXCEPTION 'definer routine set drifted: %', found;
  END IF;
END $$;

-- The retirement really landed on the server: neither the relation nor any
-- overload of its registrar survives an apply.
DO $$ BEGIN
  ASSERT to_regclass('catalog.release_flow_test_evidence') IS NULL,
         'the retired release evidence relation survived the apply';
  ASSERT NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_proc AS routine
    JOIN pg_catalog.pg_namespace AS namespace
      ON namespace.oid = routine.pronamespace
   WHERE namespace.nspname = 'catalog'
     AND routine.proname = 'register_release_flow_test_evidence'),
         'an overload of the retired evidence registrar survived the apply';
END $$;

SET app.tenant = 'tenant-a';
INSERT INTO catalog.catalogs
  (tenant_id,catalog_id,version,environment,schema_version,state)
VALUES ('tenant-a','cat',1,'dev','0.1','applied');
INSERT INTO catalog.flow_artifacts
  (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash)
VALUES ('tenant-a','flow-a',1,'0.1','{{}}','graph','artifact-a');
INSERT INTO catalog.releases
  (tenant_id,catalog_id,catalog_version)
VALUES ('tenant-a','cat',1);
INSERT INTO catalog.release_flows
  (tenant_id,catalog_id,catalog_version,flow_id,flow_version)
VALUES ('tenant-a','cat',1,'flow-a',1);
INSERT INTO catalog.flow_drafts
  (tenant_id,draft_id,flow_id,revision,definition)
VALUES ('tenant-a','draft-a','flow-a',1,'{{}}');
INSERT INTO wamn_run.authoring_test_run_reservations
  (tenant_id,report_id,command_hash,validated_draft_id,
   catalog_id,catalog_version,case_count,whole_deadline_at)
VALUES ('tenant-a','report-a','sha256:'||repeat('1',64),'validated-a',
        'cat',1,1,clock_timestamp()+interval '1 hour');
INSERT INTO wamn_run.authoring_test_reports
  (tenant_id,report_id,validated_draft_id,catalog_id,
   catalog_version,passed,summary)
VALUES ('tenant-a','report-a','validated-a','cat',1,true,'{{}}'::jsonb);

DO $$ DECLARE
  first_attested_at timestamptz;
BEGIN
  first_attested_at := catalog.register_deployment_attestation(
    'tenant-a','cat',1,'org-a','project-a','dev','sha256:'||repeat('2',64),
    '2026-08-15T12:00:00Z'::timestamptz);
  ASSERT first_attested_at = catalog.register_deployment_attestation(
      'tenant-a','cat',1,'org-a','project-a','dev','sha256:'||repeat('2',64),
      '2026-08-15T12:00:00Z'::timestamptz),
    'exact attestation retry must return the original row';
END $$;
DO $$ BEGIN BEGIN
  PERFORM catalog.register_deployment_attestation(
    'tenant-a','cat',1,'org-a','project-a','dev','sha256:'||repeat('4',64),
    '2026-08-15T12:00:00Z'::timestamptz);
  ASSERT false, 'same attestation coordinate with different content must conflict';
EXCEPTION WHEN unique_violation THEN
  ASSERT SQLERRM = 'deployment-attestation-content-conflict';
END; END $$;

DO $$ BEGIN
  ASSERT NOT has_schema_privilege('wamn_portable_probe','catalog','USAGE');
  ASSERT NOT has_table_privilege(
    'wamn_portable_probe','catalog.deployment_attestations','SELECT');
  ASSERT NOT has_table_privilege(
    'wamn_portable_probe','catalog.deployment_attestations','INSERT');
  ASSERT NOT has_table_privilege(
    'wamn_portable_probe','catalog.deployment_attestations','UPDATE');
  ASSERT NOT has_table_privilege(
    'wamn_portable_probe','catalog.deployment_attestations','DELETE');
  ASSERT (SELECT relrowsecurity AND relforcerowsecurity FROM pg_class
          WHERE oid='catalog.deployment_attestations'::regclass);
  ASSERT NOT EXISTS (
    SELECT 1 FROM pg_constraint
    WHERE contype='f'
      AND conrelid = 'catalog.deployment_attestations'::regclass
      AND confrelid::regclass::text LIKE 'registry.%'
  ), 'cross-plane coordinates must never become foreign keys';
  -- wamn-0h0g.8.5.5: the draft-safe connection-grant relation is deleted, so
  -- the server must not hold it at all -- neither freshly created nor left
  -- behind by an earlier apply that the retirement block failed to converge.
  ASSERT to_regclass('catalog.draft_safe_connection_grants') IS NULL,
    'the retired draft-safe connection grant relation is still installed';
END $$;
RESET ROLE;
"#
    ));

    let mut child = Command::new("psql")
        .arg(&url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn psql");
    child
        .stdin
        .take()
        .expect("psql stdin")
        .write_all(script.as_bytes())
        .expect("write psql script");
    let output = child.wait_with_output().expect("wait for psql");
    assert!(
        output.status.success(),
        "portable-store PostgreSQL proof failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// wamn-0h0g.8.18 live PostgreSQL 18 proof: two tenants, an owner-maintained
/// login-to-tenant mapping, restrictive policies that a caller cannot widen by
/// rewriting `app.tenant`, the exact positive-operation matrix, and the denial
/// matrix.
///
/// Two SCOPES, not two generations of one scope: one management instance serves
/// exactly one `(org, project, environment)`, so `(acme, receiving, dev)` maps to
/// `tenant-a` and `(acme, shipping, dev)` maps to `tenant-b`. Each proof stage
/// authenticates as its own author login, because the mapping resolves on
/// `session_user` and `SET ROLE` leaves `session_user` alone.
#[test]
fn control_author_two_tenant_authority_holds_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL") else {
        eprintln!(
            "skipping control_author_two_tenant_authority_holds_on_postgres \
             (set WAMN_CONTROL_PORTABLE_PG_URL)"
        );
        return;
    };
    let database = {
        let parsed = url::Url::parse(&url).expect("the control PG URL parses");
        parsed.path().trim_start_matches('/').to_owned()
    };
    assert!(
        !database.is_empty(),
        "WAMN_CONTROL_PORTABLE_PG_URL must name a database"
    );

    // The deterministic generation names the credential contract mints. Both are
    // `[a-z0-9_]` and 62 bytes, so they need no quoting and fit PostgreSQL's
    // 63-byte identifier limit.
    let author_a = wamn_control_provision::control_author_generation_role(
        "acme",
        "receiving",
        "dev",
        &database,
        wamn_control_provision::CredentialGeneration::A,
    );
    let author_b = wamn_control_provision::control_author_generation_role(
        "acme",
        "shipping",
        "dev",
        &database,
        wamn_control_provision::CredentialGeneration::B,
    );
    assert_ne!(author_a, author_b);
    const PASSWORD: &str = "control-author-live-proof";

    let mut install = String::from(CURRENT_DATABASE_PUBLIC_CONNECT_SQL);
    install.push_str(
        "DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_authority CASCADE;\n\
         DO $$ BEGIN\n\
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN\n\
             CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
           END IF;\n\
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_portable_probe') THEN\n\
             CREATE ROLE wamn_portable_probe NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
           END IF;\n\
         END $$;\n",
    );
    // ctl owns the stable NOLOGIN role and the scoped LOGIN generations; this is
    // the exact text it applies.
    install.push_str(&wamn_control_provision::sql::ensure_control_author_acl_role_sql());
    install.push('\n');
    for role in [&author_a, &author_b] {
        install.push_str(
            &wamn_control_provision::sql::prepare_control_author_generation_sql(
                &database,
                role,
                PASSWORD,
                "2099-01-01T00:00:00Z",
            ),
        );
        install.push('\n');
    }
    install.push_str(
        "DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
           current_database()); END $$;\n\
         SET ROLE wamn_system;\n",
    );
    install.push_str(&ddl());
    install.push('\n');
    // Named replay mutants: same-name policy objects with wrong command,
    // permissiveness, roles, USING, and WITH CHECK facts. The second apply must
    // replace both, not confuse name equality with policy equality.
    install.push_str(
        r#"
DROP POLICY flow_drafts_tenant ON catalog.flow_drafts;
CREATE POLICY flow_drafts_tenant ON catalog.flow_drafts
  AS RESTRICTIVE FOR SELECT TO wamn_portable_probe USING (false);
DROP POLICY flow_drafts_author_tenant ON catalog.flow_drafts;
CREATE POLICY flow_drafts_author_tenant ON catalog.flow_drafts
  AS PERMISSIVE FOR SELECT TO PUBLIC USING (true);
"#,
    );
    install.push_str(&ddl());
    install.push_str(
        r#"
DO $policy_replay$ BEGIN
  ASSERT COALESCE((
    SELECT p.polcmd = '*'
       AND p.polpermissive
       AND ARRAY(
             SELECT CASE role_oid WHEN 0 THEN 'PUBLIC'::text
                    ELSE pg_get_userbyid(role_oid)::text END
               FROM unnest(p.polroles) role_oid ORDER BY 1
           ) = ARRAY['PUBLIC']::text[]
       AND pg_get_expr(p.polqual, p.polrelid, true) =
           'tenant_id = NULLIF(current_setting(''app.tenant''::text, true), ''''::text)'
       AND pg_get_expr(p.polwithcheck, p.polrelid, true) =
           'tenant_id = NULLIF(current_setting(''app.tenant''::text, true), ''''::text)'
      FROM pg_policy p
     WHERE p.polrelid = 'catalog.flow_drafts'::regclass
       AND p.polname = 'flow_drafts_tenant'
  ), false), 'same-name tenant policy drift survived replay';

  ASSERT COALESCE((
    SELECT p.polcmd = '*'
       AND NOT p.polpermissive
       AND ARRAY(
             SELECT CASE role_oid WHEN 0 THEN 'PUBLIC'::text
                    ELSE pg_get_userbyid(role_oid)::text END
               FROM unnest(p.polroles) role_oid ORDER BY 1
           ) = ARRAY['wamn_control_author']::text[]
       AND pg_get_expr(p.polqual, p.polrelid, true) =
           'tenant_id = wamn_authority.session_author_tenant()'
       AND pg_get_expr(p.polwithcheck, p.polrelid, true) =
           'tenant_id = wamn_authority.session_author_tenant()'
      FROM pg_policy p
     WHERE p.polrelid = 'catalog.flow_drafts'::regclass
       AND p.polname = 'flow_drafts_author_tenant'
  ), false), 'same-name author policy drift survived replay';
END $policy_replay$;
"#,
    );
    install.push_str(&format!(
        r#"
INSERT INTO wamn_authority.author_login_tenants
  (login_identity, tenant_id, org_id, project_id, environment)
VALUES ('{author_a}', 'tenant-a', 'acme', 'receiving', 'dev'),
       ('{author_b}', 'tenant-b', 'acme', 'shipping', 'dev');

DO $seed$ DECLARE tenant text; BEGIN
  FOREACH tenant IN ARRAY ARRAY['tenant-a', 'tenant-b'] LOOP
    PERFORM set_config('app.tenant', tenant, false);
    INSERT INTO catalog.catalogs
      (tenant_id,catalog_id,version,environment,schema_version,state)
    VALUES (tenant,'cat',1,'dev','0.1','applied');
    INSERT INTO catalog.catalog_heads
      (tenant_id,catalog_id,environment,applied_catalog_version)
    VALUES (tenant,'cat','dev',1);
    INSERT INTO catalog.flow_artifacts
      (tenant_id,flow_id,flow_version,schema_version,graph_json,graph_hash,artifact_hash)
    VALUES (tenant,'flow-a',1,'0.1','{{}}','graph','artifact-'||tenant);
    INSERT INTO catalog.releases
      (tenant_id,catalog_id,catalog_version)
    VALUES (tenant,'cat',1);
    INSERT INTO catalog.release_flows
      (tenant_id,catalog_id,catalog_version,flow_id,flow_version)
    VALUES (tenant,'cat',1,'flow-a',1);
    INSERT INTO catalog.flow_drafts
      (tenant_id,draft_id,flow_id,revision,definition)
    VALUES (tenant,'draft-a','flow-a',1,'{{}}');
    INSERT INTO wamn_run.authoring_test_run_reservations
      (tenant_id,report_id,command_hash,validated_draft_id,
       catalog_id,catalog_version,case_count,whole_deadline_at)
    VALUES (tenant,'report-a','sha256:'||repeat('1',64),'validated-'||tenant,
            'cat',1,1,clock_timestamp()+interval '1 hour');
    INSERT INTO wamn_run.authoring_test_case_runs
      (tenant_id,report_id,ordinal,case_id,run_id,catalog_id,catalog_version,
       validated_draft_id,case_deadline_at)
    VALUES (tenant,'report-a',0,'case-a','run-'||tenant,'cat',1,
            'validated-'||tenant,clock_timestamp()+interval '1 minute');
  END LOOP;
  PERFORM set_config('app.tenant', '', false);
END $seed$;
RESET ROLE;
"#
    ));
    psql_ok(&url, "control-author install and seed", &install);

    // ---- Stage 2: the mapped author, connected as itself. -------------------
    let matrix = format!(
        r#"
DO $identity$ BEGIN
  ASSERT session_user = '{author_a}', 'the proof must authenticate as the author';
  ASSERT current_user = session_user, 'the author must not have entered another role';
  ASSERT wamn_authority.session_author_tenant() = 'tenant-a',
         'the mapping must resolve this login to exactly one tenant';
  ASSERT NOT pg_has_role(session_user, 'wamn_system', 'USAGE'),
         'the author must not reach the store owner';
  ASSERT pg_has_role(session_user, 'wamn_control_author', 'USAGE'),
         'the author must inherit the stable ACL role';
END $identity$;

-- POSITIVE MATRIX: exactly the ratified lifecycle class, under this tenant.
SET app.tenant = 'tenant-a';
DO $positive$ BEGIN
  ASSERT (SELECT count(*) FROM catalog.flow_drafts) = 1,
         'the author reads its own drafts';
  ASSERT (SELECT count(*) FROM catalog.catalog_heads) = 1;
  ASSERT (SELECT count(*) FROM catalog.release_flows) = 1;
  ASSERT (SELECT count(*) FROM catalog.flow_artifacts) = 1;
  ASSERT (SELECT count(*) FROM catalog.releases) = 1;
  ASSERT (SELECT count(*) FROM wamn_run.authoring_test_run_reservations) = 1;
  ASSERT (SELECT count(*) FROM wamn_run.authoring_test_case_runs) = 1;
END $positive$;

INSERT INTO catalog.flow_drafts (tenant_id,draft_id,flow_id,revision,definition)
VALUES ('tenant-a','draft-b','flow-a',1,'{{}}');
UPDATE catalog.flow_drafts
   SET revision = revision + 1, definition = '{{"edited":true}}',
       edited_at = clock_timestamp() + interval '1 microsecond'
 WHERE tenant_id = 'tenant-a' AND draft_id = 'draft-b';
INSERT INTO catalog.authoring_command_audit
  (tenant_id,command_id,command_kind,principal_id,principal_kind,principal_subject,
   effective_role,org,project,environment,target_ref,request_hash,outcome_bytes)
VALUES ('tenant-a','command-1','save-draft','principal-1','human','someone',
        'project-author','acme','receiving','dev','draft-b',
        'sha256:'||repeat('2',64),'\x7b7d'::bytea);
UPDATE wamn_run.authoring_test_case_runs
   SET state = 'finalized', passed = true, summary = '{{}}'::jsonb,
       finalized_at = clock_timestamp()
 WHERE tenant_id = 'tenant-a' AND report_id = 'report-a' AND ordinal = 0;
UPDATE wamn_run.authoring_test_run_reservations
   SET state = 'finalized', finalized_at = clock_timestamp()
 WHERE tenant_id = 'tenant-a' AND report_id = 'report-a';

-- WIDENING MATRIX: `app.tenant` is a consistency assertion only. Rewriting it
-- cannot reach another tenant's rows, and cannot write one either.
SET app.tenant = 'tenant-b';
DO $no_widening$ BEGIN
  ASSERT (SELECT count(*) FROM catalog.flow_drafts) = 0,
         'app.tenant widened a read to another tenant';
  ASSERT (SELECT count(*) FROM catalog.catalog_heads) = 0;
  ASSERT (SELECT count(*) FROM wamn_run.authoring_test_run_reservations) = 0;
  BEGIN
    INSERT INTO catalog.flow_drafts (tenant_id,draft_id,flow_id,revision,definition)
    VALUES ('tenant-b','forged','flow-a',1,'{{}}');
    ASSERT false, 'app.tenant widened a write to another tenant';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
END $no_widening$;

-- An absent claim is not a wildcard: the permissive floor fails too.
RESET app.tenant;
DO $no_claim$ BEGIN
  ASSERT (SELECT count(*) FROM catalog.flow_drafts) = 0,
         'an absent app.tenant claim read rows';
END $no_claim$;

-- The claim must agree with the mapping; it can only ever narrow.
SET app.tenant = 'tenant-a';
DO $denials$ BEGIN
  -- Deployment attestations: the author is not the publisher.
  BEGIN PERFORM 1 FROM catalog.deployment_attestations;
    ASSERT false, 'the author read deployment attestations';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN PERFORM catalog.register_deployment_attestation(
      'tenant-a','cat',1,'acme','receiving','dev','sha256:'||repeat('5',64),now());
    ASSERT false, 'the author published a deployment attestation';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;

  -- Unrelated control relations, including the release record set and the
  -- catalog registry the publisher owns.
  BEGIN PERFORM 1 FROM catalog.catalogs;
    ASSERT false, 'the author read the catalog registry';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN PERFORM 1 FROM catalog.release_sources;
    ASSERT false, 'the author read release sources';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN PERFORM 1 FROM catalog.release_attachments;
    ASSERT false, 'the author read release attachments';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN PERFORM 1 FROM catalog.release_exposure_manifests;
    ASSERT false, 'the author read release exposure manifests';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;

  -- Tenant authority is not self-service: the mapping is unreadable and
  -- unwritable, so an author cannot re-point itself at another tenant.
  BEGIN PERFORM 1 FROM wamn_authority.author_login_tenants;
    ASSERT false, 'the author read its own tenant mapping';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN INSERT INTO wamn_authority.author_login_tenants
        (login_identity,tenant_id,org_id,project_id,environment)
        VALUES (session_user,'tenant-b','acme','shipping','dev');
    ASSERT false, 'the author rewrote its own tenant mapping';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;

  -- No UPDATE or DELETE over an immutable fact, and no DELETE anywhere.
  BEGIN UPDATE catalog.authoring_command_audit SET target_ref = 'moved';
    ASSERT false, 'the author mutated the command ledger';
  EXCEPTION WHEN insufficient_privilege OR SQLSTATE '55000' THEN NULL; END;
  BEGIN UPDATE wamn_run.authoring_test_reports SET passed = false;
    ASSERT false, 'the author mutated a finalized report';
  EXCEPTION WHEN insufficient_privilege OR SQLSTATE '55000' THEN NULL; END;
  BEGIN DELETE FROM catalog.flow_drafts;
    ASSERT false, 'the author deleted a draft';
  EXCEPTION WHEN insufficient_privilege OR SQLSTATE '55000' THEN NULL; END;
  BEGIN DELETE FROM wamn_run.authoring_test_case_runs;
    ASSERT false, 'the author deleted a case mapping';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  -- The head pointer moves only through the narrow bridge, never by UPDATE.
  BEGIN UPDATE catalog.catalog_heads SET applied_catalog_version = 2;
    ASSERT false, 'the author moved the applied catalog head';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;

  -- No schema creation, no routine creation, no grant options.
  BEGIN CREATE SCHEMA author_owned;
    ASSERT false, 'the author created a schema';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN CREATE TABLE catalog.author_owned (id int);
    ASSERT false, 'the author created a relation in catalog';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN CREATE TABLE wamn_run.author_owned (id int);
    ASSERT false, 'the author created a relation in wamn_run';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  BEGIN CREATE FUNCTION catalog.author_owned() RETURNS int LANGUAGE sql AS 'SELECT 1';
    ASSERT false, 'the author created a routine';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
  -- A non-owner GRANT without grant option is a WARNING, not an error, so the
  -- proof is that nothing was actually granted.
  GRANT SELECT ON catalog.flow_drafts TO wamn_portable_probe;
  ASSERT NOT has_table_privilege('wamn_portable_probe','catalog.flow_drafts','SELECT'),
         'the author re-granted its own authority';
END $denials$;

-- Attribute and reach assertions that need no failing statement.
DO $reach$ DECLARE other text; BEGIN
  ASSERT NOT (SELECT rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication
                     OR rolbypassrls
                FROM pg_roles WHERE rolname = session_user),
         'the author holds a cluster attribute it must not';
  ASSERT NOT has_schema_privilege(session_user, 'catalog', 'CREATE');
  ASSERT NOT has_schema_privilege(session_user, 'wamn_run', 'CREATE');
  ASSERT NOT has_schema_privilege(session_user, 'wamn_authority', 'CREATE');
  ASSERT NOT has_table_privilege(session_user,
             'wamn_authority.author_login_tenants', 'SELECT');
  -- Cross-database CONNECT: exactly this control database, nothing else.
  FOR other IN SELECT datname FROM pg_database
                WHERE NOT datistemplate AND datname <> current_database() LOOP
    ASSERT NOT has_database_privilege(session_user, other, 'CONNECT'),
           format('the author may connect to %I', other);
  END LOOP;
  -- No object anywhere in the cluster is owned by the author.
  ASSERT NOT EXISTS (
    SELECT 1 FROM pg_shdepend dependency
     JOIN pg_roles role ON role.oid = dependency.refobjid
    WHERE dependency.refclassid = 'pg_authid'::regclass
      AND dependency.deptype = 'o'
      AND role.rolname = session_user),
    'the author owns an object';
END $reach$;
"#
    );
    psql_ok(
        &as_role(&url, &author_a, PASSWORD),
        "control-author tenant-a matrix",
        &matrix,
    );

    // ---- Stage 3: the second tenant's author sees only its own rows. --------
    let second = format!(
        r#"
SET app.tenant = 'tenant-b';
DO $second$ BEGIN
  ASSERT session_user = '{author_b}';
  ASSERT wamn_authority.session_author_tenant() = 'tenant-b';
  -- tenant-a's author inserted `draft-b` above; tenant-b must not see it, and
  -- must see exactly its own seeded draft.
  ASSERT (SELECT count(*) FROM catalog.flow_drafts) = 1,
         'the second tenant observed the first tenant''s authored rows';
  ASSERT (SELECT count(*) FROM catalog.flow_drafts
           WHERE draft_id = 'draft-b') = 0;
  ASSERT (SELECT count(*) FROM catalog.authoring_command_audit) = 0,
         'the second tenant read the first tenant''s command ledger';
  ASSERT (SELECT state FROM wamn_run.authoring_test_run_reservations) = 'pending',
         'the second tenant observed the first tenant''s finalization';
END $second$;
"#
    );
    psql_ok(
        &as_role(&url, &author_b, PASSWORD),
        "control-author tenant-b isolation",
        &second,
    );

    // ---- Stage 4: retirement revokes authority and authentication. ----------
    let retire = format!(
        "{}\nDO $retired$ BEGIN\n  \
           ASSERT NOT (SELECT rolcanlogin FROM pg_roles WHERE rolname = '{author_a}'),\n         \
             'a retired generation can still authenticate';\n  \
           ASSERT NOT has_database_privilege('{author_a}', current_database(), 'CONNECT'),\n         \
             'a retired generation can still connect';\n  \
           ASSERT NOT pg_has_role('{author_a}', 'wamn_control_author', 'USAGE'),\n         \
             'a retired generation still inherits author authority';\n\
         END $retired$;\n",
        wamn_control_provision::sql::retire_control_author_generation_sql(&database, &author_a),
    );
    psql_ok(&url, "control-author retirement", &retire);
}

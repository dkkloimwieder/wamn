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
        "catalog.releases",
        "catalog.catalog_heads",
        // wamn-nb2e registered the component library in BOTH planes. wamn-0h0g.12.1
        // pins the control copy here: the artifact's own seven-name inventory
        // guard asserts it and this list did not, so the record's presence half
        // was one relation short of the record's exactness half.
        "catalog.component_library",
        "catalog.connection_requirements",
        "catalog.authoring_command_audit",
        "catalog.deployment_attestations",
        // wamn-0h0g.8.18: the owner-maintained login-identity-to-tenant mapping.
        "wamn_authority.author_login_tenants",
        // wamn-0h0g.8.5.6: the one durable fact an accepted gate produces,
        // keyed by the judged document's own wiring hash.
        "wamn_run.gate_reports",
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
        // wamn-0h0g.8.5.5: a draft is a CLIENT-SIDE FILE, so the mutable
        // document store and its monotonic-revision trigger are gone too.
        "catalog.flow_drafts",
        // wamn-0h0g.26.16: flow-shaped release TEST EVIDENCE named a release
        // member by `flow_id` under a retired identity, and nothing in the
        // workspace ever executed its registrar.
        "catalog.release_flow_test_evidence",
        // wamn-0h0g.26.21: the flow-era release plane retires in BOTH planes, so
        // these five are no longer declared here either -- the retirement block
        // asserted below is what removes them from an existing store.
        "catalog.flow_artifacts",
        "catalog.release_flows",
        "catalog.release_exposure_manifests",
        "catalog.release_sources",
        "catalog.release_attachments",
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
        // wamn-0h0g.8.5.5: the whole run-plane gate-report lineage deletes. A
        // relation whose only production writer and only production reader are
        // both removed by the same change does not survive it (owner ruling,
        // 2026-08-25), so `wamn_run` is now declared EMPTY.
        "wamn_run.authoring_test_run_reservations",
        "wamn_run.authoring_test_case_runs",
        "wamn_run.authoring_test_reports",
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
    assert!(sql.contains("DROP TABLE catalog.flow_drafts RESTRICT"));
    // The trigger function outlives its triggers unless it is dropped by name.
    assert!(sql.contains("DROP FUNCTION IF EXISTS catalog.guard_flow_draft_update()"));
    assert!(!sql.contains("CREATE OR REPLACE FUNCTION catalog.guard_flow_draft_update"));
    assert!(!sql.contains("CREATE TRIGGER flow_drafts_controlled_update"));
    assert!(!sql.contains("CREATE TRIGGER flow_drafts_delete_immutable"));
    // The ledger vocabulary follows the contract's two surviving commands.
    assert!(sql.contains("CHECK (command_kind IN ('test-set-run', 'publish'))"));
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
        "DROP TABLE wamn_run.authoring_test_reports RESTRICT;",
        "DROP TABLE wamn_run.authoring_test_case_runs RESTRICT;",
        "DROP TABLE wamn_run.authoring_test_run_reservations RESTRICT;",
        "'control-portable-retired-audit-ledger-requires-reprovision'",
        "'control-portable-retired-release-members-requires-reprovision'",
        // wamn-0h0g.26.21: the flow-era release plane, child first.
        "'catalog.release_attachments',",
        "'catalog.release_sources',",
        "'catalog.release_exposure_manifests',",
        "'catalog.release_flows',",
        "'catalog.flow_artifacts'",
        "DROP TABLE %s RESTRICT",
    ] {
        assert!(sql.contains(retirement), "missing retirement: {retirement}");
    }
    // wamn-0h0g.15.159: the sealed member snapshot left the release identity row.
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
        "catalog.releases",
        "catalog.catalog_heads",
        "catalog.connection_requirements",
    ];
    // Append-only facts: immutable after append. The reservation-era gate-report
    // lineage was the author's only STATE-MACHINE authority and its three
    // relations are deleted (wamn-0h0g.8.5.5); the report row wamn-0h0g.8.5.6
    // put back is written once and never revised, so the author still holds
    // nothing but reads and appends.
    let append_only = ["catalog.authoring_command_audit", "wamn_run.gate_reports"];

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
    for relation in read_only.iter().chain(&append_only) {
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

/// Substitute a bound statement's positional params in as literals, so a builder
/// the driver would run with `$n` binds runs under `psql` (the shape is the same
/// one; only the transport differs). Highest-to-lowest so `$1` never matches
/// inside `$10`+.
fn render(statement: &wamn_schema_control::SqlStatement) -> String {
    let mut sql = statement.sql.clone();
    for (index, value) in statement.params.iter().enumerate().rev() {
        sql = sql.replace(&format!("${}", index + 1), &lit(value));
    }
    sql
}

fn lit(value: &wamn_schema_control::Value) -> String {
    match value {
        wamn_schema_control::Value::Text(text)
        | wamn_schema_control::Value::NullableText(Some(text)) => {
            format!("'{}'", text.replace('\'', "''"))
        }
        wamn_schema_control::Value::NullableText(None)
        | wamn_schema_control::Value::NullableInt(None) => "NULL".to_owned(),
        wamn_schema_control::Value::Int(int)
        | wamn_schema_control::Value::NullableInt(Some(int)) => int.to_string(),
        wamn_schema_control::Value::Bool(flag) => flag.to_string(),
    }
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
INSERT INTO catalog.releases
  (tenant_id,catalog_id,catalog_version)
VALUES ('tenant-a','cat',1);

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
  -- wamn-0h0g.8.5.5: the whole run-plane gate-report lineage is deleted, and
  -- `wamn_run` is declared EMPTY. Asserted on the SERVER, not over the file
  -- text, because the convergence arm is what an already-provisioned store
  -- takes and a fresh apply never does.
  ASSERT to_regclass('wamn_run.authoring_test_reports') IS NULL,
    'the retired gate report relation is still installed';
  ASSERT to_regclass('wamn_run.authoring_test_case_runs') IS NULL,
    'the retired gate case-run relation is still installed';
  ASSERT to_regclass('wamn_run.authoring_test_run_reservations') IS NULL,
    'the retired gate reservation relation is still installed';
  -- The schema is this artifact's exclusively, and it now declares exactly one
  -- relation in it (wamn-0h0g.8.5.6). Asserted as an EXACT inventory rather than
  -- as presence: a stray relation here is drift whether it arrived beside the
  -- declared one or instead of it.
  ASSERT (SELECT array_agg(tablename::text ORDER BY tablename) FROM pg_tables
           WHERE schemaname = 'wamn_run') = ARRAY['gate_reports']::text[],
    'the control store wamn_run inventory is not exactly the gate report';
  -- Append-only in the schema too, not merely by the absence of a grant.
  ASSERT EXISTS (SELECT 1 FROM pg_trigger
                  WHERE tgrelid = 'wamn_run.gate_reports'::regclass
                    AND tgname = 'gate_reports_immutable' AND NOT tgisinternal),
    'the gate report relation is rewritable';
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
DROP POLICY authoring_command_audit_tenant ON catalog.authoring_command_audit;
CREATE POLICY authoring_command_audit_tenant ON catalog.authoring_command_audit
  AS RESTRICTIVE FOR SELECT TO wamn_portable_probe USING (false);
DROP POLICY authoring_command_audit_author_tenant ON catalog.authoring_command_audit;
CREATE POLICY authoring_command_audit_author_tenant ON catalog.authoring_command_audit
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
     WHERE p.polrelid = 'catalog.authoring_command_audit'::regclass
       AND p.polname = 'authoring_command_audit_tenant'
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
     WHERE p.polrelid = 'catalog.authoring_command_audit'::regclass
       AND p.polname = 'authoring_command_audit_author_tenant'
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
    INSERT INTO catalog.releases
      (tenant_id,catalog_id,catalog_version)
    VALUES (tenant,'cat',1);
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
  ASSERT (SELECT count(*) FROM catalog.catalog_heads) = 1;
  ASSERT (SELECT count(*) FROM catalog.releases) = 1;
END $positive$;

INSERT INTO catalog.authoring_command_audit
  (tenant_id,command_id,command_kind,principal_id,principal_kind,principal_subject,
   effective_role,org,project,environment,target_ref,request_hash,outcome_bytes)
VALUES ('tenant-a','command-1','test-set-run','principal-1','human','someone',
        'project-author','acme','receiving','dev','validated-tenant-a',
        'sha256:'||repeat('2',64),'\x7b7d'::bytea);

-- WIDENING MATRIX: `app.tenant` is a consistency assertion only. Rewriting it
-- cannot reach another tenant's rows, and cannot write one either.
SET app.tenant = 'tenant-b';
DO $no_widening$ BEGIN
  ASSERT (SELECT count(*) FROM catalog.catalog_heads) = 0,
         'app.tenant widened a read to another tenant';
  ASSERT (SELECT count(*) FROM catalog.releases) = 0;
  BEGIN
    INSERT INTO catalog.authoring_command_audit
      (tenant_id,command_id,command_kind,principal_id,principal_kind,
       principal_subject,effective_role,org,project,environment,target_ref,
       request_hash,outcome_bytes)
    VALUES ('tenant-b','forged','test-set-run','principal-1','human','someone',
            'project-author','acme','shipping','dev','forged',
            'sha256:'||repeat('3',64),'\x7b7d'::bytea);
    ASSERT false, 'app.tenant widened a write to another tenant';
  EXCEPTION WHEN insufficient_privilege THEN NULL; END;
END $no_widening$;

-- An absent claim is not a wildcard: the permissive floor fails too.
RESET app.tenant;
DO $no_claim$ BEGIN
  ASSERT (SELECT count(*) FROM catalog.catalog_heads) = 0,
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

  -- Unrelated control relations, including the catalog registry the publisher
  -- owns.
  BEGIN PERFORM 1 FROM catalog.catalogs;
    ASSERT false, 'the author read the catalog registry';
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
  BEGIN DELETE FROM catalog.authoring_command_audit;
    ASSERT false, 'the author deleted a ledger row';
  EXCEPTION WHEN insufficient_privilege OR SQLSTATE '55000' THEN NULL; END;
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
  GRANT SELECT ON catalog.catalog_heads TO wamn_portable_probe;
  ASSERT NOT has_table_privilege('wamn_portable_probe','catalog.catalog_heads','SELECT'),
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
  -- tenant-a's author appended a ledger row above; tenant-b must not see it,
  -- and must see exactly its own seeded rows.
  ASSERT (SELECT count(*) FROM catalog.releases) = 1,
         'the second tenant lost its own seeded rows';
  ASSERT (SELECT count(*) FROM catalog.authoring_command_audit) = 0,
         'the second tenant read the first tenant''s command ledger';
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

/// wamn-0h0g.8.21 live PostgreSQL 18 proof for the RUST binding of
/// `catalog.register_deployment_attestation`.
///
/// The plpgsql leg inside
/// `control_portable_store_applies_twice_and_enforces_contract_on_postgres`
/// already proves the ROUTINE — the idempotent exact retry, the content-conflict
/// raise, the zero probe grants — and none of that is repeated here. What is
/// unproven without this is the STATEMENT `wamn_schema_control::attestation`
/// builds: that PostgreSQL accepts it against the installed signature at all, at
/// the arity and per-position types the binding assumes; that each named field
/// lands in its own column; and that the refusal the Rust boundary keys on is
/// the one the server actually returns.
///
/// The relation is CONTROL-plane only, so the store this applies is
/// `deploy/sql/control-portable-store.sql` — the `catalog.releases` the
/// attestation's foreign key targets is the control copy, not the project one.
#[test]
fn deployment_attestation_rust_binding_holds_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL") else {
        eprintln!(
            "skipping deployment_attestation_rust_binding_holds_on_postgres \
             (set WAMN_CONTROL_PORTABLE_PG_URL)"
        );
        return;
    };

    let hash = format!("sha256:{}", "a".repeat(64));
    let other_hash = format!("sha256:{}", "b".repeat(64));
    let attestation = wamn_schema_control::attestation::Attestation {
        tenant_id: "tenant-a",
        catalog_id: "orders",
        catalog_version: 7,
        org_id: "acme",
        project_id: "billing",
        environment: "prod",
        deployed_manifest_hash: &hash,
        attested_at: "2026-08-15T12:00:00Z",
    };
    let conflicting = wamn_schema_control::attestation::Attestation {
        deployed_manifest_hash: &other_hash,
        ..attestation
    };
    let write = wamn_schema_control::attestation::register_attestation(&attestation);
    let conflicting_write =
        wamn_schema_control::attestation::register_attestation(&conflicting);

    let mut script = String::from(
        "CREATE EXTENSION IF NOT EXISTS pgcrypto;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
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
         END $$;\n\
         DO $$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
           current_database()); END $$;\n\
         SET ROLE wamn_system;\n",
    );
    script.push_str(&ddl());
    script.push_str(&format!(
        r"
SET app.tenant = '{tenant}';
INSERT INTO catalog.catalogs
  (tenant_id,catalog_id,version,environment,schema_version,state)
VALUES ('{tenant}','{catalog}',{version},'{environment}','0.1','applied');
INSERT INTO catalog.releases
  (tenant_id,catalog_id,catalog_version)
VALUES ('{tenant}','{catalog}',{version});

-- The server's own answer about the Rust statement. It must PREPARE against the
-- installed routine at all, and it must type every position the way the binding
-- assumes: a dropped argument, a moved `$n` across the one non-text position, or
-- a lost `::text` on the instant each change this array.
PREPARE wamn_rust_attestation AS {prepared};
DO $types$ DECLARE found text[]; BEGIN
  SELECT parameter_types::text[] INTO found
    FROM pg_prepared_statements WHERE name = 'wamn_rust_attestation';
  ASSERT found = ARRAY['text','text','integer','text','text','text','text','text']::text[],
    format('PostgreSQL types the Rust binding as %s', found);
END $types$;
DEALLOCATE wamn_rust_attestation;

DO $bind$ DECLARE first_at timestamptz; BEGIN
  first_at := ({write});
  ASSERT first_at = ({write}),
    'the Rust-bound write is not idempotent on an exact retry';
  ASSERT (SELECT count(*) FROM catalog.deployment_attestations) = 1,
    'the exact retry of the Rust-bound write inserted a second row';
  -- Named field to named column. Two same-typed parts swapped in the binder or
  -- in the wrapper string would key the attestation under a placement nothing
  -- deployed to, and only the server can say which column each part landed in.
  ASSERT EXISTS (
    SELECT 1 FROM catalog.deployment_attestations
     WHERE tenant_id = '{tenant}'
       AND catalog_id = '{catalog}'
       AND catalog_version = {version}
       AND org_id = '{org}'
       AND project_id = '{project}'
       AND environment = '{environment}'
       AND deployed_manifest_hash = '{hash}'
       AND attested_at = first_at),
    'the Rust-bound write placed a part in the wrong column';
END $bind$;

-- The refusal is reported back rather than merely asserted here, so the Rust
-- boundary is translating the server's own SQLSTATE and message, not a copy.
DO $refusal$ DECLARE refused timestamptz; BEGIN
  BEGIN
    refused := ({conflicting});
    RAISE EXCEPTION 'a differing attestation at the same Rust-bound coordinate was accepted';
  EXCEPTION WHEN unique_violation THEN
    RAISE NOTICE 'WAMN-RUST-ATTESTATION-REFUSAL % %', SQLSTATE, SQLERRM;
  END;
END $refusal$;
RESET ROLE;
",
        tenant = attestation.tenant_id,
        catalog = attestation.catalog_id,
        version = attestation.catalog_version,
        org = attestation.org_id,
        project = attestation.project_id,
        environment = attestation.environment,
        hash = attestation.deployed_manifest_hash,
        prepared = write.sql,
        write = render(&write),
        conflicting = render(&conflicting_write),
    ));

    let output = psql(&url, &script);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "deployment-attestation Rust binding proof failed:\n{stderr}"
    );

    let reported = stderr
        .lines()
        .find_map(|line| line.split_once("WAMN-RUST-ATTESTATION-REFUSAL "))
        .map(|(_, refusal)| refusal.trim().to_owned())
        .expect("the server reported its refusal of the conflicting Rust-bound write");
    let (sqlstate, message) = reported
        .split_once(' ')
        .expect("the reported refusal carries a SQLSTATE and a message");

    // One condition, one literal: the boundary keys on what the DDL raises.
    assert_eq!(message, wamn_schema_control::attestation::CONTENT_CONFLICT);
    let error = wamn_schema_control::attestation::translate_failure(
        &conflicting,
        Some(sqlstate),
        message,
    );
    assert_eq!(
        error.kind(),
        wamn_schema_control::attestation::AttestationErrorKind::ContentConflict,
        "the server refused with {sqlstate}/{message:?} and the write's owning \
         boundary did not recognize it"
    );
    assert_eq!(error.coordinate(), "tenant-a/orders@7 -> acme/billing/prod");
    assert!(
        error
            .to_string()
            .starts_with(wamn_schema_control::attestation::CONTENT_CONFLICT)
    );
}

// ---------------------------------------------------------------------------
// wamn-0h0g.12.1 — the control half of the cut-3 record, proved as an UPGRADE.
//
// The artifact's retirement block exists to converge an ALREADY-PROVISIONED
// store, and every one of its arms is guarded by a presence probe. A gate that
// only ever applies to a fresh database therefore never takes a single true
// branch: a mutation that stops retiring is invisible to it. The gate below
// installs the legacy store first, so the convergence path is the path under
// test, and then proves the converged store is byte-identical to a fresh one and
// that replaying the artifact a third time changes nothing.
// ---------------------------------------------------------------------------

/// The legacy control store an already-provisioned database may still carry.
///
/// The foreign keys are not decoration: they are what fixes the artifact's drop
/// ORDER, and a fixture without them would pass against a retirement block whose
/// arms had been shuffled. `release_flows` sits on `execution_bundles` (the
/// documented reason the flow-era release plane retires BEFORE the bundles), the
/// evidence row sits on both `validated_flow_drafts` and `execution_bundles`
/// (the documented reason it retires before either), and the gate-report
/// children sit on the reservation (the documented child-first order). Every
/// drop in the artifact is `RESTRICT`, so a wrong order refuses instead of
/// amputating.
const LEGACY_CONTROL_STORE_SQL: &str = r"
SET ROLE wamn_system;

CREATE TABLE catalog.execution_bundles (
    tenant_id text NOT NULL,
    execution_bundle_hash text NOT NULL,
    bundle_bytes bytea NOT NULL,
    PRIMARY KEY (tenant_id, execution_bundle_hash)
);
CREATE TABLE catalog.flow_artifacts (
    tenant_id text NOT NULL,
    artifact_hash text NOT NULL,
    PRIMARY KEY (tenant_id, artifact_hash)
);
CREATE TABLE catalog.release_flows (
    tenant_id text NOT NULL,
    flow_id text NOT NULL,
    artifact_hash text NOT NULL,
    execution_bundle_hash text NOT NULL,
    PRIMARY KEY (tenant_id, flow_id),
    FOREIGN KEY (tenant_id, artifact_hash)
        REFERENCES catalog.flow_artifacts (tenant_id, artifact_hash),
    FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)
);
CREATE TABLE catalog.release_exposure_manifests (
    tenant_id text NOT NULL,
    manifest_id text NOT NULL,
    PRIMARY KEY (tenant_id, manifest_id)
);
CREATE TABLE catalog.release_sources (
    tenant_id text NOT NULL,
    source_id text NOT NULL,
    manifest_id text NOT NULL,
    PRIMARY KEY (tenant_id, source_id),
    FOREIGN KEY (tenant_id, manifest_id)
        REFERENCES catalog.release_exposure_manifests (tenant_id, manifest_id)
);
CREATE TABLE catalog.release_attachments (
    tenant_id text NOT NULL,
    attachment_id text NOT NULL,
    flow_id text NOT NULL,
    source_id text NOT NULL,
    PRIMARY KEY (tenant_id, attachment_id),
    FOREIGN KEY (tenant_id, flow_id)
        REFERENCES catalog.release_flows (tenant_id, flow_id),
    FOREIGN KEY (tenant_id, source_id)
        REFERENCES catalog.release_sources (tenant_id, source_id)
);

CREATE TABLE catalog.validated_flow_drafts (
    tenant_id text NOT NULL,
    draft_id text NOT NULL,
    PRIMARY KEY (tenant_id, draft_id)
);
CREATE TABLE catalog.draft_safe_connection_grants (
    tenant_id text NOT NULL,
    draft_id text NOT NULL,
    PRIMARY KEY (tenant_id, draft_id)
);
CREATE FUNCTION catalog.guard_flow_draft_update() RETURNS trigger
LANGUAGE plpgsql AS $legacy$ BEGIN RETURN NEW; END $legacy$;
CREATE TABLE catalog.flow_drafts (
    tenant_id text NOT NULL,
    draft_id text NOT NULL,
    revision int NOT NULL,
    PRIMARY KEY (tenant_id, draft_id)
);
CREATE TRIGGER flow_drafts_controlled_update BEFORE UPDATE ON catalog.flow_drafts
FOR EACH ROW EXECUTE FUNCTION catalog.guard_flow_draft_update();

CREATE TABLE catalog.release_flow_test_evidence (
    tenant_id text NOT NULL,
    catalog_id text NOT NULL,
    catalog_version int NOT NULL,
    flow_id text NOT NULL,
    draft_id text NOT NULL,
    execution_bundle_hash text NOT NULL,
    tested_resolution_map jsonb NOT NULL,
    PRIMARY KEY (tenant_id, catalog_id, catalog_version, flow_id),
    FOREIGN KEY (tenant_id, draft_id)
        REFERENCES catalog.validated_flow_drafts (tenant_id, draft_id),
    FOREIGN KEY (tenant_id, execution_bundle_hash)
        REFERENCES catalog.execution_bundles (tenant_id, execution_bundle_hash)
);
-- Two overloads on purpose: the artifact drops the registrar by NAME over every
-- overload, and a pinned-signature drop would leave the second standing as an
-- owner-only entry point onto a dropped relation.
CREATE FUNCTION catalog.register_release_flow_test_evidence(
    p_tenant_id text, p_catalog_id text, p_catalog_version int, p_flow_id text)
RETURNS void LANGUAGE plpgsql AS $legacy$ BEGIN RETURN; END $legacy$;
CREATE FUNCTION catalog.register_release_flow_test_evidence(
    p_tenant_id text, p_catalog_id text)
RETURNS void LANGUAGE plpgsql AS $legacy$ BEGIN RETURN; END $legacy$;

CREATE TABLE wamn_run.authoring_test_run_reservations (
    tenant_id text NOT NULL,
    reservation_id text NOT NULL,
    PRIMARY KEY (tenant_id, reservation_id)
);
CREATE TABLE wamn_run.authoring_test_case_runs (
    tenant_id text NOT NULL,
    reservation_id text NOT NULL,
    ordinal int NOT NULL,
    PRIMARY KEY (tenant_id, reservation_id, ordinal),
    FOREIGN KEY (tenant_id, reservation_id)
        REFERENCES wamn_run.authoring_test_run_reservations
            (tenant_id, reservation_id)
);
CREATE TABLE wamn_run.authoring_test_reports (
    tenant_id text NOT NULL,
    reservation_id text NOT NULL,
    PRIMARY KEY (tenant_id, reservation_id),
    FOREIGN KEY (tenant_id, reservation_id)
        REFERENCES wamn_run.authoring_test_run_reservations
            (tenant_id, reservation_id)
);

ALTER TABLE catalog.deployment_attestations
    ADD COLUMN deployed_resolution_map jsonb;
CREATE FUNCTION catalog.register_deployment_attestation(
    p_tenant_id text, p_catalog_id text, p_catalog_version int, p_org_id text,
    p_project_id text, p_environment text, p_deployed_manifest_hash text,
    p_deployed_resolution_map jsonb, p_attested_at timestamptz)
RETURNS timestamptz LANGUAGE plpgsql
AS $legacy$ BEGIN RETURN p_attested_at; END $legacy$;
RESET ROLE;
";

/// The legacy fixture really landed, asked of the server rather than assumed
/// from the fixture's own exit status: an injection that silently no-opped would
/// turn the upgrade proof back into the virgin install it exists to replace.
const LEGACY_CONTROL_STORE_PRESENT_SQL: &str = r"
DO $legacy_present$
DECLARE
    legacy text;
BEGIN
    FOREACH legacy IN ARRAY ARRAY[
        'catalog.execution_bundles', 'catalog.flow_artifacts',
        'catalog.release_flows', 'catalog.release_exposure_manifests',
        'catalog.release_sources', 'catalog.release_attachments',
        'catalog.validated_flow_drafts', 'catalog.draft_safe_connection_grants',
        'catalog.flow_drafts', 'catalog.release_flow_test_evidence',
        'wamn_run.authoring_test_run_reservations',
        'wamn_run.authoring_test_case_runs', 'wamn_run.authoring_test_reports'
    ]
    LOOP
        ASSERT to_regclass(legacy) IS NOT NULL,
            format('the legacy fixture did not install %s', legacy);
    END LOOP;
    ASSERT (SELECT count(*) FROM pg_catalog.pg_proc AS routine
             JOIN pg_catalog.pg_namespace AS namespace
               ON namespace.oid = routine.pronamespace
            WHERE namespace.nspname = 'catalog'
              AND routine.proname = 'register_release_flow_test_evidence') = 2,
        'the legacy fixture did not install both evidence registrar overloads';
    ASSERT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute
                    WHERE attrelid = 'catalog.deployment_attestations'::regclass
                      AND attname = 'deployed_resolution_map'
                      AND NOT attisdropped),
        'the legacy fixture did not install the superseded attestation column';
    ASSERT EXISTS (SELECT 1 FROM pg_catalog.pg_proc AS routine
                    JOIN pg_catalog.pg_namespace AS namespace
                      ON namespace.oid = routine.pronamespace
                   WHERE namespace.nspname = 'catalog'
                     AND routine.proname = 'register_deployment_attestation'
                     AND routine.pronargs = 9),
        'the legacy fixture did not install the superseded attestation overload';
    ASSERT EXISTS (SELECT 1 FROM pg_catalog.pg_proc AS routine
                    JOIN pg_catalog.pg_namespace AS namespace
                      ON namespace.oid = routine.pronamespace
                   WHERE namespace.nspname = 'catalog'
                     AND routine.proname = 'guard_flow_draft_update'),
        'the legacy fixture did not install the draft guard function';
END
$legacy_present$;
";

/// Not one legacy object survives the upgrade — relation, routine overload, or
/// column. Asked of the server, because the convergence arm is the one an
/// already-provisioned store takes and a fresh apply never does.
const LEGACY_CONTROL_STORE_ABSENT_SQL: &str = r"
DO $legacy_absent$
DECLARE
    legacy text;
BEGIN
    FOREACH legacy IN ARRAY ARRAY[
        'catalog.execution_bundles', 'catalog.flow_artifacts',
        'catalog.release_flows', 'catalog.release_exposure_manifests',
        'catalog.release_sources', 'catalog.release_attachments',
        'catalog.validated_flow_drafts', 'catalog.draft_safe_connection_grants',
        'catalog.flow_drafts', 'catalog.release_flow_test_evidence',
        'wamn_run.authoring_test_run_reservations',
        'wamn_run.authoring_test_case_runs', 'wamn_run.authoring_test_reports'
    ]
    LOOP
        ASSERT to_regclass(legacy) IS NULL,
            format('the retirement block left %s installed', legacy);
    END LOOP;
    ASSERT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc AS routine
                        JOIN pg_catalog.pg_namespace AS namespace
                          ON namespace.oid = routine.pronamespace
                       WHERE namespace.nspname = 'catalog'
                         AND routine.proname IN (
                               'register_release_flow_test_evidence',
                               'guard_flow_draft_update')),
        'a retired routine survived the upgrade';
    ASSERT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc AS routine
                        JOIN pg_catalog.pg_namespace AS namespace
                          ON namespace.oid = routine.pronamespace
                       WHERE namespace.nspname = 'catalog'
                         AND routine.proname = 'register_deployment_attestation'
                         AND routine.pronargs <> 8),
        'a superseded attestation overload survived the upgrade';
    ASSERT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute
                        WHERE attrelid = 'catalog.deployment_attestations'::regclass
                          AND attname = 'deployed_resolution_map'
                          AND NOT attisdropped),
        'the superseded attestation column survived the upgrade';
END
$legacy_absent$;
";

/// The whole installed record as the SERVER reports it, in one digest.
///
/// This is deliberately NOT compared against a pinned literal. A pinned digest
/// over a static checked-in artifact is near-tautological and breaks on
/// reformatting; what it cannot be is wrong about EQUALITY, which is the
/// property the upgrade proof needs: a converged legacy store must reach exactly
/// the record a fresh install reaches, and a replay must move nothing. Relation,
/// owner, row security, column, constraint, index, trigger, policy, table grant,
/// column grant, routine and routine grant are all folded in, so any of them
/// moving separates two digests that must be equal.
const CONTROL_RECORD_FINGERPRINT_SQL: &str = r#"
WITH scope AS (
    SELECT relation.oid, namespace.nspname, relation.relname, relation.relkind
      FROM pg_catalog.pg_class AS relation
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = relation.relnamespace
     WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
       AND relation.relkind IN ('r', 'p', 'i', 'S', 'v', 'm')
), facts AS (
    SELECT format('relation|%s.%s|%s|%s|%s|%s', scope.nspname, scope.relname,
                  scope.relkind, pg_catalog.pg_get_userbyid(relation.relowner),
                  relation.relrowsecurity, relation.relforcerowsecurity) AS fact
      FROM scope JOIN pg_catalog.pg_class AS relation ON relation.oid = scope.oid
    UNION ALL
    SELECT format('column|%s.%s|%s|%s|%s|%s|%s', scope.nspname, scope.relname,
                  att.attnum, att.attname,
                  pg_catalog.format_type(att.atttypid, att.atttypmod),
                  att.attnotnull,
                  COALESCE(pg_catalog.pg_get_expr(def.adbin, def.adrelid, true), '-'))
      FROM scope
      JOIN pg_catalog.pg_attribute AS att
        ON att.attrelid = scope.oid AND att.attnum > 0 AND NOT att.attisdropped
      LEFT JOIN pg_catalog.pg_attrdef AS def
        ON def.adrelid = att.attrelid AND def.adnum = att.attnum
    UNION ALL
    SELECT format('constraint|%s.%s|%s|%s', scope.nspname, scope.relname,
                  con.conname, pg_catalog.pg_get_constraintdef(con.oid, true))
      FROM scope JOIN pg_catalog.pg_constraint AS con ON con.conrelid = scope.oid
    UNION ALL
    SELECT format('index|%s.%s|%s', scope.nspname, scope.relname,
                  pg_catalog.pg_get_indexdef(idx.indexrelid))
      FROM scope JOIN pg_catalog.pg_index AS idx ON idx.indrelid = scope.oid
    UNION ALL
    SELECT format('trigger|%s.%s|%s|%s', scope.nspname, scope.relname, trg.tgname,
                  pg_catalog.pg_get_triggerdef(trg.oid, true))
      FROM scope
      JOIN pg_catalog.pg_trigger AS trg
        ON trg.tgrelid = scope.oid AND NOT trg.tgisinternal
    UNION ALL
    SELECT format('policy|%s.%s|%s|%s|%s|%s|%s|%s', scope.nspname, scope.relname,
                  pol.polname, pol.polcmd, pol.polpermissive,
                  (SELECT string_agg(role_name, ',' ORDER BY role_name COLLATE "C")
                     FROM (SELECT CASE grantee WHEN 0 THEN 'PUBLIC'
                                  ELSE pg_catalog.pg_get_userbyid(grantee) END
                             FROM unnest(pol.polroles) AS grantee)
                          AS roles(role_name)),
                  COALESCE(pg_catalog.pg_get_expr(pol.polqual, pol.polrelid, true), '-'),
                  COALESCE(pg_catalog.pg_get_expr(pol.polwithcheck, pol.polrelid, true), '-'))
      FROM scope JOIN pg_catalog.pg_policy AS pol ON pol.polrelid = scope.oid
    UNION ALL
    SELECT format('table-grant|%s.%s|%s|%s|%s', scope.nspname, scope.relname,
                  CASE acl.grantee WHEN 0 THEN 'PUBLIC'
                  ELSE pg_catalog.pg_get_userbyid(acl.grantee) END,
                  acl.privilege_type, acl.is_grantable)
      FROM scope
      JOIN pg_catalog.pg_class AS relation ON relation.oid = scope.oid
     CROSS JOIN LATERAL pg_catalog.aclexplode(
       COALESCE(relation.relacl,
                pg_catalog.acldefault('r', relation.relowner))) AS acl
     WHERE scope.relkind IN ('r', 'p')
    UNION ALL
    SELECT format('column-grant|%s.%s|%s|%s|%s|%s', scope.nspname, scope.relname,
                  att.attname,
                  CASE acl.grantee WHEN 0 THEN 'PUBLIC'
                  ELSE pg_catalog.pg_get_userbyid(acl.grantee) END,
                  acl.privilege_type, acl.is_grantable)
      FROM scope
      JOIN pg_catalog.pg_attribute AS att
        ON att.attrelid = scope.oid AND att.attnum > 0 AND NOT att.attisdropped
     CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) AS acl
     WHERE att.attacl IS NOT NULL
    UNION ALL
    SELECT format('routine|%s|%s|%s|%s', routine.oid::regprocedure::text,
                  pg_catalog.pg_get_userbyid(routine.proowner), routine.prosecdef,
                  COALESCE(array_to_string(routine.proconfig, ','), '-'))
      FROM pg_catalog.pg_proc AS routine
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = routine.pronamespace
     WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
    UNION ALL
    SELECT format('routine-grant|%s|%s|%s|%s', routine.oid::regprocedure::text,
                  CASE acl.grantee WHEN 0 THEN 'PUBLIC'
                  ELSE pg_catalog.pg_get_userbyid(acl.grantee) END,
                  acl.privilege_type, acl.is_grantable)
      FROM pg_catalog.pg_proc AS routine
      JOIN pg_catalog.pg_namespace AS namespace
        ON namespace.oid = routine.pronamespace
     CROSS JOIN LATERAL pg_catalog.aclexplode(
       COALESCE(routine.proacl,
                pg_catalog.acldefault('f', routine.proowner))) AS acl
     WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
    UNION ALL
    SELECT format('schema-grant|%s|%s|%s|%s', namespace.nspname,
                  CASE acl.grantee WHEN 0 THEN 'PUBLIC'
                  ELSE pg_catalog.pg_get_userbyid(acl.grantee) END,
                  acl.privilege_type, acl.is_grantable)
      FROM pg_catalog.pg_namespace AS namespace
     CROSS JOIN LATERAL pg_catalog.aclexplode(
       COALESCE(namespace.nspacl,
                pg_catalog.acldefault('n', namespace.nspowner))) AS acl
     WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
)
SELECT encode(sha256(convert_to(
         string_agg(fact, E'\n' ORDER BY fact COLLATE "C"), 'UTF8')), 'hex')
  FROM facts;
"#;

/// The independent drift check over the installed control record: the exact
/// relation, owner, row-security, trigger, policy, table-grant and column-grant
/// inventories, each asserted as a SET so a missing entry and an excess entry
/// fail alike.
///
/// Independent of the artifact in two senses. It asks pg_catalog, aclexplode and
/// pg_policy rather than reading the DDL text — a scan over static checked-in SQL
/// cannot tell a declaration from a comment mentioning one, and near enough only
/// asserts that a file contains what the file contains. And it pins what the
/// artifact does NOT self-guard: `$drift$` covers the relation inventory, the
/// retained column and constraint facts and the attestation shape, but nothing
/// in the artifact asserts an owner, an enabled-and-forced row-security
/// inventory, a trigger inventory or a policy inventory, and `$author_bounds$`
/// bounds exactly one principal rather than the whole grant surface.
const CONTROL_RECORD_INVENTORY_SQL: &str = r#"
DO $record$
DECLARE
    observed text[];
BEGIN
    SELECT array_agg(tablename ORDER BY tablename) INTO observed
      FROM pg_catalog.pg_tables WHERE schemaname = 'catalog';
    ASSERT observed = ARRAY[
        'authoring_command_audit', 'catalog_heads', 'catalogs',
        'component_library', 'connection_requirements',
        'deployment_attestations', 'releases']::text[],
        format('control catalog relation inventory drifted: %s', observed);

    SELECT array_agg(tablename ORDER BY tablename) INTO observed
      FROM pg_catalog.pg_tables WHERE schemaname = 'wamn_run';
    ASSERT observed = ARRAY['gate_reports']::text[],
        format('control wamn_run relation inventory drifted: %s', observed);

    SELECT array_agg(tablename ORDER BY tablename) INTO observed
      FROM pg_catalog.pg_tables WHERE schemaname = 'wamn_authority';
    ASSERT observed = ARRAY['author_login_tenants']::text[],
        format('control wamn_authority relation inventory drifted: %s', observed);

    -- Owner. Nothing in the artifact asserts this, and a relation created by
    -- anyone but the store owner would still satisfy every CREATE ... IF NOT
    -- EXISTS above it while escaping FORCE ROW LEVEL SECURITY's owner arm.
    SELECT array_agg(entry ORDER BY entry COLLATE "C") INTO observed FROM (
      SELECT format('%s.%s:%s', namespace.nspname, relation.relname,
                    pg_catalog.pg_get_userbyid(relation.relowner)) AS entry
        FROM pg_catalog.pg_class AS relation
        JOIN pg_catalog.pg_namespace AS namespace
          ON namespace.oid = relation.relnamespace
       WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
         AND relation.relkind IN ('r', 'p')
         AND pg_catalog.pg_get_userbyid(relation.relowner) <> 'wamn_system'
    ) AS entries;
    ASSERT observed IS NULL,
        format('a control relation is not owned by the store owner: %s', observed);

    -- Row security, as an exact inventory of the relations that carry it. The
    -- mapping relation deliberately carries none: the resolver over it is
    -- SECURITY DEFINER and FORCE would make every author session resolve NULL.
    SELECT array_agg(entry ORDER BY entry COLLATE "C") INTO observed FROM (
      SELECT format('%s.%s', namespace.nspname, relation.relname) AS entry
        FROM pg_catalog.pg_class AS relation
        JOIN pg_catalog.pg_namespace AS namespace
          ON namespace.oid = relation.relnamespace
       WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
         AND relation.relkind IN ('r', 'p')
         AND relation.relrowsecurity AND relation.relforcerowsecurity
    ) AS entries;
    ASSERT observed = ARRAY[
        'catalog.authoring_command_audit', 'catalog.catalog_heads',
        'catalog.catalogs', 'catalog.component_library',
        'catalog.connection_requirements', 'catalog.deployment_attestations',
        'catalog.releases', 'wamn_run.gate_reports']::text[],
        format('the enabled-and-forced row-security inventory drifted: %s', observed);
    ASSERT NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_class AS relation
          JOIN pg_catalog.pg_namespace AS namespace
            ON namespace.oid = relation.relnamespace
         WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
           AND relation.relkind IN ('r', 'p')
           AND relation.relrowsecurity <> relation.relforcerowsecurity),
        'a control relation enables row security without forcing it';

    -- Trigger inventory. `catalog.catalogs` is the one record relation with no
    -- immutability trigger, because the lifecycle rewrites its state column.
    SELECT array_agg(entry ORDER BY entry COLLATE "C") INTO observed FROM (
      SELECT format('%s.%s:%s', namespace.nspname, relation.relname,
                    trg.tgname) AS entry
        FROM pg_catalog.pg_trigger AS trg
        JOIN pg_catalog.pg_class AS relation ON relation.oid = trg.tgrelid
        JOIN pg_catalog.pg_namespace AS namespace
          ON namespace.oid = relation.relnamespace
       WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
         AND NOT trg.tgisinternal
    ) AS entries;
    ASSERT observed = ARRAY[
        'catalog.authoring_command_audit:authoring_command_audit_immutable',
        'catalog.component_library:component_library_immutable',
        'catalog.connection_requirements:connection_requirements_immutable',
        'catalog.deployment_attestations:deployment_attestations_immutable',
        'catalog.releases:releases_immutable',
        'wamn_run.gate_reports:gate_reports_immutable']::text[],
        format('the control trigger inventory drifted: %s', observed);

    -- Policy inventory: one tenant policy per row-secured relation, and one
    -- restrictive author policy per relation the author may reach.
    SELECT array_agg(entry ORDER BY entry COLLATE "C") INTO observed FROM (
      SELECT format('%s.%s:%s', namespace.nspname, relation.relname,
                    pol.polname) AS entry
        FROM pg_catalog.pg_policy AS pol
        JOIN pg_catalog.pg_class AS relation ON relation.oid = pol.polrelid
        JOIN pg_catalog.pg_namespace AS namespace
          ON namespace.oid = relation.relnamespace
       WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
    ) AS entries;
    ASSERT observed = ARRAY[
        'catalog.authoring_command_audit:authoring_command_audit_author_tenant',
        'catalog.authoring_command_audit:authoring_command_audit_tenant',
        'catalog.catalog_heads:catalog_heads_author_tenant',
        'catalog.catalog_heads:catalog_heads_tenant',
        'catalog.catalogs:catalogs_tenant',
        'catalog.component_library:component_library_tenant',
        'catalog.connection_requirements:connection_requirements_author_tenant',
        'catalog.connection_requirements:connection_requirements_tenant',
        'catalog.deployment_attestations:deployment_attestations_tenant',
        'catalog.releases:releases_author_tenant',
        'catalog.releases:releases_tenant',
        'wamn_run.gate_reports:gate_reports_author_tenant',
        'wamn_run.gate_reports:gate_reports_tenant']::text[],
        format('the control policy inventory drifted: %s', observed);

    -- Table grants, to every grantee that is not the relation's own owner, so
    -- an added grantee and a widened privilege both surface. PUBLIC is inside
    -- this set, not excluded from it.
    SELECT array_agg(entry ORDER BY entry COLLATE "C") INTO observed FROM (
      SELECT format('%s.%s:%s:%s', namespace.nspname, relation.relname,
                    CASE acl.grantee WHEN 0 THEN 'PUBLIC'
                    ELSE pg_catalog.pg_get_userbyid(acl.grantee) END,
                    acl.privilege_type) AS entry
        FROM pg_catalog.pg_class AS relation
        JOIN pg_catalog.pg_namespace AS namespace
          ON namespace.oid = relation.relnamespace
       CROSS JOIN LATERAL pg_catalog.aclexplode(
         COALESCE(relation.relacl,
                  pg_catalog.acldefault('r', relation.relowner))) AS acl
       WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
         AND relation.relkind IN ('r', 'p')
         AND acl.grantee <> relation.relowner
    ) AS entries;
    ASSERT observed = ARRAY[
        'catalog.authoring_command_audit:wamn_control_author:INSERT',
        'catalog.authoring_command_audit:wamn_control_author:SELECT',
        'catalog.catalog_heads:wamn_control_author:SELECT',
        'catalog.connection_requirements:wamn_control_author:SELECT',
        'catalog.releases:wamn_control_author:SELECT',
        'wamn_run.gate_reports:wamn_control_author:INSERT',
        'wamn_run.gate_reports:wamn_control_author:SELECT']::text[],
        format('the control table-grant surface drifted: %s', observed);

    -- Column grants: the control record holds none at all. A column grant is
    -- invisible to has_table_privilege, so a store that acquired one would pass
    -- every relation-level check above it.
    SELECT array_agg(entry ORDER BY entry COLLATE "C") INTO observed FROM (
      SELECT format('%s.%s.%s:%s:%s', namespace.nspname, relation.relname,
                    att.attname,
                    CASE acl.grantee WHEN 0 THEN 'PUBLIC'
                    ELSE pg_catalog.pg_get_userbyid(acl.grantee) END,
                    acl.privilege_type) AS entry
        FROM pg_catalog.pg_class AS relation
        JOIN pg_catalog.pg_namespace AS namespace
          ON namespace.oid = relation.relnamespace
        JOIN pg_catalog.pg_attribute AS att
          ON att.attrelid = relation.oid AND att.attnum > 0
         AND NOT att.attisdropped
       CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) AS acl
       WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
         AND att.attacl IS NOT NULL
    ) AS entries;
    ASSERT observed IS NULL,
        format('the control record acquired a column grant: %s', observed);

    -- Schema and routine grants, same rule: everything that is not the owner's.
    SELECT array_agg(entry ORDER BY entry COLLATE "C") INTO observed FROM (
      SELECT format('%s:%s:%s', namespace.nspname,
                    CASE acl.grantee WHEN 0 THEN 'PUBLIC'
                    ELSE pg_catalog.pg_get_userbyid(acl.grantee) END,
                    acl.privilege_type) AS entry
        FROM pg_catalog.pg_namespace AS namespace
       CROSS JOIN LATERAL pg_catalog.aclexplode(
         COALESCE(namespace.nspacl,
                  pg_catalog.acldefault('n', namespace.nspowner))) AS acl
       WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
         AND acl.grantee <> namespace.nspowner
    ) AS entries;
    ASSERT observed = ARRAY[
        'catalog:wamn_control_author:USAGE',
        'wamn_authority:wamn_control_author:USAGE',
        'wamn_run:wamn_control_author:USAGE']::text[],
        format('the control schema-grant surface drifted: %s', observed);

    SELECT array_agg(entry ORDER BY entry COLLATE "C") INTO observed FROM (
      SELECT format('%s:%s:%s', routine.oid::regprocedure::text,
                    CASE acl.grantee WHEN 0 THEN 'PUBLIC'
                    ELSE pg_catalog.pg_get_userbyid(acl.grantee) END,
                    acl.privilege_type) AS entry
        FROM pg_catalog.pg_proc AS routine
        JOIN pg_catalog.pg_namespace AS namespace
          ON namespace.oid = routine.pronamespace
       CROSS JOIN LATERAL pg_catalog.aclexplode(
         COALESCE(routine.proacl,
                  pg_catalog.acldefault('f', routine.proowner))) AS acl
       WHERE namespace.nspname IN ('catalog', 'wamn_run', 'wamn_authority')
         AND acl.grantee <> routine.proowner
    ) AS entries;
    ASSERT observed = ARRAY[
        'wamn_authority.session_author_tenant():wamn_control_author:EXECUTE'
        ]::text[],
        format('the control routine-grant surface drifted: %s', observed);
END
$record$;
"#;

/// Run one script through `psql` in unaligned tuples-only mode and return the
/// single value it produced.
fn psql_scalar(url: &str, stage: &str, script: &str) -> String {
    let mut child = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-A", "-t", "-q", "-f", "-"])
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
        "{stage} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(!value.is_empty(), "{stage} produced no value");
    value
}

/// Drop the three control schemas and every privilege the artifact granted, so a
/// leftover healthy object cannot satisfy a `CREATE ... IF NOT EXISTS` and mask
/// a mutated artifact. `DROP OWNED BY` runs inside an existence check because
/// the ACL role is created by ctl, not by this gate, and it is never dropped:
/// the two-tenant gate's scoped generations are members of it.
fn control_store_reset_sql() -> String {
    let mut sql = String::from(CURRENT_DATABASE_PUBLIC_CONNECT_SQL);
    sql.push_str(
        "CREATE EXTENSION IF NOT EXISTS pgcrypto;\n\
         DROP SCHEMA IF EXISTS catalog CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_run CASCADE;\n\
         DROP SCHEMA IF EXISTS wamn_authority CASCADE;\n\
         DO $reset$ BEGIN\n\
           IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_system') THEN\n\
             CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOINHERIT NOREPLICATION NOBYPASSRLS;\n\
           END IF;\n\
           IF EXISTS (SELECT FROM pg_roles WHERE rolname='wamn_control_author') THEN\n\
             DROP OWNED BY wamn_control_author;\n\
           END IF;\n\
         END $reset$;\n",
    );
    sql.push_str(&wamn_control_provision::sql::ensure_control_author_acl_role_sql());
    sql.push_str(
        "\nDO $grant$ BEGIN EXECUTE format('GRANT CREATE ON DATABASE %I TO wamn_system', \
           current_database()); END $grant$;\n",
    );
    sql
}

/// One apply of the artifact, as the store owner — the same text and the same
/// principal provisioning uses.
fn control_store_apply_sql() -> String {
    format!("SET ROLE wamn_system;\n{}", ddl())
}

/// wamn-0h0g.12.1: the control record proved as an UPGRADE, not a virgin
/// install.
///
/// A fresh store, then the legacy store an already-provisioned database still
/// carries, then the artifact — twice. Three properties, none of which an
/// exit-status-only convergence test can see:
///
/// 1. the converged legacy store reaches EXACTLY the record a fresh install
///    reaches, so retirement is a convergence and not a second shape;
/// 2. a third apply moves nothing at all, so replay is genuinely a no-op rather
///    than merely non-failing;
/// 3. no legacy relation, routine overload or column survives.
///
/// Then the independent record inventory: owner, row security, trigger, policy
/// and grant surface, asked of pg_catalog rather than read off the DDL text.
#[test]
fn control_portable_store_converges_a_legacy_store_to_the_fresh_record_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL") else {
        eprintln!(
            "skipping control_portable_store_converges_a_legacy_store_to_the_fresh_record_on_postgres \
             (set WAMN_CONTROL_PORTABLE_PG_URL)"
        );
        return;
    };

    let apply = control_store_apply_sql();
    psql_ok(
        &url,
        "reset the control schemas",
        &control_store_reset_sql(),
    );
    psql_ok(&url, "the fresh apply", &apply);
    let fresh = psql_scalar(
        &url,
        "the fresh record fingerprint",
        CONTROL_RECORD_FINGERPRINT_SQL,
    );

    psql_ok(&url, "install the legacy store", LEGACY_CONTROL_STORE_SQL);
    psql_ok(
        &url,
        "the legacy store is really installed",
        LEGACY_CONTROL_STORE_PRESENT_SQL,
    );

    psql_ok(&url, "the upgrade apply", &apply);
    let upgraded = psql_scalar(
        &url,
        "the upgraded record fingerprint",
        CONTROL_RECORD_FINGERPRINT_SQL,
    );
    psql_ok(&url, "the replay apply", &apply);
    let replayed = psql_scalar(
        &url,
        "the replayed record fingerprint",
        CONTROL_RECORD_FINGERPRINT_SQL,
    );

    assert_eq!(
        upgraded, fresh,
        "converging a legacy store did not reach the fresh control record"
    );
    assert_eq!(
        replayed, upgraded,
        "replaying the artifact over a converged store was not a no-op"
    );

    psql_ok(
        &url,
        "no legacy object survived the upgrade",
        LEGACY_CONTROL_STORE_ABSENT_SQL,
    );
    psql_ok(
        &url,
        "the control record inventory",
        CONTROL_RECORD_INVENTORY_SQL,
    );
}

/// The two legacy shapes the artifact deliberately REFUSES rather than
/// half-migrates, because neither can be reached by an `ALTER` on a retained
/// relation: a dropped column keeps its `attnum` and an added one lands past the
/// tail, and `control-portable-retained-shape-drift` hashes `attnum` itself.
///
/// Both arms are invisible to any gate that applies to a fresh database, and an
/// arm that stopped refusing would let a store past the retirement block and
/// into the drift guard, where it would fail under a DIFFERENT name — so the
/// refusal is asserted by message, not merely by non-zero exit.
#[test]
fn control_portable_store_refuses_legacy_shapes_that_cannot_converge_on_postgres() {
    let Ok(url) = std::env::var("WAMN_CONTROL_PORTABLE_PG_URL") else {
        eprintln!(
            "skipping control_portable_store_refuses_legacy_shapes_that_cannot_converge_on_postgres \
             (set WAMN_CONTROL_PORTABLE_PG_URL)"
        );
        return;
    };
    let apply = control_store_apply_sql();

    for (label, legacy, refusal) in [
        (
            "the sealed member snapshot on the release identity row",
            "SET ROLE wamn_system;\n\
             ALTER TABLE catalog.releases ADD COLUMN members_json jsonb NOT NULL \
               DEFAULT '[]'::jsonb;\n\
             RESET ROLE;\n",
            "control-portable-retired-release-members-requires-reprovision",
        ),
        (
            "an audit ledger predating the ratified retry columns",
            "SET ROLE wamn_system;\n\
             ALTER TABLE catalog.authoring_command_audit DROP COLUMN request_hash;\n\
             RESET ROLE;\n",
            "control-portable-retired-audit-ledger-requires-reprovision",
        ),
    ] {
        psql_ok(
            &url,
            "reset the control schemas",
            &control_store_reset_sql(),
        );
        psql_ok(&url, "the fresh apply", &apply);
        psql_ok(&url, label, legacy);

        let refused = psql(&url, &apply);
        assert!(
            !refused.status.success(),
            "{label} was accepted instead of refused"
        );
        let stderr = String::from_utf8_lossy(&refused.stderr).into_owned();
        assert!(
            stderr.contains(refusal),
            "{label} refused under the wrong name; wanted {refusal} in:\n{stderr}"
        );
    }

    // Leave the shared database on the record, not on a deliberately broken
    // shape: the suite runs single-threaded against one cluster.
    psql_ok(
        &url,
        "reset the control schemas",
        &control_store_reset_sql(),
    );
    psql_ok(&url, "the fresh apply", &apply);
}

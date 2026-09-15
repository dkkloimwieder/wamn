//! The hand-written DDL's tenant floor derives from `current_user`.
//!
//! `wamn-0h0g.22.6.3` established the guest tenant floor across four artifacts;
//! the current package-era relation set contains 39 governed relations, all off
//! the settable `app.tenant` claim and onto
//! `wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()`,
//! each with the expression index that keeps the predicate sargable.
//!
//! `wamn-0h0g.22.17` gave every governed relation a SECOND arm. The floor is the GUEST
//! floor, narrowed `TO wamn_app`; PostgreSQL default-denies when RLS is enabled
//! and no policy matches the connected role, so that narrowing LOCKS OUT every
//! platform principal rather than exempting it — and locks it out at zero rows,
//! not at an error. One permissive arm `TO wamn_platform` per relation is what
//! admits them, and their table grants stay the thing that limits them.
//!
//! The tests apply the authored artifacts and assert the server's answer from
//! `pg_policy`, `pg_index`, ACL catalogs, and authenticated sessions.
//!
//! The record-history cases (`wamn-emtx.2`) apply `deploy/sql/record-history.sql`
//! through `CATALOG_SCHEMA_SQL` and write as the production `wamn_app` guest.
//! They test `wamn_history.stamp_row()` against spec tests 1, 3, 4, 8, 9, and 12.
//! The record-history log cases (`wamn-emtx.11`) test
//! `wamn_history.create_history_table` and `wamn_history.log_row_change()`
//! against level-2 spec tests 2, 3, 4, 5, 7, 8, 10, and 16.
//! The `app_system` history case (`wamn-emtx.21`) tests spec test 20 for the
//! six history tables of `deploy/sql/app-schema.sql`.
//! Every case starts a PostgreSQL server of its own, because every case
//! rebuilds cluster-wide roles and the `wamn` database.

use std::process::Command;

use wamn_control_provision::CredentialGeneration;
use wamn_control_provision::sql;
use wamn_control_provision::tenant_key::tenant_key;
use wamn_control_provision::workload_role::{
    WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
};
use wamn_record_history::{HistoryRow, RowState, state_at};

const POSTGRES_INIT: &str = include_str!("../../../../deploy/sql/postgres-init.sql");
const CATALOG_SCHEMA: &str = wamn_catalog::CATALOG_SCHEMA_SQL;
const RUN_STATE: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");
const RECORD_HISTORY: &str = include_str!("../../../../deploy/sql/record-history.sql");
const RECORD_HISTORY_APP_GRANTS: &str =
    include_str!("../../../../deploy/sql/record-history-app-grants.sql");

/// The role this file's live arm mints as its control probe. Named here so
/// `reset` can drop it: roles are CLUSTER-wide, and the arm's whole point is
/// that a leftover healthy membership masks a mutated builder.
const PLATFORM_PROBE_OUTSIDER: &str = "wamn_floor_outsider";

/// The superuser login, not named `postgres`, that one case installs the
/// catalog schema as (`wamn-txmd`). Named here so `reset` can drop it.
const CATALOG_INSTALLER: &str = "wamn_floor_installer";

/// The PLATFORM-GRAIN generation login the shared `TO wamn_platform` arm is
/// probed with, composed by the real mint rather than spelled by hand.
///
/// `Retention`, NOT `EffectWriter` (`wamn-0h0g.22.40`). `wamn-0h0g.22.32`
/// demoted the writer out of the platform grain, so a writer probe measures the
/// PER-RELATION `TO wamn_effect_writer` arm and says nothing at all about the
/// shared one. `Retention` is platform grain AND holds a real grant on a
/// governed relation — `wamn_run.runs` — which the arm below reads back from
/// `has_any_column_privilege` rather than assuming.
fn platform_probe_retention() -> String {
    workload_generation_role(
        WorkloadRoleFamily::Retention,
        WorkloadRoleScope::Tenant {
            tenant: "t1",
            database: "wamn",
        },
        CredentialGeneration::A,
    )
    .expect("Retention takes a tenant scope")
}

/// The effect-writer generation login the PER-RELATION `TO wamn_effect_writer`
/// arms are probed with.
///
/// KEPT when `wamn-0h0g.22.40` re-pointed the platform probe above: this is the
/// only cover those arms have anywhere, and it is what killed
/// `wamn-0h0g.22.32`'s second mutant.
fn effect_writer_probe() -> String {
    workload_generation_role(
        WorkloadRoleFamily::EffectWriter,
        WorkloadRoleScope::Tenant {
            tenant: "t1",
            database: "wamn",
        },
        CredentialGeneration::A,
    )
    .expect("EffectWriter takes a tenant scope")
}

/// The two relations whose claim is HOST-INJECTED, measured from
/// `has_table_privilege` rather than assumed: the guest ACL role holds nothing
/// on either, so re-keying them would be change without a threat.
const HOST_INJECTED: [&str; 2] = ["wamn_run.operator_run_actions", "wamn_run.run_queue"];

/// THE PLATFORM-GRAIN FAMILY SET, PINNED AS A VALUE (`wamn-0h0g.15.137.1`).
///
/// # Why a literal and not a call to `is_platform_grain`
///
/// The live arm below used to DERIVE its expected member set from the very
/// function it was measuring, so mutating `is_platform_grain` moved BOTH SIDES
/// and the assertion stayed true. SHOWN, not suspected: a mutant setting
/// `is_platform_grain(Retention)` to false AND removing the matching edge from
/// `postgres-init.sql` did NOT kill that test. Only the live retention gate saw
/// the consequence — `reported_one = false`, `old_gone = false`: a SILENT
/// LOCKOUT WITH EXIT 0.
///
/// That is the tautology shape this branch has paid for before: when every
/// consumer delegates to one function, a test comparing two of them shows
/// nothing. The remedy is the same as last time — PIN THE VALUE, and keep one
/// consumer that does NOT delegate. Admitting or demoting a family now costs one
/// deliberate edit here, which is the point.
///
/// # Deliberate changes
///
/// `wamn-0h0g.22.32` DEMOTED `wamn_effect_writer` — 8 members down to 7 — and
/// this edit is the deliberate cost that pin was built to charge. The reason is
/// not preference: the writer's stable role is under
/// `RunPlaneActionKind::VerifyEffectWriterRole`, which refuses the role with
/// 42501 `effect-writer-role-out-of-bounds` when it holds ANY `pg_auth_members`
/// row as a member. The membership and the guard cannot both hold. The writer
/// keeps its reads through PER-RELATION arms naming it directly in
/// `deploy/sql/run-state.sql`, so unlike the silent-lockout mutant described
/// above, this demotion does NOT strand a reader — and the live gate that would
/// have caught a stranding, `services/ctl/tests/run_plane_live.rs`, goes from
/// 8 failures to 0 across the change.
///
/// `wamn-ctc8.15.2` adds the approved session-role reader to the platform group.
/// Its explicit column grants and tenant predicates bound its reads.
///
/// Sorted, because both consumers compare against a sorted list.
const PLATFORM_GRAIN_ACL_ROLES: [&str; 8] = [
    "wamn_dispatch_reader",
    "wamn_event_materializer",
    "wamn_executor_platform",
    "wamn_http_admitter",
    "wamn_management_admitter",
    "wamn_run_retention",
    "wamn_service_reader",
    "wamn_session_role_reader",
];

/// The host-only group that is NOT a [`WorkloadRoleFamily`] and, since
/// `wamn-0h0g.22.27`, is NOT a `wamn_platform` member either.
///
/// `deploy/sql/postgres-init.sql` used to grant it the membership and NOTHING
/// ELSE did: the converge path that creates the role,
/// `wamn_schema_control::ensure_scenario_author_role_sql`, grants none. A fresh
/// install therefore granted what a converge did not. THE EMITTERS NOW AGREE AT
/// ZERO GRANTS, which is what the arm below reads back from `pg_auth_members`
/// on both paths against the same server.
const SCENARIO_AUTHOR_GROUP_MEMBER: &str = "wamn_scenario_author";

/// THE PURE HALF OF THE PLATFORM-ARM GUARD, and the reason
/// `wamn-0h0g.15.137.1` is closed.
///
/// The membership assertion that matters lives in a LIVE gate, and a mutant
/// that dies only in a live gate ships green in the ordinary sweep. This one
/// needs no server: it compares `is_platform_grain`'s output against the pinned
/// value, so flipping any arm of that function fails HERE, in a plain
/// `cargo test`, with the family named.
#[test]
fn the_platform_grain_family_set_is_pinned_and_not_derived() {
    let mut derived: Vec<&str> = WorkloadRoleFamily::ALL
        .iter()
        .filter(|family| family.is_platform_grain())
        .map(|family| family.acl_role())
        .collect();
    derived.sort_unstable();
    assert_eq!(
        derived, PLATFORM_GRAIN_ACL_ROLES,
        "is_platform_grain moved. A family that gains the arm reads EVERY \
         tenant's rows on the relations it holds grants on; one that loses it \
         reads ZERO ROWS with no error and no failing live gate. Move the pin \
         deliberately, or put the family back"
    );
    assert!(
        !derived.contains(&SCENARIO_AUTHOR_GROUP_MEMBER),
        "the scenario author is not a WorkloadRoleFamily and must not be \
         reachable through the family derivation"
    );
}

/// Start a PostgreSQL server that one case owns, and return it with its
/// superuser URL.
fn owned_server() -> (wamn_test_postgres::OwnedPostgres, String) {
    let server = wamn_test_postgres::start(&[]).expect("start a test PostgreSQL server");
    let admin = server
        .database("postgres")
        .expect("the test server has its postgres database")
        .url()
        .to_owned();
    (server, admin)
}

fn psql(url: &str, database: Option<&str>, script: &str) -> String {
    let mut command = Command::new("psql");
    command
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-Atqc", script]);
    if let Some(database) = database {
        command.args(["-d", database]);
    }
    let out = command.output().expect("psql runs");
    assert!(
        out.status.success(),
        "psql failed:\n--- stderr ---\n{}\n--- script ---\n{script}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Apply one file the way its own header says it must be applied.
///
/// `postgres-init.sql` carries `CREATE DATABASE` and `\connect`, so it cannot
/// run inside a transaction; `CATALOG_SCHEMA` owns its own `BEGIN`. Neither
/// tolerates `psql -1`.
fn apply(url: &str, sql: &str) {
    let out = Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .expect("stdin")
                .write_all(sql.as_bytes())
                .expect("write script");
            child.wait_with_output().expect("psql completes")
        })
        .expect("spawn psql (is it installed?)");
    assert!(
        out.status.success(),
        "apply failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Drop everything `postgres-init.sql` creates, so it can create it again.
///
/// `DROP DATABASE` cannot run inside a transaction and `psql -c` wraps a
/// multi-statement string in one, so each statement gets its own invocation.
/// `DROP OWNED BY` before `DROP ROLE`, inside an existence check: a leftover
/// healthy role satisfies an `IF NOT EXISTS` guard elsewhere and would mask a
/// mutated builder.
///
/// `wamn_platform` is in the list for exactly that reason and it is the one that
/// matters most: it is the ONLY role a mutant can leave behind healthy. Drop the
/// membership from the builder and re-run against a surviving cluster, and the
/// edge granted by the PREVIOUS run still admits every platform read — the
/// mutant passes and the arm it deleted is never missed.
fn reset(admin_url: &str) {
    apply(admin_url, "DROP DATABASE IF EXISTS \"wamn\";\n");
    // `DROP DATABASE` first is what makes the plain `DROP OWNED BY` below
    // sufficient: it takes every in-database ACL entry naming these roles with
    // it, and a role still named by a `relacl` cannot be dropped.
    for role in [
        "wamn_app",
        "wamn_scenario_author",
        "wamn_effect_writer",
        // `wamn-0h0g.12.69`: run-state.sql creates and grants to this one for
        // the same reason it creates `wamn_platform` — it NAMES it, and a GRANT
        // to a missing role fails the whole apply. Dropped here BEFORE the group
        // it is a member of, so the reset leaves no edge behind.
        "wamn_run_retention",
        // `record-history.sql` creates this one because it grants to it.
        "wamn_db_owner",
        // `CATALOG_SCHEMA_SQL` creates the stable audit retention role
        // (`wamn-emtx.13`).
        "wamn_audit_retention",
        "wamn_platform",
        // Probe roles this file's live arms mint. A leftover one fails the next
        // run's `CREATE ROLE` rather than masking anything, but the gate is
        // supposed to be re-runnable against a surviving cluster.
        PLATFORM_PROBE_OUTSIDER,
        CATALOG_INSTALLER,
        &platform_probe_retention(),
        &effect_writer_probe(),
        &record_history_guest(),
    ] {
        apply(
            admin_url,
            &format!(
                "DO $$ BEGIN \
                   IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{role}') THEN \
                     DROP OWNED BY {role}; DROP ROLE {role}; END IF; \
                 END $$;\n"
            ),
        );
    }
}

/// THE FLOOR, AS THE SERVER SEES IT.
///
/// Applies the real files to a fresh cluster and then asks `pg_policy`,
/// `pg_index` and `has_table_privilege` — never the file text — three things:
/// no guest-reachable relation is still on a settable claim, every re-keyed
/// relation carries its expression index, and a minted guest reads its own
/// tenant and only its own.
#[test]
fn the_swept_floor_admits_only_the_connected_guest_on_postgres() {
    let (_server, admin) = owned_server();

    // HERMETIC, and it has to be: `postgres-init.sql` carries a bare
    // `CREATE DATABASE wamn`, and a run that found the database already
    // populated would be asserting against LAST run's schema, not this build's.
    // Roles are CLUSTER-wide, so this gate OWNS its server.
    reset(&admin);
    // Seed the retired shared-login posture so the REAL init artifact must
    // converge it. The candidate is test-owned and deliberately unrelated to
    // any historical production password.
    apply(
        &admin,
        "CREATE ROLE wamn_app LOGIN PASSWORD 'retired-shared-probe' INHERIT;\n",
    );
    apply(&admin, POSTGRES_INIT);
    let base = admin.rsplit_once('/').expect("url names a database").0;
    let db_url = format!("{base}/wamn");
    let app_attributes = psql(
        &db_url,
        None,
        "SELECT concat_ws(' ', rolcanlogin, rolsuper, rolbypassrls, rolcreatedb, \
                rolcreaterole, rolinherit, rolreplication, rolpassword IS NOT NULL) \
           FROM pg_authid WHERE rolname = 'wamn_app'",
    );
    assert_eq!(
        app_attributes, "f f f f f f f f",
        "postgres-init.sql did not converge the retired shared LOGIN to a \
         passwordless NOLOGIN NOINHERIT ACL role (order: login, super, \
         bypassrls, createdb, createrole, inherit, replication, password set)"
    );
    for sql in [CATALOG_SCHEMA, RUN_STATE, RUN_QUEUE, APP_SCHEMA] {
        apply(&db_url, sql);
    }

    // 1. NOTHING guest-reachable is still on a settable claim.
    let settable = psql(
        &db_url,
        None,
        "SELECT coalesce(string_agg(rel, ' ' ORDER BY rel), '<none>') FROM (\
           SELECT n.nspname||'.'||c.relname AS rel \
             FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%app.tenant%' \
              AND has_table_privilege('wamn_app', c.oid, 'SELECT')) t",
    );
    assert_eq!(
        settable, "<none>",
        "a guest-reachable relation still keys on a claim the session can set"
    );

    // …and the two that keep the claim are exactly the ruled pair, still
    // holding no guest privilege. Asserted so the sweep cannot pass by having
    // granted the guest access to them instead.
    let kept = psql(
        &db_url,
        None,
        "SELECT string_agg(rel, ' ' ORDER BY rel) FROM (\
           SELECT n.nspname||'.'||c.relname AS rel \
             FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%app.tenant%') t",
    );
    assert_eq!(
        kept,
        HOST_INJECTED.join(" "),
        "the ruled exception set moved"
    );

    // 2. EVERY re-keyed relation carries its expression index. Without it the
    //    predicate sequential-scans, which is the cliff option (c) had to
    //    answer for. wamn-0h0g.22.15 makes this a standing denial-gate arm.
    let uncovered = psql(
        &db_url,
        None,
        "SELECT coalesce(string_agg(rel, ' ' ORDER BY rel), '<none>') FROM (\
           SELECT n.nspname||'.'||c.relname AS rel, p.polrelid AS oid \
             FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%current_tenant_key%') k \
          WHERE NOT EXISTS (SELECT 1 FROM pg_index i \
                             WHERE i.indrelid = k.oid \
                               AND pg_get_indexdef(i.indexrelid) LIKE '%tenant_key%')",
    );
    assert_eq!(
        uncovered, "<none>",
        "a re-keyed relation has no tenant-key expression index"
    );
    let governed = psql(
        &db_url,
        None,
        "SELECT count(*)::text FROM pg_policy p \
          WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%current_tenant_key%'",
    );
    assert_eq!(
        governed, "39",
        "the sweep must cover exactly the 39 governed relations"
    );

    // 2b. BORN PARKED (wamn-0h0g.20.30 for the attempt record, wamn-0h0g.20.32
    //     for its two siblings; owner ruling on wamn-0h0g.20.28). THE SERVER'S OWN
    //     ANSWER over the REAL DDL, with no reconciler in the loop:
    //     `wamn_effect_writer` — the stable ACL role every provisioned generation
    //     LOGIN inherits with INHERIT TRUE — READS the effect tables and cannot
    //     APPEND to any of the three, at table level or at any column. Prose
    //     calling the writer parked is not evidence; this is.
    let writer_table_grants = psql(
        &db_url,
        None,
        "SELECT concat_ws(' ', \
           has_table_privilege('wamn_effect_writer','wamn_run.effect_attempts','SELECT'), \
           has_table_privilege('wamn_effect_writer','wamn_run.effect_attempts','INSERT'), \
           has_any_column_privilege('wamn_effect_writer','wamn_run.effect_attempts','INSERT'), \
           has_table_privilege('wamn_effect_writer','wamn_run.effect_attempt_dispatches','INSERT'), \
           has_table_privilege('wamn_effect_writer','wamn_run.effect_attempt_outcomes','INSERT'), \
           (SELECT NOT (rolsuper OR rolbypassrls) FROM pg_roles \
             WHERE rolname='wamn_effect_writer'))",
    );
    assert_eq!(
        writer_table_grants, "t f f f f t",
        "the schema of record did not mint a READ-ONLY effect writer across the \
         three effect tables (order: attempt SELECT, attempt INSERT, attempt \
         column INSERT, dispatches INSERT, outcomes INSERT, role is unprivileged)"
    );

    // 3. A MINTED GUEST READS ITS OWN TENANT AND ONLY ITS OWN. The role name is
    //    composed by the mint, not by hand, so this also shows the digest the
    //    provisioner would issue matches the key the predicate computes.
    let guest = workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: "t1",
            database: "wamn",
        },
        CredentialGeneration::A,
    )
    .expect("App takes a tenant scope");
    assert_eq!(
        &guest[guest.len() - 42..guest.len() - 2],
        tenant_key("t1", "wamn"),
        "the minted role must carry the tenant key the predicate computes"
    );
    apply(
        &admin,
        &format!(
            "DO $$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = '{guest}') THEN \
                 DROP OWNED BY \"{guest}\"; DROP ROLE \"{guest}\"; END IF; \
             END $$;\n\
             CREATE ROLE \"{guest}\" NOLOGIN;\n\
             GRANT wamn_app TO \"{guest}\";\n"
        ),
    );
    apply(
        &db_url,
        &format!(
            "INSERT INTO catalog.packages \
               (tenant_id, package_id, package_version, manifest_sha256) \
             VALUES \
               ('t1', 'receiving', '1.0.0', 'sha256:' || repeat('a', 64)), \
               ('t2', 'receiving', '1.0.0', 'sha256:' || repeat('b', 64));\n\
             BEGIN;\n\
             SET LOCAL ROLE \"{guest}\";\n\
             SET LOCAL app.tenant = 't2';\n\
             DO $$ BEGIN\n\
                 ASSERT (SELECT count(*) FROM catalog.packages) = 1, \
                        'the guest sees exactly its own tenant';\n\
                 ASSERT (SELECT count(*) FROM catalog.packages WHERE tenant_id = 't2') = 0, \
                        'CROSS-TENANT READ';\n\
             END $$;\n\
             COMMIT;\n"
        ),
    );
    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// THE PLATFORM ARM, AS THE SERVER SEES IT (`wamn-0h0g.22.17`).
///
/// # What this closes, and why the old arms could not see it
///
/// The floor was UNTARGETED, so it applied to every role — and it calls
/// `wamn_authority.current_tenant_key()`, which only `wamn_app` may EXECUTE.
/// Measured against these very files on PostgreSQL 18.6: `wamn_effect_writer`
/// reading `wamn_run.effect_attempts` and `wamn_scenario_author` reading
/// `catalog.effective_release_heads` both got `ERROR: permission denied for function
/// current_tenant_key`. Loud, and therefore survivable.
///
/// Narrowing the floor `TO wamn_app` turns that error into something worse.
/// PostgreSQL DEFAULT-DENIES when RLS is enabled and no policy matches the
/// connected role, so the narrowing does not EXEMPT a platform principal — it
/// LOCKS IT OUT, at zero rows, with no exception at all. Every assertion here is
/// therefore a ROW COUNT or a membership fact, never the absence of an error.
///
/// # The two-hop chain, and the silent way it dies
///
/// A policy `TO wamn_platform` admits a generation login only through
/// generation login -> stable ACL role -> `wamn_platform`, and PostgreSQL 16+
/// walks that by the PER-EDGE `inherit_option`. `ensure_workload_acl_role_sql`
/// mints every stable ACL role `NOINHERIT`, and a role's `rolinherit` is the
/// DEFAULT for memberships granted TO it — so a bare
/// `GRANT wamn_platform TO wamn_run_retention` lands `inherit_option = false`
/// and every platform read silently returns zero. Measured both ways on 18.6:
/// bare grant 0 rows, `INHERIT TRUE` all rows. The edge option is asserted below
/// from `pg_auth_members`, not assumed from the builder's text.
#[test]
fn the_platform_arm_admits_every_platform_family_from_the_server() {
    let (_server, admin) = owned_server();

    reset(&admin);
    apply(&admin, POSTGRES_INIT);
    let base = admin.rsplit_once('/').expect("url names a database").0;
    let db_url = format!("{base}/wamn");
    for sql in [CATALOG_SCHEMA, RUN_STATE, RUN_QUEUE, APP_SCHEMA] {
        apply(&db_url, sql);
    }

    // 1. THE GROUP ROLE IS A GROUP, NOT AN EXEMPTION. The rejected shortcut was
    //    BYPASSRLS; this asserts the role that replaced it cannot connect, cannot
    //    escalate, and above all does not bypass RLS.
    //
    //    `pg_authid`, NOT `pg_roles`: the view substitutes the literal
    //    `'********'` for every row's `rolpassword`, so `rolpassword IS NOT NULL`
    //    reads TRUE against `pg_roles` for a role that has no password at all.
    let attributes = psql(
        &db_url,
        None,
        "SELECT concat_ws(' ', rolcanlogin, rolsuper, rolbypassrls, rolcreatedb, \
                rolcreaterole, rolreplication, rolpassword IS NOT NULL) \
           FROM pg_authid WHERE rolname = 'wamn_platform'",
    );
    assert_eq!(
        attributes, "f f f f f f f",
        "wamn_platform must be a NOLOGIN NOBYPASSRLS group role carrying no \
         credential (order: login, super, bypassrls, createdb, createrole, \
         replication, password set)"
    );

    // 2. EVERY GOVERNED RELATION CARRIES EXACTLY ONE ARM OF EACH KIND, counted
    //    PER RELATION rather than in total: a relation with two platform arms and
    //    one with none sum to the same 39 and leave a silent lockout standing.
    let missing_arm = psql(
        &db_url,
        None,
        "SELECT coalesce(string_agg(rel, ' ' ORDER BY rel), '<none>') FROM (\
           SELECT n.nspname||'.'||c.relname AS rel, c.oid AS oid \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE EXISTS (SELECT 1 FROM pg_policy p WHERE p.polrelid = c.oid \
                            AND pg_get_expr(p.polqual, p.polrelid) \
                                LIKE '%current_tenant_key%')) g \
          WHERE (SELECT count(*) FROM pg_policy p JOIN pg_roles r ON r.oid = ANY(p.polroles) \
                  WHERE p.polrelid = g.oid AND p.polpermissive \
                    AND r.rolname = 'wamn_platform') <> 1",
    );
    assert_eq!(
        missing_arm, "<none>",
        "a governed relation does not carry exactly one permissive wamn_platform \
         arm — every platform principal reads it at ZERO ROWS, silently"
    );
    let unnarrowed_floor = psql(
        &db_url,
        None,
        "SELECT coalesce(string_agg(rel, ' ' ORDER BY rel), '<none>') FROM (\
           SELECT n.nspname||'.'||c.relname AS rel \
             FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE pg_get_expr(p.polqual, p.polrelid) LIKE '%current_tenant_key%' \
              AND p.polroles <> ARRAY[(SELECT oid FROM pg_roles WHERE rolname = 'wamn_app')]) t",
    );
    assert_eq!(
        unnarrowed_floor, "<none>",
        "the tenant floor is the GUEST floor and must name wamn_app alone"
    );
    // …and NOTHING on a governed relation is untargeted. An arm widened to
    // PUBLIC still carries `USING (true)`, so it passes every predicate-shaped
    // check above while handing every tenant's rows to any role holding the
    // table grant. `polroles = '{0}'` is how PostgreSQL spells PUBLIC.
    let public_arm = psql(
        &db_url,
        None,
        "SELECT coalesce(string_agg(rel, ' ' ORDER BY rel), '<none>') FROM (\
           SELECT n.nspname||'.'||c.relname AS rel \
             FROM pg_policy p JOIN pg_class c ON c.oid = p.polrelid \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
            WHERE p.polroles = '{0}' \
              AND EXISTS (SELECT 1 FROM pg_policy g WHERE g.polrelid = c.oid \
                            AND pg_get_expr(g.polqual, g.polrelid) \
                                LIKE '%current_tenant_key%')) t",
    );
    assert_eq!(
        public_arm, "<none>",
        "a governed relation carries a policy targeting PUBLIC: the arm is one \
         group role, not an open door"
    );

    // 3. THE MEMBER SET, FROM THE REAL BUILDER, WITH ITS EDGE OPTIONS.
    //    `platform_group_membership_sql` is applied for every family,
    //    including those that must NOT be members — its revoke arm is what
    //    keeps a demoted family from silently retaining the arm.
    //
    //    The stable ACL role is ensured FIRST, by its own builder: the membership
    //    builder deliberately does not create it (it would harden the legacy
    //    LOGIN-capable `wamn_dispatch_reader` to NOLOGIN and destroy that
    //    credential), so it no-ops against a family whose role does not exist yet.
    for family in WorkloadRoleFamily::ALL {
        apply(&db_url, &sql::ensure_workload_acl_role_sql(family));
        apply(&db_url, &sql::platform_group_membership_sql(family));
    }
    // THE PINNED LITERAL, NOT A CALL TO `is_platform_grain`
    // (`wamn-0h0g.15.137.1`). See [`PLATFORM_GRAIN_ACL_ROLES`]. The host-only
    // scenario author is deliberately NOT in it (`wamn-0h0g.22.27`).
    let expected: Vec<&str> = PLATFORM_GRAIN_ACL_ROLES.to_vec();
    let members = psql(
        &db_url,
        None,
        "SELECT coalesce(string_agg(m.rolname, ' ' ORDER BY m.rolname), '<none>') \
           FROM pg_auth_members am JOIN pg_roles m ON m.oid = am.member \
           JOIN pg_roles g ON g.oid = am.roleid WHERE g.rolname = 'wamn_platform'",
    );
    assert_eq!(
        members,
        expected.join(" "),
        "the wamn_platform member set moved: the guest family must never appear, \
         and a control-plane family has no governed relation to reach"
    );
    let edges = psql(
        &db_url,
        None,
        "SELECT concat_ws(' ', bool_and(am.inherit_option), bool_and(NOT am.admin_option), \
                bool_and(NOT am.set_option)) \
           FROM pg_auth_members am JOIN pg_roles g ON g.oid = am.roleid \
          WHERE g.rolname = 'wamn_platform'",
    );
    assert_eq!(
        edges, "t t t",
        "every wamn_platform edge must be INHERIT TRUE, ADMIN FALSE, SET FALSE — \
         a defaulted INHERIT option on a NOINHERIT ACL role reads ZERO ROWS in \
         silence (order: inherit, no admin, no set)"
    );
    let guest_is_not_a_member = psql(
        &db_url,
        None,
        "SELECT pg_has_role('wamn_app', 'wamn_platform', 'USAGE')",
    );
    assert_eq!(
        guest_is_not_a_member, "f",
        "the guest family must not reach the platform arm: it would read every \
         tenant's rows, which is the exact hole wamn-0h0g.22.6 closed"
    );

    // 4. THE ADMISSION ITSELF, END TO END, ONE PROBE PER ARM
    //    (`wamn-0h0g.22.40`).
    //
    //    TWO different arms admit a non-guest principal to a governed relation:
    //    the SHARED `TO wamn_platform` arm this test is named for, and the
    //    PER-RELATION `TO wamn_effect_writer` arms `wamn-0h0g.22.32` added when
    //    it demoted the writer out of the platform grain. Those are two claims,
    //    and one probe cannot carry both — an `EffectWriter` login stopped
    //    measuring the shared arm the day that demotion landed, under a test name
    //    and a failure message that both still said PLATFORM. Each arm now has
    //    its own probe over the relation its own family holds a grant on, and
    //    each names the arm and the family it measures when it fails.
    //
    //    Two tenants' rows on each relation. The platform-grain principal reads
    //    BOTH (the shared arm is `USING (true)`, and `current_tenant_key` derives
    //    NULL for a retention login, so there is no tenant grain to narrow to),
    //    the writer reads BOTH through its own arm, and a login holding the same
    //    grants and NEITHER membership reads NONE of either.
    let hash = "'sha256:' || repeat('0', 64)";
    apply(
        &db_url,
        &format!(
            "INSERT INTO catalog.packages \
               (tenant_id, package_id, package_version, manifest_sha256) \
             SELECT t, 'receiving', '1.0.0', {hash} \
               FROM unnest(ARRAY['t1', 't2']) AS t;\n\
             INSERT INTO catalog.effective_releases \
               (tenant_id, effective_release_id, environment) \
             SELECT t, 1, 'dev' FROM unnest(ARRAY['t1', 't2']) AS t;\n\
             INSERT INTO wamn_run.environment_policies \
               (tenant_id, expected_environment, durability_class) \
             SELECT t, 'dev', 'standard' FROM unnest(ARRAY['t1', 't2']) AS t;\n\
             INSERT INTO wamn_run.runs \
               (tenant_id, run_id, package_id, effective_release_id, environment, \
                flow_id, flow_version) \
             SELECT t, 'r', 'receiving', 1, 'dev', 'f', 1 \
               FROM unnest(ARRAY['t1', 't2']) AS t;\n\
             INSERT INTO wamn_run.effect_attempts \
               (tenant_id, attempt_id, run_id, root_plan_hash, current_plan_hash, frame_id, \
                local_node_id, source_artifact_hash, requirement_name, occurrence, seq, \
                generation_fact_kind, attempt_started_at, attempt_deadline_at, \
                attempt_input_ref, created_at) \
             SELECT t, gen_random_uuid(), 'r', {hash}, {hash}, 0, 'n', {hash}, 'req', 0, 0, \
                    'not-required', now(), now(), 'ref', now() \
               FROM unnest(ARRAY['t1', 't2']) AS t;\n"
        ),
    );
    let retention = platform_probe_retention();
    let writer = effect_writer_probe();
    // `outsider` holds the SAME grants on BOTH relations and NEITHER membership.
    // Without it a probe read shows only that SELECT was granted, not that an
    // arm is what admitted the rows.
    apply(
        &admin,
        &format!(
            "CREATE ROLE \"{retention}\" NOLOGIN NOSUPERUSER NOBYPASSRLS;\n\
             GRANT wamn_run_retention TO \"{retention}\" \
               WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;\n\
             CREATE ROLE \"{writer}\" NOLOGIN NOSUPERUSER NOBYPASSRLS;\n\
             GRANT wamn_effect_writer TO \"{writer}\" \
               WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;\n\
             CREATE ROLE {PLATFORM_PROBE_OUTSIDER} NOLOGIN NOSUPERUSER NOBYPASSRLS;\n"
        ),
    );
    apply(
        &db_url,
        &format!(
            "GRANT USAGE ON SCHEMA wamn_run TO {PLATFORM_PROBE_OUTSIDER};\n\
             GRANT SELECT (tenant_id, status, created_at) \
               ON wamn_run.runs TO {PLATFORM_PROBE_OUTSIDER};\n\
             GRANT SELECT ON wamn_run.effect_attempts TO {PLATFORM_PROBE_OUTSIDER};\n"
        ),
    );
    // EACH PROBE READS THE RELATION ITS OWN FAMILY IS GRANTED, read back rather
    // than assumed: a family that lost its grant reads zero rows for a reason
    // that has nothing to do with the arm, and the row counts below would blame
    // the arm for it.
    let probe_grants = psql(
        &db_url,
        None,
        "SELECT concat_ws(' ', \
           has_any_column_privilege('wamn_run_retention', 'wamn_run.runs', 'SELECT'), \
           has_table_privilege('wamn_effect_writer', 'wamn_run.effect_attempts', 'SELECT'))",
    );
    assert_eq!(
        probe_grants, "t t",
        "a probe family holds no SELECT on the relation its arm is measured over, \
         so a zero-row read below would blame the arm for a missing privilege \
         (order: wamn_run_retention on wamn_run.runs, wamn_effect_writer on \
         wamn_run.effect_attempts)"
    );
    // A SUPERUSER FIXTURE MASKS RLS ENTIRELY, so the probe roles are asserted
    // unprivileged from `pg_roles` before a single row is counted.
    let unprivileged = psql(
        &db_url,
        None,
        &format!(
            "SELECT bool_and(NOT (rolsuper OR rolbypassrls)) FROM pg_roles \
              WHERE rolname IN ('{retention}', '{writer}', '{PLATFORM_PROBE_OUTSIDER}', \
                                'wamn_run_retention', 'wamn_effect_writer', 'wamn_platform')"
        ),
    );
    assert_eq!(
        unprivileged, "t",
        "a probe role that is superuser or BYPASSRLS establishes nothing about RLS"
    );
    apply(
        &db_url,
        &format!(
            "BEGIN;\n\
             SET LOCAL ROLE \"{retention}\";\n\
             DO $$ BEGIN\n\
                 ASSERT (SELECT count(tenant_id) FROM wamn_run.runs) = 2, \
                        'THE PLATFORM ARM DOES NOT ADMIT: the retention family is \
                         platform grain and reads wamn_run.runs short';\n\
             END $$;\n\
             COMMIT;\n\
             BEGIN;\n\
             SET LOCAL ROLE \"{writer}\";\n\
             DO $$ BEGIN\n\
                 ASSERT (SELECT count(*) FROM wamn_run.effect_attempts) = 2, \
                        'THE EFFECT-WRITER ARM DOES NOT ADMIT: the writer is not \
                         platform grain, so wamn_run.effect_attempts reads short \
                         without a per-relation arm naming it';\n\
             END $$;\n\
             COMMIT;\n\
             BEGIN;\n\
             SET LOCAL ROLE {PLATFORM_PROBE_OUTSIDER};\n\
             DO $$ BEGIN\n\
                 ASSERT (SELECT count(tenant_id) FROM wamn_run.runs) = 0, \
                        'THE FLOOR LEAKS: a login in neither wamn_app nor \
                         wamn_platform read wamn_run.runs';\n\
                 ASSERT (SELECT count(*) FROM wamn_run.effect_attempts) = 0, \
                        'THE FLOOR LEAKS: a login in neither wamn_app nor \
                         wamn_effect_writer read wamn_run.effect_attempts';\n\
             END $$;\n\
             COMMIT;\n"
        ),
    );
    // `DROP OWNED BY` FIRST, and in the project database, not the admin one:
    // the outsider holds a schema USAGE and a table SELECT, and `DROP ROLE`
    // refuses while an ACL entry names it. `DROP OWNED BY` is per-database.
    apply(
        &db_url,
        &format!("DROP OWNED BY \"{retention}\", \"{writer}\", {PLATFORM_PROBE_OUTSIDER};\n"),
    );
    apply(
        &admin,
        &format!(
            "DROP ROLE \"{retention}\";\nDROP ROLE \"{writer}\";\n\
             DROP ROLE {PLATFORM_PROBE_OUTSIDER};\n"
        ),
    );
}

/// THE TWO SCENARIO-AUTHOR EMITTERS, READ BACK FROM THE SAME SERVER
/// (`wamn-0h0g.22.27`).
///
/// # The defect this closes is DRIFT, not the membership itself
///
/// `wamn_scenario_author` is CREATED by
/// `wamn_schema_control::ensure_scenario_author_role_sql`, which grants no
/// membership at all. `deploy/sql/postgres-init.sql` granted it `wamn_platform`
/// and nothing else did. So a FRESH INSTALL granted what a CONVERGE did not,
/// and the two appliers disagreed about the authority a role carries — the
/// two-appliers drift class, independent of whether the membership was ever
/// wanted. The owner ruled DELETE FROM THE INSTALL PATH: agreeing by GRANTING
/// would ratify through the side door a membership that was explicitly not
/// pre-ratified. THE EMITTERS AGREE AT ZERO GRANTS.
///
/// # Why this reads `pg_auth_members` and not an exit status
///
/// An emitter that raises nothing has established nothing. Both appliers succeed
/// today and always did — the disagreement was in the POST-STATE, which is the
/// only thing asserted below. Every arm reads the member set out of the
/// catalog, and the role's own existence is asserted first so an empty set
/// cannot pass vacuously because the role was never created.
///
/// # A SECOND APPLY IS A NO-OP, and that is a post-state claim too
///
/// The converge emitter is applied TWICE and the member set is read after each,
/// so a builder that grants on replay — or one whose `ELSIF` harden arm gains a
/// membership — fails here rather than at the next reconcile. `postgres-init.sql`
/// cannot be applied twice against a surviving cluster at all (its bare
/// `CREATE DATABASE wamn` is `wamn-0h0g.12.188`), so its replay is the one
/// following a `reset`, which is the second install arm below.
#[test]
fn the_two_scenario_author_emitters_agree_at_zero_memberships() {
    let (_server, admin) = owned_server();

    let memberships = |label: &str| -> String {
        let existing = psql(
            &admin,
            None,
            &format!(
                "SELECT count(*) FROM pg_catalog.pg_roles \
                  WHERE rolname = '{SCENARIO_AUTHOR_GROUP_MEMBER}'"
            ),
        );
        assert_eq!(
            existing, "1",
            "{label}: the role is absent, so an empty membership set would pass \
             vacuously"
        );
        psql(
            &admin,
            None,
            &format!(
                "SELECT coalesce(string_agg(parent.rolname, ' ' ORDER BY parent.rolname), \
                        '<none>') \
                   FROM pg_catalog.pg_auth_members AS membership \
                   JOIN pg_catalog.pg_roles AS parent ON parent.oid = membership.roleid \
                   JOIN pg_catalog.pg_roles AS member ON member.oid = membership.member \
                  WHERE member.rolname = '{SCENARIO_AUTHOR_GROUP_MEMBER}'"
            ),
        )
    };

    // 1. THE FRESH-INSTALL PATH, from an empty cluster.
    reset(&admin);
    apply(&admin, POSTGRES_INIT);
    let install = memberships("fresh install");
    assert_eq!(
        install, "<none>",
        "deploy/sql/postgres-init.sql grants wamn_scenario_author a membership \
         the converge path does not: a fresh install and a converge would ship \
         different authority for the same role"
    );

    // 2. THE CONVERGE PATH, applied ON TOP of the install, then AGAIN.
    let converge = wamn_schema_control::ensure_scenario_author_role_sql();
    apply(&admin, converge);
    let converged = memberships("converge over install");
    apply(&admin, converge);
    let converged_twice = memberships("second converge");
    assert_eq!(
        converged, install,
        "the two emitters disagree about wamn_scenario_author's memberships"
    );
    assert_eq!(
        converged_twice, converged,
        "a second apply is not a no-op: the converge emitter moved the member set"
    );

    // 3. THE CONVERGE PATH FIRST, on a cluster the install has never touched,
    //    then the install on top. Order must not decide the outcome, and this is
    //    the arm that would catch an install-path grant that only lands when the
    //    role already exists.
    reset(&admin);
    apply(&admin, converge);
    let converge_only = memberships("converge on an empty cluster");
    apply(&admin, POSTGRES_INIT);
    let install_over_converge = memberships("install over converge");
    assert_eq!(
        [converge_only.as_str(), install_over_converge.as_str()],
        ["<none>", "<none>"],
        "the emitters must agree at ZERO GRANTS in either application order"
    );
}

/// The applier of the stable audit retention role (`wamn-emtx.13`).
///
/// `CATALOG_SCHEMA_SQL` creates the role as a NOLOGIN grant carrier with no
/// `wamn_platform` edge. The system database applies `SYSTEM_SCHEMA_SQL` as the
/// NOCREATEROLE `wamn_system` owner, and some test harnesses apply it as a
/// superuser. Each apply succeeds and creates no role.
#[test]
fn the_audit_retention_role_comes_from_the_catalog_schema_alone_on_postgres() {
    const ROLE: &str = "wamn_audit_retention";
    const SYSTEM_DATABASE: &str = "wamn_floor_system";
    let (_server, admin) = owned_server();
    let shape = |url: &str| {
        psql(
            url,
            None,
            &format!(
                "SELECT coalesce((SELECT concat_ws(' ', rolcanlogin, rolsuper, rolinherit, \
                          rolcreaterole, rolcreatedb, rolreplication, rolbypassrls, \
                          rolpassword IS NOT NULL, \
                          EXISTS (SELECT FROM pg_catalog.pg_auth_members \
                                   WHERE member = pg_authid.oid)) \
                   FROM pg_catalog.pg_authid WHERE rolname = '{ROLE}'), '<absent>')"
            ),
        )
    };

    reset(&admin);
    apply(&admin, POSTGRES_INIT);
    assert_eq!(
        shape(&admin),
        "<absent>",
        "the audit retention role exists before CATALOG_SCHEMA_SQL runs"
    );
    let base = admin.rsplit_once('/').expect("url names a database").0;
    let db_url = format!("{base}/wamn");
    apply(&db_url, CATALOG_SCHEMA);
    assert_eq!(
        shape(&db_url),
        "f f f f f f f f f",
        "CATALOG_SCHEMA_SQL does not create the stable audit retention role \
         (order: login, super, inherit, createrole, createdb, replication, \
         bypassrls, password set, any membership)"
    );

    apply(
        &admin,
        &format!(
            "DROP ROLE {ROLE};\n\
             DROP DATABASE IF EXISTS {SYSTEM_DATABASE};\n\
             DO $$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
                 DROP ROLE wamn_system; END IF; \
             END $$;\n\
             CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOREPLICATION NOBYPASSRLS;\n"
        ),
    );
    let system_url = format!("{base}/{SYSTEM_DATABASE}");
    for applier in ["wamn_system", "postgres"] {
        // The dev environment prepares the system database owner this way.
        apply(
            &admin,
            &format!(
                "DROP DATABASE IF EXISTS {SYSTEM_DATABASE};\n\
                 CREATE DATABASE {SYSTEM_DATABASE};\n\
                 GRANT CREATE ON DATABASE {SYSTEM_DATABASE} TO wamn_system;\n"
            ),
        );
        apply(&system_url, sql::ensure_db_owner_role_sql());
        apply(
            &system_url,
            &format!(
                "SET ROLE {applier};\n{}\nRESET ROLE;\n",
                wamn_control_provision::SYSTEM_SCHEMA_SQL
            ),
        );
        assert_eq!(
            psql(
                &system_url,
                None,
                "SELECT nspowner::regrole::text FROM pg_catalog.pg_namespace \
                  WHERE nspname = 'wamn_history'"
            ),
            applier,
            "SYSTEM_SCHEMA_SQL did not run as {applier}"
        );
        assert_eq!(
            shape(&system_url),
            "<absent>",
            "SYSTEM_SCHEMA_SQL applied as {applier} created the audit retention role"
        );
    }
    apply(
        &admin,
        &format!("DROP DATABASE {SYSTEM_DATABASE};\nDROP ROLE wamn_system;\n"),
    );
}

/// The catalog schema names no installing role (`wamn-txmd`).
///
/// A superuser with any name installs `CATALOG_SCHEMA_SQL` through its own
/// login, and the catalog schema belongs to that superuser.
#[test]
fn the_catalog_schema_installs_under_a_superuser_not_named_postgres() {
    const PASSWORD: &str = "floor-installer-probe";
    let (_server, admin) = owned_server();

    reset(&admin);
    apply(&admin, POSTGRES_INIT);
    apply(
        &admin,
        &format!("CREATE ROLE {CATALOG_INSTALLER} LOGIN SUPERUSER PASSWORD '{PASSWORD}';\n"),
    );
    let mut installer_url = url::Url::parse(&admin).expect("admin url parses");
    installer_url
        .set_username(CATALOG_INSTALLER)
        .expect("admin url takes a user name");
    installer_url
        .set_password(Some(PASSWORD))
        .expect("admin url takes a password");
    installer_url.set_path("/wamn");
    let installer_url = installer_url.as_str();

    apply(installer_url, CATALOG_SCHEMA);
    assert_eq!(
        psql(
            installer_url,
            None,
            "SELECT nspowner::regrole::text FROM pg_catalog.pg_namespace \
              WHERE nspname = 'catalog'"
        ),
        CATALOG_INSTALLER,
        "CATALOG_SCHEMA_SQL applied as {CATALOG_INSTALLER} gave the catalog schema \
         another owner"
    );
}

/// The production guest identity the record-history cases write as, composed
/// by the real mint. A member of `wamn_app` and nothing else.
fn record_history_guest() -> String {
    workload_generation_role(
        WorkloadRoleFamily::App,
        WorkloadRoleScope::Tenant {
            tenant: "t1",
            database: "wamn",
        },
        CredentialGeneration::A,
    )
    .expect("App takes a tenant scope")
}

/// Actors the record-history cases bind. No users row names A, B, or C, so a
/// stamp that lands shows that the function reads no users row.
const ACTOR_A: &str = "00000000-0000-4000-8000-00000000000a";
const ACTOR_B: &str = "00000000-0000-4000-8000-00000000000b";
const ACTOR_C: &str = "00000000-0000-4000-8000-00000000000c";
const OPERATOR: &str = "00000000-0000-4000-8000-0000000000e0";

/// Apply the real artifacts and create two stamped relations the way
/// apply-package does. `wamn_db_owner` owns the tables and creates the
/// triggers, so the fixture itself exercises the schema USAGE and function
/// EXECUTE grants. Returns the project database URL and the guest role.
fn record_history_fixture(admin: &str) -> (String, String) {
    reset(admin);
    apply(admin, POSTGRES_INIT);
    let base = admin.rsplit_once('/').expect("url names a database").0;
    let db_url = format!("{base}/wamn");
    for sql in [CATALOG_SCHEMA, RUN_STATE, RUN_QUEUE, APP_SCHEMA] {
        apply(&db_url, sql);
    }
    let guest = record_history_guest();
    apply(
        admin,
        &format!("CREATE ROLE \"{guest}\" NOLOGIN;\nGRANT wamn_app TO \"{guest}\";\n"),
    );
    apply(
        &db_url,
        "GRANT CREATE ON DATABASE wamn TO wamn_db_owner;\n\
         BEGIN;\n\
         SET LOCAL ROLE wamn_db_owner;\n\
         CREATE SCHEMA rh_probe;\n\
         CREATE TABLE rh_probe.stamped (\n\
             id integer PRIMARY KEY, note text NOT NULL,\n\
             created_at timestamptz NOT NULL, created_by uuid NOT NULL,\n\
             updated_at timestamptz NOT NULL, updated_by uuid NOT NULL);\n\
         CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON rh_probe.stamped\n\
             FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(\n\
                 'created_at', 'created_by', 'updated_at', 'updated_by');\n\
         CREATE TABLE rh_probe.timestamps_only (\n\
             id integer PRIMARY KEY, note text NOT NULL,\n\
             created_at timestamptz NOT NULL, updated_at timestamptz NOT NULL);\n\
         CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON rh_probe.timestamps_only\n\
             FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'updated_at');\n\
         GRANT USAGE ON SCHEMA rh_probe TO wamn_app;\n\
         GRANT SELECT, INSERT, UPDATE ON rh_probe.stamped, rh_probe.timestamps_only TO wamn_app;\n\
         COMMIT;\n",
    );
    (db_url, guest)
}

/// Run `body` as the guest in one transaction with `app.user_id` bound to
/// `actor`, the way the host claims transaction binds it.
fn as_guest(db_url: &str, guest: &str, actor: &str, body: &str) {
    apply(
        db_url,
        &format!(
            "BEGIN;\n\
             SET LOCAL ROLE \"{guest}\";\n\
             SELECT set_config('app.user_id', '{actor}', true);\n\
             {body}\n\
             COMMIT;\n"
        ),
    );
}

/// Spec tests 1, 3, the no-op half of 8, and 9, over the production guest.
#[test]
fn the_stamp_trigger_stamps_guest_writes_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_fixture(&admin);

    // THE GRANTS AND THE FUNCTION SHAPE, from the server catalogs.
    let shape = psql(
        &db_url,
        None,
        &format!(
            "SELECT concat_ws(' ', \
               has_schema_privilege('wamn_db_owner', 'wamn_history', 'USAGE'), \
               has_function_privilege('wamn_db_owner', 'wamn_history.stamp_row()', 'EXECUTE'), \
               has_schema_privilege('wamn_app', 'wamn_history', 'USAGE'), \
               has_function_privilege('wamn_app', 'wamn_history.stamp_row()', 'EXECUTE'), \
               has_function_privilege('{guest}', 'wamn_history.stamp_row()', 'EXECUTE'), \
               p.prosecdef, array_to_string(p.proconfig, ',')) \
               FROM pg_proc p WHERE p.oid = 'wamn_history.stamp_row()'::regprocedure"
        ),
    );
    assert_eq!(
        shape, "t t t f f f search_path=pg_catalog",
        "wamn_db_owner must hold schema USAGE and function EXECUTE, wamn_app must \
         hold schema USAGE for wamn_history.row_image, neither wamn_app nor the \
         guest must hold EXECUTE, and the function must be SECURITY INVOKER with a \
         pinned search_path (order: owner usage, owner execute, wamn_app usage, \
         wamn_app execute, guest execute, security definer, config)"
    );

    // 1. An insert stamps four with one instant per transaction, and replaces
    //    supplied stamp values. The sleep separates statement time from
    //    transaction time.
    as_guest(
        &db_url,
        &guest,
        ACTOR_A,
        &format!(
            "INSERT INTO rh_probe.stamped (id, note, created_at, created_by, updated_at, updated_by) \
               VALUES (1, 'first', '2000-01-01', '{ACTOR_B}', '2000-01-01', '{ACTOR_B}');\n\
             SELECT pg_sleep(0.01);\n\
             INSERT INTO rh_probe.stamped (id, note) VALUES (2, 'second');\n\
             INSERT INTO rh_probe.timestamps_only (id, note, created_at) \
               VALUES (1, 'first', '2000-01-01');\n\
             DO $$ BEGIN\n\
               ASSERT (SELECT count(*) FROM rh_probe.stamped \
                        WHERE created_at = transaction_timestamp() \
                          AND updated_at = transaction_timestamp() \
                          AND created_by = '{ACTOR_A}' AND updated_by = '{ACTOR_A}') = 2, \
                      'an insert must stamp all four columns with the transaction instant';\n\
               ASSERT (SELECT created_at = transaction_timestamp() \
                          AND updated_at = transaction_timestamp() \
                        FROM rh_probe.timestamps_only WHERE id = 1), \
                      'a timestamps-only insert must stamp both times';\n\
             END $$;"
        ),
    );

    // 1 and 3. An update moves the updated pair and keeps the created pair,
    //    even when the statement supplies created values.
    as_guest(
        &db_url,
        &guest,
        ACTOR_B,
        &format!(
            "UPDATE rh_probe.stamped \
               SET note = 'changed', created_at = '2000-01-01', created_by = '{ACTOR_C}' \
             WHERE id = 1;\n\
             UPDATE rh_probe.timestamps_only SET note = 'changed', created_at = '2000-01-01' \
             WHERE id = 1;\n\
             DO $$ BEGIN\n\
               ASSERT (SELECT created_by = '{ACTOR_A}' \
                          AND created_at < transaction_timestamp() \
                          AND updated_by = '{ACTOR_B}' \
                          AND updated_at = transaction_timestamp() \
                        FROM rh_probe.stamped WHERE id = 1), \
                      'an update must keep the created pair and stamp the updated pair';\n\
               ASSERT (SELECT updated_by = '{ACTOR_A}' AND updated_at < transaction_timestamp() \
                        FROM rh_probe.stamped WHERE id = 2), \
                      'an untouched row must keep its stamps';\n\
               ASSERT (SELECT created_at < transaction_timestamp() \
                          AND updated_at = transaction_timestamp() \
                        FROM rh_probe.timestamps_only WHERE id = 1), \
                      'a timestamps-only update must keep created_at and stamp updated_at';\n\
             END $$;"
        ),
    );

    // 8. A true no-op keeps every stamp, including when the statement supplies
    //    stamp values.
    as_guest(
        &db_url,
        &guest,
        ACTOR_C,
        &format!(
            "UPDATE rh_probe.stamped \
               SET note = note, updated_at = '2000-01-01', updated_by = '{ACTOR_C}' \
             WHERE id = 1;\n\
             UPDATE rh_probe.timestamps_only SET note = note WHERE id = 1;\n\
             DO $$ BEGIN\n\
               ASSERT (SELECT note = 'changed' AND created_by = '{ACTOR_A}' \
                          AND updated_by = '{ACTOR_B}' \
                          AND updated_at < transaction_timestamp() \
                        FROM rh_probe.stamped WHERE id = 1), \
                      'a no-op update must keep the OLD stamps';\n\
               ASSERT (SELECT updated_at < transaction_timestamp() \
                        FROM rh_probe.timestamps_only WHERE id = 1), \
                      'a timestamps-only no-op must keep updated_at';\n\
             END $$;"
        ),
    );

    // 9. A rolled-back write leaves the stamps unchanged.
    apply(
        &db_url,
        &format!(
            "BEGIN;\n\
             SET LOCAL ROLE \"{guest}\";\n\
             SELECT set_config('app.user_id', '{ACTOR_C}', true);\n\
             UPDATE rh_probe.stamped SET note = 'rolled back' WHERE id = 1;\n\
             ROLLBACK;\n"
        ),
    );
    as_guest(
        &db_url,
        &guest,
        ACTOR_C,
        &format!(
            "DO $$ BEGIN\n\
               ASSERT (SELECT note = 'changed' AND updated_by = '{ACTOR_B}' \
                        FROM rh_probe.stamped WHERE id = 1), \
                      'a rolled-back update must leave the stamps unchanged';\n\
             END $$;"
        ),
    );

    // 9. An upsert's update branch keeps the created pair.
    as_guest(
        &db_url,
        &guest,
        ACTOR_C,
        &format!(
            "INSERT INTO rh_probe.stamped (id, note) VALUES (1, 'upserted') \
               ON CONFLICT (id) DO UPDATE SET note = EXCLUDED.note;\n\
             DO $$ BEGIN\n\
               ASSERT (SELECT note = 'upserted' AND created_by = '{ACTOR_A}' \
                          AND created_at < transaction_timestamp() \
                          AND updated_by = '{ACTOR_C}' \
                          AND updated_at = transaction_timestamp() \
                        FROM rh_probe.stamped WHERE id = 1), \
                      'an upsert update branch must keep the created pair';\n\
             END $$;"
        ),
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// Assert that `statement` raises SQLSTATE 55000 with `actor-required`.
fn refuses_without_actor(statement: &str) -> String {
    format!(
        "DO $$ BEGIN\n\
           {statement};\n\
           RAISE EXCEPTION 'the write succeeded without a bound actor: {}';\n\
         EXCEPTION WHEN object_not_in_prerequisite_state THEN\n\
           ASSERT SQLERRM = 'actor-required', 'wrong message: ' || SQLERRM;\n\
         END $$;\n",
        statement.replace('\'', "''")
    )
}

/// The database half of spec test 4 and spec test 12.
#[test]
fn the_stamp_trigger_refuses_a_write_without_an_actor_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_fixture(&admin);
    as_guest(
        &db_url,
        &guest,
        ACTOR_A,
        "INSERT INTO rh_probe.stamped (id, note) VALUES (1, 'first');\n\
         INSERT INTO rh_probe.timestamps_only (id, note) VALUES (1, 'first');",
    );

    // 4. The guest with no bound actor, and with the empty binding the host
    //    sends for an absent claim, on both relations. A no-op update refuses
    //    too.
    let writes = [
        "INSERT INTO rh_probe.stamped (id, note) VALUES (2, 'second')",
        "UPDATE rh_probe.stamped SET note = 'changed' WHERE id = 1",
        "INSERT INTO rh_probe.timestamps_only (id, note) VALUES (2, 'second')",
        "UPDATE rh_probe.timestamps_only SET note = note WHERE id = 1",
    ];
    let refusals = writes.map(refuses_without_actor).concat();
    apply(
        &db_url,
        &format!("BEGIN;\nSET LOCAL ROLE \"{guest}\";\n{refusals}COMMIT;\n"),
    );
    as_guest(&db_url, &guest, "", &refusals);

    // 12. Administrative SQL without app.user_id refuses. Bound to the
    //     operator's person row, it stamps that row and replaces a supplied
    //     value. The users insert also binds its operation for the log.
    apply(
        &db_url,
        &format!(
            "BEGIN;\n\
             SELECT set_config('app.user_id', '{OPERATOR}', true), \
                    set_config('app.operation', 'admin:seed-operator-fixture', true);\n\
             INSERT INTO app_system.users (tenant_id, id, type, email) \
               VALUES ('t1', '{OPERATOR}', 'person', 'operator@example.test');\n\
             COMMIT;\n\
             BEGIN;\n{refusals}COMMIT;\n\
             BEGIN;\n\
             SELECT set_config('app.user_id', \
               (SELECT id::text FROM app_system.users WHERE email = 'operator@example.test'), true);\n\
             UPDATE rh_probe.stamped SET note = 'operator', updated_by = '{ACTOR_B}' WHERE id = 1;\n\
             INSERT INTO rh_probe.stamped (id, note, created_by) VALUES (3, 'operator', '{ACTOR_B}');\n\
             DO $$ BEGIN\n\
               ASSERT (SELECT created_by = '{ACTOR_A}' AND updated_by = '{OPERATOR}' \
                        FROM rh_probe.stamped WHERE id = 1), \
                      'administrative SQL must stamp the operator person row';\n\
               ASSERT (SELECT created_by = '{OPERATOR}' AND updated_by = '{OPERATOR}' \
                        FROM rh_probe.stamped WHERE id = 3), \
                      'administrative SQL must not keep a supplied stamp value';\n\
             END $$;\n\
             COMMIT;\n"
        ),
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// The operations the record-history log cases bind.
const CREATE_OPERATION: &str = "wamn-probe:logged/create@1.0.0";
const UPDATE_OPERATION: &str = "wamn-probe:logged/update@1.0.0";
const REPAIR_OPERATION: &str = "admin:repair-grades";

/// Apply `record-history.sql` a second time, then create three logged
/// relations the way apply-package does, as `wamn_db_owner`. `logged` has the
/// stamp trigger and a column that an overlay migration adds. `tenanted` has a
/// tenant history table. `keyless` has no primary key. The guest holds only
/// INSERT on each history table.
fn record_history_log_fixture(admin: &str) -> (String, String) {
    let (db_url, guest) = record_history_fixture(admin);
    apply(&db_url, RECORD_HISTORY);
    apply(
        &db_url,
        "BEGIN;\n\
         SET LOCAL ROLE wamn_db_owner;\n\
         CREATE TABLE rh_probe.logged (\n\
             id integer PRIMARY KEY, note text NOT NULL, revision integer NOT NULL,\n\
             created_at timestamptz NOT NULL, created_by uuid NOT NULL,\n\
             updated_at timestamptz NOT NULL, updated_by uuid NOT NULL);\n\
         ALTER TABLE rh_probe.logged ADD COLUMN overlay_grade text;\n\
         CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON rh_probe.logged\n\
             FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(\n\
                 'created_at', 'created_by', 'updated_at', 'updated_by');\n\
         SELECT wamn_history.create_history_table('rh_probe', 'logged', false);\n\
         SELECT wamn_history.create_history_table('rh_probe', 'logged', false);\n\
         CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE ON rh_probe.logged\n\
             FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');\n\
         CREATE TABLE rh_probe.tenanted (\n\
             tenant_id text NOT NULL, id integer NOT NULL, note text NOT NULL,\n\
             PRIMARY KEY (tenant_id, id));\n\
         SELECT wamn_history.create_history_table('rh_probe', 'tenanted', true);\n\
         CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE ON rh_probe.tenanted\n\
             FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('P30D');\n\
         CREATE TABLE rh_probe.keyless (id integer NOT NULL, note text NOT NULL);\n\
         SELECT wamn_history.create_history_table('rh_probe', 'keyless', false);\n\
         CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE ON rh_probe.keyless\n\
             FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');\n\
         GRANT SELECT, INSERT, UPDATE, DELETE\n\
             ON rh_probe.logged, rh_probe.tenanted, rh_probe.keyless TO wamn_app;\n\
         GRANT INSERT ON rh_probe.logged_history, rh_probe.tenanted_history,\n\
             rh_probe.keyless_history TO wamn_app;\n\
         COMMIT;\n",
    );
    (db_url, guest)
}

/// One guest transaction with `app.user_id` and `app.operation` bound, the way
/// the host claims transaction binds them. The session time zone is not UTC,
/// so an image that matches a later `wamn_history.row_image` read shows that
/// the image does not depend on the session time zone.
fn logged_guest_transaction(guest: &str, actor: &str, operation: &str, body: &str) -> String {
    format!(
        "BEGIN;\n\
         SET LOCAL ROLE \"{guest}\";\n\
         SET LOCAL TimeZone = 'Pacific/Auckland';\n\
         SELECT set_config('app.user_id', '{actor}', true), \
                set_config('app.operation', '{operation}', true);\n\
         {body}\n\
         COMMIT;\n"
    )
}

/// A DO block that asserts that `statement` raises `condition` and that
/// `check` holds in the handler. The handler sees the state after the
/// subtransaction rolls back, and `refused_constraint` names the constraint.
fn refuses_log_write(statement: &str, condition: &str, check: &str) -> String {
    format!(
        "DO $$ DECLARE refused_constraint text; BEGIN\n\
           {statement};\n\
           RAISE EXCEPTION 'the write succeeded: {}';\n\
         EXCEPTION WHEN {condition} THEN\n\
           GET STACKED DIAGNOSTICS refused_constraint = CONSTRAINT_NAME;\n\
           ASSERT {check}, 'wrong refusal: ' || SQLERRM;\n\
         END $$;\n",
        statement.replace('\'', "''")
    )
}

/// Run `sql` and expect psql to stop on an error. Returns the verbose error
/// output, which carries the SQLSTATE.
fn apply_refused(url: &str, sql: &str) -> String {
    let out = Command::new("psql")
        .arg(url)
        .args(["-v", "VERBOSITY=verbose", "-Atqc", sql])
        .output()
        .expect("psql runs");
    assert!(
        !out.status.success(),
        "the script succeeded, but it must fail:\n{sql}"
    );
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The history table shape, the function configuration and grants, and the
/// 64-byte refusal, from the server catalogs.
#[test]
fn the_history_table_function_creates_one_fixed_shape_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_log_fixture(&admin);

    let functions = psql(
        &db_url,
        None,
        &format!(
            "SELECT concat_ws(' ', \
               has_function_privilege('wamn_db_owner', \
                 'wamn_history.create_history_table(text, text, boolean)', 'EXECUTE'), \
               has_function_privilege('wamn_db_owner', 'wamn_history.log_row_change()', 'EXECUTE'), \
               has_function_privilege('wamn_app', \
                 'wamn_history.create_history_table(text, text, boolean)', 'EXECUTE'), \
               has_function_privilege('wamn_app', 'wamn_history.log_row_change()', 'EXECUTE'), \
               has_function_privilege('{guest}', 'wamn_history.log_row_change()', 'EXECUTE'))"
        ),
    );
    assert_eq!(
        functions, "t t f f f",
        "wamn_db_owner must hold EXECUTE on both functions after the second apply, \
         and wamn_app and the guest must hold neither (order: owner create, owner \
         log, wamn_app create, wamn_app log, guest log)"
    );
    let config = psql(
        &db_url,
        None,
        "SELECT string_agg(concat_ws(' ', p.proname, p.prosecdef, \
                 array_to_string(p.proconfig, ',')), '; ' ORDER BY p.proname) \
           FROM pg_proc p WHERE p.pronamespace = 'wamn_history'::regnamespace",
    );
    assert_eq!(
        config,
        "create_history_table f search_path=pg_catalog; \
         log_row_change f search_path=pg_catalog; \
         row_image f search_path=pg_catalog,TimeZone=UTC; \
         stamp_row f search_path=pg_catalog; \
         timestamptz_image f search_path=pg_catalog,TimeZone=UTC",
        "every record-history function must be SECURITY INVOKER with a pinned \
         search_path, and the row image function must pin TimeZone to UTC"
    );

    let columns = |relation: &str| {
        psql(
            &db_url,
            None,
            &format!(
                "SELECT string_agg(concat_ws(':', attname, format_type(atttypid, atttypmod), \
                         attnotnull, attidentity), ', ' ORDER BY attnum) \
                   FROM pg_attribute \
                  WHERE attrelid = 'rh_probe.{relation}'::regclass \
                    AND attnum > 0 AND NOT attisdropped"
            ),
        )
    };
    let fixed = "row_key:jsonb:t:, kind:text:t:, operation:text:t:, changed_by:uuid:t:, \
                 changed_at:timestamp with time zone:t:, transaction_id:bigint:t:, \
                 before:jsonb:t:, after:jsonb:t:";
    assert_eq!(
        columns("logged_history"),
        format!("position:bigint:t:a, {fixed}"),
        "a history table without the tenant flag must have the fixed shape and no tenant_id"
    );
    assert_eq!(
        columns("tenanted_history"),
        format!("position:bigint:t:a, tenant_id:text:t:, {fixed}"),
        "the tenant flag must add tenant_id NOT NULL after position"
    );

    let objects = psql(
        &db_url,
        None,
        "SELECT concat_ws(' | ', \
           (SELECT string_agg(conname || ':' || contype::text, ', ' ORDER BY conname) \
              FROM pg_constraint WHERE conrelid = 'rh_probe.logged_history'::regclass), \
           (SELECT pg_get_constraintdef(oid) FROM pg_constraint \
             WHERE conname = 'logged_history_pkey'), \
           pg_get_serial_sequence('rh_probe.logged_history', 'position'), \
           (SELECT string_agg(DISTINCT relowner::regrole::text, ',') FROM pg_class \
             WHERE oid IN ('rh_probe.logged_history'::regclass, \
                           'rh_probe.logged_history_position_seq'::regclass)))",
    );
    assert_eq!(
        objects,
        "logged_history_after_not_null:n, logged_history_before_not_null:n, \
         logged_history_changed_at_not_null:n, logged_history_changed_by_not_null:n, \
         logged_history_kind_check:c, logged_history_kind_not_null:n, \
         logged_history_operation_check:c, logged_history_operation_not_null:n, \
         logged_history_pkey:p, logged_history_position_not_null:n, \
         logged_history_row_key_not_null:n, logged_history_transaction_id_not_null:n \
         | PRIMARY KEY (row_key, \"position\") \
         | rh_probe.logged_history_position_seq \
         | wamn_db_owner",
        "the function must name every derived object, key the table by (row_key, \
         position), and leave the table and its sequence with the caller"
    );

    // The kind CHECK accepts only insert, update, and delete.
    apply(
        &db_url,
        &refuses_log_write(
            "INSERT INTO rh_probe.logged_history (row_key, kind, operation, changed_by, \
               changed_at, transaction_id, before, after) \
             VALUES ('{\"id\": 1}', 'upsert', 'admin:probe', \
               '00000000-0000-4000-8000-00000000000a', now(), 1, '{}', '{}')",
            "check_violation",
            "refused_constraint = 'logged_history_kind_check'",
        ),
    );

    // The longest derived name is <relation>_history_transaction_id_not_null,
    // 32 bytes longer than the relation. A 31-byte relation fits, and a 32-byte
    // relation refuses by bytes, not characters.
    let refusal = |relation: &str| {
        refuses_log_write(
            &format!("PERFORM wamn_history.create_history_table('rh_probe', {relation}, true)"),
            "invalid_parameter_value",
            &format!("SQLERRM = 'history-name-too-long: ' || {relation}"),
        )
    };
    apply(
        &db_url,
        &format!(
            "BEGIN;\n\
             SET LOCAL ROLE wamn_db_owner;\n\
             {}{}\
             SELECT wamn_history.create_history_table('rh_probe', repeat('r', 31), false);\n\
             COMMIT;\n\
             DO $$ BEGIN\n\
               ASSERT to_regclass('rh_probe.' || repeat('r', 32) || '_history') IS NULL \
                  AND to_regclass('rh_probe.' || repeat('é', 16) || '_history') IS NULL, \
                      'a refused relation must get no history table';\n\
               ASSERT (SELECT max(octet_length(conname)) = 63 \
                          AND bool_or(conname = repeat('r', 31) || '_history_transaction_id_not_null') \
                        FROM pg_constraint \
                        WHERE conrelid = ('rh_probe.' || repeat('r', 31) || '_history')::regclass), \
                      'a 31-byte relation must get untruncated names up to 63 bytes';\n\
             END $$;\n",
            refusal("repeat('r', 32)"),
            refusal("repeat('é', 16)"),
        ),
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// Every grant on schema `wamn_history` and its functions, except the grants
/// of the owner, from the server ACLs.
fn history_grants(url: &str) -> String {
    psql(
        url,
        None,
        "SELECT coalesce(string_agg(entry, '; ' ORDER BY entry COLLATE \"C\"), '<none>') FROM ( \
           SELECT 'wamn_history ' || acl.grantee::regrole::text || ' ' || acl.privilege_type \
                  AS entry \
             FROM pg_catalog.pg_namespace n, pg_catalog.aclexplode(n.nspacl) acl \
            WHERE n.nspname = 'wamn_history' AND acl.grantee <> n.nspowner \
           UNION ALL \
           SELECT p.proname || ' ' || acl.grantee::regrole::text || ' ' || acl.privilege_type \
             FROM pg_catalog.pg_proc p, pg_catalog.aclexplode(p.proacl) acl \
            WHERE p.pronamespace = 'wamn_history'::regnamespace \
              AND acl.grantee <> p.proowner) entries",
    )
}

/// Ruling 78 (`wamn-emtx.31`). The `wamn_app` record history grants come from
/// `record-history-app-grants.sql` alone. `CATALOG_SCHEMA_SQL` composes that
/// file, so the guest writes a logged relation, and the write fails without the
/// `row_image` grant. `SYSTEM_SCHEMA_SQL` grants nothing to `wamn_app`, as the
/// NOCREATEROLE `wamn_system` owner and as a superuser, with and without the
/// role. The grant file refuses a database without `wamn_app`.
#[test]
fn the_wamn_app_history_grants_come_from_the_grant_file_alone_on_postgres() {
    const SYSTEM_DATABASE: &str = "wamn_floor_history_system";
    const OWNER_GRANTS: [&str; 6] = [
        "create_history_table wamn_db_owner EXECUTE",
        "log_row_change wamn_db_owner EXECUTE",
        "row_image wamn_db_owner EXECUTE",
        "stamp_row wamn_db_owner EXECUTE",
        "timestamptz_image wamn_db_owner EXECUTE",
        "wamn_history wamn_db_owner USAGE",
    ];
    const APP_GRANTS: [&str; 3] = [
        "row_image wamn_app EXECUTE",
        "timestamptz_image wamn_app EXECUTE",
        "wamn_history wamn_app USAGE",
    ];
    let (_server, admin) = owned_server();
    let joined = |grants: &[&str]| {
        let mut grants = grants.to_vec();
        grants.sort_unstable();
        grants.join("; ")
    };

    let (db_url, guest) = record_history_log_fixture(&admin);
    assert_eq!(
        history_grants(&db_url),
        joined(&[OWNER_GRANTS.as_slice(), APP_GRANTS.as_slice()].concat()),
        "CATALOG_SCHEMA_SQL must give wamn_app exactly schema USAGE and EXECUTE on \
         row_image and timestamptz_image, and record-history.sql applied again must \
         keep them"
    );
    apply(
        &db_url,
        &format!(
            "{}\
             DO $$ BEGIN\n\
               ASSERT (SELECT count(*) FROM rh_probe.logged_history WHERE kind = 'insert') = 1, \
                      'a guest insert must write one entry';\n\
             END $$;\n",
            logged_guest_transaction(
                &guest,
                ACTOR_A,
                CREATE_OPERATION,
                "INSERT INTO rh_probe.logged (id, note, revision) VALUES (1, 'first', 1);",
            )
        ),
    );
    let refused = apply_refused(
        &db_url,
        &format!(
            "REVOKE EXECUTE ON FUNCTION wamn_history.row_image(record) FROM wamn_app; \
             SET ROLE \"{guest}\"; \
             SELECT set_config('app.user_id', '{ACTOR_A}', true), \
                    set_config('app.operation', '{CREATE_OPERATION}', true); \
             INSERT INTO rh_probe.logged (id, note, revision) VALUES (2, 'second', 1);"
        ),
    );
    assert!(
        refused.contains("42501") && refused.contains("row_image"),
        "a guest write to a logged relation must fail without the row_image grant:\n{refused}"
    );
    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));

    reset(&admin);
    let base = admin.rsplit_once('/').expect("url names a database").0;
    let system_url = format!("{base}/{SYSTEM_DATABASE}");
    apply(
        &admin,
        &format!(
            "DROP DATABASE IF EXISTS {SYSTEM_DATABASE};\n\
             DO $$ BEGIN \
               IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_system') THEN \
                 DROP ROLE wamn_system; END IF; \
             END $$;\n\
             CREATE ROLE wamn_system NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE \
               NOREPLICATION NOBYPASSRLS;\n"
        ),
    );
    for applier in ["wamn_system", "postgres"] {
        for app_role in [false, true] {
            apply(
                &admin,
                &format!(
                    "DROP DATABASE IF EXISTS {SYSTEM_DATABASE};\n\
                     DROP ROLE IF EXISTS wamn_app;\n\
                     CREATE DATABASE {SYSTEM_DATABASE};\n\
                     GRANT CREATE ON DATABASE {SYSTEM_DATABASE} TO wamn_system;\n"
                ),
            );
            if app_role {
                apply(&admin, &sql::ensure_app_acl_role_sql());
            }
            apply(&system_url, sql::ensure_db_owner_role_sql());
            apply(
                &system_url,
                &format!(
                    "SET ROLE {applier};\n{}\nRESET ROLE;\n",
                    wamn_control_provision::SYSTEM_SCHEMA_SQL
                ),
            );
            assert_eq!(
                history_grants(&system_url),
                joined(&OWNER_GRANTS),
                "SYSTEM_SCHEMA_SQL applied as {applier} (wamn_app present: {app_role}) \
                 must grant wamn_history to wamn_db_owner alone"
            );
            if app_role {
                assert_eq!(
                    psql(
                        &system_url,
                        None,
                        "SELECT concat_ws(' ', \
                           has_schema_privilege('wamn_app', 'wamn_history', 'USAGE'), \
                           has_function_privilege('wamn_app', \
                             'wamn_history.row_image(record)', 'EXECUTE'), \
                           has_function_privilege('wamn_app', \
                             'wamn_history.timestamptz_image(timestamptz)', 'EXECUTE'))"
                    ),
                    "f f f",
                    "SYSTEM_SCHEMA_SQL applied as {applier} gave an existing wamn_app a \
                     history privilege (order: usage, row_image, timestamptz_image)"
                );
            } else {
                let refused = apply_refused(&system_url, RECORD_HISTORY_APP_GRANTS);
                assert!(
                    refused.contains("42704")
                        && refused.contains("role \"wamn_app\" does not exist"),
                    "the grant file must refuse a database without wamn_app:\n{refused}"
                );
            }
        }
    }
    apply(
        &admin,
        &format!("DROP DATABASE {SYSTEM_DATABASE};\nDROP ROLE wamn_app;\nDROP ROLE wamn_system;\n"),
    );
}

/// Level-2 spec tests 2, 5, 8, and 16, the '{}' images, the tenant flag, and
/// the transaction id, over the production guest.
#[test]
fn the_log_trigger_writes_one_entry_per_row_change_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_log_fixture(&admin);

    // An insert writes the full row in after and '{}' in before. The row image
    // of the row matches the entry. A tenant history table copies tenant_id.
    let insert = logged_guest_transaction(
        &guest,
        ACTOR_A,
        CREATE_OPERATION,
        "INSERT INTO rh_probe.logged (id, note, revision) VALUES (1, 'first', 1);\n\
         INSERT INTO rh_probe.tenanted (tenant_id, id, note) VALUES ('t1', 1, 'first');\n\
         SELECT set_config('rh.first_xact', pg_current_xact_id()::text, false);",
    );
    apply(
        &db_url,
        &format!(
            "{insert}\
             DO $$ BEGIN\n\
               ASSERT (SELECT count(*) FROM rh_probe.logged_history) = 1, \
                      'one insert must write one entry';\n\
               ASSERT (SELECT h.row_key = '{{\"id\": 1}}' AND h.kind = 'insert' \
                          AND h.operation = '{CREATE_OPERATION}' AND h.changed_by = '{ACTOR_A}' \
                          AND h.changed_at = l.created_at \
                          AND h.transaction_id = current_setting('rh.first_xact')::bigint \
                          AND h.before = '{{}}' AND h.after = wamn_history.row_image(l) \
                        FROM rh_probe.logged_history h, rh_probe.logged l WHERE l.id = 1), \
                      'an insert entry must hold the key, the actor, the operation, the \
                       transaction time and id, an empty before, and the full row';\n\
               ASSERT (SELECT h.tenant_id = 't1' AND h.kind = 'insert' \
                          AND h.row_key = '{{\"id\": 1, \"tenant_id\": \"t1\"}}' \
                          AND h.before = '{{}}' AND h.after = to_jsonb(t) \
                        FROM rh_probe.tenanted_history h, rh_probe.tenanted t), \
                      'a tenant history entry must copy tenant_id from the row';\n\
             END $$;\n"
        ),
    );

    // 2 and 5. Two changes write two update entries of the changed columns in
    // position order. A no-op, a replayed claim, and a no-op upsert write none.
    let updates = logged_guest_transaction(
        &guest,
        ACTOR_B,
        UPDATE_OPERATION,
        "UPDATE rh_probe.logged SET note = 'second', revision = revision + 1 WHERE id = 1;\n\
         UPDATE rh_probe.logged SET note = 'third', revision = revision + 1 WHERE id = 1;\n\
         UPDATE rh_probe.logged SET note = note WHERE id = 1;\n\
         INSERT INTO rh_probe.logged (id, note, revision) VALUES (1, 'replayed', 1) \
           ON CONFLICT (id) DO NOTHING;\n\
         INSERT INTO rh_probe.logged (id, note, revision) VALUES (1, 'third', 3) \
           ON CONFLICT (id) DO UPDATE SET note = EXCLUDED.note, revision = EXCLUDED.revision;\n\
         SELECT set_config('rh.second_xact', pg_current_xact_id()::text, false);",
    );
    apply(
        &db_url,
        &format!(
            "SELECT pg_sleep(0.01);\n\
             {updates}\
             DO $$ BEGIN\n\
               ASSERT (SELECT array_agg(kind ORDER BY position) FROM rh_probe.logged_history) \
                        = ARRAY['insert', 'update', 'update'], \
                      'two changes must write two entries in position order, and a no-op \
                       and a replay must write none';\n\
               ASSERT (SELECT bool_and(changed_by = '{ACTOR_B}' AND operation = '{UPDATE_OPERATION}' \
                          AND transaction_id = current_setting('rh.second_xact')::bigint) \
                        FROM rh_probe.logged_history WHERE kind = 'update'), \
                      'each update entry must carry its actor, operation, and transaction id';\n\
               ASSERT (SELECT u.before = jsonb_build_object('note', 'first', 'revision', 1, \
                            'updated_at', i.after -> 'updated_at', 'updated_by', '{ACTOR_A}') \
                          AND u.after = jsonb_build_object('note', 'second', 'revision', 2, \
                            'updated_at', wamn_history.row_image(l) -> 'updated_at', \
                            'updated_by', '{ACTOR_B}') \
                        FROM rh_probe.logged_history u, rh_probe.logged_history i, rh_probe.logged l \
                        WHERE u.kind = 'update' AND i.kind = 'insert' AND l.id = 1 \
                        ORDER BY u.position LIMIT 1), \
                      'the first update must record the changed columns, the stamps and \
                       the revision included';\n\
               ASSERT (SELECT before = '{{\"note\": \"second\", \"revision\": 2}}' \
                          AND after = '{{\"note\": \"third\", \"revision\": 3}}' \
                        FROM rh_probe.logged_history WHERE kind = 'update' \
                        ORDER BY position DESC LIMIT 1), \
                      'the second update in the transaction must record only its changes';\n\
             END $$;\n"
        ),
    );

    // 8 and 16. An overlay column change appears in the entry of the row. A key
    // change writes a delete under the old key and an insert under the new key.
    // A delete writes the full row in before and '{}' in after.
    let repair = logged_guest_transaction(
        &guest,
        ACTOR_A,
        REPAIR_OPERATION,
        "UPDATE rh_probe.logged SET overlay_grade = 'A' WHERE id = 1;\n\
         UPDATE rh_probe.logged SET note = 'fourth', overlay_grade = 'B' WHERE id = 1;\n\
         UPDATE rh_probe.logged SET id = 2 WHERE id = 1;\n\
         DELETE FROM rh_probe.logged WHERE id = 2;\n\
         DELETE FROM rh_probe.tenanted WHERE id = 1;\n\
         SELECT set_config('rh.third_xact', pg_current_xact_id()::text, false);",
    );
    apply(
        &db_url,
        &format!(
            "SELECT pg_sleep(0.01);\n\
             {repair}\
             DO $$ DECLARE third bigint := current_setting('rh.third_xact')::bigint; BEGIN\n\
               ASSERT (SELECT array_agg(kind ORDER BY position) FROM rh_probe.logged_history \
                        WHERE row_key = '{{\"id\": 1}}') \
                        = ARRAY['insert', 'update', 'update', 'update', 'update', 'delete'] \
                  AND (SELECT array_agg(kind ORDER BY position) FROM rh_probe.logged_history \
                        WHERE row_key = '{{\"id\": 2}}') = ARRAY['insert', 'delete'], \
                      'the entries of each key must follow the changes in position order';\n\
               ASSERT (SELECT array_agg(k ORDER BY k) = ARRAY['overlay_grade', 'updated_at', 'updated_by'] \
                          AND bool_and(h.before -> 'overlay_grade' = 'null' \
                                       AND h.after -> 'overlay_grade' = '\"A\"') \
                        FROM rh_probe.logged_history h, jsonb_object_keys(h.after) k \
                        WHERE h.kind = 'update' AND h.transaction_id = third \
                          AND h.after ? 'updated_by'), \
                      'an overlay column change must record the overlay column and the stamps';\n\
               ASSERT (SELECT before = '{{\"note\": \"third\", \"overlay_grade\": \"A\"}}' \
                          AND after = '{{\"note\": \"fourth\", \"overlay_grade\": \"B\"}}' \
                        FROM rh_probe.logged_history \
                        WHERE kind = 'update' AND transaction_id = third \
                          AND NOT after ? 'updated_by'), \
                      'a base and an overlay change must share one entry';\n\
               ASSERT (SELECT d.after = '{{}}' AND d.before ->> 'note' = 'fourth' \
                          AND (SELECT count(*) FROM jsonb_object_keys(d.before)) = 8 \
                          AND i.before = '{{}}' AND i.after = d.before || '{{\"id\": 2}}' \
                          AND d.transaction_id = third AND i.transaction_id = third \
                          AND d.position < i.position \
                        FROM rh_probe.logged_history d, rh_probe.logged_history i \
                        WHERE d.row_key = '{{\"id\": 1}}' AND d.kind = 'delete' \
                          AND i.row_key = '{{\"id\": 2}}' AND i.kind = 'insert'), \
                      'a key change must write a full delete under the old key and a full \
                       insert under the new key in one transaction';\n\
               ASSERT (SELECT d.before = i.after AND d.after = '{{}}' \
                          AND d.operation = '{REPAIR_OPERATION}' \
                        FROM rh_probe.logged_history d, rh_probe.logged_history i \
                        WHERE d.row_key = '{{\"id\": 2}}' AND d.kind = 'delete' \
                          AND i.row_key = '{{\"id\": 2}}' AND i.kind = 'insert'), \
                      'a delete must record the full row in before and an empty after';\n\
               ASSERT (SELECT d.tenant_id = 't1' AND d.before = i.after AND d.after = '{{}}' \
                          AND d.transaction_id = third \
                        FROM rh_probe.tenanted_history d, rh_probe.tenanted_history i \
                        WHERE d.kind = 'delete' AND i.kind = 'insert'), \
                      'a tenant delete must copy tenant_id from the deleted row';\n\
             END $$;\n"
        ),
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// Level-2 spec tests 4, 7, and 10, and the refusal of a relation with no
/// primary key, over the production guest.
#[test]
fn the_log_trigger_refuses_and_rolls_back_the_write_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_log_fixture(&admin);
    apply(
        &db_url,
        &logged_guest_transaction(
            &guest,
            ACTOR_A,
            CREATE_OPERATION,
            "INSERT INTO rh_probe.logged (id, note, revision) VALUES (1, 'first', 1);\n\
             INSERT INTO rh_probe.tenanted (tenant_id, id, note) VALUES ('t1', 1, 'first');",
        ),
    );

    // 10. The operation CHECK accepts an operation token, wamn:<component>, and
    //     admin:<kebab-purpose>.
    let accepted = [
        UPDATE_OPERATION,
        "acme2:quality-inspection/create@2.1.0-rc.1+build.5",
        "a:b-2/c3@v1",
        "wamn:audit-retention",
        "wamn:apply-package",
        REPAIR_OPERATION,
    ];
    let accepted_writes = accepted
        .map(|operation| {
            format!(
                "SELECT set_config('app.operation', '{operation}', true);\n\
                 UPDATE rh_probe.logged SET revision = revision + 1 WHERE id = 1;\n"
            )
        })
        .concat();
    let accepted_list = accepted
        .map(|operation| format!("'{operation}'"))
        .join(", ");
    apply(
        &db_url,
        &format!(
            "{}\
             DO $$ BEGIN\n\
               ASSERT (SELECT array_agg(operation ORDER BY position) FROM rh_probe.logged_history \
                        WHERE kind = 'update') = ARRAY[{accepted_list}], \
                      'the operation CHECK must accept each operation shape';\n\
             END $$;\n",
            logged_guest_transaction(&guest, ACTOR_A, CREATE_OPERATION, &accepted_writes),
        ),
    );

    // 4 and 10. The CHECK refuses every other value, and the refusal rolls back
    // the business write with the entry.
    let refused = [
        "wamn-probe:logged/update",
        "wamn-probe:logged@1.0.0",
        "wamn-probe:logged/update@",
        "Wamn-probe:logged/update@1.0.0",
        "wamn_probe:logged/update@1.0.0",
        "wamn--probe:logged/update@1.0.0",
        "wamn-probe-:logged/update@1.0.0",
        "1wamn:logged/update@1.0.0",
        "wamn-probe:logged/update@1.0.0/extra",
        "wamn-probe:logged/update@1.0@2",
        "wamn-probe:logged/update@ 1.0.0",
        "wamn-probe:logged/update@1.0.0\u{a0}",
        "wamn:",
        "wamn:Audit-retention",
        "wamn:audit_retention",
        "wamn:audit-retention/run",
        "admin:",
        "admin:fix--grades",
        "admin:fix-grades ",
        " admin:fix-grades",
        "operator:fix-grades",
    ];
    let check_refusals = refused
        .map(|operation| {
            refuses_log_write(
                &format!(
                    "PERFORM set_config('app.operation', '{operation}', true);\n\
                     UPDATE rh_probe.logged SET note = 'refused' WHERE id = 1"
                ),
                "check_violation",
                "refused_constraint = 'logged_history_operation_check' \
                 AND (SELECT note FROM rh_probe.logged WHERE id = 1) = 'first'",
            )
        })
        .concat();
    apply(
        &db_url,
        &logged_guest_transaction(&guest, ACTOR_A, CREATE_OPERATION, &check_refusals),
    );
    let stopped = apply_refused(
        &db_url,
        &logged_guest_transaction(
            &guest,
            ACTOR_A,
            "not an operation",
            "UPDATE rh_probe.logged SET note = 'lost', revision = revision + 1 WHERE id = 1;",
        ),
    );
    assert!(
        stopped.contains("23514") && stopped.contains("logged_history_operation_check"),
        "the refused entry must stop the transaction with the operation CHECK:\n{stopped}"
    );

    // 10. An unbound operation, and the empty binding the host sends for an
    //     absent claim, raise operation-required. An unbound actor raises
    //     actor-required on a relation without the stamp trigger.
    let operation_required = [
        "UPDATE rh_probe.logged SET note = 'refused' WHERE id = 1",
        "INSERT INTO rh_probe.tenanted (tenant_id, id, note) VALUES ('t1', 2, 'second')",
        "DELETE FROM rh_probe.tenanted WHERE id = 1",
    ]
    .map(|statement| {
        refuses_log_write(
            statement,
            "object_not_in_prerequisite_state",
            "SQLERRM = 'operation-required'",
        )
    })
    .concat();
    let actor_required = [
        "INSERT INTO rh_probe.tenanted (tenant_id, id, note) VALUES ('t1', 2, 'second')",
        "DELETE FROM rh_probe.tenanted WHERE id = 1",
    ]
    .map(|statement| {
        refuses_log_write(
            statement,
            "object_not_in_prerequisite_state",
            "SQLERRM = 'actor-required'",
        )
    })
    .concat();
    apply(
        &db_url,
        &format!(
            "BEGIN;\n\
             SET LOCAL ROLE \"{guest}\";\n\
             SELECT set_config('app.user_id', '{ACTOR_A}', true);\n\
             {operation_required}\
             SELECT set_config('app.operation', '', true);\n\
             {operation_required}\
             SELECT set_config('app.user_id', '', true), \
                    set_config('app.operation', '{UPDATE_OPERATION}', true);\n\
             {actor_required}\
             COMMIT;\n"
        ),
    );

    // A relation with no primary key refuses the write.
    // 7. The guest holds INSERT on the history table and cannot update or
    //    delete an entry.
    let guest_refusals = [
        refuses_log_write(
            "INSERT INTO rh_probe.keyless (id, note) VALUES (1, 'first')",
            "object_not_in_prerequisite_state",
            "SQLERRM = 'history-key-required'",
        ),
        refuses_log_write(
            "UPDATE rh_probe.logged_history SET kind = 'update'",
            "insufficient_privilege",
            "true",
        ),
        refuses_log_write(
            "DELETE FROM rh_probe.logged_history",
            "insufficient_privilege",
            "true",
        ),
    ]
    .concat();
    apply(
        &db_url,
        &format!(
            "{}\
             DO $$ BEGIN\n\
               ASSERT (SELECT note = 'first' AND revision = {} FROM rh_probe.logged WHERE id = 1), \
                      'every refused write must leave the row unchanged';\n\
               ASSERT (SELECT count(*) FROM rh_probe.logged_history) = {} \
                  AND (SELECT count(*) FROM rh_probe.tenanted_history) = 1 \
                  AND NOT EXISTS (SELECT FROM rh_probe.keyless) \
                  AND NOT EXISTS (SELECT FROM rh_probe.keyless_history), \
                      'every refused write must leave no entry';\n\
             END $$;\n",
            logged_guest_transaction(&guest, ACTOR_A, REPAIR_OPERATION, &guest_refusals),
            1 + accepted.len(),
            1 + accepted.len(),
        ),
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// Start one psql session that runs the SQL the test writes to its standard
/// input. `PGAPPNAME` names the session in `pg_stat_activity`.
fn start_session(url: &str, name: &str) -> std::process::Child {
    Command::new("psql")
        .arg(url)
        .args(["-v", "ON_ERROR_STOP=1", "-q", "-f", "-"])
        .env("PGAPPNAME", name)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn psql (is it installed?)")
}

fn send(session: &mut std::process::Child, sql: &str) {
    use std::io::Write;
    let stdin = session.stdin.as_mut().expect("session stdin");
    stdin.write_all(sql.as_bytes()).expect("write session SQL");
    stdin.flush().expect("flush session SQL");
}

/// Wait until `query` returns true on the server.
fn wait_until(url: &str, query: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while psql(url, None, query) != "t" {
        assert!(
            std::time::Instant::now() < deadline,
            "the server did not reach the state: {query}"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn finish(session: std::process::Child) {
    let out = session.wait_with_output().expect("session completes");
    assert!(
        out.status.success(),
        "session failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Level-2 spec test 3. The second writer waits on the row lock of the first,
/// and the entries agree with the serialized row changes. Writers of different
/// rows that commit out of position order keep every entry, and a rolled-back
/// competitor leaves no entry.
#[test]
fn the_log_trigger_serializes_concurrent_changes_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_log_fixture(&admin);
    // A third actor inserts, so each update moves updated_by.
    apply(
        &db_url,
        &logged_guest_transaction(
            &guest,
            ACTOR_C,
            CREATE_OPERATION,
            "INSERT INTO rh_probe.logged (id, note, revision) VALUES (1, 'first', 1);",
        ),
    );

    let claims = |actor: &str| {
        format!(
            "BEGIN;\n\
             SET LOCAL ROLE \"{guest}\";\n\
             SELECT set_config('app.user_id', '{actor}', true), \
                    set_config('app.operation', '{UPDATE_OPERATION}', true);\n"
        )
    };
    let mut first = start_session(&db_url, "rh-first");
    send(
        &mut first,
        &format!(
            "{}UPDATE rh_probe.logged SET note = 'from first', revision = revision + 1 \
               WHERE id = 1;\n\
             SELECT set_config('application_name', 'rh-first-locked', false);\n",
            claims(ACTOR_A)
        ),
    );
    wait_until(
        &db_url,
        "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE application_name = 'rh-first-locked')",
    );
    let mut second = start_session(&db_url, "rh-second");
    send(
        &mut second,
        &format!(
            "{}UPDATE rh_probe.logged SET note = 'from second', revision = revision + 1 \
               WHERE id = 1;\n\
             COMMIT;\n",
            claims(ACTOR_B)
        ),
    );
    drop(second.stdin.take());
    wait_until(
        &db_url,
        "SELECT EXISTS (SELECT FROM pg_stat_activity \
                         WHERE application_name = 'rh-second' AND wait_event_type = 'Lock')",
    );
    send(&mut first, "COMMIT;\n");
    drop(first.stdin.take());
    finish(first);
    finish(second);

    apply(
        &db_url,
        &format!(
            "DO $$ BEGIN\n\
               ASSERT (SELECT note = 'from second' AND revision = 3 \
                        FROM rh_probe.logged WHERE id = 1), \
                      'both changes must commit in order';\n\
               ASSERT (SELECT array_agg(kind ORDER BY position) = ARRAY['insert', 'update', 'update'] \
                          AND count(DISTINCT position) = 3 \
                          AND count(DISTINCT transaction_id) = 3 \
                        FROM rh_probe.logged_history), \
                      'each committed change must write one entry at its own position';\n\
               ASSERT (SELECT f.changed_by = '{ACTOR_A}' AND s.changed_by = '{ACTOR_B}' \
                          AND f.position < s.position \
                          AND f.after ->> 'note' = 'from first' AND f.after -> 'revision' = '2' \
                          AND s.before = f.after \
                          AND s.after ->> 'note' = 'from second' AND s.after -> 'revision' = '3' \
                        FROM rh_probe.logged_history f, rh_probe.logged_history s \
                        WHERE f.kind = 'update' AND s.kind = 'update' AND f.position < s.position), \
                      'the second entry must start from the first result at a later position';\n\
             END $$;\n"
        ),
    );

    // Different rows: neither writer waits. Both hold an assigned transaction
    // open at the same time, and the writer with the earlier position commits
    // last.
    apply(
        &db_url,
        &logged_guest_transaction(
            &guest,
            ACTOR_C,
            CREATE_OPERATION,
            "INSERT INTO rh_probe.logged (id, note, revision) VALUES (2, 'first', 1), (3, 'first', 1);",
        ),
    );
    let mut earlier = start_session(&db_url, "rh-earlier");
    send(
        &mut earlier,
        &format!(
            "{}UPDATE rh_probe.logged SET note = 'from earlier', revision = revision + 1 \
               WHERE id = 2;\n\
             SELECT set_config('application_name', 'rh-earlier-held', false);\n",
            claims(ACTOR_A)
        ),
    );
    wait_until(
        &db_url,
        "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE application_name = 'rh-earlier-held')",
    );
    let mut later = start_session(&db_url, "rh-later");
    send(
        &mut later,
        &format!(
            "{}UPDATE rh_probe.logged SET note = 'from later', revision = revision + 1 \
               WHERE id = 3;\n\
             SELECT set_config('application_name', 'rh-later-held', false);\n",
            claims(ACTOR_B)
        ),
    );
    wait_until(
        &db_url,
        "SELECT count(*) = 2 FROM pg_stat_activity \
          WHERE application_name IN ('rh-earlier-held', 'rh-later-held') \
            AND state = 'idle in transaction' AND backend_xid IS NOT NULL",
    );
    send(&mut later, "COMMIT;\n");
    drop(later.stdin.take());
    finish(later);
    send(&mut earlier, "COMMIT;\n");
    drop(earlier.stdin.take());
    finish(earlier);

    apply(
        &db_url,
        &format!(
            "DO $$ BEGIN\n\
               ASSERT (SELECT array_agg(note ORDER BY id) = ARRAY['from earlier', 'from later'] \
                        FROM rh_probe.logged WHERE id IN (2, 3)), \
                      'both changes must commit';\n\
               ASSERT (SELECT count(*) = 7 AND count(DISTINCT position) = 7 \
                        FROM rh_probe.logged_history), \
                      'every committed change must write one entry, and no position may repeat';\n\
               ASSERT (SELECT count(*) = 2 AND bool_and(r.kinds = ARRAY['insert', 'update']) \
                        FROM (SELECT array_agg(kind ORDER BY position) AS kinds \
                                FROM rh_probe.logged_history \
                               WHERE row_key IN ('{{\"id\": 2}}', '{{\"id\": 3}}') \
                               GROUP BY row_key) AS r), \
                      'the positions of each row must rise with its changes';\n\
               ASSERT (SELECT e.changed_by = '{ACTOR_A}' AND e.after ->> 'note' = 'from earlier' \
                          AND l.changed_by = '{ACTOR_B}' AND l.after ->> 'note' = 'from later' \
                          AND e.position < l.position \
                        FROM rh_probe.logged_history e, rh_probe.logged_history l \
                        WHERE e.row_key = '{{\"id\": 2}}' AND e.kind = 'update' \
                          AND l.row_key = '{{\"id\": 3}}' AND l.kind = 'update'), \
                      'each entry must keep its own position when the later position commits first';\n\
             END $$;\n"
        ),
    );

    // A rolled-back competitor: the committed writer waits on the row lock of
    // a writer that logs a change and rolls back. Gaps are allowed.
    let mut aborted = start_session(&db_url, "rh-aborted");
    send(
        &mut aborted,
        &format!(
            "{}UPDATE rh_probe.logged SET note = 'rolled back', revision = revision + 1 \
               WHERE id = 1;\n\
             SELECT set_config('application_name', 'rh-aborted-locked', false);\n",
            claims(ACTOR_A)
        ),
    );
    wait_until(
        &db_url,
        "SELECT EXISTS (SELECT FROM pg_stat_activity WHERE application_name = 'rh-aborted-locked')",
    );
    let mut committed = start_session(&db_url, "rh-committed");
    send(
        &mut committed,
        &format!(
            "{}UPDATE rh_probe.logged SET note = 'after rollback', revision = revision + 1 \
               WHERE id = 1;\n\
             COMMIT;\n",
            claims(ACTOR_B)
        ),
    );
    drop(committed.stdin.take());
    wait_until(
        &db_url,
        "SELECT EXISTS (SELECT FROM pg_stat_activity \
                         WHERE application_name = 'rh-committed' AND wait_event_type = 'Lock')",
    );
    send(&mut aborted, "ROLLBACK;\n");
    drop(aborted.stdin.take());
    finish(aborted);
    finish(committed);

    apply(
        &db_url,
        "DO $$ BEGIN\n\
           ASSERT (SELECT note = 'after rollback' AND revision = 4 \
                    FROM rh_probe.logged WHERE id = 1), \
                  'the committed change must apply to the committed row';\n\
           ASSERT (SELECT count(*) = 8 AND count(DISTINCT position) = 8 \
                      AND count(*) FILTER (WHERE before ->> 'note' = 'rolled back' \
                                              OR after ->> 'note' = 'rolled back') = 0 \
                    FROM rh_probe.logged_history), \
                  'the rolled-back change must leave no entry';\n\
           ASSERT (SELECT l.before ->> 'note' = 'from second' AND l.before -> 'revision' = '3' \
                      AND l.after -> 'revision' = '4' \
                      AND l.position > ALL (SELECT position FROM rh_probe.logged_history \
                                             WHERE row_key = '{\"id\": 1}' AND position <> l.position) \
                    FROM rh_probe.logged_history l \
                    WHERE l.row_key = '{\"id\": 1}' AND l.after ->> 'note' = 'after rollback'), \
                  'the committed change must start from the committed row at a higher position';\n\
         END $$;\n",
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// Spec test 18, the stamp half of Beads `wamn-emtx.25`. `stamp_row` compares
/// JSONB text, so a numeric scale-only update moves the updated pair. A true
/// no-op keeps every stamp.
#[test]
fn a_numeric_scale_only_update_moves_the_stamps_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_fixture(&admin);
    apply(
        &db_url,
        "BEGIN;\n\
         SET LOCAL ROLE wamn_db_owner;\n\
         CREATE TABLE rh_probe.measured (\n\
             id integer PRIMARY KEY, amount numeric NOT NULL,\n\
             created_at timestamptz NOT NULL, created_by uuid NOT NULL,\n\
             updated_at timestamptz NOT NULL, updated_by uuid NOT NULL);\n\
         CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON rh_probe.measured\n\
             FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(\n\
                 'created_at', 'created_by', 'updated_at', 'updated_by');\n\
         GRANT SELECT, INSERT, UPDATE ON rh_probe.measured TO wamn_app;\n\
         COMMIT;\n",
    );
    as_guest(
        &db_url,
        &guest,
        ACTOR_A,
        "INSERT INTO rh_probe.measured (id, amount) VALUES (1, 1.0);\nSELECT pg_sleep(0.01);",
    );
    as_guest(
        &db_url,
        &guest,
        ACTOR_B,
        &format!(
            "UPDATE rh_probe.measured SET amount = 1.00 WHERE id = 1;\n\
             DO $$ BEGIN\n\
               ASSERT (SELECT amount::text = '1.00' AND created_by = '{ACTOR_A}' \
                          AND created_at < transaction_timestamp() \
                          AND updated_by = '{ACTOR_B}' \
                          AND updated_at = transaction_timestamp() \
                        FROM rh_probe.measured WHERE id = 1), \
                      'a scale-only update must keep the created pair and move the updated pair';\n\
             END $$;\n\
             SELECT pg_sleep(0.01);"
        ),
    );
    as_guest(
        &db_url,
        &guest,
        ACTOR_C,
        &format!(
            "UPDATE rh_probe.measured SET amount = amount WHERE id = 1;\n\
             DO $$ BEGIN\n\
               ASSERT (SELECT amount::text = '1.00' AND updated_by = '{ACTOR_B}' \
                          AND updated_at < transaction_timestamp() \
                        FROM rh_probe.measured WHERE id = 1), \
                      'a true no-op must keep every stamp';\n\
             END $$;"
        ),
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// Spec test 18, the log half of Beads `wamn-emtx.25`. `log_row_change`
/// compares JSONB text, so a numeric scale-only update writes an entry with
/// both spellings. A true no-op writes none. The relation has no stamp
/// trigger, so only the amount changes.
#[test]
fn a_numeric_scale_only_update_writes_a_log_entry_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_fixture(&admin);
    apply(
        &db_url,
        "BEGIN;\n\
         SET LOCAL ROLE wamn_db_owner;\n\
         CREATE TABLE rh_probe.amounts (id integer PRIMARY KEY, amount numeric NOT NULL);\n\
         SELECT wamn_history.create_history_table('rh_probe', 'amounts', false);\n\
         CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE ON rh_probe.amounts\n\
             FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');\n\
         GRANT SELECT, INSERT, UPDATE ON rh_probe.amounts TO wamn_app;\n\
         GRANT INSERT ON rh_probe.amounts_history TO wamn_app;\n\
         COMMIT;\n",
    );
    apply(
        &db_url,
        &format!(
            "{}{}\
             DO $$ BEGIN\n\
               ASSERT (SELECT array_agg(kind ORDER BY position) FROM rh_probe.amounts_history) \
                        = ARRAY['insert', 'update'], \
                      'a scale-only update must write one entry, and a no-op none';\n\
               ASSERT (SELECT before::text = '{{\"amount\": 1.0}}' \
                          AND after::text = '{{\"amount\": 1.00}}' \
                        FROM rh_probe.amounts_history WHERE kind = 'update'), \
                      'the entry must hold both spellings of the amount';\n\
             END $$;\n",
            logged_guest_transaction(
                &guest,
                ACTOR_A,
                CREATE_OPERATION,
                "INSERT INTO rh_probe.amounts (id, amount) VALUES (1, 1.0);",
            ),
            logged_guest_transaction(
                &guest,
                ACTOR_B,
                UPDATE_OPERATION,
                "UPDATE rh_probe.amounts SET amount = 1.00 WHERE id = 1;\n\
                 UPDATE rh_probe.amounts SET amount = amount WHERE id = 1;",
            ),
        ),
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// Receiving, which logs `purchase_order` and declares its history read.
const RECEIVING_MANIFEST: &[u8] = include_bytes!("../../../../apps/wamn_receiving/wamn.json");
const RECEIVING_MIGRATION: &str =
    include_str!("../../../../apps/wamn_receiving/migrations/0001_initial.sql");
const RECEIVING_HISTORY_READ: &str =
    include_str!("../../../../apps/wamn_receiving/query/load_purchase_order_history.sql");
const HISTORY_PURCHASE_ORDER: &str = "7a1c0a4e-2b9d-4f3e-8a61-0c5d2e9b4f17";

/// Run `sql` as the guest under the Receiving schema and return its rows, with
/// the unit separator between fields and the record separator between rows.
fn guest_rows(db_url: &str, guest: &str, sql: &str) -> Vec<Vec<String>> {
    let out = Command::new("psql")
        .arg(db_url)
        .args([
            "-X",
            "-v",
            "ON_ERROR_STOP=1",
            "-Atq",
            "-F",
            "\u{1f}",
            "-R",
            "\u{1e}",
        ])
        .arg("-c")
        .arg(format!(
            "SET ROLE \"{guest}\"; SET search_path = receiving; {sql}"
        ))
        .output()
        .expect("psql runs");
    assert!(
        out.status.success(),
        "the guest read failed:\n{}\n--- script ---\n{sql}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .expect("psql output is UTF-8")
        .trim_end_matches('\n')
        .split('\u{1e}')
        .filter(|record| !record.is_empty())
        .map(|record| record.split('\u{1f}').map(str::to_owned).collect())
        .collect()
}

/// Every page of the Receiving history read of `item`, two entries a page.
fn history_pages(db_url: &str, guest: &str, item: &str) -> Vec<Vec<String>> {
    let read = RECEIVING_HISTORY_READ.trim_end().trim_end_matches(';');
    let mut rows: Vec<Vec<String>> = Vec::new();
    loop {
        let after = rows.last().map_or("0", |row| row[0].as_str()).to_owned();
        let page = guest_rows(
            db_url,
            guest,
            &format!(
                "PREPARE history_page (uuid, bigint, bigint) AS {read}; \
                 EXECUTE history_page ('{item}', {after}, 2)"
            ),
        );
        assert!(page.len() <= 2, "a page must hold at most its limit");
        if page.is_empty() {
            return rows;
        }
        rows.extend(page);
    }
}

/// Whether a JSON plan has a node that writes or takes a row lock, the two
/// nodes for which generation classifies a statement as transactional.
fn writes_or_locks(plan: &serde_json::Value) -> bool {
    match plan {
        serde_json::Value::Object(node) => {
            matches!(
                node.get("Node Type").and_then(serde_json::Value::as_str),
                Some("ModifyTable" | "LockRows")
            ) || node.values().any(writes_or_locks)
        }
        serde_json::Value::Array(nodes) => nodes.iter().any(writes_or_locks),
        _ => false,
    }
}

/// The columns of `image` as the fold splits them.
fn image_columns(image: &str) -> Vec<(String, String)> {
    let row = HistoryRow {
        position: 1,
        kind: "insert",
        before: "{}",
        current: image,
        head_position: 1,
    };
    match state_at(&[row], 1) {
        Ok(RowState::Present(image)) => image
            .columns()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        other => panic!("{image} is not a row image: {other:?}"),
    }
}

/// The reconstruction half of spec test 1, over Receiving. The guest holds the
/// grants that generation derives for Receiving, writes an insert, two updates,
/// and a delete of a purchase order, and reads the history through the
/// Receiving SQL. The fold then shows the row at each retained position.
#[test]
fn the_history_read_reconstructs_a_row_at_retained_positions_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_fixture(&admin);
    apply(
        &db_url,
        &format!(
            "BEGIN;\n\
             SET LOCAL ROLE wamn_db_owner;\n\
             CREATE SCHEMA receiving;\n\
             {RECEIVING_MIGRATION}\
             CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON receiving.purchase_order\n\
                 FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row(\n\
                     'created_at', 'created_by', 'updated_at', 'updated_by');\n\
             SELECT wamn_history.create_history_table('receiving', 'purchase_order', false);\n\
             SELECT wamn_history.create_history_table('receiving', 'purchase_order_line', false);\n\
             CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE \
                 ON receiving.purchase_order\n\
                 FOR EACH ROW EXECUTE FUNCTION wamn_history.log_row_change('unlimited');\n\
             COMMIT;\n"
        ),
    );

    // The grants that generation derives from the Receiving manifest and the
    // server catalog. Receiving declares no generated insert or delete of a
    // purchase order, so the guest gets its writes from the test.
    let relation_fields = psql(
        &db_url,
        None,
        "SELECT c.relname || ':' || string_agg(a.attname, ',' ORDER BY a.attnum) \
           FROM pg_class c JOIN pg_attribute a ON a.attrelid = c.oid \
          WHERE c.relnamespace = 'receiving'::regnamespace AND c.relkind = 'r' \
            AND a.attnum > 0 AND NOT a.attisdropped \
          GROUP BY c.relname ORDER BY c.relname",
    )
    .lines()
    .map(|line| {
        let (table, fields) = line.split_once(':').expect("table and fields");
        wamn_schema_generator::DataAccessRelationFields::new(
            "receiving",
            table,
            fields.split(',').map(str::to_owned).collect(),
        )
    })
    .collect::<Vec<_>>();
    let overlay = wamn_schema_generator::derive_data_access_overlay_from_relation_fields(
        &relation_fields,
        RECEIVING_MANIFEST,
    )
    .expect("the Receiving manifest derives its data access");
    let grants = wamn_schema_generator::render_effective_data_access_sql(
        &wamn_schema_generator::derive_effective_data_access(&relation_fields, &[overlay])
            .expect("the Receiving data access is one installed set"),
    )
    .expect("the Receiving data access renders");
    apply(
        &db_url,
        &format!(
            "BEGIN;\n{grants}\
             GRANT INSERT, UPDATE, DELETE ON receiving.purchase_order TO wamn_app;\n\
             COMMIT;\n"
        ),
    );

    // The server plans the history read as a read, so it runs without a transaction.
    let plan = guest_rows(
        &db_url,
        &guest,
        &format!(
            "EXPLAIN (GENERIC_PLAN, FORMAT JSON) {}",
            RECEIVING_HISTORY_READ.trim_end().trim_end_matches(';')
        ),
    );
    let plan: serde_json::Value =
        serde_json::from_str(&plan[0][0]).expect("EXPLAIN returns a JSON plan");
    assert!(
        !writes_or_locks(&plan),
        "the history read must not write or lock: {plan}"
    );

    // A row with no retained entries returns an empty page.
    let unknown = history_pages(&db_url, &guest, "00000000-0000-4000-8000-000000000999");
    assert!(unknown.is_empty(), "an unknown row must return no entries");
    assert_eq!(state_at(&[], 1), Ok(RowState::Unavailable));

    // An insert, an update of the status and the revision, an update of a
    // text and a uuid, and a delete, each in its own transaction. After each write, the row image of
    // the row is the state that the fold must reconstruct.
    let image = || {
        psql(
            &db_url,
            None,
            &format!(
                "SELECT COALESCE((SELECT wamn_history.row_image(p)::text \
                   FROM receiving.purchase_order p WHERE id = '{HISTORY_PURCHASE_ORDER}'), '{{}}')"
            ),
        )
    };
    let mut images = Vec::new();
    for (actor, operation, write) in [
        (
            ACTOR_A,
            CREATE_OPERATION,
            format!(
                "INSERT INTO purchase_order (id, purchase_order_number, supplier_id) \
                   VALUES ('{HISTORY_PURCHASE_ORDER}', 'PO-history', \
                           '00000000-0000-4000-8000-000000000501');"
            ),
        ),
        (
            ACTOR_B,
            UPDATE_OPERATION,
            format!(
                "UPDATE purchase_order SET status = 'complete', row_version = 2 \
                   WHERE id = '{HISTORY_PURCHASE_ORDER}';"
            ),
        ),
        (
            ACTOR_B,
            UPDATE_OPERATION,
            format!(
                "UPDATE purchase_order SET purchase_order_number = 'PO-history-2', \
                   supplier_id = '00000000-0000-4000-8000-000000000502' \
                   WHERE id = '{HISTORY_PURCHASE_ORDER}';"
            ),
        ),
        (
            ACTOR_C,
            REPAIR_OPERATION,
            format!("DELETE FROM purchase_order WHERE id = '{HISTORY_PURCHASE_ORDER}';"),
        ),
    ] {
        apply(
            &db_url,
            &format!(
                "SELECT pg_sleep(0.01);\n{}",
                logged_guest_transaction(
                    &guest,
                    actor,
                    operation,
                    &format!("SET LOCAL search_path = receiving;\n{write}"),
                )
            ),
        );
        images.push(image());
    }

    let pages = history_pages(&db_url, &guest, HISTORY_PURCHASE_ORDER);
    let rows = pages
        .iter()
        .map(|row| HistoryRow {
            position: row[0].parse().expect("position is a bigint"),
            kind: &row[1],
            before: &row[6],
            current: &row[8],
            head_position: row[9].parse().expect("head position is a bigint"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows.iter().map(|row| row.kind).collect::<Vec<_>>(),
        ["insert", "update", "update", "delete"],
        "the read must return every entry of the row in position order"
    );
    assert!(
        rows.iter().all(|row| row.current == "{}"),
        "the current image of a deleted row must be '{{}}'"
    );
    assert_eq!(
        pages.iter().map(|row| row[2].as_str()).collect::<Vec<_>>(),
        [
            CREATE_OPERATION,
            UPDATE_OPERATION,
            UPDATE_OPERATION,
            REPAIR_OPERATION
        ]
    );

    let state = |position: i64| match state_at(&rows, position).expect("the read folds") {
        RowState::Present(image) => Some(
            image
                .columns()
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect::<Vec<_>>(),
        ),
        RowState::Absent => None,
        RowState::Unavailable => {
            panic!("position {position} is retained")
        }
    };
    for (row, expected) in rows.iter().zip(&images) {
        assert_eq!(
            state(row.position),
            (expected != "{}").then(|| image_columns(expected)),
            "the fold must reconstruct the row after the {} at position {}",
            row.kind,
            row.position
        );
    }
    assert_eq!(
        state_at(&rows, rows[0].position - 1),
        Ok(RowState::Unavailable)
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

/// Spec test 20 for the `app_system` log (epic rulings 40, 42, 43, and 58).
/// `deploy/sql/app-schema.sql` gives each `app_system` relation a
/// `wamn_record_history_log` trigger with the argument `unlimited`. After
/// apply-package converges the audit retention grants, the audit retention
/// role holds no privilege on `app_system` or on any object in it. The
/// `P30D` fixture relation is the control: the same convergence grants the
/// role its history table.
#[test]
fn the_app_system_history_stays_out_of_audit_retention_reach_on_postgres() {
    let (_server, admin) = owned_server();
    let (db_url, guest) = record_history_log_fixture(&admin);
    apply(
        &db_url,
        &format!(
            "BEGIN;\n{};\n{}\nCOMMIT;\n",
            wamn_control_provision::audit_retention::AUDIT_RETENTION_LOCK_SQL,
            wamn_control_provision::audit_retention::reconcile_audit_retention_grants_sql()
        ),
    );

    let logs = psql(
        &db_url,
        None,
        "SELECT string_agg(c.relname || ':' || encode(t.tgargs, 'escape'), ' ' \
                           ORDER BY c.relname COLLATE \"C\") \
           FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid \
          WHERE c.relnamespace = 'app_system'::regnamespace \
            AND t.tgname = 'wamn_record_history_log' AND NOT t.tgisinternal",
    );
    assert_eq!(
        logs,
        "api_keys:unlimited\\000 configurations:unlimited\\000 permissions:unlimited\\000 \
         roles:unlimited\\000 user_roles:unlimited\\000 users:unlimited\\000",
        "every app_system relation must log with the retention unlimited"
    );
    let targets = psql(
        &db_url,
        None,
        wamn_control_provision::audit_retention::AUDIT_RETENTION_TARGETS_SQL,
    );
    assert_eq!(
        targets, "rh_probe|tenanted|tenanted_history|30",
        "the retention targets must be the P30D fixture relation alone"
    );
    let reach = psql(
        &db_url,
        None,
        "SELECT concat_ws(' ', \
           has_table_privilege('wamn_audit_retention', 'rh_probe.tenanted_history', 'DELETE'), \
           has_schema_privilege('wamn_audit_retention', 'app_system', 'USAGE'), \
           (SELECT count(*) FROM pg_namespace n \
             CROSS JOIN LATERAL aclexplode(n.nspacl) AS acl \
             WHERE n.nspname = 'app_system' \
               AND acl.grantee = 'wamn_audit_retention'::regrole), \
           (SELECT count(*) FROM pg_class c \
             CROSS JOIN LATERAL aclexplode(c.relacl) AS acl \
             WHERE c.relnamespace = 'app_system'::regnamespace \
               AND acl.grantee = 'wamn_audit_retention'::regrole), \
           (SELECT count(*) FROM pg_attribute a \
             JOIN pg_class c ON c.oid = a.attrelid \
             CROSS JOIN LATERAL aclexplode(a.attacl) AS acl \
             WHERE c.relnamespace = 'app_system'::regnamespace \
               AND acl.grantee = 'wamn_audit_retention'::regrole))",
    );
    assert_eq!(
        reach, "t f 0 0 0",
        "the audit retention role reaches the app_system log (order: control \
         DELETE on the P30D history table, app_system USAGE, schema ACL \
         entries, relation ACL entries, column ACL entries)"
    );

    apply(&admin, &format!("DROP ROLE \"{guest}\";\n"));
}

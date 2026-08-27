//! The hand-written DDL's tenant floor derives from `current_user`.
//!
//! `wamn-0h0g.22.6.3` swept 43 guest-reachable relations across four files off
//! the settable `app.tenant` claim and onto
//! `wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()`,
//! each with the expression index that keeps the predicate sargable.
//!
//! `wamn-0h0g.22.17` then gave all 43 a SECOND arm. The floor is the GUEST
//! floor, narrowed `TO wamn_app`; PostgreSQL default-denies when RLS is enabled
//! and no policy matches the connected role, so that narrowing LOCKS OUT every
//! platform principal rather than exempting it — and locks it out at zero rows,
//! not at an error. One permissive arm `TO wamn_platform` per relation is what
//! admits them, and their table grants stay the thing that limits them.
//!
//! # Why these tests read the files as text
//!
//! `wamn-hopk` R5 forbids tests that scan source as text, with ONE exemption:
//! identity pins — byte-equality against a named artifact. The bootstrap block
//! is generated SQL checked into four files, so byte-equality against the
//! builder is exactly that exemption, and it is the only thing that catches a
//! hand-edit of a security-critical function definition.
//!
//! The live half asserts the SERVER's answer (`pg_policy`, `pg_index`,
//! `has_table_privilege`), never the file text.
//!
//! ```bash
//! docker run -d --name wamn-floor-pg -e POSTGRES_PASSWORD=probe \
//!   -p 127.0.0.1:5434:5432 postgres:18
//! until psql postgres://postgres:probe@localhost:5434/postgres -Atqc 'select 1'; do :; done
//! WAMN_TENANT_FLOOR_PG_URL=postgres://postgres:probe@localhost:5434/postgres \
//!   cargo test -p wamn-control-provision --test deploy_sql_authority
//! docker rm -f wamn-floor-pg      # BY EXPLICIT NAME. Never prune.
//! ```

use std::process::Command;

use wamn_control_provision::CredentialGeneration;
use wamn_control_provision::sql;
use wamn_control_provision::tenant_key::{authority_derivations_bootstrap_sql, tenant_key};
use wamn_control_provision::workload_role::{
    PLATFORM_GROUP_ROLE, WorkloadRoleFamily, WorkloadRoleScope, workload_generation_role,
};

const POSTGRES_INIT: &str = include_str!("../../../../deploy/sql/postgres-init.sql");
const CATALOG_SCHEMA: &str = include_str!("../../../../deploy/sql/catalog-schema.sql");
const RUN_STATE: &str = include_str!("../../../../deploy/sql/run-state.sql");
const RUN_QUEUE: &str = include_str!("../../../../deploy/sql/run-queue.sql");
const APP_SCHEMA: &str = include_str!("../../../../deploy/sql/app-schema.sql");

/// The retired predicate, exactly as the files spelled it.
const RETIRED: &str = "tenant_id = NULLIF(current_setting('app.tenant', true), '')";

/// The governed predicate every swept policy carries.
const GOVERNED: &str = "wamn_authority.tenant_key(tenant_id) = wamn_authority.current_tenant_key()";

/// The narrowing that makes the floor the GUEST floor (`wamn-0h0g.22.17`),
/// spelled with the `USING` that follows it so a `GRANT … TO wamn_app` on the
/// same relation cannot be counted as one.
const GUEST_TARGETED: &str = "TO wamn_app\n";

/// The one permissive arm per governed relation. `AS PERMISSIVE` and the
/// command are spelled out at every site, so this substring identifies an arm
/// and nothing else.
const PLATFORM_ARM: &str = "TO wamn_platform\n";

/// The two roles this file's platform arm mints as probes. Named here so
/// `reset` can drop them: roles are CLUSTER-wide, and the arm's whole point is
/// that a leftover healthy membership masks a mutated builder.
const PLATFORM_PROBE_OUTSIDER: &str = "wamn_floor_outsider";

/// The effect-writer generation login the platform arm probes with, composed by
/// the real mint rather than spelled by hand.
fn platform_probe_writer() -> String {
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
/// and the assertion stayed true. PROVEN, not suspected: a mutant setting
/// `is_platform_grain(Retention)` to false AND removing the matching edge from
/// `postgres-init.sql` did NOT kill that test. Only the live retention gate saw
/// the consequence — `reported_one = false`, `old_gone = false`: a SILENT
/// LOCKOUT WITH EXIT 0.
///
/// That is the tautology shape this branch has paid for before: when every
/// consumer delegates to one function, a test comparing two of them proves
/// nothing. The remedy is the same as last time — PIN THE VALUE, and keep one
/// consumer that does NOT delegate. Admitting or demoting a family now costs one
/// deliberate edit here, which is the point.
///
/// Sorted, because both consumers compare against a sorted list.
const PLATFORM_GRAIN_ACL_ROLES: [&str; 8] = [
    "wamn_dispatch_reader",
    "wamn_effect_writer",
    "wamn_event_materializer",
    "wamn_executor_platform",
    "wamn_http_admitter",
    "wamn_management_admitter",
    "wamn_run_retention",
    "wamn_service_reader",
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

/// Every file that carries the bootstrap, with the number of governed policies
/// it re-keyed and the number of retired predicates it deliberately keeps.
const FILES: [(&str, &str, usize, usize); 4] = [
    ("catalog-schema.sql", CATALOG_SCHEMA, 24, 0),
    ("app-schema.sql", APP_SCHEMA, 7, 0),
    // run-state.sql keeps `operator_run_actions`: one policy, two clauses.
    ("run-state.sql", RUN_STATE, 5, 2),
    ("postgres-init.sql", POSTGRES_INIT, 7, 0),
];

/// IDENTITY PIN (the R5 exemption): the checked-in bootstrap is the builder's
/// output byte for byte. An attacker who can hand-edit `tenant_key` in one of
/// these files owns every predicate in that database, and nothing else in the
/// tree would notice.
#[test]
fn every_file_carries_the_generated_bootstrap_verbatim() {
    let bootstrap = authority_derivations_bootstrap_sql();
    for (name, sql, _, _) in FILES {
        assert_eq!(
            sql.matches(&bootstrap).count(),
            1,
            "{name} must carry authority_derivations_bootstrap_sql() exactly once, \
             byte for byte"
        );
    }
}

/// Both directions. Presence of the governed predicate is not enough: a policy
/// that KEPT the retired one would still hand every tenant's rows to whoever
/// sets the claim.
#[test]
fn the_swept_files_carry_the_governed_predicate_and_only_the_ruled_exceptions() {
    for (name, sql, governed, retired) in FILES {
        // Each policy spells the predicate twice (USING + WITH CHECK) unless it
        // is read-only, so this counts clauses, not policies — the count is
        // pinned per file rather than derived, so a lost clause shows up.
        assert!(
            sql.matches(GOVERNED).count() >= governed,
            "{name} must carry at least {governed} governed predicate clauses"
        );
        assert_eq!(
            sql.matches(RETIRED).count(),
            retired,
            "{name} carries an unexpected number of retired predicates"
        );
        assert_eq!(
            sql.matches("((wamn_authority.tenant_key(tenant_id)))")
                .count(),
            governed,
            "{name} must carry one tenant-key expression index per re-keyed relation"
        );
    }
    // The file that was left alone entirely, and the determination in writing.
    assert_eq!(
        RUN_QUEUE.matches(RETIRED).count(),
        2,
        "run-queue.sql keeps its host-injected claim"
    );
    assert_eq!(
        RUN_QUEUE.matches(GOVERNED).count(),
        0,
        "run-queue.sql is not guest-reachable and must not be re-keyed"
    );
}

/// THE ARM COUNT, PER FILE, EXACT — because the existing guard above is BLIND
/// to this change.
///
/// Adding a `TO` clause moves no governed clause, no retired clause and no
/// expression index, so a narrowing applied to 40 of the 43 relations passes
/// every assertion in this file that predates `wamn-0h0g.22.17`. That is the
/// false-coverage shape, and this is what closes it: both halves of every
/// relation's floor are counted, per file, against a number written down here.
///
/// Both directions matter and the second is the sharp one. A floor narrowed
/// `TO wamn_app` with its platform arm MISSING does not raise — PostgreSQL
/// default-denies when RLS is on and no policy matches the connected role, and
/// `current_tenant_key` derives NULL outside the guest generation pattern — so
/// the platform principal reads ZERO ROWS in silence. An arm count short by one
/// is a silent cross-relation lockout, which is why it is `assert_eq!` and not
/// a floor.
#[test]
fn every_governed_relation_carries_both_the_guest_floor_and_one_platform_arm() {
    for (name, sql, governed, _) in FILES {
        assert_eq!(
            sql.matches(GUEST_TARGETED).count(),
            governed,
            "{name} must narrow exactly {governed} floor policies TO wamn_app"
        );
        assert_eq!(
            sql.matches(PLATFORM_ARM).count(),
            governed,
            "{name} must carry exactly one permissive TO wamn_platform arm per \
             governed relation ({governed})"
        );
    }
    // The ruled exceptions keep the host-injected claim and get NO arm: they are
    // not guest-reachable, so there is no lockout to repair.
    assert_eq!(
        RUN_QUEUE.matches(PLATFORM_ARM).count(),
        0,
        "run-queue.sql is host-injected and takes no platform arm"
    );
    // The rejected shortcut — BYPASSRLS — is NOT checked here. All three
    // occurrences of the word in these files are prose ("no BYPASSRLS"), so a
    // text count reads the comments, not the DDL. The role attribute is asserted
    // from `pg_authid` in the live arm below, which is the only place it is a
    // fact rather than a sentence.
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
/// run inside a transaction; `catalog-schema.sql` owns its own `BEGIN`. Neither
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
        "wamn_platform",
        // Probe roles this file's live arms mint. A leftover one fails the next
        // run's `CREATE ROLE` rather than masking anything, but the gate is
        // supposed to be re-runnable against a surviving cluster.
        PLATFORM_PROBE_OUTSIDER,
        &platform_probe_writer(),
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
    let Ok(admin) = std::env::var("WAMN_TENANT_FLOOR_PG_URL") else {
        eprintln!(
            "skipping the_swept_floor_admits_only_the_connected_guest_on_postgres \
             (set WAMN_TENANT_FLOOR_PG_URL to run)"
        );
        return;
    };

    // HERMETIC, and it has to be: `postgres-init.sql` carries a bare
    // `CREATE DATABASE wamn` and bare `CREATE ROLE`s, so a second run against a
    // surviving cluster fails on the first statement — and a run that found the
    // database already populated would be asserting against LAST run's schema,
    // not this build's. Roles are CLUSTER-wide, so this gate OWNS its server:
    // point it only at a disposable one.
    reset(&admin);
    apply(&admin, POSTGRES_INIT);
    let base = admin.rsplit_once('/').expect("url names a database").0;
    let db_url = format!("{base}/wamn");
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
        governed, "43",
        "the sweep must cover exactly the 43 guest-reachable relations"
    );

    // 2b. BORN PARKED (wamn-0h0g.20.30 for the attempt ledger, wamn-0h0g.20.32
    //     for its two siblings; owner ruling on wamn-0h0g.20.28). THE SERVER'S OWN
    //     ANSWER over the REAL DDL, with no reconciler in the loop:
    //     `wamn_effect_writer` — the stable ACL role every provisioned generation
    //     LOGIN inherits with INHERIT TRUE — READS the effect ledgers and cannot
    //     APPEND to any of the three, at table level or at any column. Prose
    //     calling the writer parked is not evidence; this is.
    let writer_ledger_acl = psql(
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
        writer_ledger_acl, "t f f f f t",
        "the schema of record did not mint a READ-ONLY effect writer across the \
         three effect ledgers (order: attempt SELECT, attempt INSERT, attempt \
         column INSERT, dispatches INSERT, outcomes INSERT, role is unprivileged)"
    );

    // 3. A MINTED GUEST READS ITS OWN TENANT AND ONLY ITS OWN. The role name is
    //    composed by the mint, not by hand, so this also proves the digest the
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
            "INSERT INTO catalog.catalogs (tenant_id, catalog_id, version, schema_version, state) \
               VALUES ('t1', 'c1', 1, 1, 'draft'), ('t2', 'c2', 1, 1, 'draft');\n\
             BEGIN;\n\
             SET LOCAL ROLE \"{guest}\";\n\
             SET LOCAL app.tenant = 't2';\n\
             DO $$ BEGIN\n\
                 ASSERT (SELECT count(*) FROM catalog.catalogs) = 1, \
                        'the guest sees exactly its own tenant';\n\
                 ASSERT (SELECT count(*) FROM catalog.catalogs WHERE tenant_id = 't2') = 0, \
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
/// `catalog.catalog_heads` both got `ERROR: permission denied for function
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
/// `GRANT wamn_platform TO wamn_effect_writer` lands `inherit_option = false`
/// and every platform read silently returns zero. Measured both ways on 18.6:
/// bare grant 0 rows, `INHERIT TRUE` all rows. The edge option is asserted below
/// from `pg_auth_members`, not assumed from the builder's text.
#[test]
fn the_platform_arm_admits_every_platform_family_from_the_server() {
    let Ok(admin) = std::env::var("WAMN_TENANT_FLOOR_PG_URL") else {
        eprintln!(
            "skipping the_platform_arm_admits_every_platform_family_from_the_server \
             (set WAMN_TENANT_FLOOR_PG_URL to run)"
        );
        return;
    };

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
    //    one with none sum to the same 43 and leave a silent lockout standing.
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
    //    `platform_group_membership_sql` is applied for ALL twelve families,
    //    including the four that must NOT be members — its revoke arm is what
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

    // 4. THE ADMISSION ITSELF, END TO END, over a relation whose grants the
    //    schema of record already carries. Two tenants' rows; the platform
    //    principal reads BOTH (it has no tenant grain to narrow to), the guest
    //    reads ONE, and a login in neither group reads NONE.
    let hash = "'sha256:' || repeat('0', 64)";
    apply(
        &db_url,
        &format!(
            "INSERT INTO wamn_run.effect_attempts \
               (tenant_id, attempt_id, run_id, root_plan_hash, current_plan_hash, frame_id, \
                local_node_id, source_artifact_hash, requirement_name, occurrence, seq, \
                generation_fact_kind, attempt_started_at, attempt_deadline_at, \
                attempt_input_ref, created_at) \
             SELECT t, gen_random_uuid(), 'r', {hash}, {hash}, 0, 'n', {hash}, 'req', 0, 0, \
                    'not-required', now(), now(), 'ref', now() \
               FROM unnest(ARRAY['t1', 't2']) AS t;\n"
        ),
    );
    let writer = platform_probe_writer();
    // `outsider` holds the SAME table grant and NEITHER membership. Without it a
    // platform read proves only that SELECT was granted, not that the arm is
    // what admitted the rows.
    apply(
        &admin,
        &format!(
            "CREATE ROLE \"{writer}\" NOLOGIN NOSUPERUSER NOBYPASSRLS;\n\
             GRANT wamn_effect_writer TO \"{writer}\" \
               WITH ADMIN FALSE, INHERIT TRUE, SET FALSE;\n\
             CREATE ROLE {PLATFORM_PROBE_OUTSIDER} NOLOGIN NOSUPERUSER NOBYPASSRLS;\n"
        ),
    );
    apply(
        &db_url,
        &format!(
            "GRANT USAGE ON SCHEMA wamn_run TO {PLATFORM_PROBE_OUTSIDER};\n\
             GRANT SELECT ON wamn_run.effect_attempts TO {PLATFORM_PROBE_OUTSIDER};\n"
        ),
    );
    // A SUPERUSER FIXTURE MASKS RLS ENTIRELY, so the probe roles are asserted
    // unprivileged from `pg_roles` before a single row is counted.
    let unprivileged = psql(
        &db_url,
        None,
        &format!(
            "SELECT bool_and(NOT (rolsuper OR rolbypassrls)) FROM pg_roles \
              WHERE rolname IN ('{writer}', '{PLATFORM_PROBE_OUTSIDER}', 'wamn_effect_writer', \
                                'wamn_platform')"
        ),
    );
    assert_eq!(
        unprivileged, "t",
        "a probe role that is superuser or BYPASSRLS proves nothing about RLS"
    );
    apply(
        &db_url,
        &format!(
            "BEGIN;\n\
             SET LOCAL ROLE \"{writer}\";\n\
             DO $$ BEGIN\n\
                 ASSERT (SELECT count(*) FROM wamn_run.effect_attempts) = 2, \
                        'THE PLATFORM ARM DOES NOT ADMIT: a governed relation the \
                         effect writer holds SELECT on reads short';\n\
             END $$;\n\
             COMMIT;\n\
             BEGIN;\n\
             SET LOCAL ROLE {PLATFORM_PROBE_OUTSIDER};\n\
             DO $$ BEGIN\n\
                 ASSERT (SELECT count(*) FROM wamn_run.effect_attempts) = 0, \
                        'THE FLOOR LEAKS: a login in neither wamn_app nor \
                         wamn_platform read rows';\n\
             END $$;\n\
             COMMIT;\n"
        ),
    );
    // `DROP OWNED BY` FIRST, and in the project database, not the admin one:
    // the outsider holds a schema USAGE and a table SELECT, and `DROP ROLE`
    // refuses while an ACL entry names it. `DROP OWNED BY` is per-database.
    apply(
        &db_url,
        &format!("DROP OWNED BY \"{writer}\", {PLATFORM_PROBE_OUTSIDER};\n"),
    );
    apply(
        &admin,
        &format!("DROP ROLE \"{writer}\";\nDROP ROLE {PLATFORM_PROBE_OUTSIDER};\n"),
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
/// An emitter that raises nothing has proved nothing. Both appliers succeed
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
    let Ok(admin) = std::env::var("WAMN_TENANT_FLOOR_PG_URL") else {
        eprintln!(
            "skipping the_two_scenario_author_emitters_agree_at_zero_memberships \
             (set WAMN_TENANT_FLOOR_PG_URL to run)"
        );
        return;
    };

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

/// THE SWEEP-VISIBLE HALF of `wamn-0h0g.22.27`.
///
/// The claim that matters — the member set both appliers leave behind — is a
/// POST-STATE read and lives in the live arm above. But a mutant that dies only
/// in a live gate ships green in the ordinary sweep, and this branch has paid
/// for that repeatedly. Emitter agreement is a property of the EMITTED TEXT, so
/// it is assertable here with no server at all: neither artifact may name
/// `wamn_platform` in a grant to the scenario author.
///
/// It is deliberately NOT a privilege claim. Whether that membership would
/// actually confer anything is the server's answer and the live arm asks it.
#[test]
fn neither_scenario_author_emitter_grants_the_platform_group() {
    let converge = wamn_schema_control::ensure_scenario_author_role_sql();
    for (label, sql) in [
        ("deploy/sql/postgres-init.sql", POSTGRES_INIT),
        ("ensure_scenario_author_role_sql", converge),
    ] {
        for line in sql.lines() {
            let statement = line.split("--").next().unwrap_or(line);
            assert!(
                !(statement.contains("GRANT")
                    && statement.contains(PLATFORM_GROUP_ROLE)
                    && statement.contains(SCENARIO_AUTHOR_GROUP_MEMBER)),
                "{label} grants {} to {SCENARIO_AUTHOR_GROUP_MEMBER}: the two \
                 emitters must agree, and they agree at ZERO GRANTS \
                 (wamn-0h0g.22.27). Replicating it into the converge path is \
                 REJECTED — it would ratify an unruled membership through the \
                 side door",
                PLATFORM_GROUP_ROLE,
            );
        }
    }
}

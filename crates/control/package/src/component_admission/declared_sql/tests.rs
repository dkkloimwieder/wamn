use super::{RefusalKind, check_declared_statement};

fn assert_refused(kind: RefusalKind, statements: &[&str]) {
    for sql in statements {
        let refusal =
            check_declared_statement(sql).expect_err(&format!("admission must refuse {sql:?}"));
        assert_eq!(refusal.kind(), kind, "{sql:?}: {refusal}");
    }
}

fn assert_admitted(statements: &[&str]) {
    for sql in statements {
        if let Err(refusal) = check_declared_statement(sql) {
            panic!("admission must admit {sql:?}: {refusal}");
        }
    }
}

// wamn-9e4ks: the guest reaches no platform counter function. These were the
// run-time refusals of `reject_claim_mutation` on a declared statement.
#[test]
fn refuses_counter_function_calls() {
    assert_refused(
        RefusalKind::CounterFunction,
        &[
            "SELECT platform.bump_counter($1::text::uuid, 'po', '')",
            "select PLATFORM.ASSIGN_COUNTER($1::uuid, 'batch', $2)",
            "SELECT platform.set_counter($1::uuid, 'po', '', 1)",
            "SELECT \"platform\".\"bump_counter\"($1::uuid, 'po', '')",
            "SELECT platform . bump_counter($1::uuid, 'po', '')",
            "SELECT bump_counter($1::uuid, 'po', '')",
            "SELECT pg_catalog.bump_counter($1::uuid, 'po', '')",
            "SELECT wamn.platform.assign_counter($1::uuid, 'po', '')",
            "WITH n AS (SELECT platform./* x */bump_counter($1::uuid, 'po', '')) SELECT 1",
            "SELECT '/*', platform.set_counter($1::uuid, 'po', '', 1), '*/'",
            "SELECT platform/* a */./* b */\"set_counter\"($1::uuid, 'po', '', 1)",
            "SELECT platform.\n-- line\nbump_counter($1::uuid, 'po', '')",
            "SELECT Platform.Bump_Counter($1::uuid, 'po', '')",
            "SELECT \"BUMP_COUNTER\"($1::uuid, 'po', '')",
            // Nested: subquery, CTE, lateral, CASE, cast, default, coalesce.
            "SELECT id FROM purchase_order WHERE number = (SELECT platform.bump_counter($1::uuid, 'po', '')::text)",
            "WITH a AS (WITH b AS (SELECT platform.assign_counter($1::uuid, 'po', '')) SELECT * FROM b) SELECT * FROM a",
            "SELECT * FROM purchase_order p, LATERAL (SELECT platform.set_counter($1::uuid, 'po', '', 1)) s",
            "SELECT CASE WHEN $1::boolean THEN platform.bump_counter($2::uuid, 'po', '') END",
            "SELECT CAST(platform.bump_counter($1::uuid, 'po', '') AS text)",
            "SELECT platform.bump_counter($1::uuid, 'po', '')::text::bigint",
            "INSERT INTO purchase_order (number) VALUES (platform.bump_counter($1::uuid, 'po', '')::text)",
            "INSERT INTO purchase_order (number) VALUES (DEFAULT) ON CONFLICT (id) DO UPDATE SET number = platform.set_counter($1::uuid, 'po', '', 1)::text",
            "UPDATE purchase_order SET number = coalesce(number, platform.assign_counter($1::uuid, 'po', '')::text) WHERE id = $2",
            "DELETE FROM purchase_order WHERE id IN (SELECT id FROM purchase_order WHERE platform.bump_counter($1::uuid, 'po', '') > 0)",
            "SELECT * FROM platform.bump_counter($1::uuid, 'po', '')",
            "SELECT * FROM ROWS FROM (platform.assign_counter($1::uuid, 'po', ''))",
            "SELECT array_agg(x) FILTER (WHERE platform.bump_counter($1::uuid, 'po', '') > 0) FROM t",
            "SELECT 1 FROM t ORDER BY platform.bump_counter($1::uuid, 'po', '')",
            "CALL platform.set_counter($1::uuid, 'po', '', 1)",
            // A name held in a string, or cast to a function reference.
            "SELECT 'platform.bump_counter'::regproc",
            "SELECT $1::text WHERE 'x' <> 'assign_counter'",
            // Unicode escapes the parser decodes in an E'' string.
            "SELECT E'\\u0062ump_counter'::regproc",
        ],
    );
}

#[test]
fn refuses_unicode_escapes() {
    assert_refused(
        RefusalKind::UnicodeEscape,
        &[
            "SELECT U&\"\\0062ump_counter\"($1::uuid, 'po', '')",
            "SELECT platform.U&\"\\0073et_counter\"($1::uuid, 'po', '', 1)",
            "SELECT u&\"bump_counter\"($1::uuid, 'po', '')",
            "SELECT U&\"!0062ump_counter\" UESCAPE '!' ($1::uuid, 'po', '')",
            "SELECT U&'\\0062ump_counter'::regproc",
            "SELECT * FROM U&\"\\0070latform\".setting",
            "INSERT INTO U&\"\\0070latform\".setting (key) VALUES ($1)",
            "SELECT 1 FROM t WHERE name = U&'plain'",
        ],
    );
}

#[test]
fn refuses_do_blocks_and_function_bodies() {
    assert_refused(
        RefusalKind::CodeBlock,
        &[
            "DO $$ BEGIN PERFORM 1; END $$",
            "do language plpgsql $$ BEGIN EXECUTE 'SELECT 1'; END $$",
            "DO $body$ BEGIN EXECUTE 'SELECT platform.' || 'bump_counter($1)'; END $body$",
            "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$ BEGIN EXECUTE 'SELECT 1'; END $$",
            "CREATE OR REPLACE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1'",
            "CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN EXECUTE 'SELECT 1'; END $$",
        ],
    );
}

#[test]
fn refuses_dynamic_execute() {
    assert_refused(
        RefusalKind::DynamicExecute,
        &[
            "EXECUTE counter_call",
            "execute counter_call(1, 'po')",
            "EXPLAIN ANALYZE EXECUTE counter_call",
            "CREATE TABLE copy AS EXECUTE counter_call",
        ],
    );
}

#[test]
fn refuses_xml_query_functions() {
    assert_refused(
        RefusalKind::XmlQuery,
        &[
            "SELECT query_to_xml('SELECT 1', true, false, '')",
            "SELECT pg_catalog.query_to_xml($1, true, false, '')",
            "SELECT \"query_to_xmlschema\"($1, true, false, '')",
            "SELECT query_to_xml_and_xmlschema($1, true, false, '')",
            "SELECT QUERY_TO_XML($1, true, false, '')",
            "SELECT cursor_to_xml('c', 1, true, false, '')",
            "SELECT cursor_to_xmlschema('c', true, false, '')",
            "SELECT pg_catalog . /* c */ Cursor_To_Xml($1, 1, true, false, '')",
            "WITH x AS (SELECT query_to_xml($1, true, false, '') AS doc) SELECT doc FROM x",
            "SELECT id FROM t WHERE (SELECT query_to_xml($1, true, false, '')) IS NOT NULL",
            "SELECT query_to_xml($1, true, false, '')::text",
        ],
    );
}

// wamn-cjv.2: the in-band claim and role mutation guard.
#[test]
fn refuses_session_writes() {
    assert_refused(
        RefusalKind::SessionWrite,
        &[
            "SET app.env_key = 'victim'",
            "set local app.env_key = 'victim'",
            "SET SESSION app.env_key TO 'victim'",
            "SET ROLE postgres",
            "set session authorization postgres",
            "SET app.role = 'admin'",
            "set local app.user_id = '00000000-0000-4000-8000-000000000000'",
            "RESET app.role",
            "RESET app.env_key",
            "RESET ALL",
            "   \n\t SET app.env_key='victim'",
            "/* sneaky */ SET app.env_key='victim'",
            "-- lead\nSET app.env_key='victim'",
            "SELECT set_config('app.env_key','victim',false)",
            "WITH t AS (SELECT set_config('app.env_key','victim',true)) SELECT 1",
            "select pg_catalog.set_config('app.env_key','victim',false)",
            "SELECT SET_CONFIG('app.env_key','victim',false)",
            "SELECT \"set_config\"('app.role','admin',true)",
            "WITH t AS (SELECT set_config('app.user_id','00000000-0000-4000-8000-000000000000',true)) SELECT 1",
            "SELECT set_config('app.application', 'other', true)",
        ],
    );
}

// l5i9.12.2: the guest wamn.* logical-message forgery guard. The parsed check
// refuses every guest logical message, because a computed prefix hides `wamn.`.
#[test]
fn refuses_logical_messages() {
    assert_refused(
        RefusalKind::LogicalMessage,
        &[
            "SELECT pg_logical_emit_message(true,'wamn.causation','{}')",
            "select PG_LOGICAL_EMIT_MESSAGE(true, 'wamn.causation', $1)",
            "SELECT pg_logical_emit_message_bytea(true,'wamn.anything','\\x00')",
            "/* hide */ SELECT pg_logical_emit_message(false,'wamn.x','y')",
            "WITH t AS (SELECT pg_logical_emit_message(true,'wamn.causation','z')) SELECT 1",
            "SELECT pg_catalog.pg_logical_emit_message(true, 'wa' || 'mn.causation', $1)",
        ],
    );
}

#[test]
fn refuses_platform_writes() {
    assert_refused(
        RefusalKind::PlatformWrite,
        &[
            "INSERT INTO platform.number_counter (sequence_id, counter, key_value, value) VALUES ($1, 'po', '', 1)",
            "UPDATE platform.number_counter SET value = 1",
            "update only platform.setting set value = $1",
            "DELETE FROM platform.write_log",
            "delete from \"platform\".\"user\" where id = $1",
            "MERGE INTO platform.setting AS t USING s ON true WHEN MATCHED THEN DELETE",
            "TRUNCATE platform.number_counter",
            "truncate table only platform.setting",
            "TRUNCATE purchase_order, platform.setting",
            "COPY platform.setting FROM STDIN",
            "INSERT /* x */ INTO\n  platform . number_sequence (application, name, pattern) VALUES ($1, $2, $3)",
            "WITH w AS (UPDATE platform.setting SET value = $1 RETURNING 1) SELECT * FROM w",
            "WITH w AS (DELETE FROM platform.setting RETURNING 1) SELECT '/*', '*/' FROM w",
            "INSERT INTO \"PLATFORM\".setting (key) VALUES ($1)",
            "INSERT INTO wamn.platform.setting (key) VALUES ($1)",
            "WITH a AS (WITH b AS (INSERT INTO platform.setting (key) VALUES ($1) RETURNING 1) SELECT * FROM b) SELECT * FROM a",
        ],
    );
}

#[test]
fn refuses_unparsed_and_multiple_statements() {
    assert_refused(
        RefusalKind::Unparsed,
        &[
            "SELECT FROM WHERE",
            "SELECT platform.bump_counter(",
            "SELECT 'unterminated",
        ],
    );
    assert_refused(
        RefusalKind::NotOneStatement,
        &[
            "",
            "-- only a comment",
            "SELECT 1; SELECT 2",
            "SELECT '/*'; WITH w AS (DELETE FROM platform.setting RETURNING 1) SELECT '*/'",
        ],
    );
}

#[test]
fn admits_platform_reads_and_application_sql() {
    assert_admitted(&[
        "SELECT value FROM platform.number_counter WHERE sequence_id = $1::text::uuid",
        "SELECT s.id FROM platform.number_sequence AS s WHERE s.name = $1",
        "SELECT * FROM platform.setting WHERE key = 'timezone'",
        "INSERT INTO purchase_order (number) VALUES ($1)",
        "UPDATE purchase_order SET reset_counter = 0 WHERE id = $1",
        "UPDATE myplatform.setting SET value = 1",
        "INSERT INTO receiving.platform_note (body) VALUES ($1)",
        "SELECT count(*) FROM s2.rls_secrets WHERE secret LIKE $1",
        "INSERT INTO t (env_key, k) VALUES (current_setting('app.env_key', true), $1)",
        "SELECT current_setting('app.env_key', true)",
        "DELETE FROM assets WHERE id = $1",
        "SELECT table_to_xml('t', true, false, '')",
        "SELECT id, bump_counter_total FROM purchase_order",
        "SELECT 1;",
        "INSERT INTO t (a) SELECT $1 FROM platform.setting WHERE key = 'x' RETURNING id",
        "SELECT id FROM t FOR UPDATE",
    ]);
}

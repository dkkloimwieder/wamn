-- The per-project SYSTEM SCHEMA v1 (wamn-as5, docs/archive/platform-plan.md §2.4). The
-- application-facing auth/RBAC/config tables that live IN a project database:
-- user, role (+ the user↔role linkage, user_role), permission, and
-- environment, and the platform configuration tables of
-- docs/plan/platform-config.md (PLATFORM CONFIGURATION, at the end).
--
-- This is the AUTH/RBAC half of item 2.4. Package coordinates, immutable
-- migration tables, effective releases, and release membership are already
-- shipped by deploy/sql/catalog-schema.sql and are deliberately not redefined
-- here. A
-- `deployments` table is deferred — a live WorkloadDeployment is a K8s CR, so a
-- registry table would duplicate cluster state until there is a concrete reader
-- (follow-up bead).
--
-- DISTINCT FROM the T1 control-plane registry (deploy/sql/system-schema.sql,
-- wamn-q3n.3): that is the system database, owned by wamn_system. It holds
-- registry.org, registry.project, and registry.project_env.
-- THIS schema belongs to one project environment database. Its tables have no
-- row security. The catalog requires a protected caller identity. wamn_app's
-- WRITE authority is NOT uniform across these tables — see WRITE AUTHORITY
-- below. Different plane, different owner, different security model — hence a
-- different file, and deliberately NOT named system-schema.sql.
--
-- Project provisioning applies this schema after deploy/sql/catalog-schema.sql.
-- The restricted wamn_app role must already exist.
--
-- RECORD HISTORY: every table carries created_at, created_by, updated_at, and
-- updated_by as NOT NULL columns, and a wamn_record_history_stamp trigger that calls
-- wamn_history.stamp_row(). An applier installs deploy/sql/record-history.sql
-- first. CATALOG_SCHEMA_SQL carries it, so an applier that applies the catalog
-- schema first already has it.
-- The tables listed in HISTORY TABLES keep an unlimited log. Its wamn_record_history_log trigger
-- writes each row change to <table>_history.
-- The HISTORY TABLES section at the end creates them.
--
-- SECURITY SHAPE: one database per project environment separates these rows.
-- The account and configuration tables and their history tables have no project
-- column, row policy, or project-key index. Shared catalog and run tables retain
-- their existing project policies and use the authority functions below.
--
-- WRITE AUTHORITY (R11): grants control which relations a caller can read or write.
--
--   PLATFORM-PROTECTED — user, role, user_role, permission, env,
--   and the platform configuration tables.
--   SELECT only. The trust chain consumes these rows as authorization INPUT. Author SQL
--   holding DML here could write itself the credential its own policies are then
--   evaluated against, and the compiler would faithfully enforce the forgery. A
--   project's authorization state is written BY the platform, never by the
--   project's own SQL. There is no wamn_app writer to lose today: the write path
--   is platform-pending — 4.2 (wamn-0xd) resolves the identity from these rows and
--   the first-party identity authority (wamn-ctc8) mints them, both on a
--   platform identity, not on this grant. The application advances
--   number_counter through platform.bump_counter, platform.assign_counter and
--   platform.set_counter, the three routines it can EXECUTE in this schema.
--
--   WRITE LOG — write_log. INSERT, SELECT, UPDATE (result) and DELETE only.
--   The engine claims the key of every create, update, delete and command here. It is outside
--   the project base grants and the record history: see THE WRITE LOG below.
--
--   HISTORY: the <table>_history tables. No grant. The log trigger fires as
--   the writer, and wamn_app writes no table that has a history table.
--
-- TRUNCATE is not mentioned anywhere below because it was never granted to
-- wamn_app: only the table owner holds it.
--
-- IDENTITY INTEGRATION (3.5 / 4.2): `user.id` is a `uuid` — the ownership target
-- row policies read through wamn_authority.current_identity() and its accessors.
-- The host inserts the caller before application SQL runs.

CREATE SCHEMA IF NOT EXISTS platform AUTHORIZATION CURRENT_USER;

-- Provisioning installs the shared privilege roles before applying this schema.
GRANT USAGE ON SCHEMA platform TO wamn_app;

-- ---------------------------------------------------------------------------
-- User — application accounts. `id` (uuid) is org-issued at birth and has no
-- database default. Identity
-- only: no credential material lives here (auth is 4.2/8.1).
-- The host checks `active` when the account authenticates. Email is unique in
-- the database.
--
-- `type` names the type of principal: a user, a service (a station or an
-- integration), or a platform component. It has no default, so every insert
-- names its type. A platform row carries its `wamn:<component>` name in
-- `display_name` and the id that `wamn-identity-protocol` derives from that name.
-- `user_platform_principal_check` pins each (display_name, id) pair and
-- refuses a `wamn:` name on any other type. The schema seeds
-- the platform rows and the workflow service before installing their triggers.
--
-- The database is the org boundary (owner ruling of 2026-10-23 on
-- wamn-83eth.6), so the table has no project_id or row policy.
-- ---------------------------------------------------------------------------
CREATE TABLE platform."user" (
    id           uuid NOT NULL,
    issuer       text NOT NULL,
    sub          text NOT NULL,
    type         text NOT NULL,
    email        text NOT NULL,
    display_name text,
    active       boolean NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL,
    created_by   uuid NOT NULL,
    updated_at   timestamptz NOT NULL,
    updated_by   uuid NOT NULL,
    PRIMARY KEY (id),
    UNIQUE (issuer, sub),
    UNIQUE (email),
    CONSTRAINT user_type_check
        CHECK (type IN ('user', 'service', 'platform')),
    CONSTRAINT user_platform_principal_check
        CHECK (CASE WHEN type = 'platform'
                    THEN COALESCE((display_name, id) IN (
                             ('wamn:provisioning', '770df186-ac15-579e-b46b-c297cae2011b'::uuid),
                             ('wamn:apply-package', '7695180f-4b9a-581f-84ef-d7e9cdbd2b77'::uuid),
                             ('wamn:executor', 'd318d033-29ea-5cb0-ab56-24340413fbcc'::uuid),
                             ('wamn:audit-retention', '34cbd151-990e-5800-93be-4fba8878b943'::uuid)),
                         false)
                    ELSE display_name IS NULL OR display_name NOT LIKE 'wamn:%'
               END)
);
INSERT INTO platform."user"
    (id, issuer, sub, type, email, display_name, created_at, created_by, updated_at, updated_by)
SELECT id::uuid, CASE WHEN type = 'service' THEN 'wamn' ELSE 'wamn:platform' END,
       CASE WHEN type = 'service' THEN display_name ELSE id END, type, email, display_name,
       transaction_timestamp(), '770df186-ac15-579e-b46b-c297cae2011b'::uuid,
       transaction_timestamp(), '770df186-ac15-579e-b46b-c297cae2011b'::uuid
FROM (VALUES
    ('770df186-ac15-579e-b46b-c297cae2011b', 'platform', 'provisioning@platform.invalid', 'wamn:provisioning'),
    ('7695180f-4b9a-581f-84ef-d7e9cdbd2b77', 'platform', 'apply-package@platform.invalid', 'wamn:apply-package'),
    ('d318d033-29ea-5cb0-ab56-24340413fbcc', 'platform', 'executor@platform.invalid', 'wamn:executor'),
    ('34cbd151-990e-5800-93be-4fba8878b943', 'platform', 'audit-retention@platform.invalid', 'wamn:audit-retention'),
    ('42b638ce-4892-52a4-95a9-316c4c0e820d', 'service', 'workflow@platform.invalid', 'workflow')
) AS seed(id, type, email, display_name);

-- Install stamp keys now that the project principals exist.
ALTER TABLE platform."user"
    ADD CONSTRAINT user_created_by_fkey FOREIGN KEY (created_by)
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    ADD CONSTRAINT user_updated_by_fkey FOREIGN KEY (updated_by)
        REFERENCES platform."user" (id) ON DELETE RESTRICT;

-- The project applier creates the catalog tables before this file.
ALTER TABLE catalog.package
    ADD CONSTRAINT package_created_by_fkey FOREIGN KEY (created_by)
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    ADD CONSTRAINT package_updated_by_fkey FOREIGN KEY (updated_by)
        REFERENCES platform."user" (id) ON DELETE RESTRICT;
ALTER TABLE platform.credential
    ADD CONSTRAINT credential_created_by_fkey FOREIGN KEY (created_by)
        REFERENCES platform."user" (id) ON DELETE RESTRICT;
ALTER TABLE platform.oauth_authorization
    ADD CONSTRAINT oauth_authorization_created_by_fkey FOREIGN KEY (created_by)
        REFERENCES platform."user" (id) ON DELETE RESTRICT;
ALTER TABLE platform.connection_generation
    ADD CONSTRAINT connection_generation_created_by_fkey FOREIGN KEY (created_by)
        REFERENCES platform."user" (id) ON DELETE RESTRICT;
ALTER TABLE catalog.workflow
    ADD CONSTRAINT workflow_created_by_fkey FOREIGN KEY (created_by)
        REFERENCES platform."user" (id) ON DELETE RESTRICT;

CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform."user"
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
GRANT SELECT ON platform."user" TO wamn_app;

-- Only the platform login reads or writes PATs and their history.
CREATE TABLE platform.pat (
    id         uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id    uuid NOT NULL REFERENCES platform."user" (id) ON DELETE RESTRICT,
    prefix     text NOT NULL UNIQUE CHECK (prefix ~ '^[0-9a-f]{16}$'),
    hash       text NOT NULL CHECK (hash ~ '^sha256:[0-9a-f]{64}$'),
    label      text NOT NULL CHECK (btrim(label) <> '' AND octet_length(label) <= 200),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL
        CONSTRAINT pat_created_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
        CONSTRAINT pat_updated_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    CHECK (expires_at > created_at)
);
CREATE INDEX pat_user_idx ON platform.pat (user_id);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.pat
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');

-- ---------------------------------------------------------------------------
-- Roles — named roles. `admin` is the one built-in role:
-- apply-package writes it, and it holds every operation the serving release
-- serves with no permission rows. Every other role is authored. A user or
-- service holds a role through `wamn grant-role`. Only active roles grant
-- authority. Deactivation keeps assignments and permissions. `role_name_check`
-- enforces the role slug of identity (`wamn_identity_protocol::RoleName`).
-- ---------------------------------------------------------------------------
CREATE TABLE platform.role (
    name        text NOT NULL,
    description text,
    active      boolean NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL
        CONSTRAINT role_created_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL
        CONSTRAINT role_updated_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    PRIMARY KEY (name),
    CONSTRAINT role_name_check
        CHECK (name ~ '^[a-z0-9][a-z0-9-]*$' AND octet_length(name) <= 64)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.role
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
GRANT SELECT ON platform.role TO wamn_app;

-- ---------------------------------------------------------------------------
-- User↔role linkage (many-to-many). A grant prevents deletion of either
-- referenced row.
-- ---------------------------------------------------------------------------
CREATE TABLE platform.user_role (
    user_id    uuid NOT NULL,
    role_name  text NOT NULL,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL
        CONSTRAINT user_role_created_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
        CONSTRAINT user_role_updated_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    PRIMARY KEY (user_id, role_name),
    FOREIGN KEY (user_id)
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    FOREIGN KEY (role_name)
        REFERENCES platform.role (name) ON DELETE RESTRICT
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.user_role
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
GRANT SELECT ON platform.user_role TO wamn_app;

-- ---------------------------------------------------------------------------
-- Permissions — the operations an authored role holds
-- (docs/plan/trx-pkg-plan.md, Authorization). `permission` is a stable
-- operation reference `<package>:<interface>/<operation>`, with no package
-- version. A caller holds exactly the rows of its roles; nothing derives
-- into them. A row that the serving release does not serve grants nothing.
-- `admin` has no rows. A permission prevents deletion of its role.
-- ---------------------------------------------------------------------------
CREATE TABLE platform.permission (
    role_name   text NOT NULL,
    permission  text NOT NULL,
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL
        CONSTRAINT permission_created_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL
        CONSTRAINT permission_updated_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    PRIMARY KEY (role_name, permission),
    FOREIGN KEY (role_name)
        REFERENCES platform.role (name) ON DELETE RESTRICT,
    CONSTRAINT permission_admin_check CHECK (role_name <> 'admin'),
    CONSTRAINT permission_reference_check
        CHECK (permission ~ '^[a-z][a-z0-9]*(-[a-z0-9]+)*:[a-z][a-z0-9]*(-[a-z0-9]+)*/[a-z][a-z0-9]*(-[a-z0-9]+)*$'
            OR permission ~ '^wamn-workflow:[A-Za-z0-9_.~-]+/[A-Za-z0-9_.~-]+$')
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.permission
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
GRANT SELECT ON platform.permission TO wamn_app;

-- ---------------------------------------------------------------------------
-- Admission mirror — active when both the project and its env are active.
-- Control writes this row through the project administration login.
-- The host reads it as wamn_app. No row means active.
-- ---------------------------------------------------------------------------
CREATE TABLE platform.env (
    id         boolean NOT NULL DEFAULT true CHECK (id),
    active     boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL
        CONSTRAINT env_created_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
        CONSTRAINT env_updated_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    PRIMARY KEY (id)
);
CREATE TRIGGER wamn_record_history_stamp
    BEFORE INSERT OR UPDATE ON platform.env
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.stamp_row('created_at', 'created_by', 'updated_at', 'updated_by');
GRANT SELECT ON platform.env TO wamn_app;

-- ---------------------------------------------------------------------------
-- HISTORY TABLES. The database separates environments, so these tables have
-- no project column or row policy. Each log keeps unlimited history.
-- ---------------------------------------------------------------------------
DO $history_tables$ BEGIN
  PERFORM wamn_history.create_history_table('platform', 'user', false);
  PERFORM wamn_history.create_history_table('platform', relation, false)
     FROM unnest(ARRAY['role', 'user_role', 'permission', 'env', 'pat']) AS relation;
END $history_tables$;

CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform."user"
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.role
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.user_role
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.permission
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.env
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

CREATE TRIGGER wamn_record_history_log
    AFTER INSERT OR UPDATE OR DELETE ON platform.pat
    FOR EACH ROW
    EXECUTE FUNCTION wamn_history.log_row_change('unlimited');

-- ---------------------------------------------------------------------------
-- THE WRITE LOG (`docs/architecture/data-access.md#inputs-queries-and-transactions`, wamn-z7uv). One idempotency record
-- for every create and command in this database. For an operation with SQL,
-- the engine inserts the claim in the item's host transaction, the guest does
-- the work in it, and the engine stores the result and commits, so the row
-- commits with its result. A refused item rolls its claim back, so it leaves
-- no row. An operation without SQL claims in two steps
-- (`docs/plan/host-transaction.md` 4.3): the claim commits with a null
-- `result`, the finish stores it, and a refused item deletes its claim. A
-- committed row with a null `result` is a claim that began and never
-- finished, which is uncertain.
--
-- The primary key is the operation and the key. `operation` is the contract
-- operation without its @version, so a retry across a release answers the
-- stored result, and two packages in one database do not collide.
--
-- The table is outside the project base grants and the record history on purpose.
-- The database is the project, so it has no project_id and no row policy. The
-- rows that the work writes carry the actor, so it has no user columns and no
-- stamp or log trigger. wamn_app inserts a claim, reads a claim, writes the
-- result and deletes a refused two-step claim, and changes no other column.
--
-- IF NOT EXISTS, so that this section runs again on a database that has it.
-- ---------------------------------------------------------------------------
CREATE TABLE IF NOT EXISTS platform.write_log (
    operation       text NOT NULL CHECK (operation <> ''),
    idempotency_key text NOT NULL CHECK (idempotency_key <> ''),
    request         bytea NOT NULL CHECK (octet_length(request) > 0),
    result          text,
    created_at      timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT write_log_pkey PRIMARY KEY (operation, idempotency_key)
);
CREATE INDEX IF NOT EXISTS write_log_created_at_idx ON platform.write_log (created_at);
GRANT SELECT, INSERT ON platform.write_log TO wamn_app;
GRANT UPDATE (result) ON platform.write_log TO wamn_app;
GRANT DELETE ON platform.write_log TO wamn_app;

-- ---------------------------------------------------------------------------
-- PLATFORM CONFIGURATION (docs/plan/platform-config.md, Tables and Numbering).
-- The platform writes these rows through its routes and the host, and the
-- application reads them: wamn_app holds SELECT on each table and EXECUTE on
-- the three counter functions of NUMBER COUNTERS below, and nothing else. An
-- integration is connector code, named here by its catalog code. A connection
-- is a configured instance of an integration. An external system is the
-- persistent identity that a group of connections reach. Credentials and
-- targets stay in the control plane, so no table here holds them.
--
-- The database is the org boundary (owner ruling of 2026-10-23 on
-- wamn-83eth.6), so these tables have no project_id or row policy.
-- Each table has the record history of the tables above.
-- number_counter is the one exception: it has no history table and no log
-- trigger, because one history entry per bump would double the writes of
-- every create, and the write log is the audit (owner ruling, wamn-aqd0m.1).
--
-- Org and project ids are the text slugs of the system database, so target_id
-- and project_id are text. A role target names a role by name, and an
-- application target names an application by name.
--
-- ---------------------------------------------------------------------------
-- File objects belong to one application and use the active storage generation.
CREATE TABLE platform.file (
    id                    uuid NOT NULL DEFAULT uuidv7(),
    app                   text NOT NULL CHECK (app <> ''),
    connection_id         uuid NOT NULL REFERENCES platform.connection (id),
    name                  text NOT NULL CHECK (name <> ''),
    media_type            text NOT NULL CHECK (media_type <> ''),
    size                  bigint NOT NULL CHECK (size >= 0),
    sha256                text NOT NULL CHECK (sha256 ~ '^sha256:[0-9a-f]{64}$'),
    blob_key              text NOT NULL UNIQUE,
    exposure              text NOT NULL DEFAULT 'app'
                              CHECK (exposure IN ('private', 'app', 'public')),
    url                   text,
    width                 integer CHECK (width > 0),
    height                integer CHECK (height > 0),
    created_at            timestamptz NOT NULL,
    created_by            uuid NOT NULL
        CONSTRAINT file_created_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    updated_at            timestamptz NOT NULL,
    updated_by            uuid NOT NULL
        CONSTRAINT file_updated_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    PRIMARY KEY (id),
    CHECK (blob_key = '_wamn_files/live/' || id::text
        OR blob_key ~ ('^[a-z0-9-]+/_wamn_files/live/' || id::text || '$')),
    CHECK ((exposure = 'public') = (url IS NOT NULL)),
    CHECK (url IS NULL OR url <> '')
);

CREATE FUNCTION platform.require_file_store() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog AS $$
BEGIN
    IF NOT EXISTS (
        SELECT FROM platform.connection
        WHERE id = NEW.connection_id AND active AND integration_type IN ('gcs', 's3', 'disk')
    ) THEN
        RAISE EXCEPTION 'file_connection_requires_host_store' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION platform.require_file_store() FROM PUBLIC;
CREATE TRIGGER file_store BEFORE INSERT OR UPDATE OF connection_id ON platform.file
    FOR EACH ROW EXECUTE FUNCTION platform.require_file_store();

-- One document number sequence of one application, and its pattern.
CREATE TABLE platform.number_sequence (
    id          uuid NOT NULL DEFAULT uuidv7(),
    app         text NOT NULL CHECK (app <> ''),
    name        text NOT NULL CHECK (name <> ''),
    pattern     text NOT NULL CHECK (pattern <> ''),
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL
        CONSTRAINT number_sequence_created_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL
        CONSTRAINT number_sequence_updated_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    PRIMARY KEY (id),
    UNIQUE (app, name)
);

-- One counter of a sequence for one key value. `value` is the next value to
-- render. The '' key value is the allocator row of a per= counter.
CREATE TABLE platform.number_counter (
    sequence_id uuid NOT NULL,
    counter     text NOT NULL CHECK (counter <> ''),
    key_value   text NOT NULL,
    value       bigint NOT NULL CHECK (value >= 1),
    created_at  timestamptz NOT NULL,
    created_by  uuid NOT NULL
        CONSTRAINT number_counter_created_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    updated_at  timestamptz NOT NULL,
    updated_by  uuid NOT NULL
        CONSTRAINT number_counter_updated_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    PRIMARY KEY (sequence_id, counter, key_value),
    FOREIGN KEY (sequence_id)
        REFERENCES platform.number_sequence (id) ON DELETE RESTRICT
);

-- Display and view settings, resolved from least to most specific target. A
-- locked row stops overrides at more specific targets.
CREATE TABLE platform.setting (
    target_type text NOT NULL,
    target_id   text NOT NULL CHECK (target_id <> ''),
    key        text NOT NULL CHECK (key <> ''),
    value      jsonb NOT NULL,
    locked     boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL,
    created_by uuid NOT NULL
        CONSTRAINT setting_created_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    updated_at timestamptz NOT NULL,
    updated_by uuid NOT NULL
        CONSTRAINT setting_updated_by_fkey
        REFERENCES platform."user" (id) ON DELETE RESTRICT,
    PRIMARY KEY (target_type, target_id, key),
    CONSTRAINT setting_target_type_check
        CHECK (target_type IN ('org', 'project', 'app', 'role', 'user'))
);

GRANT SELECT ON platform.external_system, platform.connection TO wamn_app;

-- The stamp trigger of each table, and the SELECT of wamn_app.
DO $platform_configuration$
DECLARE relation text;
BEGIN
  FOREACH relation IN ARRAY ARRAY['file', 'number_sequence',
                                  'number_counter', 'setting'] LOOP
    EXECUTE format('CREATE TRIGGER wamn_record_history_stamp BEFORE INSERT OR UPDATE ON platform.%I '
                   'FOR EACH ROW EXECUTE FUNCTION '
                   'wamn_history.stamp_row(''created_at'', ''created_by'', ''updated_at'', ''updated_by'')',
                   relation);
    EXECUTE format('GRANT SELECT ON platform.%I TO wamn_app', relation);
  END LOOP;
END $platform_configuration$;

-- The history table of each table except number_counter, with no project_id.
DO $platform_configuration_history$
DECLARE relation text;
BEGIN
  FOREACH relation IN ARRAY ARRAY['file', 'number_sequence', 'setting'] LOOP
    PERFORM wamn_history.create_history_table('platform', relation, false);
    EXECUTE format('CREATE TRIGGER wamn_record_history_log AFTER INSERT OR UPDATE OR DELETE '
                   'ON platform.%I FOR EACH ROW EXECUTE FUNCTION '
                   'wamn_history.log_row_change(''unlimited'')',
                   relation);
  END LOOP;
END $platform_configuration_history$;

-- PostgreSQL owns timezone syntax, including abbreviations and POSIX expressions.
-- Report an invalid zone without aborting the caller's operation transaction.
CREATE OR REPLACE FUNCTION platform.valid_timezone(zone text)
RETURNS boolean
LANGUAGE plpgsql STABLE STRICT
SET search_path = pg_catalog
AS $$
BEGIN
    PERFORM transaction_timestamp() AT TIME ZONE zone;
    RETURN true;
EXCEPTION WHEN invalid_parameter_value THEN
    RETURN false;
END;
$$;
REVOKE ALL ON FUNCTION platform.valid_timezone(text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.valid_timezone(text) TO wamn_app;

-- ---------------------------------------------------------------------------
-- NUMBER COUNTERS (docs/plan/platform-config.md, Numbering and
-- Administration; owner rulings of wamn-83eth.2, wamn-83eth.3 and
-- wamn-83eth.6). The three platform functions that write number_counter.
-- Each runs as its owner, the applier, so that wamn_app needs no write on
-- number_counter. None knows a pattern or parses one; the guest renderer
-- owns pattern parsing.
--
-- bump_counter advances one counter and returns the value to render: it
-- locks the counter row, inserts it at 1 when it is absent, returns its value
-- and increments it.
--
-- assign_counter gives a per= counter one value for each distinct key: under
-- a transaction advisory lock on the (sequence, counter, key), it returns the
-- value already assigned to the key, or allocates the next value from the
-- counter's '' allocator row and inserts the key row with it. Concurrent
-- first callers of one key wait on the lock, so the allocator advances once
-- for each key, and a rollback gives the value back. The '' key value is the
-- allocator itself, so it is refused with invalid_parameter_value.
--
-- set_counter is the write of the set_counter host route: under the counter
-- row's lock, it inserts the row of (counter, key) with the value, or sets
-- the value of the row that exists. The value is the next value to render. A
-- value below 1 is refused with invalid_parameter_value. The route refuses a
-- per= counter. Neither reads an application table: a set that gives a
-- number a record already carries is caught by that model's UQ.
--
-- The database is the org boundary, so no function tests a project. No
-- function reads the application from the transaction caller.
-- Each takes a sequence id and refuses an id that is not a sequence with
-- undefined_object. The guest renderer resolves the sequence id from
-- wamn_authority.current_app() and the sequence name. It calls these
-- functions through SQL in the host operation transaction.
--
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION platform.bump_counter(target_sequence uuid, target_counter text,
                                                 target_key text)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
DECLARE
    allocated bigint;
BEGIN
    IF NOT EXISTS (SELECT FROM platform.number_sequence AS s WHERE s.id = target_sequence) THEN
        RAISE EXCEPTION 'number sequence % does not exist', target_sequence
            USING ERRCODE = 'undefined_object';
    END IF;
    INSERT INTO platform.number_counter AS c (sequence_id, counter, key_value, value)
    VALUES (target_sequence, target_counter, target_key, 2)
    ON CONFLICT (sequence_id, counter, key_value)
    DO UPDATE SET value = c.value + 1
    RETURNING c.value - 1 INTO allocated;
    RETURN allocated;
END;
$$;
REVOKE ALL ON FUNCTION platform.bump_counter(uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.bump_counter(uuid, text, text) TO wamn_app;

CREATE OR REPLACE FUNCTION platform.assign_counter(target_sequence uuid, target_counter text,
                                                   target_key text)
RETURNS bigint
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
DECLARE
    assigned bigint;
BEGIN
    IF NOT EXISTS (SELECT FROM platform.number_sequence AS s WHERE s.id = target_sequence) THEN
        RAISE EXCEPTION 'number sequence % does not exist', target_sequence
            USING ERRCODE = 'undefined_object';
    END IF;
    IF target_key = '' THEN
        RAISE EXCEPTION 'the '''' key value is the allocator row of counter %', target_counter
            USING ERRCODE = 'invalid_parameter_value';
    END IF;
    PERFORM pg_advisory_xact_lock(hashtextextended(
        format('number_counter %s %s %s', target_sequence, target_counter, target_key), 0));
    SELECT c.value INTO assigned
      FROM platform.number_counter AS c
     WHERE c.sequence_id = target_sequence
       AND c.counter = target_counter
       AND c.key_value = target_key;
    IF assigned IS NOT NULL THEN
        RETURN assigned;
    END IF;
    INSERT INTO platform.number_counter AS c (sequence_id, counter, key_value, value)
    VALUES (target_sequence, target_counter, '', 2)
    ON CONFLICT (sequence_id, counter, key_value)
    DO UPDATE SET value = c.value + 1
    RETURNING c.value - 1 INTO assigned;
    INSERT INTO platform.number_counter (sequence_id, counter, key_value, value)
    VALUES (target_sequence, target_counter, target_key, assigned);
    RETURN assigned;
END;
$$;
REVOKE ALL ON FUNCTION platform.assign_counter(uuid, text, text) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.assign_counter(uuid, text, text) TO wamn_app;

CREATE OR REPLACE FUNCTION platform.set_counter(target_sequence uuid, target_counter text,
                                                target_key text, target_value bigint)
RETURNS void
LANGUAGE plpgsql
SECURITY DEFINER
SET search_path = pg_catalog
AS $$
BEGIN
    IF NOT EXISTS (SELECT FROM platform.number_sequence AS s WHERE s.id = target_sequence) THEN
        RAISE EXCEPTION 'number sequence % does not exist', target_sequence
            USING ERRCODE = 'undefined_object';
    END IF;
    IF target_value IS NULL OR target_value < 1 THEN
        RAISE EXCEPTION 'counter % takes a value of 1 or more, not %', target_counter, target_value
            USING ERRCODE = 'invalid_parameter_value';
    END IF;
    INSERT INTO platform.number_counter AS c (sequence_id, counter, key_value, value)
    VALUES (target_sequence, target_counter, target_key, target_value)
    ON CONFLICT (sequence_id, counter, key_value)
    DO UPDATE SET value = EXCLUDED.value;
END;
$$;
REVOKE ALL ON FUNCTION platform.set_counter(uuid, text, text, bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION platform.set_counter(uuid, text, text, bigint) TO wamn_app;

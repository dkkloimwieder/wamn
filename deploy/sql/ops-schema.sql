-- Operations persistence schema 0.1.
-- schema_version: 0.1
--
-- Apply this artifact after deploy/sql/system-schema.sql. It extends the T1
-- control database with operator-only state and depends in one direction on
-- the core registry identity. The core schema never references these tables.
-- Re-applying this artifact is a no-op.

-- Logical-dump metadata. Dump bytes remain in object storage; this row only
-- records the immutable project-env identity and object key used by restore.
CREATE TABLE IF NOT EXISTS provisioning.dumps (
    org         text NOT NULL,
    project     text NOT NULL,
    env         text NOT NULL,
    object_key  text NOT NULL,
    format      text NOT NULL DEFAULT 'directory',
    byte_size   bigint,
    taken_at    timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org, project, env, object_key),
    FOREIGN KEY (org, project, env)
        REFERENCES registry.project_envs (org, project, env) ON DELETE CASCADE,
    CONSTRAINT dumps_format_check CHECK (format = 'directory')
);

-- Durable state for the operations-only copy pipeline. It is deliberately
-- separate from core provisioning sagas; the fixed kind makes a copy row
-- impossible to reinterpret as an ordinary provisioning operation.
CREATE TABLE IF NOT EXISTS provisioning.copy_sagas (
    saga_id     text PRIMARY KEY,
    kind        text NOT NULL,
    target      text NOT NULL,
    status      text NOT NULL DEFAULT 'pending',
    step        int  NOT NULL DEFAULT 0,
    total_steps int,
    last_error  text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT copy_sagas_kind_check CHECK (kind = 'copy'),
    CONSTRAINT copy_sagas_status_check
        CHECK (status IN ('pending', 'running', 'completed', 'failed',
                          'compensating', 'compensated')),
    CONSTRAINT copy_sagas_step_nonneg CHECK (step >= 0)
);

-- Append-only operator attestations for destructive migration preparation.
-- The project-env FK is the only physical cross-plane reference: catalog
-- migration identity lives in the project database and is repeated here as
-- identity facts, never reached through a cross-database FK. `confirmed_by`
-- comes from the authenticated database session, never caller-supplied text.
CREATE TABLE IF NOT EXISTS provisioning.migration_confirmations (
    org          text NOT NULL,
    project      text NOT NULL,
    env          text NOT NULL,
    tenant_id    text NOT NULL CHECK (tenant_id <> ''),
    catalog_id   text NOT NULL,
    from_version int,
    to_version   int  NOT NULL,
    kind          text NOT NULL DEFAULT 'backup-checkpoint-attested',
    confirmed_at timestamptz NOT NULL DEFAULT now(),
    confirmed_by text NOT NULL DEFAULT session_user,
    PRIMARY KEY (org, project, env, tenant_id, catalog_id, to_version),
    FOREIGN KEY (org, project, env)
        REFERENCES registry.project_envs (org, project, env) ON DELETE CASCADE,
    CONSTRAINT migration_confirmations_kind_check
        CHECK (kind = 'backup-checkpoint-attested')
);

-- Tables stay owned by the core storage owner. The stable NOLOGIN operations
-- role is created/hardened by the invoking command before this artifact runs.
-- Explicit replacement prevents inherited/default ACL drift from expanding it.
REVOKE ALL PRIVILEGES ON SCHEMA provisioning FROM PUBLIC, wamn_ops;
GRANT USAGE ON SCHEMA provisioning TO wamn_ops;

REVOKE ALL PRIVILEGES ON provisioning.dumps FROM PUBLIC, wamn_ops;
GRANT SELECT, INSERT, UPDATE ON provisioning.dumps TO wamn_ops;

REVOKE ALL PRIVILEGES ON provisioning.copy_sagas FROM PUBLIC, wamn_ops;
GRANT SELECT, INSERT, UPDATE ON provisioning.copy_sagas TO wamn_ops;

REVOKE ALL PRIVILEGES ON provisioning.migration_confirmations FROM PUBLIC, wamn_ops;
REVOKE ALL PRIVILEGES (
    org, project, env, tenant_id, catalog_id, from_version, to_version,
    kind, confirmed_at, confirmed_by
) ON provisioning.migration_confirmations FROM PUBLIC, wamn_ops;
GRANT SELECT ON provisioning.migration_confirmations TO wamn_ops;
GRANT INSERT (org, project, env, tenant_id, catalog_id, from_version, to_version)
    ON provisioning.migration_confirmations TO wamn_ops;

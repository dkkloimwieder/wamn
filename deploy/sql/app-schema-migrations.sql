
-- The platform migrations that this project database holds
-- (docs/plan/schema-upgrade.md). reconcile-run-plane records every file of
-- deploy/sql/migrations/project/ here when it installs app-schema.sql, because
-- the full schema files already hold them. upgrade-schema applies and records
-- the rest. The first run of upgrade-schema on a database installed before
-- this table runs this block. wamn_app has no privilege on it.
CREATE TABLE app_system.schema_migrations (
    ordinal integer PRIMARY KEY CHECK (ordinal > 0),
    relative_path text NOT NULL UNIQUE
        CHECK (relative_path ~ '^migrations/project/[0-9]{4}_[a-z0-9_]+\.sql$'),
    sha256 text NOT NULL CHECK (sha256 ~ '^sha256:[0-9a-f]{64}$'),
    applied_at timestamptz NOT NULL DEFAULT now(),
    CHECK (substring(relative_path FROM '^migrations/project/([0-9]{4})_')::integer = ordinal)
);

-- Platform migration records. The full schema files own fresh installations.
-- The migration lists stay empty until development is stable.
CREATE TABLE platform.schema_migrations (
    ordinal integer PRIMARY KEY CHECK (ordinal > 0),
    relative_path text NOT NULL UNIQUE
        CHECK (relative_path ~ '^migrations/project/[0-9]{4}_[a-z0-9_]+\.sql$'),
    sha256 text NOT NULL CHECK (sha256 ~ '^sha256:[0-9a-f]{64}$'),
    applied_at timestamptz NOT NULL DEFAULT now(),
    CHECK (substring(relative_path FROM '^migrations/project/([0-9]{4})_')::integer = ordinal)
);

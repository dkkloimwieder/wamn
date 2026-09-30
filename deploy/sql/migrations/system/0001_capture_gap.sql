-- The capture gap table of wamn-59z6 and the read grant of the CDC reader.
-- docs/operations/gcp.md section 7 records it as applied by hand to
-- wamn-dev on 2026-09-29, so wamn-dev takes it with --baseline 1.
CREATE TABLE registry.capture_gap (
    org        text NOT NULL,
    project    text NOT NULL,
    env        text NOT NULL,
    slot       text NOT NULL,
    start_lsn  pg_lsn,
    start_at   timestamptz NOT NULL,
    reason     text NOT NULL,
    end_lsn    pg_lsn NOT NULL,
    resync_at  timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org, project, env, created_at),
    FOREIGN KEY (org, project, env)
        REFERENCES registry.event_readers (org, project, env) ON DELETE CASCADE
);
GRANT SELECT ON TABLE registry.capture_gap TO wamn_registry_reader;

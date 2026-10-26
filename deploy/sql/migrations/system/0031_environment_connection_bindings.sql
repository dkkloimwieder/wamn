-- Applied names and target identity, separate from immutable runtime generations.
ALTER TABLE registry.project_envs ADD COLUMN target_id uuid REFERENCES registry.target(id);
CREATE FUNCTION registry.keep_environment_target() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.target_id IS NOT NULL AND NEW.target_id IS DISTINCT FROM OLD.target_id THEN
        RAISE EXCEPTION 'environment_target_changed' USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END $$;
REVOKE ALL ON FUNCTION registry.keep_environment_target() FROM PUBLIC;
CREATE TRIGGER environment_target_immutable BEFORE UPDATE OF target_id ON registry.project_envs
    FOR EACH ROW EXECUTE FUNCTION registry.keep_environment_target();
CREATE TABLE registry.environment_connection_binding (
    org text NOT NULL,
    project text NOT NULL,
    env text NOT NULL,
    application text NOT NULL,
    alias text NOT NULL,
    target_id uuid NOT NULL REFERENCES registry.target(id),
    connection_id uuid NOT NULL,
    generation bigint NOT NULL,
    mode text NOT NULL CHECK (mode IN ('read', 'write')),
    exclusive boolean NOT NULL,
    PRIMARY KEY (org, project, env, application, alias),
    FOREIGN KEY (org, project, env) REFERENCES registry.project_envs(org, project, env) ON DELETE CASCADE,
    FOREIGN KEY (org, connection_id) REFERENCES registry.connection(org, id),
    FOREIGN KEY (connection_id, generation) REFERENCES registry.connection_generation(connection_id, generation)
);
CREATE UNIQUE INDEX exclusive_connection_target ON registry.environment_connection_binding(target_id, connection_id) WHERE exclusive;

DO $reader_grant$ BEGIN
    IF EXISTS (SELECT FROM pg_roles WHERE rolname = 'wamn_conn_reader') THEN
        GRANT SELECT ON registry.project_connection TO wamn_conn_reader;
    END IF;
END $reader_grant$;

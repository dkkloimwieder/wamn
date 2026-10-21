SET LOCAL ROLE wamn_system;

CREATE TABLE registry.org_connections (
    org text NOT NULL REFERENCES registry.orgs (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (name ~ '^[a-z][a-z0-9_-]{0,62}$'),
    type text NOT NULL CHECK (type = 'http'),
    definition jsonb NOT NULL CHECK (jsonb_typeof(definition) = 'object'),
    PRIMARY KEY (org, name)
);


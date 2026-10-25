-- System-owned connector catalog and deployment target identities (C3.1).
CREATE TABLE registry.integrations (
    code text PRIMARY KEY,
    form text NOT NULL CHECK (form IN ('host', 'wasm', 'native')),
    exclusive boolean NOT NULL
);
INSERT INTO registry.integrations (code, form, exclusive)
VALUES ('http', 'host', false), ('gcs', 'host', false), ('s3', 'host', false);

CREATE TABLE registry.target (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    name text NOT NULL UNIQUE,
    type text NOT NULL CHECK (type IN ('local', 'cloud')),
    status text NOT NULL,
    last_seen_at timestamptz
);
INSERT INTO registry.target (name, type, status) VALUES ('cloud', 'cloud', 'active');

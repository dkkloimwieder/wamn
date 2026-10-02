-- The password fingerprints of the credential generations (wamn-zua8.3,
-- docs/plan/platform-ui.md section 4.10). PostgreSQL shows a password only to a
-- superuser, and the provisioner is not one. A prepare records the SHA-256 of
-- the password that it sets, and a retirement removes the row. A generation is
-- active only with a row. The password itself is stored nowhere.
CREATE TABLE registry.generation_passwords (
    role        text PRIMARY KEY,
    sha256      text NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    recorded_at timestamptz NOT NULL DEFAULT now()
);

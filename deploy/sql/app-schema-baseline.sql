CREATE TABLE platform.schema_baseline (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    sha256 text NOT NULL CHECK (sha256 ~ '^sha256:[0-9a-f]{64}$'),
    installed_at timestamptz NOT NULL DEFAULT now()
);

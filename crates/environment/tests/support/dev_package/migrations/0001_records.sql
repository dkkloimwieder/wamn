CREATE SCHEMA platform_fixture;
CREATE TABLE platform_fixture.record (
    id uuid PRIMARY KEY,
    state text NOT NULL,
    created_at timestamptz NOT NULL
);
CREATE TABLE platform_fixture.location (id uuid PRIMARY KEY);

//! Registry SQL used by database boundary tests.

/// Database fixture statement for provision.
pub fn seed_demo_org_sql() -> &'static str {
    "INSERT INTO registry.orgs (id) VALUES ('demo');"
}

/// Database fixture statement for provision.
pub fn disposable_environment_sql() -> &'static str {
    "SELECT disposable FROM registry.environments WHERE org='demo' AND env='dev'"
}

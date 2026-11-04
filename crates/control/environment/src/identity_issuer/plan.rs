use super::{IDENTITY_ISSUER_DATABASE, IDENTITY_ISSUER_ROLE};

#[expect(
    clippy::struct_excessive_bools,
    reason = "role facts mirror PostgreSQL attributes"
)]
pub(super) struct RoleState {
    pub(super) rolsuper: bool,
    pub(super) rolcreaterole: bool,
    pub(super) rolcreatedb: bool,
    pub(super) rolreplication: bool,
    pub(super) rolbypassrls: bool,
    pub(super) owned_objects: i64,
    pub(super) rolcanlogin: bool,
    pub(super) rolinherit: bool,
    pub(super) password_set: bool,
    pub(super) valid_until_finite: bool,
    pub(super) memberships: Vec<String>,
    pub(super) membership_options_exact: bool,
    pub(super) member_roles: Vec<String>,
    pub(super) connect_databases: Vec<String>,
    pub(super) valid_until: Option<String>,
    pub(super) sessions: i64,
}

pub(super) fn restricted(row: &RoleState) -> bool {
    [
        row.rolsuper,
        row.rolcreaterole,
        row.rolcreatedb,
        row.rolreplication,
        row.rolbypassrls,
    ]
    .into_iter()
    .all(|enabled| !enabled)
        && row.owned_objects == 0
}

pub(super) fn active(row: &RoleState) -> bool {
    restricted(row)
        && row.rolcanlogin
        && row.rolinherit
        && row.password_set
        && row.valid_until_finite
        && row.memberships == [IDENTITY_ISSUER_ROLE]
        && row.membership_options_exact
        && row.member_roles.is_empty()
        && row.connect_databases == [IDENTITY_ISSUER_DATABASE]
}

pub(super) fn inactive(row: &RoleState) -> bool {
    restricted(row)
        && !row.rolcanlogin
        && row.rolinherit
        && !row.password_set
        && row.valid_until.as_deref() == Some("1970-01-01T00:00:00Z")
        && row.memberships.is_empty()
        && row.member_roles.is_empty()
        && row.connect_databases.is_empty()
        && row.sessions == 0
}

use super::escape_identifier;
use wamn_role::APP_ROLE;

/// Create the passwordless privilege roles before CNPG creates the database.
pub fn role_sql() -> String {
    role_posture_sql()
}

/// Role creation and hardening half of [`role_sql`].
pub fn role_posture_sql() -> String {
    include_str!("../../../../deploy/sql/project-roles.sql").to_owned()
}

/// Keep database CONNECT privileges on individual logins. Shared ACL roles cannot connect.
pub fn privilege_sql(database: &str) -> String {
    let db = escape_identifier(database);
    format!(
        "REVOKE CONNECT, TEMPORARY ON DATABASE {db} FROM PUBLIC; \
         REVOKE CONNECT ON DATABASE {db} FROM {app};\n",
        app = escape_identifier(APP_ROLE),
    )
}

use postgres_protocol::escape::escape_identifier;
use wamn_role::APP_ROLE;

pub fn role_posture_sql() -> String {
    wamn_provision::sql::ensure_project_roles_sql().to_owned()
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

use super::{APP_ROLE, escape_identifier, sql};

/// Create the passwordless privilege roles before CNPG creates the database.
pub fn role_sql() -> String {
    role_posture_sql()
}

/// Role creation and hardening half of [`role_sql`].
pub fn role_posture_sql() -> String {
    format!(
        "{app}\n{query}\n{owner}\n",
        app = sql::ensure_app_acl_role_sql(),
        query =
            sql::ensure_workload_acl_role_sql(wamn_control_provision::WorkloadRoleFamily::Query),
        owner = sql::ensure_db_owner_role_sql(),
    )
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

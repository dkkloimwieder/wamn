//! Authorization reads owned by authentication, over a host-authorized connection.

use sqlx::Row as _;
use wamn_capability_authority::auth_cache::PrincipalGrants;

/// The status row of the environment, which `wamn_app` reads under its
/// database boundary.
const ENVIRONMENT_INACTIVE_SQL: &str = "SELECT status = 'inactive' FROM platform.environment";

/// The user row of one principal, each role it holds and each permission
/// of that role, in one read. No row means no user row. A role without
/// permissions, such as `admin`, yields one row with a null permission, and a
/// principal without roles yields one row with a null role.
const PRINCIPAL_GRANTS_SQL: &str = "SELECT u.status = 'active', r.role_name, p.permission, u.type = 'user' \
    FROM platform.\"user\" u LEFT JOIN platform.user_role r \
    ON r.user_id = u.id \
    LEFT JOIN platform.permission p \
    ON p.role_name = r.role_name \
    WHERE u.id = $1::text::uuid";

const USER_OPERATION_PERMISSIONS_SQL: &str = "SELECT user_role.role_name, permission.permission \
    FROM platform.\"user\" AS \"user\" \
    JOIN platform.user_role AS user_role \
      ON user_role.user_id = \"user\".id \
    LEFT JOIN platform.permission AS permission \
      ON permission.role_name = user_role.role_name \
    WHERE \"user\".id = $1::text::uuid AND \"user\".status = 'active'";

fn principal_grants_from_rows(
    rows: &[sqlx::postgres::PgRow],
) -> Result<wamn_capability_authority::auth_cache::PrincipalGrants, sqlx::Error> {
    let mut grants = wamn_capability_authority::auth_cache::PrincipalGrants {
        present: !rows.is_empty(),
        ..Default::default()
    };
    for row in rows {
        grants.active = row.try_get(0)?;
        grants.user = row.try_get(3)?;
        if let Some(role) = row.try_get::<Option<String>, _>(1)? {
            let permissions = grants.roles.entry(role).or_default();
            permissions.extend(row.try_get::<Option<String>, _>(2)?);
        }
    }
    Ok(grants)
}

/// Read the current user, roles and permissions without choosing the transaction identity.
pub async fn principal_grants(
    connection: &mut sqlx::PgConnection,
    user_id: &str,
) -> Result<PrincipalGrants, sqlx::Error> {
    let rows = sqlx::query(PRINCIPAL_GRANTS_SQL)
        .bind(user_id)
        .fetch_all(connection)
        .await?;
    principal_grants_from_rows(&rows)
}

/// Read the active user's role and permission rows for a host route.
pub async fn user_operation_permissions(
    connection: &mut sqlx::PgConnection,
    user_id: &str,
) -> Result<Vec<sqlx::postgres::PgRow>, sqlx::Error> {
    sqlx::query(USER_OPERATION_PERMISSIONS_SQL)
        .bind(user_id)
        .fetch_all(connection)
        .await
}

/// An absent environment row remains active.
pub async fn environment_inactive(
    connection: &mut sqlx::PgConnection,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query_scalar(ENVIRONMENT_INACTIVE_SQL)
        .fetch_optional(connection)
        .await?
        .unwrap_or(false))
}

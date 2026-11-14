//! Applied SQL and grants observed after development deployment.
use anyhow::Context as _;
use sqlx::{PgConnection as Client, Row as _};
use std::collections::BTreeSet;
const ADMIN_ROLE: &str = "admin";

/// Applying the packages leaves the built-in `admin` role and no permission
/// row: `admin` holds every served operation without rows, and no other role is
/// authored yet (docs/architecture/execution.md#host-run-routes).
pub(super) async fn verify_journey_operation_grants(project: &mut Client) -> anyhow::Result<()> {
    let roles = sqlx::query("SELECT name FROM platform.role ORDER BY name COLLATE \"C\"")
        .fetch_all(&mut *project)
        .await
        .context("read the installed roles")?
        .into_iter()
        .map(|row| row.get::<String, _>(0))
        .collect::<Vec<_>>();
    let permissions: i64 = sqlx::query("SELECT count(*) FROM platform.permission")
        .fetch_one(&mut *project)
        .await
        .context("count the installed permission rows")?
        .get(0);
    anyhow::ensure!(
        roles == [ADMIN_ROLE] && permissions == 0,
        "installed packages left roles {roles:?} and {permissions} permission rows, \
         not admin alone without rows"
    );
    Ok(())
}

fn declared_dev_data_access_grants(
    sources: &[std::path::PathBuf],
) -> anyhow::Result<BTreeSet<(String, String, String, String)>> {
    let mut expected = BTreeSet::new();
    for source in sources {
        let path = wamn_compiler::output_root(source).join("platform-policy/data-access.json");
        let policy: serde_json::Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        anyhow::ensure!(
            policy["role"] == "wamn_app",
            "{} does not declare the shared package application role",
            path.display()
        );
        let relations = policy["relations"]
            .as_array()
            .with_context(|| format!("{} carries a relations array", path.display()))?;
        for relation in relations {
            let schema = relation["schema"]
                .as_str()
                .context("data-access relation carries schema")?;
            let table = relation["table"]
                .as_str()
                .context("data-access relation carries table")?;
            for (field, privilege) in [
                ("select_fields", "SELECT"),
                ("insert_fields", "INSERT"),
                ("update_fields", "UPDATE"),
            ] {
                for column in relation[field]
                    .as_array()
                    .with_context(|| format!("{schema}.{table} carries {field}"))?
                {
                    expected.insert((
                        schema.to_owned(),
                        table.to_owned(),
                        column
                            .as_str()
                            .with_context(|| format!("{schema}.{table} {field} is a string"))?
                            .to_owned(),
                        privilege.to_owned(),
                    ));
                }
            }
            if relation["lock"].as_bool() == Some(true) {
                expected.insert((
                    schema.to_owned(),
                    table.to_owned(),
                    relation["lock_update_field"]
                        .as_str()
                        .with_context(|| format!("{schema}.{table} lock carries its carrier"))?
                        .to_owned(),
                    "UPDATE".to_owned(),
                ));
            }
        }
    }
    Ok(expected)
}

pub(super) async fn verify_dev_target_package_and_acl_state(
    project: &mut Client,
    sources: &[std::path::PathBuf],
    key: &str,
) -> anyhow::Result<()> {
    use sqlx::migrate::Migrate as _;
    for source in sources {
        let directory = wamn_package_control::package_source::read_package_directory(source)?;
        let metadata = wamn_catalog::package_schema::package_metadata(&directory)?;
        let expected = wamn_compiler::migrations::migrator(
            &wamn_package_control::package_source::migration_root(source),
        )
        .await?;
        let table =
            wamn_control_provision::migration::history_table(key, metadata.coordinate.package_id());
        let applied = project.list_applied_migrations(&table).await?;
        anyhow::ensure!(
            expected
                .iter()
                .filter(|file| file.migration_type.is_up_migration())
                .all(|file| applied
                    .iter()
                    .any(|row| row.version == file.version && row.checksum == file.checksum)),
            "dev SQLx history differs from its package sources"
        );
    }

    verify_journey_operation_grants(project).await?;

    let observed_column_grants = sqlx::query(
        "SELECT table_schema::text, table_name::text, column_name::text, privilege_type::text \
             FROM information_schema.column_privileges \
             WHERE grantee = 'wamn_app' AND table_schema = 'receiving' \
             ORDER BY table_schema COLLATE \"C\", table_name COLLATE \"C\", \
                      column_name COLLATE \"C\", privilege_type COLLATE \"C\"",
    )
    .fetch_all(&mut *project)
    .await
    .context("read the installed package data-access union")?
    .into_iter()
    .map(|row| {
        (
            row.get::<String, _>(0),
            row.get::<String, _>(1),
            row.get::<String, _>(2),
            row.get::<String, _>(3),
        )
    })
    .collect::<BTreeSet<_>>();
    let expected_column_grants = declared_dev_data_access_grants(sources)?;
    anyhow::ensure!(
        observed_column_grants == expected_column_grants,
        "wamn dev installed the wrong exact column-privilege union: {observed_column_grants:?}"
    );
    let table_grant_count: i64 = sqlx::query(
        "SELECT count(*) FROM information_schema.table_privileges \
             WHERE grantee = 'wamn_app' AND table_schema = 'receiving'",
    )
    .fetch_one(&mut *project)
    .await
    .context("read package application table-level privilege residue")?
    .get(0);
    anyhow::ensure!(
        table_grant_count == 0,
        "wamn dev left {table_grant_count} table-level package privilege grants"
    );
    let schema_usage: bool =
        sqlx::query("SELECT pg_catalog.has_schema_privilege('wamn_app', 'receiving', 'USAGE')")
            .fetch_one(&mut *project)
            .await
            .context("read package application schema usage")?
            .get(0);
    anyhow::ensure!(schema_usage, "wamn dev omitted receiving schema USAGE");
    Ok(())
}

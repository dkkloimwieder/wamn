// Receiving environment tests and helpers.

use super::*;

pub(super) fn package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/wamn_receiving")
}

pub(super) fn overlay_package_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/client_acme_receiving")
}

pub(super) fn journey_package_root(package: JourneyPackage) -> PathBuf {
    if package.id == BASE_PACKAGE_ID {
        package_root()
    } else {
        overlay_package_root()
    }
}

pub(super) fn required_journey(key: &str) -> anyhow::Result<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("set {key} for the disposable Receiving route journey"))
}

pub(super) fn required_journey_path(key: &str) -> anyhow::Result<PathBuf> {
    Ok(PathBuf::from(required_journey(key)?))
}

/// Applying the packages leaves the built-in `admin` role and no permission
/// row: `admin` holds every served operation without rows, and no other role is
/// authored yet (docs/architecture/execution.md#host-run-routes).
pub(super) async fn verify_journey_operation_grants(project: &Client) -> anyhow::Result<()> {
    let roles = project
        .query(
            "SELECT name FROM platform.role ORDER BY name COLLATE \"C\"",
            &[],
        )
        .await
        .context("read the installed roles")?
        .into_iter()
        .map(|row| row.get::<_, String>(0))
        .collect::<Vec<_>>();
    let permissions: i64 = project
        .query_one("SELECT count(*) FROM platform.permission", &[])
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

pub(super) fn repository_root() -> anyhow::Result<PathBuf> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .context("resolve the repository root for the product command")
}

pub(super) async fn seed_receiving_business_rows(project: &Client) -> anyhow::Result<()> {
    bind_fixture_principal(project).await?;
    project
        .batch_execute(
            "INSERT INTO receiving.item (id, item_number) VALUES \
               ('00000000-0000-0000-0000-000000000101', 'ITEM-101'); \
             INSERT INTO receiving.location (id, location_code) VALUES \
               ('00000000-0000-0000-0000-000000000201', 'DOCK-1'); \
             INSERT INTO receiving.supplier (id, name) VALUES \
               ('00000000-0000-0000-0000-000000000401', 'SUPPLIER-401'), \
               ('00000000-0000-0000-0000-000000000402', 'SUPPLIER-402'), \
               ('00000000-0000-0000-0000-000000000403', 'SUPPLIER-403') \
             ON CONFLICT ON CONSTRAINT supplier_id_pkey DO NOTHING; \
             INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version) \
             VALUES \
               ('00000000-0000-0000-0000-000000000301', 'PO-301', \
                '00000000-0000-0000-0000-000000000401', 'open', 1), \
               ('00000000-0000-0000-0000-000000000302', 'PO-302', \
                '00000000-0000-0000-0000-000000000402', 'open', 1); \
             INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) \
             VALUES \
               ('00000000-0000-0000-0000-000000000501', \
                '00000000-0000-0000-0000-000000000301', 1, \
                '00000000-0000-0000-0000-000000000101', 5.0000, 0.0000), \
               ('00000000-0000-0000-0000-000000000502', \
                '00000000-0000-0000-0000-000000000302', 1, \
                '00000000-0000-0000-0000-000000000101', 7.0000, 0.0000); \
             INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version, \
                acme_inspection_required, acme_quality_status) \
             VALUES \
               ('00000000-0000-0000-0000-000000000303', 'PO-303', \
                '00000000-0000-0000-0000-000000000403', 'complete', 2, \
                true, 'pending');",
        )
        .await
        .context("seed only Receiving business rows")
}

pub(super) async fn seed_materializer_order(
    project: &Client,
    order_id: &str,
    line_id: &str,
    number: &str,
) -> anyhow::Result<()> {
    bind_fixture_principal(project).await?;
    project
        .batch_execute(
            "INSERT INTO receiving.supplier (id, name) VALUES \
               ('00000000-0000-0000-0000-000000000404', 'SUPPLIER-404') \
             ON CONFLICT ON CONSTRAINT supplier_id_pkey DO NOTHING;",
        )
        .await
        .context("seed the materializer journey supplier")?;
    project
        .execute(
            "INSERT INTO receiving.purchase_order \
               (id, purchase_order_number, supplier_id, status, row_version) \
             VALUES ($1::text::uuid, $2, '00000000-0000-0000-0000-000000000404', 'open', 1)",
            &[&order_id, &number],
        )
        .await
        .context("seed the untouched materializer journey purchase order")?;
    project
        .execute(
            "INSERT INTO receiving.purchase_order_line \
               (id, purchase_order_id, line_number, item_id, ordered_quantity, received_quantity) \
             VALUES ($1::text::uuid, $2::text::uuid, 1, \
                     '00000000-0000-0000-0000-000000000101', 9.0000, 0.0000)",
            &[&line_id, &order_id],
        )
        .await
        .context("seed the untouched materializer journey purchase order line")?;
    Ok(())
}

//! Shared WMS rows for local business and deployed delivery tests.

use anyhow::Context as _;
use tokio_postgres::Client;
use wamn_gate_harness::journey::RuntimePhase;

const PRODUCT_ID: &str = "00000000-0000-0000-0000-000000000101";
pub(crate) const LOCATION_A_ID: &str = "00000000-0000-0000-0000-000000000201";
const LOCATION_B_ID: &str = "00000000-0000-0000-0000-000000000202";
pub(crate) const INVENTORY_ID: &str = "00000000-0000-0000-0000-000000000301";
pub(crate) const PACKAGING_A_ID: &str = "00000000-0000-0000-0000-000000000501";
pub(crate) const PACKAGING_B_ID: &str = "00000000-0000-0000-0000-000000000502";
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";

pub(crate) async fn seed_fixture(project: &Client, initial_revision: i64) -> anyhow::Result<()> {
    // The fixture writes as its test principal, whose row stamps itself.
    let tenant = crate::environment::TENANT;
    project.batch_execute(&format!(
        "BEGIN;\n\
         SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
                set_config('app.operation', 'admin:seed-wms-fixture', true);\n\
         INSERT INTO app_system.users (tenant_id, id, type, email) VALUES ('{tenant}', '{FIXTURE_PRINCIPAL}', 'person', 'fixture@example.invalid');\n\
         INSERT INTO wms.product (id, product_code) VALUES ('{PRODUCT_ID}', 'PROD-101');\n\
         INSERT INTO wms.location (id, location_code) VALUES ('{LOCATION_A_ID}', 'LOC-A'), ('{LOCATION_B_ID}', 'LOC-B');\n\
         INSERT INTO wms.packaging (id, type, code, location_id) VALUES ('{PACKAGING_A_ID}', 'tote', 'PKG-A', '{LOCATION_A_ID}'), ('{PACKAGING_B_ID}', 'carton', 'PKG-B', '{LOCATION_B_ID}');\n\
         INSERT INTO wms.inventory (id, product_id, packaging_id, location_id, quantity, disposition, row_version) VALUES ('{INVENTORY_ID}', '{PRODUCT_ID}', '{PACKAGING_A_ID}', '{LOCATION_A_ID}', 10, 'available', {initial_revision});\n\
         COMMIT;"
    )).await.context("seed the existing WMS application fixture")
}

pub(crate) fn runtime_phase(route_endpoint: String) -> RuntimePhase {
    RuntimePhase {
        route_endpoint,
        inventory_id: INVENTORY_ID.to_owned(),
        to_location_id: LOCATION_B_ID.to_owned(),
        to_packaging_id: PACKAGING_B_ID.to_owned(),
    }
}

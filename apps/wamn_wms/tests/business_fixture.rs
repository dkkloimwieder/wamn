//! Shared WMS rows for local business and deployed delivery tests.

use anyhow::Context as _;
use tokio_postgres::Client;
use wamn_gate_harness::journey::RuntimePhase;

const PRODUCT_ID: &str = "00000000-0000-0000-0000-000000000101";
pub(crate) const LOCATION_A_ID: &str = "00000000-0000-0000-0000-000000000201";
const LOCATION_B_ID: &str = "00000000-0000-0000-0000-000000000202";
pub(crate) const PALLET_ID: &str = "00000000-0000-0000-0000-000000000301";
const FIXTURE_PRINCIPAL: &str = "00000000-0000-4000-8000-0000000000f1";

pub(crate) async fn seed_fixture(project: &Client) -> anyhow::Result<()> {
    // The fixture writes as its test principal, whose row stamps itself.
    let tenant = crate::environment::TENANT;
    project.batch_execute(&format!(
        "BEGIN;\n\
         SELECT set_config('app.user_id', '{FIXTURE_PRINCIPAL}', true), \
                set_config('app.operation', 'admin:seed-wms-fixture', true);\n\
         INSERT INTO app_system.users (tenant_id, id, type, email) VALUES ('{tenant}', '{FIXTURE_PRINCIPAL}', 'person', 'fixture@example.invalid');\n\
         INSERT INTO wms.product (id, product_code) VALUES ('{PRODUCT_ID}', 'PROD-101');\n\
         INSERT INTO wms.location (id, location_code) VALUES ('{LOCATION_A_ID}', 'LOC-A'), ('{LOCATION_B_ID}', 'LOC-B');\n\
         INSERT INTO wms.pallet (id, pallet_code, location_id, status) VALUES ('{PALLET_ID}', 'PAL-301', '{LOCATION_A_ID}', 'available');\n\
         INSERT INTO wms.pallet_quantity (pallet_id, product_id, quantity, status) VALUES ('{PALLET_ID}', '{PRODUCT_ID}', 10, 'available');\n\
         COMMIT;"
    )).await.context("seed the existing WMS application fixture")
}

pub(crate) fn runtime_phase(route_endpoint: String) -> RuntimePhase {
    RuntimePhase {
        route_endpoint,
        pallet_id: PALLET_ID.to_owned(),
        to_location_id: LOCATION_B_ID.to_owned(),
    }
}

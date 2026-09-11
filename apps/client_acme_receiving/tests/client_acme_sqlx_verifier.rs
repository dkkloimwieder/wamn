//! Native SQLx compile verifier over the exact SQL consumed by generated native siblings.

#[expect(
    dead_code,
    reason = "compile-only SQLx verification owns generated fields and SQL references"
)]
mod client_acme_native {
    pub mod purchase_order {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/client_acme_receiving/generated/native-verifier/purchase_order.rs"
        ));
    }

    pub mod quality_approve_inspection {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/client_acme_receiving/generated/native-verifier/quality_approve_inspection.rs"
        ));
    }

    pub mod quality_create_inspection {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/client_acme_receiving/generated/native-verifier/quality_create_inspection.rs"
        ));
    }

    pub mod quality_load_purchase_order_detail {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/client_acme_receiving/generated/native-verifier/quality_load_purchase_order_detail.rs"
        ));
    }

    pub mod receiving_record_receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/client_acme_receiving/generated/native-verifier/receiving_record_receipt.rs"
        ));
    }
}

#[test]
fn client_acme_native_verifier_compiles_the_exact_runtime_sql_files() {
    let _ = sqlx::query_file_as!(
        client_acme_native::purchase_order::PurchaseOrderRow,
        "../../../apps/client_acme_receiving/generated/sql/purchase_order/get.sql",
        client_acme_native::purchase_order::get_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::purchase_order::PurchaseOrderUpdateRow,
        "../../../apps/client_acme_receiving/generated/sql/purchase_order/update.sql",
        client_acme_native::purchase_order::update_id_bind_fixture(),
        client_acme_native::purchase_order::update_expected_row_version_bind_fixture(),
        client_acme_native::purchase_order::update_acme_inspection_required_present_bind_fixture(),
        client_acme_native::purchase_order::update_acme_inspection_required_value_bind_fixture(),
        client_acme_native::purchase_order::update_acme_quality_status_present_bind_fixture(),
        client_acme_native::purchase_order::update_acme_quality_status_value_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::quality_load_purchase_order_detail::LoadPurchaseOrderDetailRow,
        "../../../apps/client_acme_receiving/query/quality_purchase_order_detail.sql",
        client_acme_native::quality_load_purchase_order_detail::
            load_purchase_order_detail_purchase_order_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::quality_approve_inspection::ApproveInspectionRow,
        "../../../apps/client_acme_receiving/command/approve_inspection/approve_inspection.sql",
        client_acme_native::quality_approve_inspection::
            approve_inspection_receipt_id_bind_fixture(),
        client_acme_native::quality_approve_inspection::
            approve_inspection_expected_row_version_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::quality_create_inspection::InsertInspectionRow,
        "../../../apps/client_acme_receiving/command/create_inspection/insert_inspection.sql",
        client_acme_native::quality_create_inspection::insert_inspection_receipt_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::quality_create_inspection::LoadInspectionRow,
        "../../../apps/client_acme_receiving/command/create_inspection/load_inspection.sql",
        client_acme_native::quality_create_inspection::load_inspection_receipt_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::receiving_record_receipt::LoadPurchaseOrderDetailRow,
        "../../../apps/client_acme_receiving/query/quality_purchase_order_detail.sql",
        client_acme_native::receiving_record_receipt::
            load_purchase_order_detail_purchase_order_id_bind_fixture()
    );
}

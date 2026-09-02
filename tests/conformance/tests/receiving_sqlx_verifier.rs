//! Native SQLx compile verifier over the exact SQL consumed by generated native siblings.

#[expect(
    dead_code,
    reason = "compile-only SQLx verification owns generated fields and SQL references"
)]
mod native {
    pub mod purchase_order {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packages/receiving/generated/native-verifier/purchase_order.rs"
        ));
    }

    pub mod receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packages/receiving/generated/native-verifier/receipt.rs"
        ));
    }

    pub mod receiving_record_receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packages/receiving/generated/native-verifier/receiving_record_receipt.rs"
        ));
    }
}

#[expect(
    dead_code,
    reason = "compile-only SQLx verification owns generated fields and SQL references"
)]
mod client_acme_native {
    pub mod purchase_order {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packages/client_acme_receiving/generated/native-verifier/purchase_order.rs"
        ));
    }

    pub mod quality_approve_inspection {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packages/client_acme_receiving/generated/native-verifier/quality_approve_inspection.rs"
        ));
    }

    pub mod quality_create_inspection {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packages/client_acme_receiving/generated/native-verifier/quality_create_inspection.rs"
        ));
    }

    pub mod quality_load_purchase_order_detail {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../packages/client_acme_receiving/generated/native-verifier/quality_load_purchase_order_detail.rs"
        ));
    }
}

#[test]
fn native_verifier_compiles_the_exact_runtime_sql_files() {
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../packages/receiving/generated/sql/purchase_order/get.sql",
        native::purchase_order::get_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../packages/receiving/query/open_purchase_order_by_purchase_order_number_ascending.sql",
        native::purchase_order::query_purchase_order_number_ascending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_purchase_order_number_ascending_status_filter_bind_fixture(),
        native::purchase_order::query_purchase_order_number_ascending_cursor_key_bind_fixture(),
        native::purchase_order::query_purchase_order_number_ascending_cursor_id_bind_fixture(),
        native::purchase_order::query_purchase_order_number_ascending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../packages/receiving/query/open_purchase_order_by_purchase_order_number_descending.sql",
        native::purchase_order::query_purchase_order_number_descending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_purchase_order_number_descending_status_filter_bind_fixture(),
        native::purchase_order::query_purchase_order_number_descending_cursor_key_bind_fixture(),
        native::purchase_order::query_purchase_order_number_descending_cursor_id_bind_fixture(),
        native::purchase_order::query_purchase_order_number_descending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../packages/receiving/query/open_purchase_order_by_status_ascending.sql",
        native::purchase_order::query_status_ascending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_status_ascending_status_filter_bind_fixture(),
        native::purchase_order::query_status_ascending_cursor_key_bind_fixture(),
        native::purchase_order::query_status_ascending_cursor_id_bind_fixture(),
        native::purchase_order::query_status_ascending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../packages/receiving/query/open_purchase_order_by_status_descending.sql",
        native::purchase_order::query_status_descending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_status_descending_status_filter_bind_fixture(),
        native::purchase_order::query_status_descending_cursor_key_bind_fixture(),
        native::purchase_order::query_status_descending_cursor_id_bind_fixture(),
        native::purchase_order::query_status_descending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../packages/receiving/query/open_purchase_order.sql",
        native::purchase_order::query_created_at_ascending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_created_at_ascending_status_filter_bind_fixture(),
        native::purchase_order::query_created_at_ascending_cursor_key_bind_fixture(),
        native::purchase_order::query_created_at_ascending_cursor_id_bind_fixture(),
        native::purchase_order::query_created_at_ascending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../packages/receiving/query/open_purchase_order_by_created_at_descending.sql",
        native::purchase_order::query_created_at_descending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_created_at_descending_status_filter_bind_fixture(),
        native::purchase_order::query_created_at_descending_cursor_key_bind_fixture(),
        native::purchase_order::query_created_at_descending_cursor_id_bind_fixture(),
        native::purchase_order::query_created_at_descending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderUpdateRow,
        "../../packages/receiving/generated/sql/purchase_order/update.sql",
        native::purchase_order::update_id_bind_fixture(),
        native::purchase_order::update_expected_row_version_bind_fixture(),
        native::purchase_order::update_supplier_id_present_bind_fixture(),
        native::purchase_order::update_supplier_id_value_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receipt::ReceiptRow,
        "../../packages/receiving/generated/sql/receipt/get.sql",
        native::receipt::get_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receipt::ReceiptRow,
        "../../packages/receiving/generated/sql/receipt/query_created_at_ascending.sql",
        native::receipt::query_created_at_ascending_cursor_key_bind_fixture(),
        native::receipt::query_created_at_ascending_cursor_id_bind_fixture(),
        native::receipt::query_created_at_ascending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::ClaimCommandRow,
        "../../packages/receiving/command/record_receipt/claim_command.sql",
        native::receiving_record_receipt::claim_command_idempotency_key_bind_fixture(),
        native::receiving_record_receipt::claim_command_canonical_command_bind_fixture(),
        native::receiving_record_receipt::claim_command_purchase_order_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::FinalizeCommandRow,
        "../../packages/receiving/command/record_receipt/finalize_command.sql",
        native::receiving_record_receipt::finalize_command_idempotency_key_bind_fixture(),
        native::receiving_record_receipt::finalize_command_canonical_command_bind_fixture(),
        native::receiving_record_receipt::finalize_command_receipt_id_bind_fixture(),
        native::receiving_record_receipt::finalize_command_purchase_order_status_bind_fixture(),
        native::receiving_record_receipt::finalize_command_row_version_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::FindReplayRow,
        "../../packages/receiving/command/record_receipt/find_replay.sql",
        native::receiving_record_receipt::find_replay_idempotency_key_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::FinishPurchaseOrderRow,
        "../../packages/receiving/command/record_receipt/finish_purchase_order.sql",
        native::receiving_record_receipt::finish_purchase_order_purchase_order_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::InsertReceiptRow,
        "../../packages/receiving/command/record_receipt/insert_receipt.sql",
        native::receiving_record_receipt::insert_receipt_receipt_id_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_idempotency_key_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_purchase_order_id_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_receipt_reference_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_occurred_at_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::InsertReceiptLineRow,
        "../../packages/receiving/command/record_receipt/insert_receipt_line.sql",
        native::receiving_record_receipt::insert_receipt_line_receipt_id_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_line_line_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::LockPurchaseOrderRow,
        "../../packages/receiving/command/record_receipt/lock_purchase_order.sql",
        native::receiving_record_receipt::lock_purchase_order_purchase_order_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::UpdatePurchaseOrderLineRow,
        "../../packages/receiving/command/record_receipt/update_purchase_order_line.sql",
        native::receiving_record_receipt::update_purchase_order_line_purchase_order_id_bind_fixture(
        ),
        native::receiving_record_receipt::update_purchase_order_line_line_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::ValidateReceiptLineRow,
        "../../packages/receiving/command/record_receipt/validate_receipt_line.sql",
        native::receiving_record_receipt::validate_receipt_line_purchase_order_id_bind_fixture(),
        native::receiving_record_receipt::validate_receipt_line_line_bind_fixture()
    );
}

#[test]
fn client_acme_native_verifier_compiles_the_exact_runtime_sql_files() {
    let _ = sqlx::query_file_as!(
        client_acme_native::purchase_order::PurchaseOrderRow,
        "../../packages/client_acme_receiving/generated/sql/purchase_order/get.sql",
        client_acme_native::purchase_order::get_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::purchase_order::PurchaseOrderUpdateRow,
        "../../packages/client_acme_receiving/generated/sql/purchase_order/update.sql",
        client_acme_native::purchase_order::update_id_bind_fixture(),
        client_acme_native::purchase_order::update_expected_row_version_bind_fixture(),
        client_acme_native::purchase_order::update_acme_inspection_required_present_bind_fixture(),
        client_acme_native::purchase_order::update_acme_inspection_required_value_bind_fixture(),
        client_acme_native::purchase_order::update_acme_quality_status_present_bind_fixture(),
        client_acme_native::purchase_order::update_acme_quality_status_value_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::quality_load_purchase_order_detail::LoadPurchaseOrderDetailRow,
        "../../packages/client_acme_receiving/query/quality_purchase_order_detail.sql",
        client_acme_native::quality_load_purchase_order_detail::
            load_purchase_order_detail_purchase_order_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::quality_approve_inspection::ApproveInspectionRow,
        "../../packages/client_acme_receiving/command/approve_inspection/approve_inspection.sql",
        client_acme_native::quality_approve_inspection::
            approve_inspection_receipt_id_bind_fixture(),
        client_acme_native::quality_approve_inspection::
            approve_inspection_expected_row_version_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::quality_create_inspection::InsertInspectionRow,
        "../../packages/client_acme_receiving/command/create_inspection/insert_inspection.sql",
        client_acme_native::quality_create_inspection::insert_inspection_receipt_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        client_acme_native::quality_create_inspection::LoadInspectionRow,
        "../../packages/client_acme_receiving/command/create_inspection/load_inspection.sql",
        client_acme_native::quality_create_inspection::load_inspection_receipt_id_bind_fixture()
    );
    // `receiving.record_receipt` composes the exact base operation and owns no local SQL.
}

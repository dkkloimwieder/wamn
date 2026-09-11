//! Native SQLx compile verifier over the exact SQL consumed by generated native siblings.

#[expect(
    dead_code,
    reason = "compile-only SQLx verification owns generated fields and SQL references"
)]
mod native {
    pub mod location_list {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/wamn_receiving/generated/native-verifier/location_list.rs"
        ));
    }

    pub mod purchase_order {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/wamn_receiving/generated/native-verifier/purchase_order.rs"
        ));
    }

    pub mod receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/wamn_receiving/generated/native-verifier/receipt.rs"
        ));
    }

    pub mod receiving_record_receipt {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/wamn_receiving/generated/native-verifier/receiving_record_receipt.rs"
        ));
    }

    pub mod receiving_load_receipt_screen {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../apps/wamn_receiving/generated/native-verifier/receiving_load_receipt_screen.rs"
        ));
    }
}

#[test]
fn native_verifier_compiles_the_exact_runtime_sql_files() {
    let _ = sqlx::query_file_as!(
        native::location_list::ListLocationsRow,
        "../../../apps/wamn_receiving/query/location.sql"
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../../apps/wamn_receiving/generated/sql/purchase_order/get.sql",
        native::purchase_order::get_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../../apps/wamn_receiving/query/open_purchase_order_by_purchase_order_number_ascending.sql",
        native::purchase_order::query_purchase_order_number_ascending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_purchase_order_number_ascending_status_filter_bind_fixture(),
        native::purchase_order::query_purchase_order_number_ascending_cursor_key_bind_fixture(),
        native::purchase_order::query_purchase_order_number_ascending_cursor_id_bind_fixture(),
        native::purchase_order::query_purchase_order_number_ascending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../../apps/wamn_receiving/query/open_purchase_order_by_purchase_order_number_descending.sql",
        native::purchase_order::query_purchase_order_number_descending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_purchase_order_number_descending_status_filter_bind_fixture(),
        native::purchase_order::query_purchase_order_number_descending_cursor_key_bind_fixture(),
        native::purchase_order::query_purchase_order_number_descending_cursor_id_bind_fixture(),
        native::purchase_order::query_purchase_order_number_descending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../../apps/wamn_receiving/query/open_purchase_order_by_status_ascending.sql",
        native::purchase_order::query_status_ascending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_status_ascending_status_filter_bind_fixture(),
        native::purchase_order::query_status_ascending_cursor_key_bind_fixture(),
        native::purchase_order::query_status_ascending_cursor_id_bind_fixture(),
        native::purchase_order::query_status_ascending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../../apps/wamn_receiving/query/open_purchase_order_by_status_descending.sql",
        native::purchase_order::query_status_descending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_status_descending_status_filter_bind_fixture(),
        native::purchase_order::query_status_descending_cursor_key_bind_fixture(),
        native::purchase_order::query_status_descending_cursor_id_bind_fixture(),
        native::purchase_order::query_status_descending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../../apps/wamn_receiving/query/open_purchase_order.sql",
        native::purchase_order::query_created_at_ascending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_created_at_ascending_status_filter_bind_fixture(),
        native::purchase_order::query_created_at_ascending_cursor_key_bind_fixture(),
        native::purchase_order::query_created_at_ascending_cursor_id_bind_fixture(),
        native::purchase_order::query_created_at_ascending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderRow,
        "../../../apps/wamn_receiving/query/open_purchase_order_by_created_at_descending.sql",
        native::purchase_order::query_created_at_descending_supplier_id_filter_bind_fixture(),
        native::purchase_order::query_created_at_descending_status_filter_bind_fixture(),
        native::purchase_order::query_created_at_descending_cursor_key_bind_fixture(),
        native::purchase_order::query_created_at_descending_cursor_id_bind_fixture(),
        native::purchase_order::query_created_at_descending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::purchase_order::PurchaseOrderUpdateRow,
        "../../../apps/wamn_receiving/generated/sql/purchase_order/update.sql",
        native::purchase_order::update_id_bind_fixture(),
        native::purchase_order::update_expected_row_version_bind_fixture(),
        native::purchase_order::update_supplier_id_present_bind_fixture(),
        native::purchase_order::update_supplier_id_value_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receipt::ReceiptRow,
        "../../../apps/wamn_receiving/generated/sql/receipt/get.sql",
        native::receipt::get_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receipt::ReceiptRow,
        "../../../apps/wamn_receiving/generated/sql/receipt/query_created_at_ascending.sql",
        native::receipt::query_created_at_ascending_cursor_key_bind_fixture(),
        native::receipt::query_created_at_ascending_cursor_id_bind_fixture(),
        native::receipt::query_created_at_ascending_limit_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::ClaimCommandRow,
        "../../../apps/wamn_receiving/command/record_receipt/claim_command.sql",
        native::receiving_record_receipt::claim_command_idempotency_key_bind_fixture(),
        native::receiving_record_receipt::claim_command_canonical_command_bind_fixture(),
        native::receiving_record_receipt::claim_command_purchase_order_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::FinalizeCommandRow,
        "../../../apps/wamn_receiving/command/record_receipt/finalize_command.sql",
        native::receiving_record_receipt::finalize_command_idempotency_key_bind_fixture(),
        native::receiving_record_receipt::finalize_command_canonical_command_bind_fixture(),
        native::receiving_record_receipt::finalize_command_receipt_id_bind_fixture(),
        native::receiving_record_receipt::finalize_command_purchase_order_status_bind_fixture(),
        native::receiving_record_receipt::finalize_command_row_version_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::FindReplayRow,
        "../../../apps/wamn_receiving/command/record_receipt/find_replay.sql",
        native::receiving_record_receipt::find_replay_idempotency_key_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::FinishPurchaseOrderRow,
        "../../../apps/wamn_receiving/command/record_receipt/finish_purchase_order.sql",
        native::receiving_record_receipt::finish_purchase_order_purchase_order_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::InsertReceiptRow,
        "../../../apps/wamn_receiving/command/record_receipt/insert_receipt.sql",
        native::receiving_record_receipt::insert_receipt_receipt_id_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_idempotency_key_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_purchase_order_id_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_receipt_reference_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_occurred_at_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::InsertReceiptLineRow,
        "../../../apps/wamn_receiving/command/record_receipt/insert_receipt_line.sql",
        native::receiving_record_receipt::insert_receipt_line_receipt_id_bind_fixture(),
        native::receiving_record_receipt::insert_receipt_line_line_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::LockPurchaseOrderRow,
        "../../../apps/wamn_receiving/command/record_receipt/lock_purchase_order.sql",
        native::receiving_record_receipt::lock_purchase_order_purchase_order_id_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::UpdatePurchaseOrderLineRow,
        "../../../apps/wamn_receiving/command/record_receipt/update_purchase_order_line.sql",
        native::receiving_record_receipt::update_purchase_order_line_purchase_order_id_bind_fixture(
        ),
        native::receiving_record_receipt::update_purchase_order_line_line_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_record_receipt::ValidateReceiptLineRow,
        "../../../apps/wamn_receiving/command/record_receipt/validate_receipt_line.sql",
        native::receiving_record_receipt::validate_receipt_line_purchase_order_id_bind_fixture(),
        native::receiving_record_receipt::validate_receipt_line_line_bind_fixture()
    );
    let _ = sqlx::query_file_as!(
        native::receiving_load_receipt_screen::LoadReceiptScreenRow,
        "../../../apps/wamn_receiving/query/load_receipt_screen.sql",
        native::receiving_load_receipt_screen::load_receipt_screen_purchase_order_id_bind_fixture()
    );
}

//! Native SQLx compile verifier over the exact shipped Receiving SQL corpus.

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
}

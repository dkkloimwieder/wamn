//! Exercise the guest numbering API over the host's real operation transaction.
#![expect(unsafe_code, reason = "wit-bindgen emits native and Wasm FFI")]

mod bindings {
    wit_bindgen::generate!({
        world: "wamn:postgres-statements/numbering-refusal-test@0.3.0",
        path: ["../../wit/wamn-postgres-0.4", "wit"],
        generate_all,
        // The example uses wamn_guest's SQL imports.
        skip: ["[method]transaction.query", "[method]transaction.query-batch", "operation-transaction"],
    });
}

struct Component;

impl bindings::Guest for Component {
    async fn run() -> (bool, bool) {
        use wamn_guest::postgres_statements::{StatementErrorType, next_number, operation_transaction};
        let mut transaction = operation_transaction().await.expect("host transaction");
        let refused = next_number(&mut transaction, "dated", &[])
            .await
            .is_err_and(|error| error.type_() == StatementErrorType::QueryError);
        let continued = next_number(&mut transaction, "bare", &[]).await.is_ok();
        (refused, continued)
    }
}

bindings::export!(Component with_types_in bindings);

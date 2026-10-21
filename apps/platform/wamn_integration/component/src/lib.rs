//! Standard integration settings and retained delivery outcomes.
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits equal length and capacity"
)]

wit_bindgen::generate!({
    world: "wamn-integration:component/wamn-integration@0.1.0",
    path: [
        "../../../../crates/execution/workflow/router/wit",
        "../../../../crates/platform/runtime/wit/deps/wamn-postgres-0.4",
        "../../../target/wamn/wamn_integration/wit",
        "wit",
    ],
    generate_all,
    async: true,
});

struct Component;
export!(Component);

mod log;
mod setting;

fn statement_error(error: &wamn_postgres_statements::StatementError) -> &'static str {
    use wamn_postgres_statements::StatementErrorType;
    match error.type_() {
        StatementErrorType::SerializationFailure | StatementErrorType::ConnectionUnavailable => {
            "retry"
        }
        StatementErrorType::StatementTimeout => "timeout",
        _ => "internal_error",
    }
}

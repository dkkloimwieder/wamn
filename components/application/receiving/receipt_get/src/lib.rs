#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen 0.44 emits Vec::from_raw_parts with equal length and capacity"
)]

//! One-operation Receiving artifact.

async fn invoke_operation(
    input: &str,
) -> Result<String, wamn_receiving_data_access::operation::InvocationError> {
    wamn_receiving_data_access::operation::receipt_get(input).await
}

include!("../../component.rs");

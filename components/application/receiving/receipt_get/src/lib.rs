//! One-operation Receiving artifact.

async fn invoke_operation(
    input: &str,
) -> Result<String, wamn_receiving_data_access::operation::InvocationError> {
    wamn_receiving_data_access::operation::receipt_get(input).await
}

include!("../../component.rs");

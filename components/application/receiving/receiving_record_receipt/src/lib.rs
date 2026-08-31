//! One-operation Receiving artifact.

async fn invoke_operation(
    input: &str,
) -> Result<String, wamn_receiving_data_access::operation::InvocationError> {
    wamn_receiving_data_access::operation::receiving_record_receipt(input).await
}

include!("../../component.rs");

use wamn_platform_fixture_data_access::generated::sequences::{label, pick_label};
use wamn_postgres_statements::Transaction;

use crate::exports::platform_fixture::number::issue as contract;

mod codec {
    use super::contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../target/wamn/platform_fixture/wit/number_issue_codec.rs"
    ));
}

async fn handle(
    transaction: &mut Transaction,
    request: contract::IssueRequest,
) -> Result<contract::IssueResult, contract::IssueError> {
    let key = pick_label::PickLabelKey {
        batch_id: request.batch_id,
    };
    let result = async {
        let records = wamn_platform_fixture_data_access::count_widgets(transaction).await?;
        Ok::<_, wamn_postgres_statements::StatementError>(contract::IssueResult {
            records,
            bare: label::next().await?,
            bare_peek: label::peek().await?,
            keyed: pick_label::next(key.clone()).await?,
            keyed_peek: pick_label::peek(key).await?,
        })
    }
    .await;
    result.map_err(|_| codec::map_error("internal_error", |_| None))
}

codec::export_operation!(
    crate::Component,
    contract,
    crate::wamn::node::types,
    handle,
    codec
);

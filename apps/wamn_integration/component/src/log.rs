//! Append one outcome, retaining the existing row on redelivery.
use super::exports::wamn_integration::log::append as contract;
use wamn_postgres_statements::{Json, Transaction};
mod operation {
    use super::contract;
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../target/wamn/wamn_integration/wit/log_append_operation.rs"
    ));
}
#[expect(
    dead_code,
    reason = "an append reads only the record_id its insert returns; the statement also returns delivery_id and outcome"
)]
mod sql {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../target/wamn/wamn_integration/wamn/log_append.rs"
    ));
}

fn appended(record_id: String, row: Option<sql::AppendLogRow>) -> contract::AppendResult {
    contract::AppendResult {
        record_id: row.map_or(record_id, |row| row.record_id),
    }
}

async fn append(
    transaction: &mut Transaction,
    request: contract::AppendRequest,
) -> Result<contract::AppendResult, super::NodeError> {
    let row = sql::append_log(
        transaction,
        request.delivery_id,
        request.record_id.clone(),
        request.outcome,
        request.detail.map(|value| Json(value.encoded_json)),
    )
    .await
    .map_err(|error| super::statement_error(&error))?;
    Ok(appended(request.record_id, row))
}
export_operation!(
    contract,
    operation,
    AppendItem,
    AppendOutcome,
    AppendError,
    wamn_postgres_statements::operation_transaction()
        .await
        .map_err(|error| super::statement_error(&error))?,
    append
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_duplicate_append_returns_the_same_record_id() {
        assert_eq!(
            appended(
                "item-1".into(),
                Some(sql::AppendLogRow {
                    delivery_id: "d".into(),
                    record_id: "item-1".into(),
                    outcome: "pushed".into()
                })
            )
            .record_id,
            appended("item-1".into(), None).record_id
        );
    }

    #[test]
    fn only_declared_outcomes_are_accepted() {
        use std::future::Future as _;
        use std::task::{Context, Poll, Waker};
        for outcome in ["pushed", "refused", "vendor_refused"] {
            let input = vec![contract::AppendItem {
                request_id: "r".into(),
                input: Ok(contract::AppendRequest {
                    idempotency_key: "r".into(),
                    delivery_id: "d".into(),
                    record_id: "i".into(),
                    outcome: outcome.into(),
                    detail: None,
                }),
            }];
            let mut state = ();
            let future = operation::run(input, &mut state, async |(), request| {
                Ok(contract::AppendResult {
                    record_id: request.record_id,
                })
            });
            let Poll::Ready(results) =
                std::pin::pin!(future).poll(&mut Context::from_waker(Waker::noop()))
            else {
                panic!("the in-memory handler completes immediately");
            };
            assert_eq!(results[0].outcome.is_ok(), outcome != "vendor_refused");
        }
    }
}

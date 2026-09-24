// @generated; do not edit.
#![expect(
    clippy::same_length_and_capacity,
    reason = "wit-bindgen emits Vec::from_raw_parts with equal length and capacity"
)]

//! The no-op participant of `wamn-receiving:receiving/record-receipt-pre-commit@1.0.0`.
//!
//! Composition plugs it into the base's optional pre-commit slot when an
//! overlay names no participant. It returns its input unchanged.

use exports::wamn_receiving::receiving::record_receipt_pre_commit::{
    Guest, RecordReceiptPreCommitRequest,
};
use wamn::node::types::{NodeContext, NodeError};

wit_bindgen::generate!({
    world: "wamn-receiving:receiving-record-receipt-pre-commit-no-op/no-op@1.0.0",
    inline: r"
        package wamn-receiving:receiving-record-receipt-pre-commit-no-op@1.0.0;

        world no-op {
          export wamn-receiving:receiving/record-receipt-pre-commit@1.0.0;
        }
    ",
    path: [
        "../../../../crates/execution/router/wit",
        "../wit/deps/wamn-receiving-receiving",
    ],
    generate_all,
    async: true,
});

struct Component;

impl Guest for Component {
    fn run(
        _context: NodeContext,
        input: RecordReceiptPreCommitRequest,
    ) -> impl Future<Output = Result<RecordReceiptPreCommitRequest, NodeError>> {
        std::future::ready(Ok(input))
    }
}

export!(Component);

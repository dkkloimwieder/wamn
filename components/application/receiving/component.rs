use exports::wamn::node::handler::{Emission, Guest, NodeContext, NodeError};
use wamn::node::types::ErrorDetail;

wit_bindgen::generate!({
    world: "receiving-operation",
    path: "../../../data/receiving-data/wit",
    generate_all,
});

struct Component;

impl Guest for Component {
    fn run(_context: NodeContext, input: String) -> Result<Emission, NodeError> {
        futures_executor::block_on(invoke_operation(&input))
            .map(|payload| Emission {
                payload,
                port: None,
            })
            .map_err(|error| {
                NodeError::InvalidInput(ErrorDetail {
                    message: error.context().to_owned(),
                    code: Some(error.code().to_owned()),
                })
            })
    }
}

export!(Component);

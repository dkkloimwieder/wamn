//! Regenerate the published connection contract JSON Schema:
//!
//! ```sh
//! cargo run -p wamn-node-manifest --example print-connection-contract-schema > docs/contracts/wamn-connection-contract.schema.json
//! ```

fn main() {
    print!(
        "{}",
        wamn_node_manifest::connection_contract_json_schema_string()
    );
}

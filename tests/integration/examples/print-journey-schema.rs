//! Regenerate the checked-in journey document schema:
//!
//! ```sh
//! cargo run --locked --offline -p wamn-integration-tests --example print-journey-schema \
//!   > tests/integration/schema/wamn-journey.schema.json
//! ```

use std::io::Write as _;

fn main() -> std::io::Result<()> {
    std::io::stdout().write_all(&wamn_gate_harness::journey::journey_document_schema_bytes())
}

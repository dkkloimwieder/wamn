//! Regenerate the checked-in `wamn dev` configuration schema:
//!
//! ```sh
//! cargo run --locked --offline -p wamn-ctl --example print-dev-config-schema \
//!   > crates/control/dev/schema/wamn-dev.schema.json
//! ```

use std::io::Write as _;

fn main() -> std::io::Result<()> {
    std::io::stdout().write_all(&wamn_dev_control::dev::config::dev_config_schema_bytes())
}

//! Print the DDL for a catalog's outbox row-event triggers (and optionally the
//! tenant floor first) — for provisioning demos and manual project setup:
//!
//! ```text
//! cargo run -p wamn-ddl --example emit-outbox -- <catalog.json> [outbox-schema] [--create]
//! ```
//!
//! `outbox-schema` defaults to `wamn_run` (deploy/run-queue.sql); `--create`
//! prepends the whole `CREATE` plan so the output is a complete provisioning
//! script for a fresh project schema (run it under the executor's
//! `search_path`).

use wamn_catalog::Catalog;
use wamn_ddl::{Confirmation, Migration, OutboxOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (flags, positional): (Vec<_>, Vec<_>) = args.iter().partition(|a| a.starts_with("--"));
    let path = positional
        .first()
        .ok_or("usage: emit-outbox <catalog.json> [outbox-schema] [--create]")?;
    let mut options = OutboxOptions::default();
    if let Some(schema) = positional.get(1) {
        options.schema = schema.to_string();
    }

    let catalog = Catalog::from_json(&std::fs::read_to_string(path)?)?;
    if flags.iter().any(|f| *f == "--create") {
        print!("{}", Migration::create(&catalog)?.sql(Confirmation::None)?);
    }
    print!(
        "{}",
        Migration::outbox_triggers(&catalog, &options)?.sql(Confirmation::None)?
    );
    Ok(())
}

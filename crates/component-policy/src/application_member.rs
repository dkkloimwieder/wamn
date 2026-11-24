use anyhow::{Context as _, bail};
use wasmparser::{Parser, Payload};

/// Refuse entry authority anywhere in an application member's component tree.
pub fn validate_application_member(bytes: &[u8]) -> anyhow::Result<()> {
    for payload in Parser::new(0).parse_all(bytes) {
        match payload.context("read application member")? {
            Payload::ComponentImportSection(imports) => {
                for import in imports {
                    let import = import.context("read application member import")?;
                    let interface = crate::instance_type(import.name.name, import.name.implements);
                    if crate::import_pkg(interface) == "wamn:operation" {
                        bail!("application member imports entry-only package wamn:operation");
                    }
                }
            }
            Payload::ComponentExportSection(exports) => {
                for export in exports {
                    let export = export.context("read application member export")?;
                    if export.name.name.split('@').next() == Some("wasi:http/handler") {
                        bail!("application member supplies its own wasi:http/handler");
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
